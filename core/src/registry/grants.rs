//! capability 承認とダッシュボードウィジェットの状態管理。
//!
//! Phase 4 タスク7の move-only コミットで `crate::plugin::registry::Registry`
//! から抽出した。分析(`docs/superpowers/specs/2026-07-31-phase4-registry-analysis.md`
//! §5)のとおり、spec 自体には dashboard 群の置き場が無いが、実体は
//! 「grants(承認の読み書き)+ `is_file` の 1 発チェック」なので、grants と
//! 同じサービスへ同居させる。
//!
//! タスク8で `set_capabilities`(+ `effective_hosts`)だけを
//! `registry::sidecar::SidecarEntry` 越しにジェネリック化し、
//! `crate::driver::registry::DriverRegistry::set_capabilities` もこのサービス
//! に統合した(driver 版と byte 同一だったのは分析 §3 のとおり: 差は
//! projection(`RegistrySubject::as_settings_manifest`)とエラー enum 変換の
//! 2点だけ、どちらも呼び出し側 wrapper の責務)。dashboard 群は引き続き
//! **plugin 専用**なので、`impl<G: GrantStorage> GrantService<G, PluginEntry>`
//! に閉じ込めて `PluginEntry` を具象のまま扱う
//! (`.claude/rules/trait-di.md` の「必要が実証されたときだけ増やす」)。
//!
//! `SidecarEntry` を再利用したのは、`manifest()`/`capabilities_json()`/
//! `sidecars_json()` の3点が `set_capabilities` に必要な面と完全に一致する
//! ため(新しい entry trait を増やさない)。
//!
//! `capabilities_lock` はコンストラクタ注入の共有 `Arc<Mutex<()>>`。plugin 側
//! `Registry`/driver 側 `DriverRegistry` は、それぞれ自分自身が
//! `refresh_sidecar_runtime`(`SidecarService` 側)で使うのと**同一の** `Arc`
//! を、自分の `GrantService` にも渡す(`registry::sidecar::SidecarService`
//! のドキュメントコメント参照。plugin と driver で `Arc` を共有するわけでは
//! ない)。`set_capabilities` は `capabilities_lock` だけを取り id 別ロックにも
//! そのマップにも触れないため、ロック取得順序(id 別ロック →
//! `capabilities_lock` の一方向のみ)を保ったまま両者を共存させられる。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::capability::GrantStorage;
use crate::capability::grants::GrantState;
use crate::host::plugin::{capabilities_json_string, parse_capability_hosts};
use crate::registry::plugin::{DashboardInfo, PluginEntry, PluginState, RegistryError};
use crate::runtime::sidecar::{parse_sidecars, SidecarRuntimeEntry};
use crate::manifest::Manifest;
use crate::capability::request::CapabilityRequest;
use crate::registry::entries::EntryTable;
use crate::registry::sidecar::{merged_effective_hosts, SidecarEntry};
use crate::registry::subject::RegistrySubject;

/// capability 承認群(`capabilities` / `set_capabilities` / `effective_hosts`)
/// と、ダッシュボードウィジェット群(`dashboard` / `set_dashboard_grant` /
/// `dashboard_widgets_for_ui` / `dashboard_asset_path` / `events_of` /
/// `build_dashboard_infos`)を束ねるサービス。
///
/// `G: GrantStorage` はディスク実装(`GrantsStore`)を挿すためのジェネリクス。
/// `E: SidecarEntry` は plugin/driver どちらの `EntryTable` 要素も受け付ける
/// ためのジェネリクス(`set_capabilities`/`effective_hosts` 用。dashboard 群は
/// `PluginEntry` 限定 -- 下の `impl` 参照)。
///
/// `plugins_dir` は `build_dashboard_infos`/`dashboard_asset_path` の
/// `is_file()` チェックと entry パス組み立て専用(元の `Registry` から
/// クローンを受け取る -- `Registry` 自身は `plugins_dir()` アクセサ用に
/// フィールドを引き続き保持する)。driver 側インスタンスは dashboard 群を
/// 使わないため実質未使用だが、`drivers_dir` を渡すだけでコストは無い。
pub(crate) struct GrantService<G: GrantStorage, E: SidecarEntry> {
    entries: EntryTable<E>,
    grants_store: Arc<G>,
    capabilities_lock: Arc<Mutex<()>>,
    plugins_dir: PathBuf,
}

/// 手書き `Clone`: `derive(Clone)` は `G`/`E` 自体に `Clone` を要求してしまうが、
/// 実際に clone が要るのは `Arc`/`EntryTable`/`PathBuf`(いずれも中身の型に
/// 関わらず `Clone`)だけなので、要らない境界を足さないよう手で書く
/// (`BusService`/`SidecarService` と同じ理由)。
impl<G: GrantStorage, E: SidecarEntry> Clone for GrantService<G, E> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            grants_store: self.grants_store.clone(),
            capabilities_lock: self.capabilities_lock.clone(),
            plugins_dir: self.plugins_dir.clone(),
        }
    }
}

/// ディスク実装(`GrantsStore`)を挿した公開面。plugin/driver どちらの
/// `EntryTable` 要素かは呼び出し側が `E` で指定する。
pub(crate) type DiskGrantService<E> = GrantService<crate::capability::grants::GrantsStore, E>;

impl<G: GrantStorage, E: SidecarEntry> GrantService<G, E> {
    pub(crate) fn new(
        entries: EntryTable<E>,
        grants_store: Arc<G>,
        capabilities_lock: Arc<Mutex<()>>,
        plugins_dir: PathBuf,
    ) -> Self {
        Self {
            entries,
            grants_store,
            capabilities_lock,
            plugins_dir,
        }
    }

    /// `id` の manifest クローンを返す(`entries` ロック保持はこの
    /// ルックアップの間だけ)。未登録 id のエラーは `E::Subject::unknown_error`
    /// に委ねる(`UnknownPlugin` vs `UnknownDriver`)。
    fn find_manifest(&self, id: &str) -> Result<E::Subject, RegistryError> {
        self.entries
            .find(
                |entry| entry.manifest().id() == id,
                |entry| entry.manifest().clone(),
            )
            .ok_or_else(|| E::Subject::unknown_error(id))
    }

    /// `id` のプラグイン/ドライバの capability 承認/取消を `GrantsStore` に
    /// 永続化し、稼働中プラグイン/ドライバが参照する共有 `capabilities_json`
    /// も更新する。
    ///
    /// `entries` ロックは manifest と `capabilities_json`/`sidecars_json` の
    /// 共有ハンドルを取得する間だけ保持し、`GrantsStore::set` のファイル I/O
    /// はロックを解放した後に行う(他プラグイン/ドライバの
    /// settings/capabilities 操作や `set_disabled` までブロックしないため)。
    ///
    /// 一方で、「ディスクへの永続化」と「共有バッファへの反映」の 2 ステップは
    /// 同じ呼び出しの中で不可分に行う必要がある。呼び出しごとに
    /// `capabilities_lock` を取り、`GrantsStore::set` の呼び出しから
    /// `capabilities_json` バッファへの書き込みまでを 1 つの臨界区間として
    /// 保持する(交互実行による fail-open な不整合を避ける -- 分析 §6
    /// リスク2)。
    ///
    /// `capabilities_json` は `refresh_sidecar_runtime`(`SidecarService` 側)
    /// とも書き込み先を共有しており、同じ `capabilities_lock` を取ってから
    /// 書くので両者の書き込みが交互実行で食い違うことはない
    /// (`SidecarService::refresh_sidecar_runtime` のドキュメントコメント
    /// 参照。ロック取得順序は常に「id 別ロック → `capabilities_lock`」の
    /// 一方向のみで、この関数は `capabilities_lock` だけを取り id 別ロックに
    /// もそのマップにも触れないため、両者を合わせてもデッドロックしない)。
    /// この関数自身はサイドカーの設定/承認を変更しないので、現在の
    /// `sidecars_json` バッファをそのまま読み(**再計算はしない**)、そこから
    /// 暗黙許可ホストを合流させる。
    pub(crate) fn set_capabilities(
        &self,
        id: &str,
        granted: bool,
    ) -> Result<GrantState, RegistryError> {
        let (subject, capabilities_json, sidecars_json) = self
            .entries
            .find(
                |entry| entry.manifest().id() == id,
                |entry| {
                    (
                        entry.manifest().clone(),
                        entry.capabilities_json().clone(),
                        entry.sidecars_json().clone(),
                    )
                },
            )
            .ok_or_else(|| E::Subject::unknown_error(id))?;

        let _capabilities_guard = self
            .capabilities_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let settings_manifest = subject.as_settings_manifest();
        let state = self
            .grants_store
            .set(&settings_manifest, granted)
            .map_err(RegistryError::Grants)?;

        let sidecar_entries: Vec<SidecarRuntimeEntry> = parse_sidecars(
            &sidecars_json
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
        .into_values()
        .collect();
        let effective_hosts = merged_effective_hosts(
            state.granted,
            settings_manifest.capability_hosts(),
            &sidecar_entries,
        );

        let capabilities_json_string = capabilities_json_string(&effective_hosts);
        *capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = capabilities_json_string;

        Ok(state)
    }

    /// `id` のプラグイン/ドライバの `capabilities_json` 共有バッファが現在
    /// 載せている実効許可ホストを返す(テスト用アクセサ)。
    /// `driver-http.send` が実際に参照するのと同じ値。
    pub(crate) fn effective_hosts(&self, id: &str) -> Result<Vec<String>, RegistryError> {
        let capabilities_json = self
            .entries
            .find(
                |entry| entry.manifest().id() == id,
                |entry| entry.capabilities_json().clone(),
            )
            .ok_or_else(|| E::Subject::unknown_error(id))?;
        let raw = capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Ok(parse_capability_hosts(&raw))
    }
}

/// dashboard 群(`capabilities` の読み出しも含む)は **plugin 専用**。
/// `PluginEntry` に閉じ込めることで、`E::Subject` が常に `Manifest` である
/// ことを型で保証する(`DashboardInfo`/`CapabilityRequest` は `Manifest` 前提
/// の型のため)。
impl<G: GrantStorage> GrantService<G, PluginEntry> {
    /// `manifest.dashboard` の宣言順に `DashboardInfo` を組み立てる。承認は
    /// `GrantsStore::dashboard_state` から、`resolved` は `plugins_dir` 配下の
    /// entry ファイルが実在するかどうかから決める。
    pub(crate) fn build_dashboard_infos(&self, manifest: &Manifest) -> Vec<DashboardInfo> {
        manifest
            .dashboard
            .iter()
            .map(|widget| {
                let grant = self.grants_store.dashboard_state(manifest, &widget.id);
                let resolved = self
                    .plugins_dir
                    .join(&manifest.id)
                    .join(&widget.entry)
                    .is_file();
                DashboardInfo {
                    request: widget.clone(),
                    grant,
                    resolved,
                }
            })
            .collect()
    }

    /// `id` の manifest が宣言する `events` フィルタ(`dashboard/list` が
    /// ウィジェットへのイベント転送範囲を UI に伝えるのに使う)。
    pub(crate) fn events_of(&self, id: &str) -> Result<Vec<String>, RegistryError> {
        Ok(self.find_manifest(id)?.events.clone())
    }

    /// `id` のダッシュボードウィジェット一覧(UI 表示用)。
    pub(crate) fn dashboard(&self, id: &str) -> Result<Vec<DashboardInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        Ok(self.build_dashboard_infos(&manifest))
    }

    /// ダッシュボードウィジェット 1 件の承認/取消。`set_bus_grant` と同じ
    /// 流儀で、更新後のウィジェット一覧全体を返す(UI が 1 往復で
    /// リスト全体を更新できるように)。
    pub(crate) fn set_dashboard_grant(
        &self,
        id: &str,
        widget: &str,
        granted: bool,
    ) -> Result<Vec<DashboardInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        if manifest.dashboard_widget(widget).is_none() {
            return Err(RegistryError::UnknownDashboard(widget.to_string()));
        }
        self.grants_store
            .set_dashboard(&manifest, widget, granted)
            .map_err(RegistryError::Grants)?;
        Ok(self.build_dashboard_infos(&manifest))
    }

    /// `dashboard/list` 用: 全プラグインの全ウィジェット
    /// (`(plugin_id, plugin_name, state, info)`)。grant の有無での絞り込みは
    /// 呼び出し側(server.rs)の責務。
    pub(crate) fn dashboard_widgets_for_ui(
        &self,
    ) -> Vec<(String, String, PluginState, DashboardInfo)> {
        let snapshot: Vec<(Manifest, PluginState)> = self.entries.with_entries(|entries| {
            entries
                .iter()
                .map(|entry| (entry.manifest.clone(), entry.state.clone()))
                .collect()
        });

        snapshot
            .into_iter()
            .flat_map(|(manifest, state)| {
                self.build_dashboard_infos(&manifest)
                    .into_iter()
                    .map(|info| {
                        (
                            manifest.id.clone(),
                            manifest.name.clone(),
                            state.clone(),
                            info,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// ウィジェットアセットの実ファイルパスを解決する。grant 必須・entry の
    /// ディレクトリ外へのトラバーサルは拒否(`/plugin-ui/...` ハンドラの
    /// 心臓部。HTTP 層は薄く保ち、判定はここで単体テストする)。
    /// `rel_path` が空のときは entry ファイル自身を返す。
    pub(crate) fn dashboard_asset_path(
        &self,
        plugin: &str,
        widget: &str,
        rel_path: &str,
    ) -> Result<PathBuf, RegistryError> {
        use std::path::Component;
        let manifest = self.find_manifest(plugin)?;
        let spec = manifest
            .dashboard_widget(widget)
            .ok_or_else(|| RegistryError::UnknownDashboard(widget.to_string()))?;
        let grant = self.grants_store.dashboard_state(&manifest, widget);
        if !grant.granted {
            return Err(RegistryError::DashboardNotGranted(widget.to_string()));
        }
        let entry = self.plugins_dir.join(&manifest.id).join(&spec.entry);
        if rel_path.is_empty() {
            return Ok(entry);
        }
        let base = entry
            .parent()
            .ok_or_else(|| RegistryError::UnknownDashboard(widget.to_string()))?;
        let rel = Path::new(rel_path);
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(RegistryError::UnknownDashboard(widget.to_string()));
        }
        Ok(base.join(rel))
    }

    /// `id` のプラグインの capability 要求一覧と現在の承認状態を返す。
    ///
    /// `values`/`set_values` と同様、`entries` ロックは manifest のクローン
    /// 取得のみに使い、ロックを解放してから `GrantsStore::state`(ディスク
    /// 読み取り)を呼ぶ。
    pub(crate) fn capabilities(
        &self,
        id: &str,
    ) -> Result<(Vec<CapabilityRequest>, GrantState), RegistryError> {
        let manifest = self.find_manifest(id)?;
        let grant_state = self.grants_store.state(&manifest);
        Ok((manifest.capabilities.clone(), grant_state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::sidecar::test_support::InMemoryGrantStorage;

    fn manifest_with_http_capability(id: &str, host: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![CapabilityRequest::Http {
                hosts: vec![host.to_string()],
                reason: "reason".into(),
            }],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    fn plugin_entry(manifest: Manifest) -> PluginEntry {
        PluginEntry {
            manifest,
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new(String::new())),
            capabilities_json: Arc::new(Mutex::new(String::new())),
            sidecars_json: Arc::new(Mutex::new(String::new())),
            filesystem_json: Arc::new(Mutex::new(String::new())),
            bus_json: Arc::new(Mutex::new(String::new())),
        }
    }

    fn test_service(
        entry: PluginEntry,
    ) -> (
        GrantService<InMemoryGrantStorage, PluginEntry>,
        Arc<InMemoryGrantStorage>,
    ) {
        let entries = EntryTable::new();
        entries.push(entry);
        let grants_store = Arc::new(InMemoryGrantStorage::new());
        let service = GrantService::new(
            entries,
            grants_store.clone(),
            Arc::new(Mutex::new(())),
            PathBuf::from("/nonexistent/edlr-task10-test-plugins"),
        );
        (service, grants_store)
    }

    #[test]
    fn set_capabilities_grants_and_exposes_the_explicit_host() {
        let manifest = manifest_with_http_capability("p1", "example.com");
        let (service, _grants_store) = test_service(plugin_entry(manifest));

        let state = service
            .set_capabilities("p1", true)
            .expect("granting should succeed");

        assert!(state.granted);
        assert_eq!(
            service.effective_hosts("p1").unwrap(),
            vec!["example.com".to_string()]
        );
    }

    #[test]
    fn set_capabilities_revoke_drops_the_explicit_host() {
        let manifest = manifest_with_http_capability("p1", "example.com");
        let (service, _grants_store) = test_service(plugin_entry(manifest));
        service.set_capabilities("p1", true).unwrap();

        let state = service
            .set_capabilities("p1", false)
            .expect("revoking should succeed");

        assert!(!state.granted);
        assert_eq!(service.effective_hosts("p1").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn set_capabilities_for_unknown_id_returns_unknown_plugin_error() {
        let (service, _grants_store) = test_service(plugin_entry(manifest_with_http_capability(
            "p1",
            "example.com",
        )));

        let err = service
            .set_capabilities("does-not-exist", true)
            .expect_err("unknown id should be rejected");

        assert!(matches!(err, RegistryError::UnknownPlugin(id) if id == "does-not-exist"));
    }
}

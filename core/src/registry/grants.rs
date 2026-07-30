//! capability 承認とダッシュボードウィジェットの状態管理。
//!
//! Phase 4 タスク7の move-only コミットで `crate::plugin::registry::Registry`
//! から抽出した。分析(`docs/superpowers/specs/2026-07-31-phase4-registry-analysis.md`
//! §5)のとおり、spec 自体には dashboard 群の置き場が無いが、実体は
//! 「grants(承認の読み書き)+ `is_file` の 1 発チェック」なので、grants と
//! 同じサービスへ同居させる。`BusService` と同じく、この 2 群は**plugin
//! 専用**(driver 側の `set_capabilities` 統合は Task 8)なので、
//! `SidecarEntry` のようなエントリ trait は導入せず `PluginEntry` を具象の
//! まま持つ(`.claude/rules/trait-di.md` の「必要が実証されたときだけ増やす」)。
//!
//! `capabilities_lock` はコンストラクタ注入の共有 `Arc<Mutex<()>>`。plugin 側
//! `Registry` は、自分自身が `refresh_sidecar_runtime`(`SidecarService` 側に
//! 移設済み)で使うのと**同一の** `Arc` を、この `GrantService` にも渡す
//! (`registry::sidecar::SidecarService` のドキュメントコメント参照)。
//! `set_capabilities` は `capabilities_lock` だけを取り id 別ロックにもその
//! マップにも触れないため、ロック取得順序(id 別ロック → `capabilities_lock`
//! の一方向のみ)を保ったまま両者を共存させられる。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::capability::GrantStorage;
use crate::plugin::grants::GrantState;
use crate::plugin::host::{capabilities_json_string, parse_capability_hosts};
use crate::plugin::registry::{DashboardInfo, PluginEntry, PluginState, RegistryError};
use crate::plugin::sidecar_runtime::{implicit_http_hosts, parse_sidecars, SidecarRuntimeEntry};
use crate::plugin::{CapabilityRequest, Manifest};
use crate::registry::entries::EntryTable;

/// capability 承認群(`capabilities` / `set_capabilities` / `effective_hosts`)
/// と、ダッシュボードウィジェット群(`dashboard` / `set_dashboard_grant` /
/// `dashboard_widgets_for_ui` / `dashboard_asset_path` / `events_of` /
/// `build_dashboard_infos`)を束ねるサービス。
///
/// `plugins_dir` は `build_dashboard_infos`/`dashboard_asset_path` の
/// `is_file()` チェックと entry パス組み立て専用(元の `Registry` から
/// クローンを受け取る -- `Registry` 自身は `plugins_dir()` アクセサ用に
/// フィールドを引き続き保持する)。
pub(crate) struct GrantService<G: GrantStorage> {
    entries: EntryTable<PluginEntry>,
    grants_store: Arc<G>,
    capabilities_lock: Arc<Mutex<()>>,
    plugins_dir: PathBuf,
}

/// 手書き `Clone`: `derive(Clone)` は `G` 自体に `Clone` を要求してしまうが、
/// 実際に clone が要るのは `Arc`/`EntryTable`/`PathBuf`(いずれも中身の型に
/// 関わらず `Clone`)だけなので、要らない境界を足さないよう手で書く
/// (`BusService`/`SidecarService` と同じ理由)。
impl<G: GrantStorage> Clone for GrantService<G> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            grants_store: self.grants_store.clone(),
            capabilities_lock: self.capabilities_lock.clone(),
            plugins_dir: self.plugins_dir.clone(),
        }
    }
}

/// ディスク実装(`GrantsStore`)を挿した公開面。
pub(crate) type DiskGrantService = GrantService<crate::plugin::grants::GrantsStore>;

impl<G: GrantStorage> GrantService<G> {
    pub(crate) fn new(
        entries: EntryTable<PluginEntry>,
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
    /// ルックアップの間だけ)。`Registry::find_manifest` と同じ流儀。
    fn find_manifest(&self, id: &str) -> Result<Manifest, RegistryError> {
        self.entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| entry.manifest.clone(),
            )
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))
    }

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

    /// `id` のプラグインの capability 承認/取消を `GrantsStore` に永続化し、
    /// 稼働中プラグインが参照する共有 `capabilities_json` も更新する。
    ///
    /// `entries` ロックは manifest と `capabilities_json` の共有ハンドルを
    /// 取得する間だけ保持し、`GrantsStore::set` のファイル I/O はロックを
    /// 解放した後に行う(`set_values` と同じ流儀。`entries` ロックをファイル
    /// I/O の間保持すると、他プラグインの settings/capabilities 操作や
    /// `set_disabled` までブロックしてしまうため)。
    ///
    /// 一方で、「ディスクへの永続化」と「共有バッファへの反映」の 2 ステップは
    /// 同じ呼び出しの中で不可分に行う必要がある。呼び出しごとに
    /// `capabilities_lock` を取り、`GrantsStore::set` の呼び出しから
    /// `capabilities_json` バッファへの書き込みまでを 1 つの臨界区間として
    /// 保持する。これが無いと、2 つの同時呼び出し(例: 2 つの RPC クライアントが
    /// 同じプラグインを同時に許可/取消)が
    /// `A.set(true) → B.set(false) → B が buffer に false を書く →
    /// A が buffer に true を書く` のように交互実行され、ディスク上は
    /// 取消済みなのに稼働中プラグインのバッファは許可済みのまま、という
    /// fail-open な不整合が起こりうる(このロックはそれぞれの呼び出しの
    /// 「永続化 + バッファ反映」を丸ごと直列化することで、ディスクとバッファの
    /// 最終状態が必ず「最後にこの臨界区間を抜けた呼び出し」の結果で一致する
    /// ことを保証する)。
    ///
    /// `GrantsStore` 自身も内部に別の `Mutex<()>` を持つが、それは
    /// `GrantsStore::set` 単体(ファイル書き込みとその直後の読み出し)の
    /// アトミック性のためのものであり、バッファ書き込みまでは面倒を見ない。
    /// そのため `capabilities_lock` は `GrantsStore` の内部ロックとは別に
    /// `GrantService` 側で持つ(`GrantsStore` に `capabilities_json` の形を
    /// 知らせたくない、という関心の分離の意味もある)。2 つのロックの取得
    /// 順序は常に `capabilities_lock` → (`GrantsStore` 内部ロック) の一方向
    /// のみなのでデッドロックの心配もない。
    ///
    /// `capabilities_json` は `refresh_sidecar_runtime`(`SidecarService` 側。
    /// サイドカーの設定変更・承認変更のたびに、承認済みサイドカーの暗黙
    /// 127.0.0.1 許可を織り込んで書き直す)とも書き込み先を共有している。
    /// そちらも同じ `capabilities_lock` を取ってから書くので、「http
    /// capability の承認/取消」と「サイドカーの設定/承認変更」が同時に
    /// 起きても、このバッファへの 2 つの書き込みが交互実行で食い違うことは
    /// ない(`SidecarService::refresh_sidecar_runtime` のドキュメント
    /// コメント参照。ロック取得順序は常に「id 別ロック(プラグイン単位)→
    /// `capabilities_lock`」の一方向のみで、この関数は `capabilities_lock`
    /// だけを取り id 別ロックにもそのマップにも触れないため、両者を合わせても
    /// デッドロックしない)。この関数自身はサイドカーの設定/承認を変更しない
    /// ので、現在の `sidecars_json` バッファをそのまま読み(**再計算はしない
    /// -- 分析 §6 リスク2**)、そこから暗黙許可ホストを合流させる。
    pub(crate) fn set_capabilities(
        &self,
        id: &str,
        granted: bool,
    ) -> Result<GrantState, RegistryError> {
        let (manifest, capabilities_json, sidecars_json) = self
            .entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| {
                    (
                        entry.manifest.clone(),
                        entry.capabilities_json.clone(),
                        entry.sidecars_json.clone(),
                    )
                },
            )
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;

        let _capabilities_guard = self
            .capabilities_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let state = self
            .grants_store
            .set(&manifest, granted)
            .map_err(RegistryError::Grants)?;

        let mut effective_hosts = if state.granted {
            manifest.capability_hosts()
        } else {
            Vec::new()
        };
        let sidecar_entries: Vec<SidecarRuntimeEntry> = parse_sidecars(
            &sidecars_json
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
        .into_values()
        .collect();
        effective_hosts.extend(implicit_http_hosts(&sidecar_entries));

        let capabilities_json_string = capabilities_json_string(&effective_hosts);
        *capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = capabilities_json_string;

        Ok(state)
    }

    /// `id` のプラグインの `capabilities_json` 共有バッファが現在載せている
    /// 実効許可ホストを返す(テスト用アクセサ)。`driver-http.send` が実際に
    /// 参照するのと同じ値。
    pub(crate) fn effective_hosts(&self, id: &str) -> Result<Vec<String>, RegistryError> {
        let capabilities_json = self
            .entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| entry.capabilities_json.clone(),
            )
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;
        let raw = capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Ok(parse_capability_hosts(&raw))
    }
}

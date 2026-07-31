//! プラグイン間バス接続(`[[bus]]`)の状態管理。
//!
//! Phase 4 タスク5の move-only コミットで `crate::registry::plugin::Registry`
//! から抽出した。`registry::filesystem::FilesystemService` と違い、bus は
//! **plugin 専用**(分析 `docs/superpowers/specs/2026-07-31-phase4-registry-analysis.md`
//! §5 のとおり、driver 側に bus grant は存在しない)なので、`FilesystemEntry`
//! のようなエントリ trait は導入せず `PluginEntry` を具象のまま持つ
//! (`E` を挿す先が 1 つしか無い抽象化はノイズにしかならない、
//! `.claude/rules/trait-di.md` の「必要が実証されたときだけ増やす」)。
//! ジェネリクスは `G: GrantStorage`(`FilesystemService` と同じ、ディスク実装
//! `GrantsStore` を挿すためのもの)だけ残す。

use std::sync::{Arc, Mutex};

use crate::capability::grants::{GrantState, GrantsStore};
use crate::capability::GrantStorage;
use crate::manifest::Manifest;
use crate::registry::driver::DriverRegistry;
use crate::registry::entries::{EntryTable, IdLocks};
use crate::registry::plugin::{BusInfo, PluginEntry, RegistryError};
use crate::runtime::bus::{bus_json_string, BusRuntimeEntry};

/// bus 群(`bus` / `bus_buffer` / `set_bus_grant` とその内部ヘルパー)を
/// 束ねるサービス。
///
/// 元の `Registry` から抽出した各フィールドの役割はそのまま:
/// `entries` は manifest クローン取得のみに使い、ディスク I/O やロック待ちに
/// 入る前に手放す(`EntryTable` のドキュメント参照)。`bus_runtime_locks` は
/// プラグイン ID ごとに `refresh_bus_runtime` の臨界区間を直列化する
/// (サイドカー・ファイルアクセスとは別マップ -- 元の `Registry` の
/// `bus_runtime_locks` ドキュメント参照)。`driver_registry` は
/// `BusInfo::resolved` の計算専用(バス接続先ドライバの現在の宣言を引く)。
pub(crate) struct BusService<G: GrantStorage> {
    entries: EntryTable<PluginEntry>,
    grants_store: Arc<G>,
    driver_registry: DriverRegistry,
    bus_runtime_locks: IdLocks,
}

/// 手書き `Clone`: `derive(Clone)` は `G` 自体に `Clone` を要求してしまうが、
/// 実際に clone が要るのは `Arc`/`EntryTable`/`DriverRegistry`/`IdLocks`
/// (いずれも中身の型に関わらず `Clone`)だけなので、要らない境界を足さない
/// よう手で書く(`FilesystemService` と同じ理由)。
impl<G: GrantStorage> Clone for BusService<G> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            grants_store: self.grants_store.clone(),
            driver_registry: self.driver_registry.clone(),
            bus_runtime_locks: self.bus_runtime_locks.clone(),
        }
    }
}

/// ディスク実装(`GrantsStore`)を挿した公開面。
pub(crate) type DiskBusService = BusService<GrantsStore>;

impl<G: GrantStorage> BusService<G> {
    pub(crate) fn new(
        entries: EntryTable<PluginEntry>,
        grants_store: Arc<G>,
        driver_registry: DriverRegistry,
        bus_runtime_locks: IdLocks,
    ) -> Self {
        Self {
            entries,
            grants_store,
            driver_registry,
            bus_runtime_locks,
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

    /// `manifest.bus` の宣言順に `BusInfo` を組み立てる。承認
    /// (`GrantsStore::bus_state`)はディスクを読む。`resolved` は
    /// `driver_registry.manifest_of` が返す `DriverManifest`(インストール済み
    /// ドライバの現在の宣言)と、この要求が挙げる `publish`/`subscribe` の
    /// 全トピックとを突き合わせて決める -- ドライバ自体が無ければ即
    /// `false`、ドライバはあってもトピックが 1 つでも無ければ `false`。
    fn build_bus_infos(&self, manifest: &Manifest) -> Vec<BusInfo> {
        manifest
            .bus
            .iter()
            .map(|request| {
                let grant = self.grants_store.bus_state(manifest, &request.driver);
                let resolved = match self.driver_registry.manifest_of(&request.driver) {
                    Some(driver_manifest) => request
                        .publish
                        .iter()
                        .chain(request.subscribe.iter())
                        .all(|topic| driver_manifest.topic(topic).is_some()),
                    None => false,
                };
                BusInfo {
                    request: request.clone(),
                    grant,
                    resolved,
                }
            })
            .collect()
    }

    /// `id` 用のバス実行時ロック(`bus_runtime_locks`)を引く。サイドカー・
    /// ファイルアクセスとは**別のマップ**であることが要点
    /// (`bus_runtime_locks` のドキュメント参照)。
    fn bus_runtime_lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        self.bus_runtime_locks.lock_for(id)
    }

    /// `id` のプラグインの現在のバス接続状態一覧(manifest の `[[bus]]`
    /// 宣言順)を返す。`BusInfo::resolved` は `DriverRegistry` の現在の登録
    /// 状況(ドライバの有無・トピックの有無)から都度計算する(承認と違い
    /// ディスクに永続化しない -- ドライバの実体そのものが真実源)。
    pub(crate) fn bus(&self, id: &str) -> Result<Vec<BusInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        Ok(self.build_bus_infos(&manifest))
    }

    /// `id` のプラグインの `bus_json` 共有バッファの中身をそのまま返す
    /// (テスト用アクセサ)。`HostCtx::check_bus` /
    /// `runner::spawn_bus_subscriber` が実際に参照するのと同じ文字列
    /// (`crate::runtime::bus::bus_json_string` の出力そのもの)。
    pub(crate) fn bus_buffer(&self, id: &str) -> Result<String, RegistryError> {
        let bus_json = self
            .entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| entry.bus_json.clone(),
            )
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;
        let buffer = bus_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Ok(buffer)
    }

    /// バス承認変更のあとに必ず呼ぶ内部ヘルパー。ファイルアクセスと同じく
    /// 「止めるべきプロセス」が無いので、`GrantsStore::bus_state` /
    /// `DriverRegistry::manifest_of` の現在値から `bus_json` を作り直す
    /// だけ。未承認の接続先は `publish`/`subscribe` を持たない
    /// (`crate::runtime::bus` のドキュメント参照)。
    ///
    /// ロックは `bus_runtime_lock_for(id)` -- サイドカー・ファイルアクセス
    /// とは別のマップ(`bus_runtime_locks` のドキュメント参照)から引く。
    fn refresh_bus_runtime(&self, id: &str) -> Result<Vec<BusInfo>, RegistryError> {
        let (manifest, bus_json) = self
            .entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| (entry.manifest.clone(), entry.bus_json.clone()),
            )
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;

        let runtime_lock = self.bus_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let infos = self.build_bus_infos(&manifest);
        let runtime_entries: Vec<BusRuntimeEntry> = infos
            .iter()
            .map(|info| BusRuntimeEntry {
                driver: info.request.driver.clone(),
                granted: info.grant.granted,
                publish: info.request.publish.clone(),
                subscribe: info.request.subscribe.clone(),
            })
            .collect();
        *bus_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = bus_json_string(&runtime_entries);

        Ok(infos)
    }

    /// `id` のプラグインの `driver` バス接続の承認/取消を `GrantsStore` に
    /// 永続化し、稼働中プラグインが参照する `bus_json` を作り直す
    /// (`set_filesystem_grant` と同じ形)。
    ///
    /// **配信は取消を即座に反映する**: `bus_json` の書き換えだけで済むのは、
    /// `runner.rs` の配信転送タスク(`spawn_bus_subscriber`)が配信のたびに
    /// この同じ `bus_json` を読み直して承認を再確認するため。ここでは
    /// 購読の解除(`Bus::subscribe` の取り消し)自体は行わない -- 取り消して
    /// も転送タスク側で捨てられるだけなので、購読表を触る必要が無い。
    pub(crate) fn set_bus_grant(
        &self,
        id: &str,
        driver: &str,
        granted: bool,
    ) -> Result<GrantState, RegistryError> {
        let manifest = self.find_manifest(id)?;
        if manifest.bus_request(driver).is_none() {
            return Err(RegistryError::UnknownBus(driver.to_string()));
        }

        let state = self
            .grants_store
            .set_bus(&manifest, driver, granted)
            .map_err(RegistryError::Grants)?;

        self.refresh_bus_runtime(id)?;

        Ok(state)
    }
}

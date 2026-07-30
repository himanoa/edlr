//! サイドカープロセスの承認・起動停止状態管理。
//!
//! `crate::plugin::registry::Registry` からサイドカー群のメソッド本体をそのまま
//! 移した(Phase 4 タスク6、move-only)。この時点ではまだ plugin 専用
//! (`GrantsStore`/`ProcessDriver`/`PluginEntry` 具象)で、driver 側への一般化・
//! ジェネリック化は次のコミットで `RegistrySubject` を使って行う。
//!
//! `capabilities_lock` は呼び出し元(`Registry`)が持つのと**同一の**
//! `Arc<Mutex<()>>` をコンストラクタで受け取る(このコミットでは
//! `Registry::new` がそのまま作って渡す一方通行だが、次のコミット以降も
//! Registry 側がこの Arc を保持し続け、`set_capabilities` と共有する)。
//! `refresh_sidecar_runtime` の手順3(`capabilities_json` 書き換え)は
//! `Registry::set_capabilities` と書き込み先を共有しているため、同じ
//! ロックでなければ両者の書き込みが交互実行で食い違いうる
//! (元の `Registry::capabilities_lock` ドキュメントコメント参照)。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use edlr_driver_process::{ProcessDriver, ProcessSpec};

use crate::plugin::grants::{GrantState, GrantsStore};
use crate::plugin::host::capabilities_json_string;
use crate::plugin::registry::{
    PluginEntry, PluginState, RegistryError, SidecarAction, SidecarInfo,
};
use crate::plugin::sidecar::{assign_ports, SidecarConfig, SidecarConfigStore};
use crate::plugin::sidecar_runtime::{
    implicit_http_hosts, sidecars_json_string, SidecarRuntimeEntry,
};
use crate::plugin::{Manifest, SidecarRequest};
use crate::registry::entries::{EntryTable, IdLocks};

/// `<plugin-id>/<sidecar-name>` の形で `ProcessDriver` のキーを組み立てる。
/// `HostCtx::sidecar_key`(`core/src/plugin/host.rs`)と同じ規則。
pub(crate) fn sidecar_key(plugin_id: &str, name: &str) -> String {
    format!("{plugin_id}/{name}")
}

/// サイドカー群(`sidecars` / `set_sidecar_config` / `set_sidecar_grant` /
/// `control_sidecar` / `stop_all_sidecars` とその内部ヘルパー)を束ねる
/// サービス。ドキュメントは元 `Registry` の対応フィールド/メソッドのものを
/// そのまま踏襲する(このコミットでは移動のみで、挙動・ロック規律は一切
/// 変えていない)。
#[derive(Clone)]
pub(crate) struct SidecarService {
    entries: EntryTable<PluginEntry>,
    grants_store: Arc<GrantsStore>,
    sidecar_config_store: Arc<SidecarConfigStore>,
    process_driver: Arc<ProcessDriver>,
    capabilities_lock: Arc<Mutex<()>>,
    sidecar_runtime_locks: IdLocks,
}

impl SidecarService {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        entries: EntryTable<PluginEntry>,
        grants_store: Arc<GrantsStore>,
        sidecar_config_store: Arc<SidecarConfigStore>,
        process_driver: Arc<ProcessDriver>,
        capabilities_lock: Arc<Mutex<()>>,
        sidecar_runtime_locks: IdLocks,
    ) -> Self {
        Self {
            entries,
            grants_store,
            sidecar_config_store,
            process_driver,
            capabilities_lock,
            sidecar_runtime_locks,
        }
    }

    /// `id` のプラグインの manifest クローンを返す(`entries` ロック保持は
    /// このルックアップの間だけ)。`Registry::find_manifest` と同じ流儀。
    fn find_manifest(&self, id: &str) -> Result<Manifest, RegistryError> {
        self.entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| entry.manifest.clone(),
            )
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))
    }

    /// `manifest.sidecars` の宣言順に `SidecarInfo` を組み立てる。設定
    /// (`SidecarConfigStore`)・承認(`GrantsStore`)はディスクを読むが、
    /// `ProcessDriver::status` は読み取り専用(プロセスを起動も停止もしない)。
    pub(crate) fn build_sidecar_infos(&self, manifest: &Manifest) -> Vec<SidecarInfo> {
        let configs = self.sidecar_config_store.effective(manifest);
        manifest
            .sidecars
            .iter()
            .map(|request| {
                let config = configs
                    .get(&request.name)
                    .cloned()
                    .unwrap_or_else(|| SidecarConfig::from_request(request));
                let grant = self.grants_store.sidecar_state(manifest, &request.name);
                self.sidecar_info_and_entry(manifest, request, config, grant)
                    .0
            })
            .collect()
    }

    /// `request` 1 件分の(既に取得済みの)設定・承認状態から、`SidecarInfo`
    /// と(`sidecars_json` バッファ用の)`SidecarRuntimeEntry` を両方組み立
    /// てる。`ProcessDriver::status` の呼び出しを両者で 1 回だけ共有する
    /// (`config`/`grant` の取得元は呼び出し側に委ねているので、ここではもう
    /// ディスクを読まない -- `refresh_sidecar_runtime` が前半で読んだ値を
    /// そのまま渡し、末尾で読み直さずに済ませるための分離)。
    fn sidecar_info_and_entry(
        &self,
        manifest: &Manifest,
        request: &SidecarRequest,
        config: SidecarConfig,
        grant: GrantState,
    ) -> (SidecarInfo, SidecarRuntimeEntry) {
        let ports = assign_ports(&config);
        let spec = ProcessSpec {
            command: PathBuf::from(&config.command),
            args: config.args.clone(),
            ports: ports.clone(),
        };
        let key = sidecar_key(&manifest.id, &request.name);
        let instances = self.process_driver.status(&key, &spec);

        let runtime_entry = SidecarRuntimeEntry {
            name: request.name.clone(),
            granted: grant.granted,
            command: config.command.clone(),
            args: config.args.clone(),
            ports,
        };
        let info = SidecarInfo {
            request: request.clone(),
            config,
            grant,
            instances,
        };
        (info, runtime_entry)
    }

    /// `id` 用のサイドカー実行時ロック(`sidecar_runtime_locks`)を引く。
    /// 無ければ作る。マップ自体を保護する `Mutex` は、id からロックの `Arc`
    /// を引く/挿入する間だけ保持し、返した `Arc<Mutex<()>>` の実際の臨界
    /// 区間(呼び出し側が別途 `.lock()` する)の間は保持しない。
    ///
    /// `pub(crate)`: 同一プラグインの他操作(`filesystem` 操作など)と
    /// 排他にならないこと・サイドカー停止待ちがどれだけロックを保持するかを
    /// 検証するテストが `crate::plugin::registry::tests` から直接この
    /// ロックを取るため。
    pub(crate) fn sidecar_runtime_lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        self.sidecar_runtime_locks.lock_for(id)
    }

    /// テスト用アクセサ: サイドカーを wasm 呼び出しを経由せずホスト側から
    /// 直接操作する統合テストのため、内部の `ProcessDriver` をそのまま返す。
    #[cfg(test)]
    pub(crate) fn process_driver(&self) -> &Arc<ProcessDriver> {
        &self.process_driver
    }

    /// テスト用アクセサ: `set_sidecar_config` の RPC 経路を経由せず、直接
    /// サイドカー設定を仕込む統合テストのため、内部の `SidecarConfigStore`
    /// をそのまま返す。
    #[cfg(test)]
    pub(crate) fn sidecar_config_store(&self) -> &Arc<SidecarConfigStore> {
        &self.sidecar_config_store
    }

    /// `id` のプラグインの現在のサイドカー状態一覧(manifest の `[[sidecar]]`
    /// 宣言順)を返す。
    pub(crate) fn sidecars(&self, id: &str) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        Ok(self.build_sidecar_infos(&manifest))
    }

    /// `id` のプラグインが現在 `Disabled` かどうか。未登録の id は `false`
    /// (`control_sidecar` はこの手前で既に `find_manifest` を通しているので、
    /// 未登録を別扱いする必要はない)。
    fn is_disabled(&self, id: &str) -> bool {
        self.entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| matches!(entry.state, PluginState::Disabled { .. }),
            )
            .unwrap_or(false)
    }

    /// サイドカーの設定変更・承認変更のあとに必ず呼ぶ内部ヘルパー。
    ///
    /// 手順は3段: 1) `stop_names` を `ProcessDriver::stop` で停止、
    /// 2) `SidecarConfigStore`/`GrantsStore` の現在値から `sidecars_json` を
    /// 作り直す、3) `capabilities_lock` を取り、承認済みサイドカーの暗黙
    /// 127.0.0.1 許可を織り込んで `capabilities_json` も作り直す。
    ///
    /// `id` 専用のロック(`sidecar_runtime_lock_for`)を、手順 1〜3 すべてを
    /// 覆う 1 つの臨界区間として保持する(`set_capabilities` の
    /// `capabilities_lock` と同じ理由: 2 つの同時呼び出しがディスクと共有
    /// バッファを食い違わせないようにするため)。ロック取得順序は常に
    /// 「id 別ロック → `capabilities_lock`」の一方向のみ。
    fn refresh_sidecar_runtime(
        &self,
        id: &str,
        stop_names: &[String],
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let (manifest, sidecars_json, capabilities_json) = self
            .entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| {
                    (
                        entry.manifest.clone(),
                        entry.sidecars_json.clone(),
                        entry.capabilities_json.clone(),
                    )
                },
            )
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;

        let runtime_lock = self.sidecar_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        for name in stop_names {
            let key = sidecar_key(&manifest.id, name);
            self.process_driver.stop(&key);
        }

        let sidecar_configs = self.sidecar_config_store.effective(&manifest);
        let mut infos = Vec::with_capacity(manifest.sidecars.len());
        let mut runtime_entries = Vec::with_capacity(manifest.sidecars.len());
        for request in &manifest.sidecars {
            let config = sidecar_configs
                .get(&request.name)
                .cloned()
                .unwrap_or_else(|| SidecarConfig::from_request(request));
            let grant = self.grants_store.sidecar_state(&manifest, &request.name);
            let (info, runtime_entry) =
                self.sidecar_info_and_entry(&manifest, request, config, grant);
            infos.push(info);
            runtime_entries.push(runtime_entry);
        }
        *sidecars_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            sidecars_json_string(&runtime_entries);

        {
            let _capabilities_guard = self
                .capabilities_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let http_granted = self.grants_store.state(&manifest).granted;
            let mut hosts = if http_granted {
                manifest.capability_hosts()
            } else {
                Vec::new()
            };
            hosts.extend(implicit_http_hosts(&runtime_entries));
            *capabilities_json
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                capabilities_json_string(&hosts);
        }

        Ok(infos)
    }

    /// `id` のプラグインの `name` サイドカーの設定を検証・永続化し、稼働中の
    /// 実行を止めてから(`refresh_sidecar_runtime`)、最新の `SidecarInfo` 一覧
    /// を返す。検証(`SidecarConfigStore::update_and_effective`)に失敗した
    /// 場合は何も変更されない。
    pub(crate) fn set_sidecar_config(
        &self,
        id: &str,
        name: &str,
        config: &SidecarConfig,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        self.sidecar_config_store
            .update_and_effective(&manifest, name, config)
            .map_err(RegistryError::SidecarConfig)?;
        let stop_names = vec![name.to_string()];
        self.refresh_sidecar_runtime(id, &stop_names)
    }

    /// `id` のプラグインの `name` サイドカーの承認/取消を `GrantsStore` に
    /// 永続化する。取消(`granted == false`)のときは稼働中の実行を止める
    /// (走り続けてよい根拠が承認と共に無くなるため)。
    ///
    /// `granted == true` のとき、`command` が未設定(空文字)のサイドカーは
    /// 拒否する(`RegistryError::Sidecar`)。`command` 未設定では承認できない
    /// という不変条件はここ(ストア/サービス側)で強制し、UI・RPC 層では
    /// 二重実装しない。取消は逆に常に許す。
    pub(crate) fn set_sidecar_grant(
        &self,
        id: &str,
        name: &str,
        granted: bool,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        if manifest.sidecar(name).is_none() {
            return Err(RegistryError::UnknownSidecar(name.to_string()));
        }

        if granted {
            let configs = self.sidecar_config_store.effective(&manifest);
            let command_configured = configs
                .get(name)
                .map(|config| !config.command.is_empty())
                .unwrap_or(false);
            if !command_configured {
                return Err(RegistryError::Sidecar(format!(
                    "sidecar {name} has no executable configured; cannot grant"
                )));
            }
        }

        self.grants_store
            .set_sidecar(&manifest, name, granted)
            .map_err(RegistryError::Grants)?;

        let stop_names: Vec<String> = if granted {
            Vec::new()
        } else {
            vec![name.to_string()]
        };
        self.refresh_sidecar_runtime(id, &stop_names)
    }

    /// `id` のプラグインの `name` サイドカーを直接操作する(ユーザー操作
    /// 起点)。`Stop` は同期版 `ProcessDriver::stop`、`Start` は
    /// `ProcessDriver::ensure_started`、`Restart` は停止してから
    /// `ensure_started`。
    ///
    /// **TOCTOU 対策**: `Start`/`Restart` は `sidecar_runtime_lock_for(id)`
    /// (`refresh_sidecar_runtime` -- `set_sidecar_config`/`set_sidecar_grant`
    /// が呼ぶ -- と共有する、プラグイン単位のロック)を、承認・設定を読む
    /// 前から `ensure_started` を呼び終えるまで保持する。これが無いと、
    /// 「grant を読む」→「(ここで承認取消が割り込む)」→「その grant を
    /// 信じて spawn する」という窓ができ、取消の裏で起動してしまう。
    ///
    /// **無効化されたプラグインは `Start`/`Restart` できない**: `set_disabled`
    /// はそのプラグインの全サイドカーを停止するが、この関数が `PluginState`
    /// を見ていないと、無効化後(あるいは並行中)に `start` が来ると、もう
    /// 生きているプラグインスレッドが無いサイドカーが再び起動してしまう。
    /// この関数は `sidecar_runtime_lock_for(id)` を取った**後**に現在の
    /// `PluginState` を読み、`Disabled` なら拒否する -- `set_disabled` 自身も
    /// サイドカー停止の前に同じ id 別ロックを取るので、「状態を `Disabled`
    /// にする」→「サイドカーを止める」という一連の操作と、この関数の
    /// 「状態を読む」→「spawn する」は互いに排他になり、無効化の裏で
    /// 起動してしまう窓は無い。`Stop` はプラグインの状態に関わらず常に許す。
    pub(crate) fn control_sidecar(
        &self,
        id: &str,
        name: &str,
        action: SidecarAction,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        let request = manifest
            .sidecar(name)
            .ok_or_else(|| RegistryError::UnknownSidecar(name.to_string()))?;
        let key = sidecar_key(&manifest.id, name);

        match action {
            SidecarAction::Stop => {
                self.process_driver.stop(&key);
            }
            SidecarAction::Start | SidecarAction::Restart => {
                let runtime_lock = self.sidecar_runtime_lock_for(id);
                let _runtime_guard = runtime_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());

                // ロック保持中に読む: `set_disabled` も同じ id 別ロックを
                // 取ってからサイドカーを止めるので、ここで見る状態は
                // 「無効化処理が確定済みならその後の値」であることが保証
                // される。
                if self.is_disabled(id) {
                    return Err(RegistryError::Sidecar(format!("plugin {id} is disabled")));
                }

                if action == SidecarAction::Restart {
                    self.process_driver.stop(&key);
                }

                // ロック保持中に読み直す: `refresh_sidecar_runtime` は同じ
                // ロックを取ってから承認取消/設定変更を確定するので、ここで
                // 読む値は「取消/変更が確定済みならその後の値」であることが
                // 保証される。
                let grant = self.grants_store.sidecar_state(&manifest, name);
                if !grant.granted {
                    return Err(RegistryError::Sidecar(format!(
                        "sidecar {name} is not granted"
                    )));
                }

                let configs = self.sidecar_config_store.effective(&manifest);
                let config = configs
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| SidecarConfig::from_request(request));
                if config.command.is_empty() {
                    return Err(RegistryError::Sidecar(format!(
                        "sidecar {name} has no executable configured"
                    )));
                }

                let spec = ProcessSpec {
                    command: PathBuf::from(&config.command),
                    args: config.args.clone(),
                    ports: assign_ports(&config),
                };
                self.process_driver
                    .ensure_started(&key, &spec)
                    .map_err(|e| RegistryError::Sidecar(e.to_string()))?;
            }
        }

        self.sidecars(id)
    }

    /// 全プラグインの全サイドカーインスタンスを停止する(デーモン shutdown 用)。
    /// `ProcessDriver::stop_all` をそのまま呼ぶ。
    pub(crate) fn stop_all(&self) {
        self.process_driver.stop_all();
    }

    /// `id` の `names` サイドカーを、`id` 専用の実行時ロックを取った上で
    /// 停止する。`Registry::set_disabled` 用(`sidecars_json`/
    /// `capabilities_json` バッファの書き換えは行わない -- 元の
    /// `set_disabled` もサイドカーを止めるだけでバッファは触っていなかった)。
    ///
    /// 同じ id 専用ロックを `control_sidecar`(`Start`/`Restart`)や
    /// `refresh_sidecar_runtime` と共有するので、無効化に伴う停止と
    /// これらの操作は互いに排他になる(`control_sidecar` のドキュメント
    /// コメント参照)。
    pub(crate) fn stop_named(&self, id: &str, names: &[String]) {
        let runtime_lock = self.sidecar_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for name in names {
            let key = sidecar_key(id, name);
            self.process_driver.stop(&key);
        }
    }
}

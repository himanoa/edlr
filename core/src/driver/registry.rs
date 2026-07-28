//! 実行中ドライバの状態を保持する共有ビュー。`start_drivers` が構築する。
//!
//! `crate::plugin::registry::Registry` と対称の構造だが、以下が異なる:
//! - bus の承認 API は持たない(プラグインの `[[bus]]` 要求を承認するのは
//!   `crate::plugin::registry::Registry` の責務 -- ドライバは自分の側から
//!   バス接続を要求しない)。
//! - `set_disabled` は状態を `Disabled` にするだけでなく `bus.disable_driver`
//!   も呼ぶ。ドライバの retained 値はドライバ自身の生存が前提であり、無効化
//!   された時点でその値を読み続けさせるのは fail-open になる
//!   (`edlr_driver_channel::Bus::disable_driver` のドキュメント参照)。

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use edlr_driver_channel::Bus;
use edlr_driver_process::{ProcessDriver, ProcessSpec};

use crate::driver::host::DriverHost;
use crate::driver::manifest::DriverManifest;
use crate::plugin::filesystem::{FilesystemConfig, FilesystemConfigStore};
use crate::plugin::fs_runtime::{filesystem_json_string, FsRuntimeEntry};
use crate::plugin::grants::{GrantState, GrantsError, GrantsStore};
use crate::plugin::host::capabilities_json_string;
use crate::plugin::manifest::SidecarRequest;
use crate::plugin::registry::{FilesystemInfo, RegistryError, SidecarAction, SidecarInfo};
use crate::plugin::settings::{SettingsError, SettingsStore};
use crate::plugin::sidecar::{assign_ports, SidecarConfig, SidecarConfigStore};
use crate::plugin::sidecar_runtime::{
    implicit_http_hosts, parse_sidecars, sidecars_json_string, SidecarRuntimeEntry,
};

/// ドライバ 1 件の現在の駆動状態。`crate::plugin::registry::PluginState` と対称。
#[derive(Debug, Clone, PartialEq)]
pub enum DriverState {
    Running,
    Disabled { reason: String },
}

/// レジストリに載る 1 ドライバ分のエントリ。`PluginEntry` と対称の形だが、
/// ドライバは `[[bus]]` 要求を持たないため `bus_json` に相当するフィールドは
/// 無い(`DriverCtx::new` が `bus_json` を取らないのと対応する)。
pub struct DriverEntry {
    pub manifest: DriverManifest,
    pub state: DriverState,
    /// `DriverCtx` と共有される effective settings JSON。
    pub settings_json: Arc<Mutex<String>>,
    /// `DriverCtx` と共有される capability 承認状態 JSON。
    pub capabilities_json: Arc<Mutex<String>>,
    /// `DriverCtx` と共有されるサイドカー承認状態・実行仕様 JSON。
    pub sidecars_json: Arc<Mutex<String>>,
    /// `DriverCtx` と共有されるファイルアクセス承認状態・実パス JSON。
    pub filesystem_json: Arc<Mutex<String>>,
}

/// RPC 応答用のドライバ情報スナップショット。`PluginInfo` と対称。
pub struct DriverInfo {
    pub manifest: DriverManifest,
    pub state: DriverState,
    pub values: serde_json::Map<String, serde_json::Value>,
    pub grant_state: GrantState,
    pub sidecars: Vec<SidecarInfo>,
    pub filesystem: Vec<FilesystemInfo>,
}

/// `DriverRegistry` の値アクセス系メソッドが返しうるエラー。
/// `crate::plugin::registry::RegistryError` と対称だが、ドライバの API 面が
/// 狭い(サイドカー/ファイルアクセスの個別承認 API を持たない)ぶん variant
/// も少ない。
#[derive(Debug)]
pub enum DriverRegistryError {
    /// 指定された `id` のドライバが登録されていない。
    UnknownDriver(String),
    /// `SettingsStore::update` による検証・永続化エラー。
    Settings(SettingsError),
    /// `GrantsStore::set` による永続化エラー。
    Grants(GrantsError),
}

impl fmt::Display for DriverRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverRegistryError::UnknownDriver(id) => write!(f, "unknown driver: {id}"),
            DriverRegistryError::Settings(e) => write!(f, "{e}"),
            DriverRegistryError::Grants(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DriverRegistryError {}

/// 起動中ドライバ一覧の共有ビュー。`crate::plugin::registry::Registry` と対称。
///
/// 内部で `DriverHost` の `Arc` も保持している。理由は `Registry` が
/// `PluginHost` を保持しているのと同じ(エポック割り込み用 ticker スレッドを
/// 生かし続けるため)。
#[derive(Clone)]
pub struct DriverRegistry {
    entries: Arc<Mutex<Vec<DriverEntry>>>,
    _host: Arc<DriverHost>,
    settings_store: Arc<SettingsStore>,
    grants_store: Arc<GrantsStore>,
    sidecar_config_store: Arc<SidecarConfigStore>,
    filesystem_config_store: Arc<FilesystemConfigStore>,
    /// サイドカープロセスを実際に所有するドライバ。`DriverHost` が全ドライバ
    /// インスタンスで共有している 1 インスタンスをそのまま指す
    /// (`crate::plugin::registry::Registry::process_driver` と同じ役回り)。
    process_driver: Arc<ProcessDriver>,
    /// プラグイン間バスの実体。`set_disabled` が `disable_driver` を呼ぶために
    /// 保持する。
    bus: Bus,
    /// `set_capabilities` の「`GrantsStore::set` への永続化」と「共有
    /// `capabilities_json` バッファへの上書き」を 1 つの臨界区間として直列化
    /// するロック。理由は `crate::plugin::registry::Registry::capabilities_lock`
    /// のドキュメントコメントと同じ。
    capabilities_lock: Arc<Mutex<()>>,
    /// `set_sidecar_config`/`set_sidecar_grant` が呼ぶ `refresh_sidecar_runtime`
    /// の臨界区間をドライバ ID ごとに直列化するロックのマップ。
    /// `crate::plugin::registry::Registry::sidecar_runtime_locks` と同じ理由・
    /// 同じ形(サイドカー停止が SIGTERM 無視の子で `shutdown_grace` 秒
    /// ブロックしうるので、無関係な別ドライバの操作まで足止めしないよう
    /// ドライバ単位に分けてある)。
    sidecar_runtime_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// `set_filesystem_config`/`set_filesystem_grant` が呼ぶ
    /// `refresh_filesystem_runtime` の臨界区間をドライバ ID ごとに直列化する
    /// ロックのマップ。`sidecar_runtime_locks` とは別マップにしてある理由も
    /// `crate::plugin::registry::Registry::filesystem_runtime_locks` と同じ
    /// (サイドカー停止待ちでファイルアクセス取消が fail-open にならないため)。
    filesystem_runtime_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    drivers_dir: PathBuf,
}

impl DriverRegistry {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        host: Arc<DriverHost>,
        settings_store: Arc<SettingsStore>,
        grants_store: Arc<GrantsStore>,
        sidecar_config_store: Arc<SidecarConfigStore>,
        filesystem_config_store: Arc<FilesystemConfigStore>,
        bus: Bus,
        drivers_dir: PathBuf,
    ) -> Self {
        let process_driver = host.process_driver();
        DriverRegistry {
            entries: Arc::new(Mutex::new(Vec::new())),
            _host: host,
            settings_store,
            grants_store,
            sidecar_config_store,
            filesystem_config_store,
            process_driver,
            bus,
            capabilities_lock: Arc::new(Mutex::new(())),
            sidecar_runtime_locks: Arc::new(Mutex::new(HashMap::new())),
            filesystem_runtime_locks: Arc::new(Mutex::new(HashMap::new())),
            drivers_dir,
        }
    }

    pub(crate) fn push(&self, entry: DriverEntry) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(entry);
    }

    /// ドライバを走査した元ディレクトリ。
    pub fn drivers_dir(&self) -> &Path {
        &self.drivers_dir
    }

    /// 現在登録されている全ドライバの `DriverInfo`(manifest・state・
    /// effective settings・capability 承認状態・サイドカー/ファイルアクセス
    /// 状態)を返す。RPC の一覧応答に使う。
    ///
    /// `entries` ロックは manifest/state のクローン取得のみに使い、ロックを
    /// 解放してから(ディスクを読む)各ストアを呼ぶ
    /// (`crate::plugin::registry::Registry::list` と同じ流儀)。
    pub fn list(&self) -> Vec<DriverInfo> {
        let snapshot: Vec<(DriverManifest, DriverState)> = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|entry| (entry.manifest.clone(), entry.state.clone()))
            .collect();

        snapshot
            .into_iter()
            .map(|(manifest, state)| {
                let settings_manifest = manifest.as_settings_manifest();
                let values = self.settings_store.effective(&settings_manifest);
                let grant_state = self.grants_store.state(&settings_manifest);
                let sidecars = self.build_sidecar_infos(&manifest);
                let filesystem = self.build_filesystem_infos(&manifest);
                DriverInfo {
                    manifest,
                    state,
                    values,
                    grant_state,
                    sidecars,
                    filesystem,
                }
            })
            .collect()
    }

    /// `<driver-id>/<sidecar-name>` の形で `ProcessDriver` のキーを組み立てる。
    /// `DriverCtx::sidecar_key` と同じ規則。
    fn sidecar_key(driver_id: &str, name: &str) -> String {
        format!("{driver_id}/{name}")
    }

    /// `manifest.sidecars` の宣言順に `SidecarInfo` を組み立てる。
    /// `crate::plugin::registry::Registry::build_sidecar_infos` と同じ流儀。
    fn build_sidecar_infos(&self, manifest: &DriverManifest) -> Vec<SidecarInfo> {
        let settings_manifest = manifest.as_settings_manifest();
        let configs = self.sidecar_config_store.effective(&settings_manifest);
        manifest
            .sidecars
            .iter()
            .map(|request| {
                let config = configs
                    .get(&request.name)
                    .cloned()
                    .unwrap_or_else(|| SidecarConfig::from_request(request));
                let grant = self
                    .grants_store
                    .sidecar_state(&settings_manifest, &request.name);
                self.sidecar_info_and_entry(manifest, request, config, grant)
                    .0
            })
            .collect()
    }

    /// `request` 1 件分の(既に取得済みの)設定・承認状態から `SidecarInfo` と
    /// (`sidecars_json` バッファ用の)`SidecarRuntimeEntry` を両方組み立てる。
    /// `crate::plugin::registry::Registry::sidecar_info_and_entry` と同じ流儀
    /// (`ProcessDriver::status` の呼び出しを両者で 1 回だけ共有する)。
    fn sidecar_info_and_entry(
        &self,
        manifest: &DriverManifest,
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
        let key = Self::sidecar_key(&manifest.id, &request.name);
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

    /// `manifest.filesystem` の宣言順に `FilesystemInfo` を組み立てる。
    /// `crate::plugin::registry::Registry::build_filesystem_infos` と同じ流儀。
    fn build_filesystem_infos(&self, manifest: &DriverManifest) -> Vec<FilesystemInfo> {
        let settings_manifest = manifest.as_settings_manifest();
        let configs = self.filesystem_config_store.effective(&settings_manifest);
        manifest
            .filesystem
            .iter()
            .map(|request| {
                let config = configs.get(&request.name).cloned().unwrap_or_else(|| {
                    crate::plugin::filesystem::FilesystemConfig {
                        path: String::new(),
                    }
                });
                let grant = self
                    .grants_store
                    .filesystem_state(&settings_manifest, &request.name);
                FilesystemInfo {
                    request: request.clone(),
                    config,
                    grant,
                }
            })
            .collect()
    }

    /// `id` のドライバの manifest クローンを返す(`entries` ロック保持は
    /// このルックアップの間だけ)。
    fn find_manifest(&self, id: &str) -> Result<DriverManifest, DriverRegistryError> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|entry| entry.manifest.id == id)
            .map(|entry| entry.manifest.clone())
            .ok_or_else(|| DriverRegistryError::UnknownDriver(id.to_string()))
    }

    /// `id` のドライバの manifest クローンを返す(存在しなければ `None`)。
    pub fn manifest_of(&self, id: &str) -> Option<DriverManifest> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|entry| entry.manifest.id == id)
            .map(|entry| entry.manifest.clone())
    }

    /// `id` のドライバの effective settings(`SettingsStore` 由来)を返す。
    pub fn values(
        &self,
        id: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, DriverRegistryError> {
        let manifest = self.find_manifest(id)?;
        Ok(self
            .settings_store
            .effective(&manifest.as_settings_manifest()))
    }

    /// `id` のドライバの settings を検証・永続化し、稼働中ドライバが参照する
    /// 共有 `settings_json` も新しい effective 値で上書きする。
    /// `crate::plugin::registry::Registry::set_values` と同じ流儀。
    pub fn set_values(
        &self,
        id: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, DriverRegistryError> {
        let (manifest, settings_json) = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .ok_or_else(|| DriverRegistryError::UnknownDriver(id.to_string()))?;
            (entry.manifest.clone(), entry.settings_json.clone())
        };

        let settings_manifest = manifest.as_settings_manifest();
        let effective = self
            .settings_store
            .update_and_effective(&settings_manifest, values)
            .map_err(DriverRegistryError::Settings)?;

        let settings_json_string =
            serde_json::to_string(&serde_json::Value::Object(effective.clone()))
                .unwrap_or_else(|_| "{}".to_string());
        *settings_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings_json_string;

        Ok(effective)
    }

    /// `id` のドライバの capability 承認/取消を `GrantsStore` に永続化し、
    /// 稼働中ドライバが参照する共有 `capabilities_json` も更新する。
    /// `crate::plugin::registry::Registry::set_capabilities` と同じ流儀
    /// (承認済みサイドカーの暗黙 127.0.0.1 許可を合流させるのも同様)。
    pub fn set_capabilities(
        &self,
        id: &str,
        granted: bool,
    ) -> Result<GrantState, DriverRegistryError> {
        let (manifest, capabilities_json, sidecars_json) = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .ok_or_else(|| DriverRegistryError::UnknownDriver(id.to_string()))?;
            (
                entry.manifest.clone(),
                entry.capabilities_json.clone(),
                entry.sidecars_json.clone(),
            )
        };

        let _capabilities_guard = self
            .capabilities_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let settings_manifest = manifest.as_settings_manifest();
        let state = self
            .grants_store
            .set(&settings_manifest, granted)
            .map_err(DriverRegistryError::Grants)?;

        let mut effective_hosts = if state.granted {
            settings_manifest.capability_hosts()
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

    /// `id` のドライバの manifest クローンを返す(`RegistryError` 版)。
    /// `find_manifest` と同じルックアップだが、サイドカー/ファイルアクセス
    /// 系のメソッド群は `crate::plugin::registry::Registry` の対応メソッドと
    /// エラー型を揃えるため(`SidecarInfo`/`FilesystemInfo`/`SidecarAction`/
    /// `RegistryError` は既にプラグイン・ドライバ両レイヤーで共有されている
    /// 型 -- タスクブリーフ参照)、`DriverRegistryError` ではなくこちらを
    /// 使う。未登録の場合は `RegistryError::UnknownDriver` を返す
    /// (`RegistryError::UnknownPlugin` ではない -- レビュー指摘: これは
    /// ドライバの未登録であり、既存の `drivers/set-capabilities` アーム
    /// (`DriverRegistryError::UnknownDriver` 経由)が既に "unknown driver:
    /// {id}" という文言を使っている以上、ここも同じ文言に揃える必要がある。
    /// `UnknownPlugin` を使い回すと、同じ「未登録のドライバ」失敗が
    /// `drivers/*` アームによって "unknown plugin: ..." と "unknown driver:
    /// ..." の 2 通りの文言に分かれてしまう)。
    fn find_manifest_for_shared(&self, id: &str) -> Result<DriverManifest, RegistryError> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|entry| entry.manifest.id == id)
            .map(|entry| entry.manifest.clone())
            .ok_or_else(|| RegistryError::UnknownDriver(id.to_string()))
    }

    /// `id` のドライバが現在 `Disabled` かどうか。
    /// `crate::plugin::registry::Registry::is_disabled` と同じ流儀。
    fn is_disabled(&self, id: &str) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|entry| entry.manifest.id == id)
            .is_some_and(|entry| matches!(entry.state, DriverState::Disabled { .. }))
    }

    /// `id` 用のサイドカー実行時ロック(`sidecar_runtime_locks`)を引く。
    /// 無ければ作る。`crate::plugin::registry::Registry::sidecar_runtime_lock_for`
    /// と同じ流儀。
    fn sidecar_runtime_lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        Self::lock_for(&self.sidecar_runtime_locks, id)
    }

    /// `id` 用のファイルアクセス実行時ロック(`filesystem_runtime_locks`)を
    /// 引く。サイドカー側とは別のマップであることが要点(理由は
    /// `filesystem_runtime_locks` のドキュメント参照)。
    fn filesystem_runtime_lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        Self::lock_for(&self.filesystem_runtime_locks, id)
    }

    fn lock_for(map: &Mutex<HashMap<String, Arc<Mutex<()>>>>, id: &str) -> Arc<Mutex<()>> {
        let mut locks = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        locks
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// `id` のドライバの現在のサイドカー状態一覧(manifest の `[[sidecar]]`
    /// 宣言順)を返す。`crate::plugin::registry::Registry::sidecars` と同じ。
    pub fn sidecars(&self, id: &str) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest_for_shared(id)?;
        Ok(self.build_sidecar_infos(&manifest))
    }

    /// `id` のドライバの現在のファイルアクセス状態一覧(manifest の
    /// `[[filesystem]]` 宣言順)を返す。
    /// `crate::plugin::registry::Registry::filesystem` と同じ。
    pub fn filesystem(&self, id: &str) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let manifest = self.find_manifest_for_shared(id)?;
        Ok(self.build_filesystem_infos(&manifest))
    }

    /// ファイルアクセスの設定変更・承認変更のあとに必ず呼ぶ内部ヘルパー。
    /// `crate::plugin::registry::Registry::refresh_filesystem_runtime` と
    /// 同じ流儀(「止めるべきプロセス」が無いので `filesystem_json` を
    /// 作り直すだけ)。
    fn refresh_filesystem_runtime(&self, id: &str) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let (manifest, filesystem_json) = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .ok_or_else(|| RegistryError::UnknownDriver(id.to_string()))?;
            (entry.manifest.clone(), entry.filesystem_json.clone())
        };

        let runtime_lock = self.filesystem_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let infos = self.build_filesystem_infos(&manifest);
        let runtime_entries: Vec<FsRuntimeEntry> = infos
            .iter()
            .map(|info| FsRuntimeEntry {
                name: info.request.name.clone(),
                granted: info.grant.granted,
                mode: info.request.mode.as_str().to_string(),
                path: info.config.path.clone(),
            })
            .collect();
        *filesystem_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            filesystem_json_string(&runtime_entries);

        Ok(infos)
    }

    /// `id` のドライバの `name` ファイルアクセスルートの設定を検証・永続化し
    /// (`FilesystemConfigStore::update_and_effective`)、稼働中ドライバが
    /// 参照する `filesystem_json` を作り直してから最新の `FilesystemInfo`
    /// 一覧を返す。検証に失敗した場合は何も変更されない。
    /// `crate::plugin::registry::Registry::set_filesystem_config` と同じ。
    pub fn set_filesystem_config(
        &self,
        id: &str,
        name: &str,
        config: &FilesystemConfig,
    ) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let manifest = self.find_manifest_for_shared(id)?;
        let settings_manifest = manifest.as_settings_manifest();
        self.filesystem_config_store
            .update_and_effective(&settings_manifest, name, config)
            .map_err(RegistryError::FilesystemConfig)?;
        self.refresh_filesystem_runtime(id)
    }

    /// `id` のドライバの `name` ファイルアクセスルートの承認/取消を
    /// `GrantsStore` に永続化する。`granted == true` のとき、ディレクトリが
    /// 未設定(空文字)のルートは拒否する(`RegistryError::Filesystem`)。
    /// `crate::plugin::registry::Registry::set_filesystem_grant` と同じ検証
    /// (UI 側の防御に加えてここでも強制する -- タスクブリーフが挙げる
    /// 「path 未設定では承認できない」の実体)。
    pub fn set_filesystem_grant(
        &self,
        id: &str,
        name: &str,
        granted: bool,
    ) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let manifest = self.find_manifest_for_shared(id)?;
        let settings_manifest = manifest.as_settings_manifest();
        if settings_manifest.filesystem_root(name).is_none() {
            return Err(RegistryError::UnknownFilesystem(name.to_string()));
        }

        if granted {
            let configs = self.filesystem_config_store.effective(&settings_manifest);
            let path_configured = configs
                .get(name)
                .map(|config| !config.path.is_empty())
                .unwrap_or(false);
            if !path_configured {
                return Err(RegistryError::Filesystem(format!(
                    "filesystem root {name} has no directory configured; cannot grant"
                )));
            }
        }

        self.grants_store
            .set_filesystem(&settings_manifest, name, granted)
            .map_err(RegistryError::Grants)?;

        self.refresh_filesystem_runtime(id)
    }

    /// サイドカーの設定変更・承認変更のあとに必ず呼ぶ内部ヘルパー。
    /// `crate::plugin::registry::Registry::refresh_sidecar_runtime` と同じ
    /// 3 手順(停止 → `sidecars_json` の作り直し → `capabilities_json` の
    /// 作り直し)を、ドライバ専用の `sidecar_runtime_lock_for(id)` の下で
    /// 行う。
    fn refresh_sidecar_runtime(
        &self,
        id: &str,
        stop_names: &[String],
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let (manifest, sidecars_json, capabilities_json) = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .ok_or_else(|| RegistryError::UnknownDriver(id.to_string()))?;
            (
                entry.manifest.clone(),
                entry.sidecars_json.clone(),
                entry.capabilities_json.clone(),
            )
        };

        let runtime_lock = self.sidecar_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        for name in stop_names {
            let key = Self::sidecar_key(&manifest.id, name);
            self.process_driver.stop(&key);
        }

        let settings_manifest = manifest.as_settings_manifest();
        let sidecar_configs = self.sidecar_config_store.effective(&settings_manifest);
        let mut infos = Vec::with_capacity(manifest.sidecars.len());
        let mut runtime_entries = Vec::with_capacity(manifest.sidecars.len());
        for request in &manifest.sidecars {
            let config = sidecar_configs
                .get(&request.name)
                .cloned()
                .unwrap_or_else(|| SidecarConfig::from_request(request));
            let grant = self
                .grants_store
                .sidecar_state(&settings_manifest, &request.name);
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
            let http_granted = self.grants_store.state(&settings_manifest).granted;
            let mut hosts = if http_granted {
                settings_manifest.capability_hosts()
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

    /// `id` のドライバの `name` サイドカーの設定を検証・永続化し、稼働中の
    /// 実行を止めてから(`refresh_sidecar_runtime`)、最新の `SidecarInfo`
    /// 一覧を返す。検証に失敗した場合は何も変更されない。
    /// `crate::plugin::registry::Registry::set_sidecar_config` と同じ。
    pub fn set_sidecar_config(
        &self,
        id: &str,
        name: &str,
        config: &SidecarConfig,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest_for_shared(id)?;
        let settings_manifest = manifest.as_settings_manifest();
        self.sidecar_config_store
            .update_and_effective(&settings_manifest, name, config)
            .map_err(RegistryError::SidecarConfig)?;
        let stop_names = vec![name.to_string()];
        self.refresh_sidecar_runtime(id, &stop_names)
    }

    /// `id` のドライバの `name` サイドカーの承認/取消を `GrantsStore` に
    /// 永続化する。取消のときは稼働中の実行を止める。`granted == true` の
    /// とき、`command` が未設定(空文字)のサイドカーは拒否する
    /// (`RegistryError::Sidecar`)。
    /// `crate::plugin::registry::Registry::set_sidecar_grant` と同じ検証
    /// (「`command` 未設定では承認できない」を UI・RPC 層ではなくここで
    /// 強制する)。
    pub fn set_sidecar_grant(
        &self,
        id: &str,
        name: &str,
        granted: bool,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest_for_shared(id)?;
        let settings_manifest = manifest.as_settings_manifest();
        if settings_manifest.sidecar(name).is_none() {
            return Err(RegistryError::UnknownSidecar(name.to_string()));
        }

        if granted {
            let configs = self.sidecar_config_store.effective(&settings_manifest);
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
            .set_sidecar(&settings_manifest, name, granted)
            .map_err(RegistryError::Grants)?;

        let stop_names: Vec<String> = if granted {
            Vec::new()
        } else {
            vec![name.to_string()]
        };
        self.refresh_sidecar_runtime(id, &stop_names)
    }

    /// `id` のドライバの `name` サイドカーを直接操作する(ユーザー操作起点)。
    /// `crate::plugin::registry::Registry::control_sidecar` と同じ(`Stop` は
    /// 常に許す、`Start`/`Restart` は未承認・`command` 未設定・ドライバが
    /// `Disabled` を `RegistryError::Sidecar` として拒否し、`sidecar_runtime_lock_for(id)`
    /// を「承認を読む」から `ensure_started` を呼び終えるまで保持して
    /// TOCTOU を防ぐ)。
    pub fn control_sidecar(
        &self,
        id: &str,
        name: &str,
        action: SidecarAction,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest_for_shared(id)?;
        let settings_manifest = manifest.as_settings_manifest();
        let request = settings_manifest
            .sidecar(name)
            .ok_or_else(|| RegistryError::UnknownSidecar(name.to_string()))?;
        let key = Self::sidecar_key(&manifest.id, name);

        match action {
            SidecarAction::Stop => {
                self.process_driver.stop(&key);
            }
            SidecarAction::Start | SidecarAction::Restart => {
                let runtime_lock = self.sidecar_runtime_lock_for(id);
                let _runtime_guard = runtime_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());

                if self.is_disabled(id) {
                    return Err(RegistryError::Sidecar(format!("driver {id} is disabled")));
                }

                if action == SidecarAction::Restart {
                    self.process_driver.stop(&key);
                }

                let grant = self.grants_store.sidecar_state(&settings_manifest, name);
                if !grant.granted {
                    return Err(RegistryError::Sidecar(format!(
                        "sidecar {name} is not granted"
                    )));
                }

                let configs = self.sidecar_config_store.effective(&settings_manifest);
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

    /// `manifest` が指すドライバを `Disabled { reason }` にし、そのドライバ
    /// が持つ全サイドカーを停止し、バスからも切り離す(`bus.disable_driver`)。
    ///
    /// **`id` だけでなく `manifest` 全体を引数に取る**(`entries` から
    /// ルックアップしない)。理由は下の「`entries` に載っているかどうかに
    /// 関わらず」の節を参照。呼び出し元(`run_driver_thread`)はどのみち
    /// `manifest` を手元に持っているので、渡すコストは無い。
    ///
    /// **`bus.disable_driver` とサイドカー停止は、`entries` に対応する
    /// `DriverEntry` が載っているかどうかに関わらず必ず実行する。** 一方
    /// `Disabled` への状態フラグ更新は `entries` に見つかった場合に限る
    /// (見つからなければ更新すべき状態そのものが無いので当然)。この 2 つを
    /// 分けているのは意図的なレース対策(最終レビューで見つかった重要な
    /// 取りこぼし): `load_and_run_driver` は `bus.register_driver` をスレッド
    /// 起動より前に行うが、`registry.push` はスレッドが `ready_tx.send
    /// (DriverState::Running)` で `Running` を報告し、メインスレッドの
    /// `ready_rx.recv()` が戻ってから初めて行う。ところがドライバ専用
    /// スレッドは `Running` を報告した直後から `messages_rx` を読み始め、
    /// バスに既に溜まっていたメッセージ(あるいは register 直後に他プラグ
    /// インが即座に `publish` したメッセージ)に対して `call_on_message` を
    /// 呼びうる -- それが trap すれば、メインスレッドがまだ `push` して
    /// いない窓の間にこの関数が呼ばれる。この窓で `entries` を見て何もしな
    /// いと、実際にはまだ誰もレジストリに載せていないだけで(場合によっては
    /// ドライバ自身がその `init`/最初のメッセージ処理中に自分で起動した
    /// サイドカーも含めて)生きているバスのスロット・サイドカープロセスが
    /// そのまま残り続けてしまう(fail-open)。
    ///
    /// **`bus.disable_driver` を呼ぶのがプラグインの `set_disabled` との
    /// 一番の違い**: ドライバが死ねば、それに接続している全プラグインの
    /// `get`/`publish` はもう最新の値を届けられない。`available` フラグを
    /// 落として retained 値を破棄しておかないと、プラグイン側は「まだ
    /// 更新が来ていないだけ」と「もう誰も更新しない」を区別できず、古い
    /// 値を握ったまま動き続けてしまう(fail-open。
    /// `edlr_driver_channel::Bus::disable_driver` のドキュメント参照)。
    ///
    /// **状態を `Disabled` にするのを、サイドカーを止めるより先に行う**
    /// (`crate::plugin::registry::Registry::set_disabled` と同じ順序・同じ
    /// ロック規律。以前はここが「サイドカーを止める → 最後に `Disabled` を
    /// 立てる」の順で、しかも `sidecar_runtime_lock_for(id)` を一切取って
    /// いなかった -- Important: 最終レビューで見つかった取りこぼし)。この順序
    /// だと、`control_sidecar` の `Start`/`Restart` 分岐(こちらも
    /// `sidecar_runtime_lock_for(id)` を取ってから `is_disabled` を読む)が
    /// ちょうど「サイドカー停止が終わった直後・`Disabled` が立つ直前」の窓に
    /// 割り込むと、まだ `Running` に見える状態を信じてサイドカーを再起動して
    /// しまい、そのドライバはもう死んでいるので誰にも止められない
    /// (エンジンプロセスが孤児化する)。`Disabled` を先に立ててから
    /// `sidecar_runtime_lock_for(id)` を取ってサイドカーを止める(`entries`
    /// ロック → id 別ロックという既存の取得順序のまま)ことで、
    /// `control_sidecar` とは常に排他になる: この関数がロックを取った時点で
    /// 状態は既に `Disabled` に確定しているので、ロック取得後に
    /// `control_sidecar` が読む状態と競合しない。
    pub fn set_disabled(&self, manifest: &DriverManifest, reason: String) {
        self.bus.disable_driver(&manifest.id);

        {
            let mut guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = guard
                .iter_mut()
                .find(|entry| entry.manifest.id == manifest.id)
            {
                entry.state = DriverState::Disabled { reason };
            }
        }

        let runtime_lock = self.sidecar_runtime_lock_for(&manifest.id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for sidecar in &manifest.sidecars {
            let key = Self::sidecar_key(&manifest.id, &sidecar.name);
            self.process_driver.stop(&key);
        }
    }

    /// 全ドライバの全サイドカーインスタンスを停止する(デーモン shutdown 用)。
    /// `crate::plugin::registry::Registry::stop_all_sidecars` と同じ流儀
    /// (`ProcessDriver::stop_all` をそのまま呼ぶ薄い入口)。
    ///
    /// **Critical: 最終レビューで見つかった取りこぼし**。デーモンの shutdown
    /// シーケンス(`core/src/bin/edlr.rs`)は、これが追加されるまでプラグイン
    /// 側の `Registry::stop_all_sidecars` しか呼んでいなかった -- `registry`
    /// と `drivers` は別々の `Arc` で、`DriverRegistry` は独自の `ProcessDriver`
    /// (`DriverHost::new` 経由)を持つため、プラグインの停止呼び出しは
    /// ドライバのサイドカーには一切効かない。`DriverHost` は `Drop` で
    /// `stop_all` を最後の砦として呼ぶが、`_host: Arc<DriverHost>` は
    /// `DriverRegistry` 自身と各ドライバ専用スレッドの両方が握っており、後者
    /// は `for message in messages_rx` でブロックし続けて(`Bus::DriverSlot`
    /// が `SyncSender<Message>` を送信側として保持し続ける限り自然終了しない)
    /// 決して drop されないので、その `Drop` は実質発火しない。だからこそ
    /// ここで明示的に呼ぶ必要がある。
    pub fn stop_all_sidecars(&self) {
        self.process_driver.stop_all();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::driver::host::DriverHost;
    use std::thread;
    use std::time::Duration;

    fn manifest_with_topic(id: &str) -> DriverManifest {
        DriverManifest {
            id: id.into(),
            name: id.into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "driver.wasm".into(),
            topics: vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            }],
            settings: Vec::new(),
            capabilities: Vec::new(),
            sidecars: Vec::new(),
            filesystem: Vec::new(),
        }
    }

    /// `ed-state` を 1 件だけ載せた `DriverRegistry`。`current-system`
    /// (retain 付き)と `ship-status` の 2 トピックを宣言する -- 前者は
    /// `crate::server` の `drivers/list` テストが `topics[0]` として見るもの、
    /// 後者は `crate::plugin::registry::tests::test_registry_with_bus_request`
    /// が宣言する `[[bus]]` 要求(`publish = ["ship-status"]`、
    /// `subscribe = ["current-system"]`)を両方とも解決させる(`resolved: true`
    /// にする)ために要る。`bus` は呼び出し元と共有させたいので引数で受け取る
    /// (`crate::plugin::registry::tests` 側のテストが同じ `Bus` を使う場合に
    /// 備える。今のところ呼び出し元は毎回新しい `Bus::new()` を渡している)。
    ///
    /// http capability も 1 件宣言する(`crate::server` の
    /// `drivers/set-capabilities` テストが「承認が実際に切り替わって永続化
    /// されること」を確認できるようにするため -- capability を 1 つも宣言
    /// しない manifest は `Manifest::capabilities_fingerprint` が `None` を
    /// 返し、`GrantsStore::set` が常に `granted: false` を返してしまい、
    /// テストが承認の可否ではなく応答の形しか確認できなくなる)。
    pub(crate) fn test_registry(bus: edlr_driver_channel::Bus) -> DriverRegistry {
        let registry = bare_registry(bus);
        let mut manifest = manifest_with_topic("ed-state");
        manifest.topics.push(edlr_driver_channel::TopicSpec {
            name: "ship-status".into(),
            retain: false,
            description: String::new(),
        });
        manifest.capabilities.push(
            crate::plugin::manifest::CapabilityRequest::Http {
                hosts: vec!["https://example.com".into()],
                reason: "test".into(),
            },
        );
        registry.push(DriverEntry {
            manifest,
            state: DriverState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(r#"{"hosts":[]}"#.to_string())),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
        });
        registry
    }

    /// ドライバを 1 件もロードしていない `DriverRegistry`。`test_registry` の
    /// 対極(「ドライバが無ければ unresolved」を示す `crate::server` のテスト
    /// 用フィクスチャ)。
    pub(crate) fn test_registry_without_ed_state(bus: edlr_driver_channel::Bus) -> DriverRegistry {
        bare_registry(bus)
    }

    fn manifest_with_sidecar(id: &str, port: u16) -> DriverManifest {
        DriverManifest {
            id: id.into(),
            name: id.into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "driver.wasm".into(),
            topics: Vec::new(),
            settings: Vec::new(),
            capabilities: Vec::new(),
            sidecars: vec![crate::plugin::manifest::SidecarRequest {
                name: "tts".into(),
                reason: "reason".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                port,
                scalable: false,
            }],
            filesystem: Vec::new(),
        }
    }

    #[test]
    fn disabling_a_driver_marks_it_and_drops_its_retained_values() {
        let manifest = manifest_with_topic("ed-state");
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(&manifest.id, manifest.topics.clone(), tx);
        bus.emit("ed-state", "current-system", b"Sol".to_vec())
            .unwrap();

        let registry = bare_registry(bus.clone());
        registry.push(DriverEntry {
            manifest: manifest.clone(),
            state: DriverState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(r#"{"hosts":[]}"#.to_string())),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
        });

        registry.set_disabled(&manifest, "on-message call failed".to_string());

        assert!(matches!(
            registry.list()[0].state,
            DriverState::Disabled { .. }
        ));
        assert_eq!(bus.retained_for("ed-state", "current-system"), None);
    }

    /// Regression test for a review finding: the driver's dedicated thread
    /// starts draining `messages_rx` (and can therefore call `call_on_message`
    /// and trap, invoking `set_disabled`) the instant it reports `Running`,
    /// which happens *before* `load_and_run_driver`'s `registry.push` runs on
    /// the main thread. `set_disabled` used to look the id up in `entries`
    /// and no-op entirely if not found yet, silently leaving the bus slot
    /// `available: true` with stale retained values for a driver that is
    /// already dead in this race window. `set_disabled` must disconnect the
    /// bus (and stop sidecars) regardless of whether the entry has landed in
    /// the registry -- only the `Disabled` state-flag update is conditional
    /// on that (there's genuinely nothing to flip if the entry isn't there).
    ///
    /// This simulates the race directly: the driver is registered on the
    /// `Bus` (as `load_and_run_driver` does before spawning the thread) but
    /// no `DriverEntry` is ever pushed (as if `set_disabled` fired before the
    /// main thread's `registry.push`).
    #[test]
    fn set_disabled_disconnects_the_bus_slot_even_before_the_entry_is_pushed() {
        let manifest = manifest_with_topic("ed-state");
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(&manifest.id, manifest.topics.clone(), tx);
        bus.emit("ed-state", "current-system", b"Sol".to_vec())
            .unwrap();

        let registry = bare_registry(bus.clone());
        // Deliberately no `registry.push(..)` here: the entry has not
        // landed yet, simulating the race window.
        assert!(registry.list().is_empty());

        registry.set_disabled(&manifest, "on-message call failed".to_string());

        assert_eq!(
            bus.retained_for("ed-state", "current-system"),
            None,
            "the bus slot must be disconnected even though no DriverEntry was ever pushed"
        );
    }

    /// Regression test mirroring
    /// `crate::plugin::registry::tests::set_disabled_stops_all_sidecars_of_that_plugin`:
    /// `set_disabled`'s sidecar-stop half was previously untested (the other
    /// `set_disabled` test's fixture declares zero sidecars), so a future
    /// refactor that dropped it would go uncaught.
    #[test]
    fn set_disabled_stops_all_sidecars_of_that_driver() {
        let manifest = manifest_with_sidecar("sc-driver", 50940);
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(&manifest.id, manifest.topics.clone(), tx);

        let registry = bare_registry(bus);
        registry.push(DriverEntry {
            manifest: manifest.clone(),
            state: DriverState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(r#"{"hosts":[]}"#.to_string())),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
        });

        let key = DriverRegistry::sidecar_key("sc-driver", "tts");
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "sleep 30".into()],
            ports: vec![50940],
        };
        registry
            .process_driver
            .ensure_started(&key, &spec)
            .expect("start sidecar directly via the driver, bypassing wasm entirely");
        assert!(
            registry.process_driver.status(&key, &spec)[0].running,
            "sidecar should be running before disabling the driver"
        );

        registry.set_disabled(&manifest, "on-message call failed".to_string());

        assert!(
            !registry.process_driver.status(&key, &spec)[0].running,
            "set_disabled must stop the disabled driver's sidecars"
        );
        assert!(matches!(
            registry.list()[0].state,
            DriverState::Disabled { .. }
        ));
    }

    /// Regression test for Minor finding 10: `run_driver_thread` used to
    /// return on a `call_init()` error without calling `registry.set_disabled`
    /// at all, so a driver whose `init()` starts a sidecar and then traps
    /// left that sidecar running forever (nothing else would ever call
    /// `set_disabled` for it, since the thread exits before reaching the
    /// `messages_rx` loop that's the only other place that calls it).
    ///
    /// This test exercises the exact race window `run_driver_thread`'s fix
    /// relies on directly at the `DriverRegistry` level (without going
    /// through real wasm): a sidecar is started for a manifest whose
    /// `DriverEntry` has **not** been pushed yet (mirroring "trapped during
    /// `init`, before `load_and_run_driver`'s `registry.push`"), and
    /// `set_disabled` must still stop it -- this is the same guarantee
    /// `set_disabled_disconnects_the_bus_slot_even_before_the_entry_is_pushed`
    /// pins for the bus slot, combined with the sidecar-stop assertion from
    /// `set_disabled_stops_all_sidecars_of_that_driver` above, which that
    /// other test's zero-sidecar fixture couldn't exercise together.
    #[test]
    fn set_disabled_stops_a_sidecar_started_during_init_before_the_entry_is_ever_pushed() {
        let manifest = manifest_with_sidecar("init-trap-driver", 50943);
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(&manifest.id, manifest.topics.clone(), tx);

        let registry = bare_registry(bus);
        // Deliberately no `registry.push(..)`: as in `run_driver_thread`,
        // `call_init` traps before the entry ever lands in the registry.

        let key = DriverRegistry::sidecar_key("init-trap-driver", "tts");
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "sleep 30".into()],
            ports: vec![50943],
        };
        registry
            .process_driver
            .ensure_started(&key, &spec)
            .expect("simulate init() starting a sidecar directly via the driver, bypassing wasm");
        assert!(
            registry.process_driver.status(&key, &spec)[0].running,
            "sidecar should be running before the simulated init() failure"
        );

        registry.set_disabled(&manifest, "init() failed: boom".to_string());

        assert!(
            !registry.process_driver.status(&key, &spec)[0].running,
            "set_disabled must stop a sidecar started during init(), even though the \
             DriverEntry was never pushed (the trap-before-push race)"
        );
    }

    /// Regression test mirroring
    /// `crate::plugin::registry::tests::control_sidecar_rejects_start_and_restart_once_the_plugin_is_disabled_but_allows_stop`.
    /// `control_sidecar` already checks `is_disabled`, so this passed before
    /// the `set_disabled` ordering fix too -- it pins the sequential
    /// (non-racing) case as a baseline alongside the concurrent regression
    /// test below.
    #[test]
    fn control_sidecar_rejects_start_and_restart_once_the_driver_is_disabled_but_allows_stop() {
        let manifest = manifest_with_sidecar("sc-driver2", 50941);
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(&manifest.id, manifest.topics.clone(), tx);

        let registry = bare_registry(bus);
        registry.push(DriverEntry {
            manifest: manifest.clone(),
            state: DriverState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(r#"{"hosts":[]}"#.to_string())),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
        });
        let settings_manifest = manifest.as_settings_manifest();
        registry
            .grants_store
            .set_sidecar(&settings_manifest, "tts", true)
            .expect("grant should persist");
        registry
            .sidecar_config_store
            .update_and_effective(
                &settings_manifest,
                "tts",
                &SidecarConfig {
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    port: 50941,
                    replicas: 1,
                },
            )
            .expect("config should persist");

        registry.set_disabled(&manifest, "on-message call failed".to_string());

        match registry.control_sidecar("sc-driver2", "tts", SidecarAction::Start) {
            Err(RegistryError::Sidecar(_)) => {}
            Err(other) => panic!(
                "Start on a disabled driver's sidecar must be rejected as RegistryError::Sidecar, got {other}"
            ),
            Ok(_) => panic!(
                "Start on a disabled driver's sidecar must be rejected as RegistryError::Sidecar"
            ),
        }
        match registry.control_sidecar("sc-driver2", "tts", SidecarAction::Restart) {
            Err(RegistryError::Sidecar(_)) => {}
            Err(other) => panic!(
                "Restart on a disabled driver's sidecar must be rejected as RegistryError::Sidecar, got {other}"
            ),
            Ok(_) => panic!(
                "Restart on a disabled driver's sidecar must be rejected as RegistryError::Sidecar"
            ),
        }

        let key = DriverRegistry::sidecar_key("sc-driver2", "tts");
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            ports: vec![50941],
        };
        assert!(
            !registry.process_driver.status(&key, &spec)[0].running,
            "rejected Start/Restart must not have spawned anything"
        );

        registry
            .control_sidecar("sc-driver2", "tts", SidecarAction::Stop)
            .expect("Stop must never be blocked by the driver's disabled state");
    }

    /// **Regression test for Important finding 2 from the final review**:
    /// `DriverRegistry::set_disabled` used to stop sidecars *before* marking
    /// the driver `Disabled`, and never took `sidecar_runtime_lock_for(id)` at
    /// all -- unlike `control_sidecar`'s `Start`/`Restart` branch, which takes
    /// that per-id lock and then reads `is_disabled`. That left a window: a
    /// trapping driver's `set_disabled` could finish stopping the sidecar,
    /// and *before* it flipped the state to `Disabled`, a concurrent
    /// `control_sidecar(Start)` could read `is_disabled() == false`, see a
    /// live grant, and spawn the sidecar right back -- with nothing left
    /// alive to ever stop it again, since the driver is dead.
    ///
    /// This hammers that exact race: many threads call `control_sidecar`
    /// `Start` in a tight loop while another thread calls `set_disabled`
    /// once, partway through. The invariant that must hold once everything
    /// settles: if the driver ends up `Disabled`, its sidecar must not be
    /// left running. On the pre-fix code (stop-then-mark, no lock) this is
    /// reliably violated within a handful of iterations; on the fixed code
    /// (mark-then-lock-then-stop, same order/locking as the plugin registry)
    /// the two operations can no longer interleave.
    #[test]
    fn concurrent_set_disabled_and_control_sidecar_start_never_leaves_a_disabled_driver_with_a_running_sidecar(
    ) {
        let manifest = manifest_with_sidecar("race-driver", 50942);
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(&manifest.id, manifest.topics.clone(), tx);

        let registry = bare_registry(bus);
        registry.push(DriverEntry {
            manifest: manifest.clone(),
            state: DriverState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(r#"{"hosts":[]}"#.to_string())),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
        });
        let settings_manifest = manifest.as_settings_manifest();
        registry
            .grants_store
            .set_sidecar(&settings_manifest, "tts", true)
            .expect("grant should persist");
        registry
            .sidecar_config_store
            .update_and_effective(
                &settings_manifest,
                "tts",
                &SidecarConfig {
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    port: 50942,
                    replicas: 1,
                },
            )
            .expect("config should persist");

        const THREADS: usize = 48;
        const ITERATIONS: usize = 1500;
        let barrier = Arc::new(std::sync::Barrier::new(THREADS + 1));

        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let registry = registry.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..ITERATIONS {
                    let _ = registry.control_sidecar("race-driver", "tts", SidecarAction::Start);
                }
            }));
        }
        let disabler = {
            let registry = registry.clone();
            let manifest = manifest.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                // Deliberately no delay: releasing at the same barrier as the
                // hammering threads maximizes overlap between `set_disabled`
                // and the very first burst of concurrent `ensure_started`
                // spawns, which is where the pre-fix race is easiest to hit
                // (a `Start` that reads `is_disabled() == false` and only
                // finishes spawning *after* `set_disabled`'s one-shot `stop`
                // call has already run and found nothing to stop).
                barrier.wait();
                registry.set_disabled(&manifest, "on-message call failed".to_string());
            })
        };

        for handle in handles {
            handle.join().expect("worker thread should not panic");
        }
        disabler.join().expect("disabler thread should not panic");

        let key = DriverRegistry::sidecar_key("race-driver", "tts");
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            ports: vec![50942],
        };
        let running = registry
            .process_driver
            .status(&key, &spec)
            .iter()
            .any(|i| i.running);
        let disabled = matches!(registry.list()[0].state, DriverState::Disabled { .. });

        assert!(disabled, "set_disabled must have completed by the time all threads joined");
        assert!(
            !running,
            "a driver left Disabled must not have a sidecar running behind it -- \
             a concurrent Start read a not-yet-disabled state and spawned it back"
        );

        registry.process_driver.stop(&key);
    }

    /// Builds an empty `DriverRegistry` (no `DriverEntry` pushed) wired to
    /// `bus`, without loading any wasm (`DriverRegistry::push` takes a
    /// hand-built `DriverEntry` directly -- same convention
    /// `plugin::registry`'s tests use with `Registry::push`).
    fn bare_registry(bus: edlr_driver_channel::Bus) -> DriverRegistry {
        let tmp = tempfile::tempdir().unwrap();
        DriverRegistry::new(
            Arc::new(DriverHost::new().expect("wasmtime engine builds")),
            Arc::new(SettingsStore::new(tmp.path().join("settings"))),
            Arc::new(GrantsStore::new_for_drivers(tmp.path().join("grants"))),
            Arc::new(SidecarConfigStore::new(tmp.path().join("settings"))),
            Arc::new(FilesystemConfigStore::new(
                tmp.path().join("settings"),
                vec![tmp.path().to_path_buf()],
            )),
            bus,
            tmp.path().join("drivers"),
        )
    }

    fn manifest_with_sidecar_and_filesystem(id: &str, port: u16) -> DriverManifest {
        DriverManifest {
            id: id.into(),
            name: id.into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "driver.wasm".into(),
            topics: Vec::new(),
            settings: Vec::new(),
            capabilities: Vec::new(),
            sidecars: vec![SidecarRequest {
                name: "engine".into(),
                reason: "reason".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                port,
                scalable: false,
            }],
            filesystem: vec![crate::plugin::manifest::FilesystemRequest {
                name: "cache".into(),
                reason: "reason".into(),
                mode: crate::plugin::manifest::FilesystemMode::ReadWrite,
            }],
        }
    }

    /// `DriverRegistry` に 1 件だけドライバ("voice")を載せる。1 つの
    /// サイドカー("engine")と 1 つのファイルアクセスルート("cache")を
    /// 宣言する -- `crate::server` の `drivers/set-sidecar-*` /
    /// `drivers/set-filesystem-*` RPC テストが土台にする。
    ///
    /// `bare_registry`(既存フィクスチャ)と違い、バッキングの `TempDir` を
    /// 呼び出し元に返す -- ファイルアクセスの承認テストは実在するディレクトリ
    /// (`FilesystemConfigStore::validate_path` が `canonicalize`/`is_dir` を
    /// 要求する)を設定先として使う必要があり、その存在をテスト関数の間ずっと
    /// 保つため(`bare_registry` のように内部で作った `TempDir` をそのまま
    /// drop すると、設定先として使うつもりのディレクトリごと消えてしまう)。
    pub(crate) fn test_registry_with_sidecar_and_filesystem(
    ) -> (DriverRegistry, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let registry = DriverRegistry::new(
            Arc::new(DriverHost::new().expect("wasmtime engine builds")),
            Arc::new(SettingsStore::new(tmp.path().join("settings"))),
            Arc::new(GrantsStore::new_for_drivers(tmp.path().join("grants"))),
            Arc::new(SidecarConfigStore::new(tmp.path().join("settings"))),
            Arc::new(FilesystemConfigStore::new(
                tmp.path().join("settings"),
                vec![tmp.path().join("grants")],
            )),
            edlr_driver_channel::Bus::new(),
            tmp.path().join("drivers"),
        );
        let manifest = manifest_with_sidecar_and_filesystem("voice", 51500);
        registry.push(DriverEntry {
            manifest,
            state: DriverState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(capabilities_json_string(&[]))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
        });
        (registry, tmp)
    }

    /// `set_sidecar_config`/`set_sidecar_grant` が永続化だけでなく、稼働中
    /// ドライバが参照する `sidecars_json` 共有バッファも実際に作り直す
    /// ことを確認する。ディスクを読み直すだけのアサーションでは
    /// `refresh_sidecar_runtime` の呼び忘れを検出できない(ブリーフが挙げる
    /// 罠そのもの)ので、`DriverEntry::sidecars_json` を直接パースして見る。
    #[test]
    fn set_sidecar_config_and_grant_update_the_shared_sidecars_buffer() {
        let (registry, _tmp) = test_registry_with_sidecar_and_filesystem();

        registry
            .set_sidecar_config(
                "voice",
                "engine",
                &SidecarConfig {
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    port: 51500,
                    replicas: 1,
                },
            )
            .expect("configuring the executable should succeed");
        registry
            .set_sidecar_grant("voice", "engine", true)
            .expect("granting a configured sidecar should succeed");

        let sidecars_json = registry
            .entries
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.manifest.id == "voice")
            .map(|entry| entry.sidecars_json.clone())
            .expect("voice entry must be present");
        let buffer = sidecars_json.lock().unwrap().clone();
        let parsed = crate::plugin::sidecar_runtime::parse_sidecars(&buffer);
        let entry = parsed
            .get("engine")
            .expect("engine sidecar must be present in the shared buffer");
        assert!(
            entry.granted,
            "the shared sidecars_json buffer must reflect the grant, not just the grants store"
        );
        assert_eq!(entry.command, "/bin/sh");
    }

    /// Regression guard mirroring
    /// `crate::plugin::registry::tests::set_sidecar_grant_rejects_granting_without_a_configured_command`:
    /// granting a driver sidecar whose `command` was never configured must
    /// be rejected as `RegistryError::Sidecar`, both at the top-level
    /// `set_sidecar_grant` call and (implicitly) via `control_sidecar`'s own
    /// separate check.
    #[test]
    fn set_sidecar_grant_rejects_granting_without_a_configured_command() {
        let (registry, _tmp) = test_registry_with_sidecar_and_filesystem();

        match registry.set_sidecar_grant("voice", "engine", true) {
            Err(RegistryError::Sidecar(_)) => {}
            Err(other) => panic!("expected RegistryError::Sidecar, got {other}"),
            Ok(_) => panic!("granting with no command configured must be rejected"),
        }
    }

    /// `crate::server`'s `drivers/sidecar-control` `start` (and, by
    /// extension, `restart`) must refuse to launch a sidecar that has never
    /// been granted, even once a `command` is configured. Mirrors
    /// `crate::plugin::registry::tests::control_sidecar_rejects_start_and_restart_once_the_plugin_is_disabled_but_allows_stop`
    /// (a narrower slice of it: the "not granted" branch specifically).
    #[test]
    fn control_sidecar_rejects_starting_an_ungranted_sidecar() {
        let (registry, _tmp) = test_registry_with_sidecar_and_filesystem();

        registry
            .set_sidecar_config(
                "voice",
                "engine",
                &SidecarConfig {
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    port: 51500,
                    replicas: 1,
                },
            )
            .expect("configuring the executable should succeed");

        match registry.control_sidecar("voice", "engine", SidecarAction::Start) {
            Err(RegistryError::Sidecar(_)) => {}
            Err(other) => panic!("expected RegistryError::Sidecar, got {other}"),
            Ok(_) => panic!("starting an ungranted sidecar must be rejected"),
        }

        let key = DriverRegistry::sidecar_key("voice", "engine");
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            ports: vec![51500],
        };
        assert!(
            !registry.process_driver.status(&key, &spec)[0].running,
            "a rejected Start must not have spawned anything"
        );
    }

    /// `set_filesystem_config`/`set_filesystem_grant` must update the shared
    /// `filesystem_json` buffer, not just persist to the `GrantsStore` --
    /// same trap as the sidecar case above, and the exact one the task
    /// brief calls out.
    #[test]
    fn set_filesystem_config_and_grant_update_the_shared_filesystem_buffer() {
        let (registry, tmp) = test_registry_with_sidecar_and_filesystem();
        let root = tmp.path().join("exports");
        std::fs::create_dir(&root).unwrap();

        registry
            .set_filesystem_config(
                "voice",
                "cache",
                &FilesystemConfig {
                    path: root.to_string_lossy().to_string(),
                },
            )
            .expect("configuring a real directory should succeed");
        registry
            .set_filesystem_grant("voice", "cache", true)
            .expect("granting a configured root should succeed");

        let filesystem_json = registry
            .entries
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.manifest.id == "voice")
            .map(|entry| entry.filesystem_json.clone())
            .expect("voice entry must be present");
        let buffer = filesystem_json.lock().unwrap().clone();
        let parsed = crate::plugin::fs_runtime::parse_filesystem(&buffer);
        let entry = parsed
            .get("cache")
            .expect("cache root must be present in the shared buffer");
        assert!(
            entry.granted,
            "the shared filesystem_json buffer must reflect the grant, not just the grants store"
        );
        assert_eq!(entry.path, root.to_string_lossy());
    }

    /// Regression guard mirroring
    /// `crate::plugin::registry::tests`'s filesystem-grant validation:
    /// granting a filesystem root that has no directory configured must be
    /// rejected as `RegistryError::Filesystem`, and must not be confused
    /// with an unrelated early return (e.g. `UnknownFilesystem`) -- pin the
    /// variant and the fact that the root is declared but unconfigured.
    #[test]
    fn set_filesystem_grant_rejects_granting_without_a_configured_path() {
        let (registry, _tmp) = test_registry_with_sidecar_and_filesystem();

        let err = registry
            .set_filesystem_grant("voice", "cache", true)
            .expect_err("granting with no directory configured must be rejected");
        assert!(
            matches!(err, RegistryError::Filesystem(_)),
            "expected RegistryError::Filesystem, got {err:?}"
        );
    }

    /// Port of
    /// `crate::plugin::registry::tests::concurrent_control_sidecar_start_and_grant_revoke_never_leaves_an_ungranted_instance_running`
    /// onto `DriverRegistry`.
    ///
    /// The review finding this closes: the plugin-side test only exercises
    /// `crate::plugin::registry::Registry`'s own `entries` Vec and its own
    /// `sidecar_runtime_locks` map -- both independently typed/instantiated
    /// fields on `DriverRegistry`, not shared with the plugin registry in any
    /// way (no `Arc` is shared between the two; `DriverRegistry::new`
    /// allocates its own `HashMap::new()`-backed lock maps and its own
    /// `Vec::new()`-backed `entries`). So the plugin-side stress test proves
    /// nothing about whether `DriverRegistry::control_sidecar`'s TOCTOU fix
    /// (taking `sidecar_runtime_lock_for(id)` before reading
    /// grant/config and holding it through `ensure_started`) is actually
    /// wired up correctly on this copy -- a regression here (e.g. someone
    /// "simplifying" `DriverRegistry::control_sidecar` to drop the lock, or
    /// reordering the read-then-spawn so the lock no longer covers both)
    /// would not be caught by the plugin suite at all. This test hammers the
    /// exact same race directly against `DriverRegistry`.
    #[test]
    fn concurrent_control_sidecar_start_and_grant_revoke_never_leaves_an_ungranted_instance_running(
    ) {
        let (registry, _tmp) = test_registry_with_sidecar_and_filesystem();

        registry
            .set_sidecar_config(
                "voice",
                "engine",
                &SidecarConfig {
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    port: 51500,
                    replicas: 1,
                },
            )
            .expect("config should persist");
        registry
            .set_sidecar_grant("voice", "engine", true)
            .expect("initial grant should persist");

        const THREADS: usize = 8;
        const ITERATIONS: usize = 20;
        let barrier = Arc::new(std::sync::Barrier::new(THREADS + 1));

        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let registry = registry.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..ITERATIONS {
                    let _ = registry.control_sidecar("voice", "engine", SidecarAction::Start);
                }
            }));
        }
        let revoker = {
            let registry = registry.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                // Revoke partway through the hammering, then leave the
                // grant revoked (last write wins on the grants file, and
                // this is this test's only writer of the grant).
                std::thread::sleep(Duration::from_millis(5));
                registry
                    .set_sidecar_grant("voice", "engine", false)
                    .expect("revoke should succeed");
            })
        };

        for handle in handles {
            handle.join().expect("worker thread should not panic");
        }
        revoker.join().expect("revoker thread should not panic");

        let manifest = registry
            .manifest_of("voice")
            .expect("voice driver must still be registered");
        let settings_manifest = manifest.as_settings_manifest();
        let disk_granted = registry
            .grants_store
            .sidecar_state(&settings_manifest, "engine")
            .granted;
        let key = DriverRegistry::sidecar_key("voice", "engine");
        let running = registry
            .process_driver
            .status(
                &key,
                &ProcessSpec {
                    command: PathBuf::from("/bin/sh"),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    ports: vec![51500],
                },
            )
            .iter()
            .any(|i| i.running);

        assert!(
            disk_granted || !running,
            "sidecar is running (running={running}) while the on-disk grant is \
             revoked (granted={disk_granted}) -- a Start must have used a grant \
             value read before the concurrent revoke took effect"
        );

        registry.process_driver.stop(&key);
    }
}

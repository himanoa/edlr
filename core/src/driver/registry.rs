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

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use edlr_driver_channel::Bus;
use edlr_driver_process::{ProcessDriver, ProcessSpec};

use crate::driver::host::DriverHost;
use crate::driver::manifest::DriverManifest;
use crate::plugin::filesystem::FilesystemConfigStore;
use crate::plugin::grants::{GrantState, GrantsError, GrantsStore};
use crate::plugin::host::capabilities_json_string;
use crate::plugin::registry::{FilesystemInfo, SidecarInfo};
use crate::plugin::settings::{SettingsError, SettingsStore};
use crate::plugin::sidecar::{assign_ports, SidecarConfig, SidecarConfigStore};
use crate::plugin::sidecar_runtime::{implicit_http_hosts, parse_sidecars, SidecarRuntimeEntry};

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
                let ports = assign_ports(&config);
                let spec = ProcessSpec {
                    command: PathBuf::from(&config.command),
                    args: config.args.clone(),
                    ports,
                };
                let key = Self::sidecar_key(&manifest.id, &request.name);
                let instances = self.process_driver.status(&key, &spec);
                SidecarInfo {
                    request: request.clone(),
                    config,
                    grant,
                    instances,
                }
            })
            .collect()
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
    pub fn set_disabled(&self, manifest: &DriverManifest, reason: String) {
        self.bus.disable_driver(&manifest.id);

        for sidecar in &manifest.sidecars {
            let key = Self::sidecar_key(&manifest.id, &sidecar.name);
            self.process_driver.stop(&key);
        }

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
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::driver::host::DriverHost;

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
}

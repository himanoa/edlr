//! `drivers_dir` を走査し、各ドライバをロードして専用スレッドで駆動する。
//!
//! `crate::plugin::runner` と同じ構造(専用 OS スレッドが `DriverHost::load`
//! → `call_init` → メッセージループを直列に実行する)だが、以下が異なる:
//! - **イベント購読タスクを作らない**。プラグインは router の broadcast
//!   イベントを tokio タスクでフィルタして転送するが、ドライバはそもそも
//!   journal イベントを購読しない。メッセージの受信は
//!   `Bus::register_driver` に渡した `SyncSender<Message>` の受け口
//!   (`Receiver<Message>`)を、ドライバ専用スレッドがそのまま
//!   `for message in messages_rx` で回すだけで完結する。
//! - ドライバは複数プラグインの結節点になりうるため、キュー容量は
//!   `DRIVER_MESSAGE_QUEUE_CAPACITY`(64)としてある。プラグイン側の
//!   `PLUGIN_WORK_QUEUE_CAPACITY` も同じ 64 だが(journal イベントとバス
//!   配信の 2 プロデューサが枠を奪い合うようになったため引き上げた -- 同
//!   定数のドキュメント参照)、そちらは 1 プラグインあたりの容量であるのに
//!   対しこちらは複数プラグインが同時に投げ込みうる結節点の容量なので、
//!   両者が同じ数字であることに深い意味は無い(たまたま揃っただけ)。

use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use edlr_driver_channel::{Bus, Message};

use crate::driver::host::{DriverCtx, DriverHost};
use crate::driver::manifest::{load_driver_manifest, DriverManifest};
use crate::driver::registry::{DriverEntry, DriverRegistry, DriverState};
use crate::plugin::filesystem::FilesystemConfigStore;
use crate::plugin::fs_runtime::{filesystem_json_string, FsRuntimeEntry};
use crate::plugin::grants::GrantsStore;
use crate::plugin::host::capabilities_json_string;
use crate::plugin::settings::SettingsStore;
use crate::plugin::sidecar::{assign_ports, SidecarConfig, SidecarConfigStore};
use crate::plugin::sidecar_runtime::{
    implicit_http_hosts, sidecars_json_string, SidecarRuntimeEntry,
};

/// ドライバ 1 件あたりのメッセージキュー容量。
///
/// ドライバは複数プラグインの結節点で溢れやすく、1 メッセージの処理が
/// `DriverInstance::CALL_DEADLINE`(30 秒)まで伸びうるため、余裕を持って
/// 大きめに取ってある(プラグイン側の 1 プラグインあたりの容量である
/// `PLUGIN_WORK_QUEUE_CAPACITY` とは性質が異なる数字なので、値が近い/同じ
/// であること自体に意味は無い)。満杯時は `publish` が `queue-full` を
/// 返す(捨てない)ので、呼び出し側が状況を知れる。
const DRIVER_MESSAGE_QUEUE_CAPACITY: usize = 64;

/// `drivers_dir` を走査し、各ドライバをロードして専用スレッドで駆動する。
///
/// 戻り値の `DriverRegistry` は起動直後から `list` 可能(= 各ドライバの
/// `load`/`init` 結果が確定した後に返る)。
///
/// **呼び出し順序が重要**: この関数は `crate::plugin::runner::start_plugins`
/// より先に呼ぶこと。ドライバの登録(`Bus::register_driver`)が完了する前に
/// プラグインが起動すると、そのプラグインの `init` 中の最初の `bus.get` 呼び
/// 出しが `unknown-driver` を見てしまう(設計書「起動順序」参照)。
pub fn start_drivers(
    drivers_dir: &Path,
    settings_store: SettingsStore,
    sidecar_config_store: SidecarConfigStore,
    filesystem_config_store: FilesystemConfigStore,
    grants_store: GrantsStore,
    bus: Bus,
    host: DriverHost,
) -> DriverRegistry {
    let host = Arc::new(host);
    let settings_store = Arc::new(settings_store);
    let grants_store = Arc::new(grants_store);
    let sidecar_config_store = Arc::new(sidecar_config_store);
    let filesystem_config_store = Arc::new(filesystem_config_store);
    let registry = DriverRegistry::new(
        host.clone(),
        settings_store.clone(),
        grants_store.clone(),
        sidecar_config_store.clone(),
        filesystem_config_store.clone(),
        bus.clone(),
        drivers_dir.to_path_buf(),
    );

    let dir_entries = match std::fs::read_dir(drivers_dir) {
        Ok(dir_entries) => dir_entries,
        Err(e) => {
            tracing::info!(
                drivers_dir = %drivers_dir.display(),
                "drivers directory not found or unreadable ({e}); starting with no drivers"
            );
            return registry;
        }
    };

    for dir_entry in dir_entries {
        let Ok(dir_entry) = dir_entry else { continue };
        let path = dir_entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest = match load_driver_manifest(&path) {
            Ok(manifest) => manifest,
            Err(e) => {
                tracing::warn!(
                    driver_dir = %path.display(),
                    "skipping invalid driver: {e}"
                );
                continue;
            }
        };

        load_and_run_driver(
            &manifest,
            &path,
            &settings_store,
            &grants_store,
            &sidecar_config_store,
            &filesystem_config_store,
            &bus,
            &host,
            &registry,
        );
    }

    registry
}

/// 1 ドライバをロードし、成功すれば専用スレッドを起動し、結果
/// (Running/Disabled)を `registry` に登録する。
#[allow(clippy::too_many_arguments)]
fn load_and_run_driver(
    manifest: &DriverManifest,
    dir: &Path,
    settings_store: &SettingsStore,
    grants_store: &GrantsStore,
    sidecar_config_store: &SidecarConfigStore,
    filesystem_config_store: &FilesystemConfigStore,
    bus: &Bus,
    host: &Arc<DriverHost>,
    registry: &DriverRegistry,
) {
    let entry_path = dir.join(&manifest.entry);
    let settings_manifest = manifest.as_settings_manifest();

    let effective = settings_store.effective(&settings_manifest);
    let settings_json_string = serde_json::to_string(&serde_json::Value::Object(effective))
        .unwrap_or_else(|_| "{}".to_string());
    let settings_json = Arc::new(Mutex::new(settings_json_string));

    let grant_state = grants_store.state(&settings_manifest);

    // `crate::plugin::runner::load_and_run_plugin` と同じ組み立て方(見た目の
    // 重複はあるが、あちらは `Registry` 経由の更新用、こちらは起動直後の
    // 初期値用で、依存するライフサイクルの起点が異なるため 1 箇所に共通化は
    // していない)。
    let sidecar_configs = sidecar_config_store.effective(&settings_manifest);
    let sidecar_entries: Vec<SidecarRuntimeEntry> = manifest
        .sidecars
        .iter()
        .map(|request| {
            let config = sidecar_configs
                .get(&request.name)
                .cloned()
                .unwrap_or_else(|| SidecarConfig::from_request(request));
            let granted = grants_store
                .sidecar_state(&settings_manifest, &request.name)
                .granted;
            SidecarRuntimeEntry {
                name: request.name.clone(),
                granted,
                command: config.command.clone(),
                args: config.args.clone(),
                ports: assign_ports(&config),
            }
        })
        .collect();
    let sidecars_json = Arc::new(Mutex::new(sidecars_json_string(&sidecar_entries)));

    let mut initial_hosts = if grant_state.granted {
        settings_manifest.capability_hosts()
    } else {
        Vec::new()
    };
    initial_hosts.extend(implicit_http_hosts(&sidecar_entries));
    let capabilities_json = Arc::new(Mutex::new(capabilities_json_string(&initial_hosts)));

    let filesystem_configs = filesystem_config_store.effective(&settings_manifest);
    let filesystem_entries: Vec<FsRuntimeEntry> = manifest
        .filesystem
        .iter()
        .map(|request| {
            let path = filesystem_configs
                .get(&request.name)
                .map(|config| config.path.clone())
                .unwrap_or_default();
            let granted = grants_store
                .filesystem_state(&settings_manifest, &request.name)
                .granted;
            FsRuntimeEntry {
                name: request.name.clone(),
                granted,
                mode: request.mode.as_str().to_string(),
                path,
            }
        })
        .collect();
    let filesystem_json = Arc::new(Mutex::new(filesystem_json_string(&filesystem_entries)));

    let (messages_tx, messages_rx) =
        std_mpsc::sync_channel::<Message>(DRIVER_MESSAGE_QUEUE_CAPACITY);
    let (ready_tx, ready_rx) = std_mpsc::channel::<DriverState>();

    // Bus への登録はスレッド起動前、`load`/`init` の成否が分かるより先に行う
    // (`start_drivers` のドキュメント参照: プラグイン起動より前に全ドライバの
    // 登録を終える必要がある)。
    bus.register_driver(&manifest.id, manifest.topics.clone(), messages_tx);

    thread::spawn({
        let host = host.clone();
        let manifest = manifest.clone();
        let settings_json = settings_json.clone();
        let capabilities_json = capabilities_json.clone();
        let sidecars_json = sidecars_json.clone();
        let filesystem_json = filesystem_json.clone();
        let bus = bus.clone();
        let registry = registry.clone();
        move || {
            run_driver_thread(
                host,
                manifest,
                entry_path,
                settings_json,
                capabilities_json,
                sidecars_json,
                filesystem_json,
                bus,
                registry,
                messages_rx,
                ready_tx,
            );
        }
    });

    let state = ready_rx.recv().unwrap_or_else(|_| DriverState::Disabled {
        reason: "driver thread exited before reporting an init result".to_string(),
    });

    if matches!(state, DriverState::Disabled { .. }) {
        // load/init に失敗したドライバのスレッドはもう `messages_rx` を読ま
        // ない。登録済みの bus スロットを `available: true` のまま放置すると、
        // `get` はいつまでも古い/存在しない値を返し続け、プラグイン側が
        // 「まだ更新が来ていないだけ」なのか「もう誰も更新しない」のかを
        // 区別できない(`DriverRegistry::set_disabled` のドキュメント参照)。
        bus.disable_driver(&manifest.id);
    }

    registry.push(DriverEntry {
        manifest: manifest.clone(),
        state,
        settings_json,
        capabilities_json,
        sidecars_json,
        filesystem_json,
    });
}

/// ドライバ専用スレッドの本体。`load` → `call_init` → メッセージループを
/// 直列に実行する。すべての wasm 呼び出しはこのスレッド上でのみ発生する。
#[allow(clippy::too_many_arguments)]
fn run_driver_thread(
    host: Arc<DriverHost>,
    manifest: DriverManifest,
    entry_path: PathBuf,
    settings_json: Arc<Mutex<String>>,
    capabilities_json: Arc<Mutex<String>>,
    sidecars_json: Arc<Mutex<String>>,
    filesystem_json: Arc<Mutex<String>>,
    bus: Bus,
    registry: DriverRegistry,
    messages_rx: std_mpsc::Receiver<Message>,
    ready_tx: std_mpsc::Sender<DriverState>,
) {
    let ctx = DriverCtx::new(
        manifest.id.clone(),
        settings_json,
        capabilities_json,
        sidecars_json,
        filesystem_json,
        bus,
        host.http_driver(),
        host.process_driver(),
        host.fs_driver(),
    );
    let mut instance = match host.load(&entry_path, ctx) {
        Ok(instance) => instance,
        Err(e) => {
            let _ = ready_tx.send(DriverState::Disabled {
                reason: format!("failed to load driver component: {e}"),
            });
            return;
        }
    };

    if let Err(e) = instance.call_init() {
        let _ = ready_tx.send(DriverState::Disabled {
            reason: format!("init() failed: {e}"),
        });
        return;
    }

    if ready_tx.send(DriverState::Running).is_err() {
        // start_drivers 側が既に受信を諦めている(通常起こらない)。
        return;
    }

    for message in messages_rx {
        if let Err(e) = instance.call_on_message(&message.from, &message.topic, &message.payload) {
            tracing::warn!(
                driver_id = %manifest.id,
                "on-message call failed, disabling driver: {e}"
            );
            registry.set_disabled(&manifest, format!("on-message call failed: {e}"));
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_drivers_dir_yields_an_empty_registry() {
        let registry = start_drivers_for_test(std::path::Path::new("/nonexistent/edlr-drivers"));
        assert!(registry.list().is_empty());
    }

    #[test]
    fn an_invalid_driver_dir_is_skipped_without_failing_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("broken")).unwrap();
        std::fs::write(dir.path().join("broken/driver.toml"), "not toml {{{").unwrap();

        // A second, *manifest-valid* driver dir alongside the broken one.
        // Its `entry` file is not real wasm, so manifest validation passes
        // (the file merely needs to exist) but `DriverHost::load` will fail,
        // landing it `Disabled`. That's still enough to prove the scan
        // continued past the broken directory -- without this fixture, the
        // test only proved "the broken driver is skipped", never "the rest
        // still load" (it would have passed even if one bad `driver.toml`
        // aborted the whole scan, since the fixture had nothing else to
        // load).
        let valid_dir = dir.path().join("ed-state");
        std::fs::create_dir(&valid_dir).unwrap();
        std::fs::write(valid_dir.join("driver.wasm"), b"not real wasm").unwrap();
        std::fs::write(
            valid_dir.join("driver.toml"),
            "id = \"ed-state\"\nname = \"ED State\"\nversion = \"0.1.0\"\nentry = \"driver.wasm\"\n",
        )
        .unwrap();

        let registry = start_drivers_for_test(dir.path());
        let infos = registry.list();
        assert_eq!(
            infos.len(),
            1,
            "the broken driver dir must be skipped, but the valid one must still load"
        );
        assert_eq!(infos[0].manifest.id, "ed-state");
        assert!(
            matches!(
                &infos[0].state,
                DriverState::Disabled { reason } if reason.contains("failed to load driver component")
            ),
            "the entry file isn't real wasm, so the driver must land Disabled \
             with a load-failure reason, not Running; got {:?}",
            infos[0].state
        );
    }

    fn start_drivers_for_test(dir: &std::path::Path) -> DriverRegistry {
        let tmp = tempfile::tempdir().unwrap();
        start_drivers(
            dir,
            SettingsStore::new(tmp.path().join("settings")),
            SidecarConfigStore::new(tmp.path().join("settings")),
            FilesystemConfigStore::new(tmp.path().join("settings"), vec![tmp.path().to_path_buf()]),
            GrantsStore::new_for_drivers(tmp.path().join("grants")),
            edlr_driver_channel::Bus::new(),
            DriverHost::new().expect("wasmtime engine builds"),
        )
    }
}

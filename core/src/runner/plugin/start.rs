//! `plugins_dir` の走査と 1 プラグインのロード・起動(専用スレッド/購読
//! タスクの立ち上げと `Registry` への登録)。ループ本体は `event_loop`、
//! 購読タスクは `subscriber` を参照。

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::thread;

use edlr_driver_channel::{Bus, Delivery};

use crate::capability::grants::GrantsStore;
use crate::host::plugin::PluginHost;
use crate::manifest::{load_manifest, Manifest};
use crate::registry::driver::DriverRegistry;
use crate::registry::plugin::{PluginEntry, PluginState, Registry};
use crate::router::Router;
use crate::runner::bootstrap::{build_initial_buffers, InitialBuffers};
use crate::runner::plugin::queue::channel;
use crate::runtime::bus::{bus_json_string, BusRuntimeEntry};
use crate::runtime::dropped::DropCounters;
use crate::schedule::store::ScheduleStore;
use crate::settings::filesystem::FilesystemConfigStore;
use crate::settings::sidecar::SidecarConfigStore;
use crate::settings::store::SettingsStore;

use super::event_loop::run_plugin_thread;
use super::subscriber::{
    spawn_bus_subscriber, spawn_event_subscriber, subscribe_with_initial_value,
};
use super::PLUGIN_WORK_QUEUE_CAPACITY;

/// `plugins_dir` を走査し、各プラグインをロードして専用タスクで駆動する。
///
/// 戻り値の `Registry` は起動直後から `snapshot` 可能(= 各プラグインの
/// `load`/`init` 結果が確定した後に返る)。
///
/// **起動時にサイドカーを自動 spawn することはない**: ここで組み立てる
/// `sidecars_json`/`capabilities_json` はあくまで承認・設定状態のスナップ
/// ショットで、実プロセスの起動はプラグイン自身の `ensure-started` 呼び出し
/// か、ユーザー操作(`Registry::control_sidecar`)を経て初めて行われる。
///
/// **`bus` は呼び出し元が構築した 1 つのインスタンスを渡す**: ここで組み立て
/// る各プラグインの `HostCtx` はこの同じ `bus` を(`Clone` して)共有する
/// (`http_driver`/`process_driver`/`fs_driver` と同様、`HostCtx::bus` の
/// ドキュメントコメント参照)。呼び出し元は `crate::runner::driver::start_drivers` を
/// この関数より先に呼び、そこで返した `DriverRegistry` の構築に使ったのと
/// 同じ `bus` をここへ渡すこと -- そうしないと、ドライバの登録
/// (`Bus::register_driver`)が完了する前にプラグインが起動し、`init` 中の
/// 最初の `bus.get` 呼び出しが `unknown-driver` を見てしまう(設計書
/// 「起動順序」参照)。
#[allow(clippy::too_many_arguments)]
pub fn start_plugins(
    plugins_dir: &Path,
    settings_store: SettingsStore,
    sidecar_config_store: SidecarConfigStore,
    filesystem_config_store: FilesystemConfigStore,
    grants_store: GrantsStore,
    schedule_store: ScheduleStore,
    router: &Router,
    bus: Bus,
    drivers: DriverRegistry,
    host: PluginHost,
) -> Registry {
    let host = Arc::new(host);
    let settings_store = Arc::new(settings_store);
    let schedule_store = Arc::new(schedule_store);
    let grants_store = Arc::new(grants_store);
    let sidecar_config_store = Arc::new(sidecar_config_store);
    let filesystem_config_store = Arc::new(filesystem_config_store);
    let process_driver = host.process_driver();
    let registry = Registry::new(
        host.clone(),
        settings_store.clone(),
        grants_store.clone(),
        sidecar_config_store.clone(),
        filesystem_config_store.clone(),
        process_driver,
        drivers.clone(),
        bus.clone(),
        plugins_dir.to_path_buf(),
    );

    let dir_entries = match std::fs::read_dir(plugins_dir) {
        Ok(dir_entries) => dir_entries,
        Err(e) => {
            tracing::info!(
                plugins_dir = %plugins_dir.display(),
                "plugins directory not found or unreadable ({e}); starting with no plugins"
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

        let manifest = match load_manifest(&path) {
            Ok(manifest) => manifest,
            Err(e) => {
                tracing::warn!(
                    plugin_dir = %path.display(),
                    "skipping invalid plugin: {e}"
                );
                continue;
            }
        };

        warn_unresolved_bus(&manifest, &drivers);

        load_and_run_plugin(
            &manifest,
            &path,
            &settings_store,
            &grants_store,
            &sidecar_config_store,
            &filesystem_config_store,
            &schedule_store,
            router,
            &bus,
            &host,
            &registry,
        );
    }

    registry
}

/// 1 プラグインをロードし、成功すれば専用スレッド・購読タスクを起動し、
/// 結果(Running/Disabled)を `registry` に登録する。
#[allow(clippy::too_many_arguments)]
fn load_and_run_plugin(
    manifest: &Manifest,
    dir: &Path,
    settings_store: &SettingsStore,
    grants_store: &GrantsStore,
    sidecar_config_store: &SidecarConfigStore,
    filesystem_config_store: &FilesystemConfigStore,
    schedule_store: &Arc<ScheduleStore>,
    router: &Router,
    bus: &Bus,
    host: &Arc<PluginHost>,
    registry: &Registry,
) {
    let entry_path = dir.join(&manifest.entry);

    // layout.kdl / layout.json は不備があってもロードを一切妨げない
    // (`crate::layout` のモジュールドキュメント参照)。パース/解決の警告は
    // ここで warn ログへ落とし、解決済みの layout(または None)だけを
    // entry へ格納する。
    let (layout, layout_warnings) = crate::layout::load::load_layout(dir);
    let (layout, layout_warnings) = match layout {
        Some(parsed) => {
            let (resolved, mut resolve_warnings) =
                crate::layout::resolve::resolve(parsed, &manifest.settings);
            let mut all = layout_warnings;
            all.append(&mut resolve_warnings);
            (Some(resolved), all)
        }
        None => (None, layout_warnings),
    };
    for warning in &layout_warnings {
        tracing::warn!(plugin = %manifest.id, "{warning}");
    }

    // settings/sidecars/capabilities/filesystem は plugin/driver 共通の
    // 組み立て方(`build_initial_buffers` のドキュメント参照)。この見た目の
    // 重複は `Registry::refresh_sidecar_runtime` 等(承認・設定変更のたびに
    // 作り直す更新用)とも意図的に共通化していない -- 依存するライフサイクル
    // の起点が異なるため。
    let InitialBuffers {
        settings_json,
        capabilities_json,
        sidecars_json,
        filesystem_json,
    } = build_initial_buffers(
        manifest,
        settings_store,
        grants_store,
        sidecar_config_store,
        filesystem_config_store,
    );

    // バス接続先ごとの承認(`GrantsStore::bus_state`)を解決して
    // `BusRuntimeEntry` にまとめる。サイドカー/ファイルアクセスの初期値組み
    // 立てと同じ流儀(未承認の接続先は `bus_json_string` が `publish`/
    // `subscribe` を落とす -- `crate::runtime::bus` のドキュメント参照)。トピック
    // 一覧自体は manifest の要求をそのまま載せる(`GrantsStore` はトピック
    // の中身を持たない)。
    let bus_entries: Vec<BusRuntimeEntry> = manifest
        .bus
        .iter()
        .map(|request| {
            let granted = grants_store.bus_state(manifest, &request.driver).granted;
            BusRuntimeEntry {
                driver: request.driver.clone(),
                granted,
                publish: request.publish.clone(),
                subscribe: request.subscribe.clone(),
            }
        })
        .collect();
    let bus_json = Arc::new(Mutex::new(bus_json_string(&bus_entries)));

    let (work_tx, work_rx) = channel();
    let (ready_tx, ready_rx) = std_mpsc::channel::<PluginState>();

    // `Stop` のアウトオブバンド経路(`Registry::shutdown_plugins` が立てる)。
    // 詳細は `run_plugin_thread` のループ先頭のコメント参照。
    let stop_flag = Arc::new(AtomicBool::new(false));

    let thread_handle = thread::spawn({
        let host = host.clone();
        let manifest = manifest.clone();
        let settings_json = settings_json.clone();
        let capabilities_json = capabilities_json.clone();
        let sidecars_json = sidecars_json.clone();
        let filesystem_json = filesystem_json.clone();
        let bus_json = bus_json.clone();
        let bus = bus.clone();
        let registry = registry.clone();
        let stop_flag = stop_flag.clone();
        let schedule_store = schedule_store.clone();
        // `submit-send` の完了通知(`PluginWork::JobComplete`)を自分の
        // キューへ push するため、スレッド自身も送信側を持つ
        // (`HostCtx::submit_send` が spawn するタスクへ配る)。
        let work_tx = work_tx.clone();
        move || {
            run_plugin_thread(
                host,
                manifest,
                entry_path,
                settings_json,
                capabilities_json,
                sidecars_json,
                filesystem_json,
                bus_json,
                bus,
                registry,
                work_rx,
                work_tx,
                ready_tx,
                stop_flag,
                schedule_store,
            );
        }
    });

    let state = ready_rx.recv().unwrap_or_else(|_| PluginState::Disabled {
        reason: "plugin thread exited before reporting an init result".to_string(),
    });
    let running = matches!(state, PluginState::Running);

    registry.push(PluginEntry {
        manifest: manifest.clone(),
        state,
        settings_json,
        capabilities_json,
        sidecars_json,
        filesystem_json,
        bus_json: bus_json.clone(),
        layout,
    });

    if running {
        // `shutdown_plugins`(デーモンの正常終了)がこのプラグインへ
        // `PluginWork::Stop` を送り、スレッドの終了を待てるように、送信側の
        // 複製とスレッドの `JoinHandle` を registry に登録する。Disabled
        // (=このブロックに入らない)プラグインのスレッドは、この時点で既に
        // `init` 失敗を `ready_tx` へ送って return 済みなので、登録せずに
        // `thread_handle` を(join せずに)そのまま drop してよい -- スレッド
        // 自体はもう終了しているか、終了する寸前でしかない。
        registry.register_plugin_thread(&manifest.id, work_tx.clone(), thread_handle, stop_flag);

        // 作業キュー満杯で捨てた件数を数える窓口。両方の購読タスク(書き手)と
        // `plugins/list`(読み手)が共有する。
        let drops = DropCounters::new();
        registry.register_drop_counters(&manifest.id, drops.clone());

        spawn_event_subscriber(
            manifest.clone(),
            router.subscribe(),
            work_tx.clone(),
            drops.clone(),
        );

        // `[[bus]]` の `subscribe` トピックがあれば、購読を登録して配信を
        // このプラグインの作業キューへ流し込む(`spawn_bus_subscriber` の
        // ドキュメントコメント参照)。登録は承認の有無に関わらず行う --
        // 承認は配信のたびに再確認するため(稼働中の取り消しを即座に効かせる
        // ため)、後から承認されても購読し直す必要がない。
        let subscribe_topics: Vec<(String, String)> = manifest
            .bus
            .iter()
            .flat_map(|request| {
                request
                    .subscribe
                    .iter()
                    .map(move |topic| (request.driver.clone(), topic.clone()))
            })
            .collect();
        if !subscribe_topics.is_empty() {
            let (delivery_tx, delivery_rx) =
                std_mpsc::sync_channel::<Delivery>(PLUGIN_WORK_QUEUE_CAPACITY);
            for (driver_id, topic) in &subscribe_topics {
                subscribe_with_initial_value(
                    bus,
                    &manifest.id,
                    driver_id,
                    topic,
                    delivery_tx.clone(),
                );
            }
            drop(delivery_tx);
            spawn_bus_subscriber(
                manifest.clone(),
                bus_json,
                delivery_rx,
                work_tx,
                registry.bus_subscriber_shutdown_flag(),
                drops,
            );
        }
    }
    // Disabled の場合、work_tx はここで drop される。プラグインスレッドは
    // 既に init 失敗で return 済みで work_rx を読まないので問題ない。
}

/// `[[bus]]` の参照先が実在しないものを warn ログに出す。
///
/// **起動は止めない**(ドライバは後から入れられるべき)。ただし黙って
/// 動くと事故になるので、プラグイン ID・ドライバ ID・トピック名を全て
/// 含めて必ず 1 件ずつ出す。UI 側は `BusInfo::resolved` を見て「未解決」
/// バッジを出す。
pub(crate) fn warn_unresolved_bus(manifest: &Manifest, drivers: &DriverRegistry) {
    for request in &manifest.bus {
        let Some(driver) = drivers.manifest_of(&request.driver) else {
            tracing::warn!(
                plugin_id = %manifest.id,
                driver_id = %request.driver,
                "plugin declares a bus connection to a driver that is not installed"
            );
            continue;
        };
        for topic in request.publish.iter().chain(request.subscribe.iter()) {
            if driver.topic(topic).is_none() {
                tracing::warn!(
                    plugin_id = %manifest.id,
                    driver_id = %request.driver,
                    topic = %topic,
                    "plugin declares a bus topic the driver does not provide"
                );
            }
        }
    }
}

//! `plugins_dir` を走査し、各プラグインをロードして専用タスク/スレッドで駆動する。
//!
//! 各プラグインは以下の構成で動く:
//! - 専用の OS スレッドが `PluginHost::load` → `call_init` → イベントループ
//!   (`call_on_event`)を直列に実行する。`PluginInstance`(wasmtime の
//!   `Store`)はこのスレッドの外に出ることがなく、`Send` かどうかを気にする
//!   必要がない。wasm 呼び出しは同期・ブロッキングだが、これは tokio の
//!   ワーカースレッドとは独立した OS スレッドなので、非同期ランタイムを
//!   ブロックしない。
//! - 専用の tokio タスクが `router.subscribe()` した `broadcast::Receiver` から
//!   イベントを受け取り、`matches_event` でフィルタしたうえで `std::sync::mpsc`
//!   経由でプラグインスレッドへ転送する(こちらは非同期処理・待ち合わせ側)。
//!
//! プラグインスレッドが `call_on_event` の `Err`(trap を含む)を受け取ると、
//! レジストリを `Disabled` にしてループを抜け、スレッドを終了する。それに伴い
//! `std::sync::mpsc` の送信側(購読タスク)への送信も失敗するようになるため、
//! 購読タスクも次のイベントで終了する。他プラグインや監視コアには一切波及しない。
//!
//! journal イベントに加えて、バス経由でドライバから届く配信(`Delivery`)も
//! 同じプラグインスレッドで処理する。`PluginInstance` は 1 スレッドの外に出ない
//! という性質を保つため、2 本目のスレッドや 2 つ目の wasm 呼び出し口を増やす
//! のではなく、両方を 1 本の `PluginWork` キューに混ぜて直列化する
//! (`PluginWork` のドキュメントコメント参照)。バス側の配信は `Bus::subscribe`
//! が要求する `SyncSender<Delivery>` を別途受け取り、それを `PluginWork` へ
//! 詰め替えて転送する専用の tokio タスク(`spawn_bus_subscriber`)が
//! `spawn_event_subscriber` と対称の形で存在する。

use std::path::{Path, PathBuf};
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::thread;

/// Capacity of the per-plugin journal-event queue (`events_tx`/`events_rx`
/// below). `driver-http.send` can block a plugin's dedicated thread for up
/// to `host::HTTP_TIMEOUT` per call (a host the plugin author controls can
/// simply not respond), during which the plugin's own thread is not
/// draining `events_rx` at all. The channel used to be unbounded
/// (`std_mpsc::channel`), so a stalled plugin let the router/monitor's
/// broadcast events accumulate in host memory without limit -- outside
/// anything `StoreLimits` can see, since it lives on the host side of the
/// boundary, not in the plugin's wasm linear memory. Bounding it here caps
/// that growth at a fixed, small number of pending events per plugin.
///
/// Overflow policy: when full, `spawn_event_subscriber` drops the new event
/// and logs a `tracing::warn!` rather than blocking the subscriber task (see
/// its doc comment) or disabling the plugin outright -- a plugin that's
/// merely slow (e.g. waiting out its own `driver-http.send` call) should be
/// allowed to catch up and keep running, not be killed for falling behind.
const PLUGIN_EVENT_QUEUE_CAPACITY: usize = 32;

use tokio::sync::broadcast;

use edlr_driver_channel::{Bus, Delivery};

use crate::driver::registry::DriverRegistry;
use crate::event::Event;
use crate::plugin::bus_runtime::{bus_json_string, parse_bus, BusRuntimeEntry};
use crate::plugin::filesystem::FilesystemConfigStore;
use crate::plugin::fs_runtime::{filesystem_json_string, FsRuntimeEntry};
use crate::plugin::grants::GrantsStore;
use crate::plugin::host::{capabilities_json_string, HostCtx, PluginHost};
use crate::plugin::manifest::{load_manifest, matches_event};
use crate::plugin::registry::{PluginEntry, PluginState, Registry};
use crate::plugin::settings::SettingsStore;
use crate::plugin::sidecar::{assign_ports, SidecarConfig, SidecarConfigStore};
use crate::plugin::sidecar_runtime::{
    implicit_http_hosts, sidecars_json_string, SidecarRuntimeEntry,
};
use crate::plugin::Manifest;
use crate::router::Router;

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
/// ドキュメントコメント参照)。呼び出し元は `crate::driver::start_drivers` を
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
    router: &Router,
    bus: Bus,
    drivers: DriverRegistry,
    host: PluginHost,
) -> Registry {
    let host = Arc::new(host);
    let settings_store = Arc::new(settings_store);
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
    router: &Router,
    bus: &Bus,
    host: &Arc<PluginHost>,
    registry: &Registry,
) {
    let entry_path = dir.join(&manifest.entry);
    let effective = settings_store.effective(manifest);
    let settings_json_string = serde_json::to_string(&serde_json::Value::Object(effective))
        .unwrap_or_else(|_| "{}".to_string());
    let settings_json = Arc::new(Mutex::new(settings_json_string));

    let grant_state = grants_store.state(manifest);

    // サイドカー 1 件ずつの設定(`SidecarConfigStore`)・承認
    // (`GrantsStore::sidecar_state`)を解決して `SidecarRuntimeEntry` に
    // まとめる。これは `Registry::refresh_sidecar_runtime` が承認・設定変更
    // のたびに作り直すのと同じ組み立て方(見た目の重複はあるが、あちらは
    // `Registry` 経由の更新用、こちらは起動直後の初期値用で、依存する
    // ライフサイクルの起点が異なるため 1 箇所に共通化はしていない)。
    let sidecar_configs = sidecar_config_store.effective(manifest);
    let sidecar_entries: Vec<SidecarRuntimeEntry> = manifest
        .sidecars
        .iter()
        .map(|request| {
            let config = sidecar_configs
                .get(&request.name)
                .cloned()
                .unwrap_or_else(|| SidecarConfig::from_request(request));
            let granted = grants_store.sidecar_state(manifest, &request.name).granted;
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

    // http capability の承認済み hosts と、承認済みサイドカーの暗黙許可を
    // 合流させる(`capabilities_json` に載るのは実効的に許可されたホストの
    // みで、サイドカーの暗黙許可は http capability の承認とは独立に効く)。
    let mut initial_hosts = if grant_state.granted {
        manifest.capability_hosts()
    } else {
        Vec::new()
    };
    initial_hosts.extend(implicit_http_hosts(&sidecar_entries));
    let initial_capabilities_json = capabilities_json_string(&initial_hosts);
    let capabilities_json = Arc::new(Mutex::new(initial_capabilities_json));

    // ファイルアクセスのルートごとの設定(`FilesystemConfigStore`)・承認
    // (`GrantsStore::filesystem_state`)を解決して `FsRuntimeEntry` にまと
    // める。サイドカーの初期値組み立てと同じ流儀(未承認のルートは
    // `filesystem_json_string` が `path` を落とす -- `fs_runtime` のドキュ
    // メント参照)。
    let filesystem_configs = filesystem_config_store.effective(manifest);
    let filesystem_entries: Vec<FsRuntimeEntry> = manifest
        .filesystem
        .iter()
        .map(|request| {
            let path = filesystem_configs
                .get(&request.name)
                .map(|config| config.path.clone())
                .unwrap_or_default();
            let granted = grants_store
                .filesystem_state(manifest, &request.name)
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

    // バス接続先ごとの承認(`GrantsStore::bus_state`)を解決して
    // `BusRuntimeEntry` にまとめる。サイドカー/ファイルアクセスの初期値組み
    // 立てと同じ流儀(未承認の接続先は `bus_json_string` が `publish`/
    // `subscribe` を落とす -- `bus_runtime` のドキュメント参照)。トピック
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

    // journal イベントとバス配信を混ぜる 1 本の作業キュー(`PluginWork` の
    // ドキュメントコメント参照)。
    let (work_tx, work_rx) = std_mpsc::sync_channel::<PluginWork>(PLUGIN_EVENT_QUEUE_CAPACITY);
    let (ready_tx, ready_rx) = std_mpsc::channel::<PluginState>();

    thread::spawn({
        let host = host.clone();
        let manifest = manifest.clone();
        let settings_json = settings_json.clone();
        let capabilities_json = capabilities_json.clone();
        let sidecars_json = sidecars_json.clone();
        let filesystem_json = filesystem_json.clone();
        let bus_json = bus_json.clone();
        let bus = bus.clone();
        let registry = registry.clone();
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
                ready_tx,
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
    });

    if running {
        spawn_event_subscriber(manifest.clone(), router.subscribe(), work_tx.clone());

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
                std_mpsc::sync_channel::<Delivery>(PLUGIN_EVENT_QUEUE_CAPACITY);
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
            spawn_bus_subscriber(manifest.clone(), bus_json, delivery_rx, work_tx);
        }
    }
    // Disabled の場合、work_tx はここで drop される。プラグインスレッドは
    // 既に init 失敗で return 済みで work_rx を読まないので問題ない。
}

/// プラグイン専用スレッドの本体。`load` → `call_init` → イベントループを
/// 直列に実行する。すべての wasm 呼び出しはこのスレッド上でのみ発生する。
#[allow(clippy::too_many_arguments)]
fn run_plugin_thread(
    host: Arc<PluginHost>,
    manifest: Manifest,
    entry_path: PathBuf,
    settings_json: Arc<Mutex<String>>,
    capabilities_json: Arc<Mutex<String>>,
    sidecars_json: Arc<Mutex<String>>,
    filesystem_json: Arc<Mutex<String>>,
    bus_json: Arc<Mutex<String>>,
    bus: Bus,
    registry: Registry,
    work_rx: std_mpsc::Receiver<PluginWork>,
    ready_tx: std_mpsc::Sender<PluginState>,
) {
    let ctx = HostCtx::new(
        manifest.id.clone(),
        settings_json,
        capabilities_json,
        sidecars_json,
        filesystem_json,
        bus_json,
        bus,
        host.http_driver(),
        host.process_driver(),
        host.fs_driver(),
    );
    let mut instance = match host.load(&entry_path, ctx) {
        Ok(instance) => instance,
        Err(e) => {
            let _ = ready_tx.send(PluginState::Disabled {
                reason: format!("failed to load plugin component: {e}"),
            });
            return;
        }
    };

    if let Err(e) = instance.call_init() {
        let _ = ready_tx.send(PluginState::Disabled {
            reason: format!("init() failed: {e}"),
        });
        return;
    }

    if ready_tx.send(PluginState::Running).is_err() {
        // start_plugins 側が既に受信を諦めている(通常起こらない)。
        return;
    }

    for work in work_rx {
        let result = match &work {
            PluginWork::Event(event) => {
                let (kind, timestamp, name, payload_json, replay) = event_params(event);
                instance
                    .call_on_event(kind, timestamp.as_deref(), name.as_deref(), &payload_json, replay)
                    .map_err(|e| format!("on-event call failed: {e}"))
            }
            PluginWork::Message(delivery) => instance
                .call_on_message(&delivery.driver_id, &delivery.topic, &delivery.payload)
                .map_err(|e| format!("on-message call failed: {e}")),
        };
        if let Err(reason) = result {
            tracing::warn!(
                plugin_id = %manifest.id,
                "wasm call failed, disabling plugin: {reason}"
            );
            registry.set_disabled(&manifest.id, reason);
            break;
        }
    }
}

/// プラグイン専用スレッドが処理する仕事。journal イベントとバスの配信を
/// 1 本のキューに混ぜることで、wasm 呼び出しが 1 スレッドに直列化される
/// 性質(`PluginInstance` が `Send` を気にしなくてよい根拠)を保つ。
#[derive(Debug)]
enum PluginWork {
    Event(Arc<Event>),
    Message(Delivery),
}

/// `router` を購読し、`manifest.events` にマッチしたイベントだけを
/// プラグインスレッドへ転送する tokio タスクを起動する。
///
/// `work_tx` は容量固定の `sync_channel`(`PLUGIN_EVENT_QUEUE_CAPACITY`)
/// なので、送信には(ブロックする `send` ではなく)`try_send` を使う。この
/// tokio タスクは非同期ランタイムのワーカースレッド上で動くため、万一
/// プラグインスレッドが `driver-http.send` のブロッキング呼び出し中で
/// `work_rx` を全く読んでいなくても、ここで待たされてはいけない
/// (ワーカースレッドを塞ぐと router/monitor 全体に波及する)。キューが
/// 満杯の間に届いたイベントは `tracing::warn!` を出して破棄する
/// (`PLUGIN_EVENT_QUEUE_CAPACITY` のドキュメント参照)。
fn spawn_event_subscriber(
    manifest: Manifest,
    mut rx: broadcast::Receiver<Arc<Event>>,
    work_tx: std_mpsc::SyncSender<PluginWork>,
) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if !matches_event(&manifest.events, &event) {
                        continue;
                    }
                    match work_tx.try_send(PluginWork::Event(event)) {
                        Ok(()) => {}
                        Err(std_mpsc::TrySendError::Full(_)) => {
                            tracing::warn!(
                                plugin_id = %manifest.id,
                                "event queue full ({PLUGIN_EVENT_QUEUE_CAPACITY} pending), \
                                 dropping event for a slow/blocked plugin"
                            );
                        }
                        Err(std_mpsc::TrySendError::Disconnected(_)) => {
                            // プラグインスレッドが終了(disabled)済み。
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        plugin_id = %manifest.id,
                        "event subscriber lagged, skipped {skipped} events"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn event_params(event: &Event) -> (&'static str, Option<String>, Option<String>, String, bool) {
    match event {
        Event::Journal {
            timestamp,
            event: name,
            raw,
            replay,
        } => (
            "journal",
            Some(timestamp.clone()),
            Some(name.clone()),
            raw.to_string(),
            *replay,
        ),
        Event::Status { raw } => ("status", None, None, raw.to_string(), false),
    }
}

/// 購読を登録し、retain 済みトピックなら現在値を 1 回だけ届ける。
///
/// 後から起動・後から承認されたプラグインにも最新値が渡るようにするため
/// (設計書「データフロー」参照)。ここで送るのは登録直後の 1 通だけで、
/// 以降は通常の `emit` 経路に乗る。
///
/// **登録が先、送信が後**: 先に `bus.subscribe` で購読表へ登録してから
/// 現在の retained 値を読んで送る。逆順にすると、値を読んだ直後・購読登録の
/// 前に割り込んだ `emit` を取りこぼす窓ができてしまう。
pub(crate) fn subscribe_with_initial_value(
    bus: &Bus,
    plugin_id: &str,
    driver_id: &str,
    topic: &str,
    sender: std_mpsc::SyncSender<Delivery>,
) {
    bus.subscribe(plugin_id, driver_id, topic, sender.clone());
    if let Some(payload) = bus.retained_for(driver_id, topic) {
        let _ = sender.try_send(Delivery {
            plugin_id: plugin_id.to_string(),
            driver_id: driver_id.to_string(),
            topic: topic.to_string(),
            payload,
        });
    }
}

/// `Bus::subscribe` に渡した `SyncSender<Delivery>` の受け口を読み、
/// 承認済み・宣言済みのままの配信だけを `PluginWork::Message` に詰め替えて
/// プラグインの作業キューへ転送する。`spawn_event_subscriber` と対称の形だが、
/// 転送元が(非同期の `broadcast::Receiver` ではなく)同期の
/// `std::sync::mpsc::Receiver` なので `tokio::task::spawn_blocking` を使う
/// (`bin/edlr.rs` の `spawn_blocking` 呼び出しと同じ流儀。非同期ランタイムの
/// ワーカースレッドを専有させない)。
///
/// **配信のたびに承認を再確認する**: 承認は稼働中も取り消せる
/// (`Registry::set_bus_grant`)ため、購読を登録した時点の承認状態を信じて
/// 転送し続けると、取り消し後も届いてしまう(fail-open)。ここでは毎回
/// `bus_json` を読み直し、`granted` かつ当該トピックが `subscribe` に
/// 含まれている場合だけ転送する(`HostCtx::check_bus` と同じ判定材料・
/// 同じ判定規則)。
fn spawn_bus_subscriber(
    manifest: Manifest,
    bus_json: Arc<Mutex<String>>,
    delivery_rx: std_mpsc::Receiver<Delivery>,
    work_tx: std_mpsc::SyncSender<PluginWork>,
) {
    tokio::task::spawn_blocking(move || {
        for delivery in delivery_rx {
            let raw = bus_json
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let entries = parse_bus(&raw);
            let still_granted = entries
                .get(&delivery.driver_id)
                .is_some_and(|entry| entry.granted && entry.subscribe.contains(&delivery.topic));
            if !still_granted {
                // 承認が取り消された(か、そもそも一度も承認されていない)。
                // 黙って捨てる -- `check_bus` が publish/get 側で同じ状況を
                // `permission-denied` として扱うのと違い、こちらはドライバ
                // 起点のプッシュ配信なので呼び出し元に返すエラーが無い。
                continue;
            }
            match work_tx.try_send(PluginWork::Message(delivery)) {
                Ok(()) => {}
                Err(std_mpsc::TrySendError::Full(_)) => {
                    tracing::warn!(
                        plugin_id = %manifest.id,
                        "work queue full ({PLUGIN_EVENT_QUEUE_CAPACITY} pending), \
                         dropping a bus delivery for a slow/blocked plugin"
                    );
                }
                Err(std_mpsc::TrySendError::Disconnected(_)) => {
                    // プラグインスレッドが終了(disabled)済み。
                    break;
                }
            }
        }
    });
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

#[cfg(test)]
mod tests {
    //! `spawn_event_subscriber` is what stands between the router's
    //! broadcast channel and a plugin thread that might be blocked for up
    //! to `host::HTTP_TIMEOUT` inside `driver-http.send`. These tests never
    //! drain `events_rx` (simulating a fully-stalled plugin thread) and
    //! assert that publishing far more events than
    //! `PLUGIN_EVENT_QUEUE_CAPACITY` neither blocks the publishing task nor
    //! grows the queue past its bound.
    use super::*;
    use std::time::Duration;

    fn test_manifest() -> Manifest {
        Manifest {
            id: "queue-test-plugin".into(),
            name: "Queue Test".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec!["*".to_string()],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
        }
    }

    fn journal_event(name: &str) -> Arc<Event> {
        Arc::new(Event::Journal {
            timestamp: "2026-07-25T00:00:00Z".into(),
            event: name.into(),
            raw: serde_json::json!({}),
            replay: false,
        })
    }

    #[tokio::test]
    async fn slow_plugin_channel_stays_bounded_and_publishing_does_not_block() {
        let (broadcast_tx, broadcast_rx) = broadcast::channel::<Arc<Event>>(4096);
        let (work_tx, work_rx) =
            std_mpsc::sync_channel::<PluginWork>(PLUGIN_EVENT_QUEUE_CAPACITY);

        spawn_event_subscriber(test_manifest(), broadcast_rx, work_tx);

        // Simulate a plugin thread that's blocked in `driver-http.send`:
        // never drain `work_rx` while publishing far more events than the
        // queue's capacity. If `try_send` were replaced with a blocking
        // `send`, this loop (running on the current task, same as the
        // subscriber would on a shared runtime) would risk deadlocking or
        // at minimum this test would take a long time; with `try_send` it
        // must complete promptly regardless of channel fullness.
        let published = PLUGIN_EVENT_QUEUE_CAPACITY * 50;
        for i in 0..published {
            broadcast_tx
                .send(journal_event(&format!("Evt{i}")))
                .expect("broadcast send should succeed (large capacity, no lag)");
        }

        // Give the subscriber tokio task a bounded window to drain the
        // broadcast channel into work_rx (or drop on overflow); this test
        // fails by hanging (via the outer test timeout) if the subscriber
        // ever blocks trying to push past the queue's capacity.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut queued = 0usize;
        while work_rx.try_recv().is_ok() {
            queued += 1;
        }

        assert!(
            queued <= PLUGIN_EVENT_QUEUE_CAPACITY,
            "queued {queued} events, expected at most {PLUGIN_EVENT_QUEUE_CAPACITY} \
             (publishing {published} events to a never-drained receiver must not \
             grow the queue past its bound)"
        );
    }

    #[test]
    fn deliveries_reach_the_plugin_queue_and_full_queues_drop_the_message() {
        // Bus::subscribe に渡すのと同じ容量 1 の sync_channel を使い、
        // 2 通目が捨てられる(＝ emit 自体は成功する)ことを確認する。
        let bus = edlr_driver_channel::Bus::new();
        let (dtx, _drx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            }],
            dtx,
        );
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        bus.subscribe("translator", "ed-state", "current-system", tx);

        bus.emit("ed-state", "current-system", b"a".to_vec()).unwrap();
        bus.emit("ed-state", "current-system", b"b".to_vec()).unwrap();

        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
        assert_eq!(
            bus.retained_for("ed-state", "current-system"),
            Some(b"b".to_vec())
        );
    }

    #[test]
    fn subscribing_to_a_retained_topic_delivers_the_current_value_once() {
        let bus = edlr_driver_channel::Bus::new();
        let (dtx, _drx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            }],
            dtx,
        );
        bus.emit("ed-state", "current-system", b"Sol".to_vec())
            .unwrap();

        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        subscribe_with_initial_value(&bus, "translator", "ed-state", "current-system", tx);

        assert_eq!(rx.try_recv().unwrap().payload, b"Sol".to_vec());
    }

    /// `spawn_bus_subscriber` が承認取消を配信のたびに再確認することの検証。
    ///
    /// テストの信頼性を担保するため、承認あり/なしの 2 ケースを **同じ**
    /// 購読・同じ emit で作り、違いは `bus_json` の `granted` だけにする
    /// (「何も送っていないから届かない」で偽陽性になるのを避けるため)。
    #[tokio::test]
    async fn bus_subscriber_forwards_only_while_still_granted() {
        use crate::plugin::bus_runtime::{bus_json_string, BusRuntimeEntry};

        let bus = edlr_driver_channel::Bus::new();
        let (dtx, _drx) = std_mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: false,
                description: String::new(),
            }],
            dtx,
        );

        let granted_entry = |granted: bool| {
            bus_json_string(&[BusRuntimeEntry {
                driver: "ed-state".into(),
                granted,
                publish: vec![],
                subscribe: vec!["current-system".into()],
            }])
        };

        // ケース 1: 承認あり。配信は work_rx へ届く。
        {
            let bus_json = Arc::new(Mutex::new(granted_entry(true)));
            let (delivery_tx, delivery_rx) = std_mpsc::sync_channel(4);
            let (work_tx, work_rx) = std_mpsc::sync_channel::<PluginWork>(4);
            bus.subscribe("translator", "ed-state", "current-system", delivery_tx);
            spawn_bus_subscriber(test_manifest(), bus_json, delivery_rx, work_tx);

            bus.emit("ed-state", "current-system", b"Sol".to_vec())
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;

            match work_rx.try_recv() {
                Ok(PluginWork::Message(delivery)) => {
                    assert_eq!(delivery.payload, b"Sol".to_vec())
                }
                other => panic!("expected a granted delivery to reach the plugin queue, got {other:?}"),
            }
        }

        // ケース 2: 承認なし。全く同じ購読・同じ emit だが、届かない。
        {
            let bus_json = Arc::new(Mutex::new(granted_entry(false)));
            let (delivery_tx, delivery_rx) = std_mpsc::sync_channel(4);
            let (work_tx, work_rx) = std_mpsc::sync_channel::<PluginWork>(4);
            bus.subscribe("translator", "ed-state", "current-system", delivery_tx);
            spawn_bus_subscriber(test_manifest(), bus_json, delivery_rx, work_tx);

            bus.emit("ed-state", "current-system", b"Sol".to_vec())
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;

            assert!(
                work_rx.try_recv().is_err(),
                "a revoked grant must not let the delivery reach the plugin queue"
            );
        }
    }
}

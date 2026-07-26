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

use crate::event::Event;
use crate::plugin::grants::GrantsStore;
use crate::plugin::host::{capabilities_json_string, HostCtx, PluginHost};
use crate::plugin::manifest::{load_manifest, matches_event};
use crate::plugin::registry::{PluginEntry, PluginState, Registry};
use crate::plugin::settings::SettingsStore;
use crate::plugin::sidecar::{assign_ports, SidecarConfig, SidecarConfigStore};
use crate::plugin::sidecar_runtime::{implicit_http_hosts, sidecars_json_string, SidecarRuntimeEntry};
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
pub fn start_plugins(
    plugins_dir: &Path,
    settings_store: SettingsStore,
    sidecar_config_store: SidecarConfigStore,
    grants_store: GrantsStore,
    router: &Router,
    host: PluginHost,
) -> Registry {
    let host = Arc::new(host);
    let settings_store = Arc::new(settings_store);
    let grants_store = Arc::new(grants_store);
    let sidecar_config_store = Arc::new(sidecar_config_store);
    let process_driver = host.process_driver();
    let registry = Registry::new(
        host.clone(),
        settings_store.clone(),
        grants_store.clone(),
        sidecar_config_store.clone(),
        process_driver,
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

        load_and_run_plugin(
            &manifest,
            &path,
            &settings_store,
            &grants_store,
            &sidecar_config_store,
            router,
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
    router: &Router,
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

    let (events_tx, events_rx) = std_mpsc::sync_channel::<Arc<Event>>(PLUGIN_EVENT_QUEUE_CAPACITY);
    let (ready_tx, ready_rx) = std_mpsc::channel::<PluginState>();

    thread::spawn({
        let host = host.clone();
        let manifest = manifest.clone();
        let settings_json = settings_json.clone();
        let capabilities_json = capabilities_json.clone();
        let sidecars_json = sidecars_json.clone();
        let registry = registry.clone();
        move || {
            run_plugin_thread(
                host,
                manifest,
                entry_path,
                settings_json,
                capabilities_json,
                sidecars_json,
                registry,
                events_rx,
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
    });

    if running {
        spawn_event_subscriber(manifest.clone(), router.subscribe(), events_tx);
    }
    // Disabled の場合、events_tx はここで drop される。プラグインスレッドは
    // 既に init 失敗で return 済みで events_rx を読まないので問題ない。
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
    registry: Registry,
    events_rx: std_mpsc::Receiver<Arc<Event>>,
    ready_tx: std_mpsc::Sender<PluginState>,
) {
    let ctx = HostCtx::new(
        manifest.id.clone(),
        settings_json,
        capabilities_json,
        sidecars_json,
        // TODO(次タスク): `[[filesystem]]` の承認・設定配線が入るまでの
        // 仮の空バッファ。実配線は Task 7 で `Registry` から渡す。
        Arc::new(Mutex::new("[]".to_string())),
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

    for event in events_rx {
        let (kind, timestamp, name, payload_json) = event_params(&event);
        if let Err(e) =
            instance.call_on_event(kind, timestamp.as_deref(), name.as_deref(), &payload_json)
        {
            tracing::warn!(
                plugin_id = %manifest.id,
                "on-event call failed, disabling plugin: {e}"
            );
            registry.set_disabled(&manifest.id, format!("on-event call failed: {e}"));
            break;
        }
    }
}

/// `router` を購読し、`manifest.events` にマッチしたイベントだけを
/// プラグインスレッドへ転送する tokio タスクを起動する。
///
/// `events_tx` は容量固定の `sync_channel`(`PLUGIN_EVENT_QUEUE_CAPACITY`)
/// なので、送信には(ブロックする `send` ではなく)`try_send` を使う。この
/// tokio タスクは非同期ランタイムのワーカースレッド上で動くため、万一
/// プラグインスレッドが `driver-http.send` のブロッキング呼び出し中で
/// `events_rx` を全く読んでいなくても、ここで待たされてはいけない
/// (ワーカースレッドを塞ぐと router/monitor 全体に波及する)。キューが
/// 満杯の間に届いたイベントは `tracing::warn!` を出して破棄する
/// (`PLUGIN_EVENT_QUEUE_CAPACITY` のドキュメント参照)。
fn spawn_event_subscriber(
    manifest: Manifest,
    mut rx: broadcast::Receiver<Arc<Event>>,
    events_tx: std_mpsc::SyncSender<Arc<Event>>,
) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if !matches_event(&manifest.events, &event) {
                        continue;
                    }
                    match events_tx.try_send(event) {
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

fn event_params(event: &Event) -> (&'static str, Option<String>, Option<String>, String) {
    match event {
        Event::Journal {
            timestamp,
            event: name,
            raw,
        } => (
            "journal",
            Some(timestamp.clone()),
            Some(name.clone()),
            raw.to_string(),
        ),
        Event::Status { raw } => ("status", None, None, raw.to_string()),
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
        }
    }

    fn journal_event(name: &str) -> Arc<Event> {
        Arc::new(Event::Journal {
            timestamp: "2026-07-25T00:00:00Z".into(),
            event: name.into(),
            raw: serde_json::json!({}),
        })
    }

    #[tokio::test]
    async fn slow_plugin_channel_stays_bounded_and_publishing_does_not_block() {
        let (broadcast_tx, broadcast_rx) = broadcast::channel::<Arc<Event>>(4096);
        let (events_tx, events_rx) =
            std_mpsc::sync_channel::<Arc<Event>>(PLUGIN_EVENT_QUEUE_CAPACITY);

        spawn_event_subscriber(test_manifest(), broadcast_rx, events_tx);

        // Simulate a plugin thread that's blocked in `driver-http.send`:
        // never drain `events_rx` while publishing far more events than the
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
        // broadcast channel into events_rx (or drop on overflow); this test
        // fails by hanging (via the outer test timeout) if the subscriber
        // ever blocks trying to push past the queue's capacity.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut queued = 0usize;
        while events_rx.try_recv().is_ok() {
            queued += 1;
        }

        assert!(
            queued <= PLUGIN_EVENT_QUEUE_CAPACITY,
            "queued {queued} events, expected at most {PLUGIN_EVENT_QUEUE_CAPACITY} \
             (publishing {published} events to a never-drained receiver must not \
             grow the queue past its bound)"
        );
    }
}

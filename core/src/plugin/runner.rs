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

use tokio::sync::broadcast;

use crate::event::Event;
use crate::plugin::host::{HostCtx, PluginHost};
use crate::plugin::manifest::{load_manifest, matches_event};
use crate::plugin::registry::{PluginEntry, PluginState, Registry};
use crate::plugin::settings::SettingsStore;
use crate::plugin::Manifest;
use crate::router::Router;

/// `plugins_dir` を走査し、各プラグインをロードして専用タスクで駆動する。
///
/// 戻り値の `Registry` は起動直後から `snapshot` 可能(= 各プラグインの
/// `load`/`init` 結果が確定した後に返る)。
pub fn start_plugins(
    plugins_dir: &Path,
    settings_store: SettingsStore,
    router: &Router,
    host: PluginHost,
) -> Registry {
    let host = Arc::new(host);
    let settings_store = Arc::new(settings_store);
    let registry = Registry::new(
        host.clone(),
        settings_store.clone(),
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

        load_and_run_plugin(&manifest, &path, &settings_store, router, &host, &registry);
    }

    registry
}

/// 1 プラグインをロードし、成功すれば専用スレッド・購読タスクを起動し、
/// 結果(Running/Disabled)を `registry` に登録する。
fn load_and_run_plugin(
    manifest: &Manifest,
    dir: &Path,
    settings_store: &SettingsStore,
    router: &Router,
    host: &Arc<PluginHost>,
    registry: &Registry,
) {
    let entry_path = dir.join(&manifest.entry);
    let effective = settings_store.effective(manifest);
    let settings_json_string = serde_json::to_string(&serde_json::Value::Object(effective))
        .unwrap_or_else(|_| "{}".to_string());
    let settings_json = Arc::new(Mutex::new(settings_json_string));

    let (events_tx, events_rx) = std_mpsc::channel::<Arc<Event>>();
    let (ready_tx, ready_rx) = std_mpsc::channel::<PluginState>();

    thread::spawn({
        let host = host.clone();
        let manifest = manifest.clone();
        let settings_json = settings_json.clone();
        let registry = registry.clone();
        move || {
            run_plugin_thread(
                host,
                manifest,
                entry_path,
                settings_json,
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
    });

    if running {
        spawn_event_subscriber(manifest.clone(), router.subscribe(), events_tx);
    }
    // Disabled の場合、events_tx はここで drop される。プラグインスレッドは
    // 既に init 失敗で return 済みで events_rx を読まないので問題ない。
}

/// プラグイン専用スレッドの本体。`load` → `call_init` → イベントループを
/// 直列に実行する。すべての wasm 呼び出しはこのスレッド上でのみ発生する。
fn run_plugin_thread(
    host: Arc<PluginHost>,
    manifest: Manifest,
    entry_path: PathBuf,
    settings_json: Arc<Mutex<String>>,
    registry: Registry,
    events_rx: std_mpsc::Receiver<Arc<Event>>,
    ready_tx: std_mpsc::Sender<PluginState>,
) {
    let ctx = HostCtx::new(manifest.id.clone(), settings_json);
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
fn spawn_event_subscriber(
    manifest: Manifest,
    mut rx: broadcast::Receiver<Arc<Event>>,
    events_tx: std_mpsc::Sender<Arc<Event>>,
) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if matches_event(&manifest.events, &event) && events_tx.send(event).is_err() {
                        // プラグインスレッドが終了(disabled)済み。
                        break;
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

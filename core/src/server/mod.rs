use crate::driver::{DriverRegistry, DriverState};
use crate::event::Event;
use crate::plugin::Registry;
use crate::router::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};

const REPLAY_CAPACITY: usize = 1000;

pub fn hello_json() -> String {
    serde_json::json!({"type": "hello", "protocol": 1}).to_string()
}

pub fn event_to_ws_json(event: &Event) -> String {
    match event {
        Event::Journal {
            timestamp,
            event,
            raw,
            replay,
        } => serde_json::json!({
            "type": "event", "kind": "journal",
            "timestamp": timestamp, "event": event, "raw": raw, "replay": replay,
        })
        .to_string(),
        Event::Status { raw } => {
            serde_json::json!({"type": "event", "kind": "status", "raw": raw}).to_string()
        }
    }
}

/// WS 配信用の共有状態。feeder タスクがロックを保持したままリングバッファ追記と
/// broadcast 送信を行うため、新規接続のスナップショット+購読(同じくロック下)
/// との間で欠落も重複も起きない。
#[derive(Clone)]
pub struct ServerState {
    inner: Arc<Mutex<ReplayBuffer>>,
    registry: Option<Registry>,
    drivers: Option<DriverRegistry>,
}

struct ReplayBuffer {
    buf: VecDeque<Arc<String>>,
    tx: broadcast::Sender<Arc<String>>,
}

impl ServerState {
    pub fn new(
        router: &Router,
        registry: Option<Registry>,
        drivers: Option<DriverRegistry>,
    ) -> Self {
        let (tx, _) = broadcast::channel(256);
        let state = Self {
            inner: Arc::new(Mutex::new(ReplayBuffer {
                buf: VecDeque::new(),
                tx,
            })),
            registry,
            drivers,
        };
        let mut rx = router.subscribe();
        let feeder = state.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => feeder.push(event_to_ws_json(&event)),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("ws feeder lagged, dropped {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        state
    }

    /// tracing ログのフレーム(`crate::logs::LogLayer` 産)をイベントと同じ
    /// ReplayBuffer + broadcast に合流させる。受信ラグ(Lagged)は捨てて
    /// 続行する -- ログ表示はベストエフォートで、イベント配信を妨げない。
    pub fn attach_log_stream(&self, mut rx: broadcast::Receiver<Arc<String>>) {
        let feeder = self.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(frame) => feeder.push_frame(frame),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    fn push(&self, json: String) {
        self.push_frame(Arc::new(json));
    }

    fn push_frame(&self, json: Arc<String>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.buf.len() == REPLAY_CAPACITY {
            inner.buf.pop_front();
        }
        inner.buf.push_back(json.clone());
        let _ = inner.tx.send(json);
    }

    fn snapshot_and_subscribe(&self) -> (Vec<Arc<String>>, broadcast::Receiver<Arc<String>>) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.buf.iter().cloned().collect(), inner.tx.subscribe())
    }
}

/// `plugins/*` RPC メソッドを処理する純関数。テスト容易性のため公開。
/// `drivers = None` で [`handle_rpc_with_drivers`] に委譲する薄いラッパ
/// (既存の呼び出し元・テストを壊さないため)。
pub fn handle_rpc(
    registry: Option<&Registry>,
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    handle_rpc_with_drivers(registry, None, method, params)
}

/// `plugins/*` / `drivers/*` RPC メソッドを処理する純関数。テスト容易性の
/// ため公開。
///
/// `registry` が `None` の場合(プラグインホスト起動失敗などで
/// `ServerState` に `Registry` が渡されなかった場合)は `plugins/*` のどの
/// メソッドも `Err("plugins unavailable")` を返す。同様に `drivers` が
/// `None` の場合は `drivers/*` のどのメソッドも `Err("drivers unavailable")`
/// を返す(ドライバホスト起動失敗などで `ServerState` に `DriverRegistry` が
/// 渡されなかった場合)。
pub fn handle_rpc_with_drivers(
    registry: Option<&Registry>,
    drivers: Option<&DriverRegistry>,
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    if let Some(method) = method.strip_prefix("drivers/") {
        let drivers = drivers.ok_or_else(|| "drivers unavailable".to_string())?;
        return handle_drivers_rpc(drivers, method, params);
    }

    let registry = registry.ok_or_else(|| "plugins unavailable".to_string())?;
    match method {
        "plugins/list" => {
            let plugins: Vec<serde_json::Value> = registry
                .list()
                .into_iter()
                .map(|info| {
                    let mut value = serde_json::json!({
                        "id": info.manifest.id,
                        "name": info.manifest.name,
                        "version": info.manifest.version,
                        "description": info.manifest.description,
                        "settings": info.manifest.settings,
                        "values": info.values,
                        "capabilities": capabilities_result_json(
                            &info.capability_requests,
                            &info.grant_state,
                        ),
                    });
                    value["sidecars"] = sidecars_result_json(&info.sidecars)["sidecars"].clone();
                    value["filesystem"] = filesystem_result_json(&info.filesystem)["roots"].clone();
                    let bus = registry.bus(&info.manifest.id).unwrap_or_default();
                    value["bus"] = bus_result_json(&bus)["bus"].clone();
                    value["dashboard"] =
                        dashboard_result_json(&info.dashboard)["dashboard"].clone();
                    value["schedules"] =
                        schedules_result_json(&info.schedules)["schedules"].clone();
                    value["dropped"] = dropped_result_json(&info.dropped);
                    // `secret` 型設定の値は `values` に含まれない(write-only)。
                    // 「設定済みかどうか」だけを UI に伝える。
                    value["secretsSet"] = serde_json::json!(info.secrets_set);
                    match info.state {
                        crate::plugin::PluginState::Running => {
                            value["state"] = serde_json::json!("running");
                        }
                        crate::plugin::PluginState::Disabled { reason } => {
                            value["state"] = serde_json::json!("disabled");
                            value["reason"] = serde_json::json!(reason);
                        }
                    }
                    value
                })
                .collect();
            Ok(serde_json::json!({
                "pluginsDir": registry.plugins_dir().to_string_lossy(),
                "plugins": plugins,
            }))
        }
        "plugins/set-bus-grant" => {
            let plugin = param_str(params, "plugin")?;
            let driver = param_str(params, "driver")?;
            let granted = params
                .get("granted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "params.granted must be a bool".to_string())?;
            registry
                .set_bus_grant(plugin, driver, granted)
                .map_err(|e| e.to_string())?;
            // `set_sidecar_grant`/`set_filesystem_grant` と同じ流儀: 1 件だけ
            // の grant state を返すのではなく、その plugin の bus 一覧全体を
            // 返す(UI が 1 往復でリスト全体を更新できるように)。
            let bus = registry.bus(plugin).map_err(|e| e.to_string())?;
            Ok(bus_result_json(&bus))
        }
        "plugins/set-dashboard-grant" => {
            let plugin = param_str(params, "plugin")?;
            let widget = param_str(params, "widget")?;
            let granted = params
                .get("granted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "params.granted must be a bool".to_string())?;
            // `set_bus_grant` と同じ流儀: その plugin のウィジェット一覧全体を
            // 返す(UI が 1 往復でリスト全体を更新できるように)。
            let dashboard = registry
                .set_dashboard_grant(plugin, widget, granted)
                .map_err(|e| e.to_string())?;
            Ok(dashboard_result_json(&dashboard))
        }
        "dashboard/list" => {
            // Dashboard 画面用: grant 済みウィジェットだけを、iframe が
            // そのまま使える URL 付きで返す。未 grant を混ぜないのは、
            // アセット配信側も未 grant を 404 にする(見えないものは
            // 取得もできない)のと対になる。
            let widgets: Vec<serde_json::Value> = registry
                .dashboard_widgets_for_ui()
                .into_iter()
                .filter(|(_, _, _, info)| info.grant.granted)
                .map(|(plugin_id, plugin_name, state, info)| {
                    let entry_file = std::path::Path::new(&info.request.entry)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("index.html");
                    let events = registry.events_of(&plugin_id).unwrap_or_default();
                    let state_str = match state {
                        crate::plugin::PluginState::Running => "running",
                        crate::plugin::PluginState::Disabled { .. } => "disabled",
                    };
                    serde_json::json!({
                        "plugin": plugin_id,
                        "pluginName": plugin_name,
                        "widget": info.request.id,
                        "title": info.request.title,
                        "url": format!("/plugin-ui/{plugin_id}/{}/{entry_file}", info.request.id),
                        "size": info.request.size.as_str(),
                        "events": events,
                        "resolved": info.resolved,
                        "state": state_str,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "widgets": widgets }))
        }
        "plugins/get-settings" => {
            let plugin = params
                .get("plugin")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "params.plugin must be a string".to_string())?;
            let values = registry.values(plugin).map_err(|e| e.to_string())?;
            Ok(serde_json::Value::Object(values))
        }
        "plugins/set-settings" => {
            let plugin = params
                .get("plugin")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "params.plugin must be a string".to_string())?;
            let values = params
                .get("values")
                .and_then(|v| v.as_object())
                .ok_or_else(|| "params.values must be an object".to_string())?;
            let updated = registry
                .set_values(plugin, values)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::Value::Object(updated))
        }
        "plugins/get-capabilities" => {
            let plugin = params
                .get("plugin")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "params.plugin must be a string".to_string())?;
            let (requests, grant_state) =
                registry.capabilities(plugin).map_err(|e| e.to_string())?;
            Ok(capabilities_result_json(&requests, &grant_state))
        }
        "plugins/set-capabilities" => {
            let plugin = params
                .get("plugin")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "params.plugin must be a string".to_string())?;
            let granted = params
                .get("granted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "params.granted must be a bool".to_string())?;
            let grant_state = registry
                .set_capabilities(plugin, granted)
                .map_err(|e| e.to_string())?;
            let (requests, _) = registry.capabilities(plugin).map_err(|e| e.to_string())?;
            Ok(capabilities_result_json(&requests, &grant_state))
        }
        "plugins/get-sidecars" => {
            let plugin = param_str(params, "plugin")?;
            let sidecars = registry.sidecars(plugin).map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
        "plugins/set-sidecar-config" => {
            let plugin = param_str(params, "plugin")?;
            let name = param_str(params, "name")?;
            let config: crate::plugin::SidecarConfig = serde_json::from_value(
                params
                    .get("config")
                    .cloned()
                    .ok_or_else(|| "params.config must be an object".to_string())?,
            )
            .map_err(|e| format!("params.config is invalid: {e}"))?;
            let sidecars = registry
                .set_sidecar_config(plugin, name, &config)
                .map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
        "plugins/set-sidecar-grant" => {
            let plugin = param_str(params, "plugin")?;
            let name = param_str(params, "name")?;
            let granted = params
                .get("granted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "params.granted must be a bool".to_string())?;
            let sidecars = registry
                .set_sidecar_grant(plugin, name, granted)
                .map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
        "plugins/sidecar-control" => {
            let plugin = param_str(params, "plugin")?;
            let name = param_str(params, "name")?;
            let action = match param_str(params, "action")? {
                "start" => crate::plugin::SidecarAction::Start,
                "stop" => crate::plugin::SidecarAction::Stop,
                "restart" => crate::plugin::SidecarAction::Restart,
                other => return Err(format!("unknown action: {other}")),
            };
            let sidecars = registry
                .control_sidecar(plugin, name, action)
                .map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
        "plugins/get-filesystem" => {
            let plugin = param_str(params, "plugin")?;
            let roots = registry.filesystem(plugin).map_err(|e| e.to_string())?;
            Ok(filesystem_result_json(&roots))
        }
        "plugins/set-filesystem-config" => {
            let plugin = param_str(params, "plugin")?;
            let name = param_str(params, "name")?;
            let config: crate::plugin::FilesystemConfig = serde_json::from_value(
                params
                    .get("config")
                    .cloned()
                    .ok_or_else(|| "params.config must be an object".to_string())?,
            )
            .map_err(|e| format!("params.config is invalid: {e}"))?;
            let roots = registry
                .set_filesystem_config(plugin, name, &config)
                .map_err(|e| e.to_string())?;
            Ok(filesystem_result_json(&roots))
        }
        "plugins/set-filesystem-grant" => {
            let plugin = param_str(params, "plugin")?;
            let name = param_str(params, "name")?;
            let granted = params
                .get("granted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "params.granted must be a bool".to_string())?;
            let roots = registry
                .set_filesystem_grant(plugin, name, granted)
                .map_err(|e| e.to_string())?;
            Ok(filesystem_result_json(&roots))
        }
        other => Err(format!("unknown method: {other}")),
    }
}

/// `drivers/*` RPC メソッドを処理する。呼び出し元([`handle_rpc_with_drivers`])
/// が `drivers/` プレフィックスを剥がして渡す(`method` はプレフィックス無し)。
/// `crate::plugin::registry::Registry` の `plugins/*` 系(`get-settings`/
/// `set-settings`/`set-capabilities`)と同じ形の応答を返す。`DriverInfo` は
/// `PluginInfo` と違い `capability_requests` を持たない(manifest の
/// `capabilities` をそのまま使う設計 -- `DriverInfo` のドキュメント参照)ため、
/// `capabilities_result_json` には `manifest.capabilities` を渡す。
fn handle_drivers_rpc(
    drivers: &DriverRegistry,
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match method {
        "list" => Ok(serde_json::json!({
            "driversDir": drivers.drivers_dir().to_string_lossy(),
            "drivers": drivers.list().into_iter().map(|info| {
                let mut value = serde_json::json!({
                    "id": info.manifest.id,
                    "name": info.manifest.name,
                    "version": info.manifest.version,
                    "description": info.manifest.description,
                    "topics": info.manifest.topics,
                    "settings": info.manifest.settings,
                    "values": info.values,
                    "capabilities": capabilities_result_json(&info.manifest.capabilities, &info.grant_state),
                    "sidecars": sidecars_result_json(&info.sidecars)["sidecars"],
                    "filesystem": filesystem_result_json(&info.filesystem)["roots"],
                });
                // `plugins/list` と同じ流儀: `reason` は `Disabled` のときだけ
                // 載せる(`ui/frontend/src/types/plugin.ts` の `reason?: string`、
                // `Drivers.tsx` の「無効: {driver.reason}」表示が診断情報を
                // 拾えるように -- 最終レビューで見つかった Minor な取りこぼし。
                // 以前はここで `state` を文字列に潰すだけで `reason` を運んで
                // いなかった)。
                match info.state {
                    DriverState::Running => {
                        value["state"] = serde_json::json!("running");
                    }
                    DriverState::Disabled { reason } => {
                        value["state"] = serde_json::json!("disabled");
                        value["reason"] = serde_json::json!(reason);
                    }
                }
                value
            }).collect::<Vec<_>>(),
        })),
        "get-settings" => {
            let driver = param_str(params, "driver")?;
            let values = drivers.values(driver).map_err(|e| e.to_string())?;
            Ok(serde_json::Value::Object(values))
        }
        "set-settings" => {
            let driver = param_str(params, "driver")?;
            let values = params
                .get("values")
                .and_then(|v| v.as_object())
                .ok_or_else(|| "params.values must be an object".to_string())?;
            let updated = drivers
                .set_values(driver, values)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::Value::Object(updated))
        }
        "set-capabilities" => {
            let driver = param_str(params, "driver")?;
            let granted = params
                .get("granted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "params.granted must be a bool".to_string())?;
            let grant_state = drivers
                .set_capabilities(driver, granted)
                .map_err(|e| e.to_string())?;
            let manifest = drivers
                .manifest_of(driver)
                .ok_or_else(|| format!("unknown driver: {driver}"))?;
            Ok(capabilities_result_json(
                &manifest.capabilities,
                &grant_state,
            ))
        }
        "set-sidecar-config" => {
            let driver = param_str(params, "driver")?;
            let name = param_str(params, "name")?;
            let config: crate::plugin::SidecarConfig = serde_json::from_value(
                params
                    .get("config")
                    .cloned()
                    .ok_or_else(|| "params.config must be an object".to_string())?,
            )
            .map_err(|e| format!("params.config is invalid: {e}"))?;
            let sidecars = drivers
                .set_sidecar_config(driver, name, &config)
                .map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
        "set-sidecar-grant" => {
            let driver = param_str(params, "driver")?;
            let name = param_str(params, "name")?;
            let granted = params
                .get("granted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "params.granted must be a bool".to_string())?;
            let sidecars = drivers
                .set_sidecar_grant(driver, name, granted)
                .map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
        "sidecar-control" => {
            let driver = param_str(params, "driver")?;
            let name = param_str(params, "name")?;
            let action = match param_str(params, "action")? {
                "start" => crate::plugin::SidecarAction::Start,
                "stop" => crate::plugin::SidecarAction::Stop,
                "restart" => crate::plugin::SidecarAction::Restart,
                other => return Err(format!("unknown action: {other}")),
            };
            let sidecars = drivers
                .control_sidecar(driver, name, action)
                .map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
        "set-filesystem-config" => {
            let driver = param_str(params, "driver")?;
            let name = param_str(params, "name")?;
            let config: crate::plugin::FilesystemConfig = serde_json::from_value(
                params
                    .get("config")
                    .cloned()
                    .ok_or_else(|| "params.config must be an object".to_string())?,
            )
            .map_err(|e| format!("params.config is invalid: {e}"))?;
            let roots = drivers
                .set_filesystem_config(driver, name, &config)
                .map_err(|e| e.to_string())?;
            Ok(filesystem_result_json(&roots))
        }
        "set-filesystem-grant" => {
            let driver = param_str(params, "driver")?;
            let name = param_str(params, "name")?;
            let granted = params
                .get("granted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "params.granted must be a bool".to_string())?;
            let roots = drivers
                .set_filesystem_grant(driver, name, granted)
                .map_err(|e| e.to_string())?;
            Ok(filesystem_result_json(&roots))
        }
        other => Err(format!("unknown method: drivers/{other}")),
    }
}

// Phase 2 で rpc/ へ移動(server 内の呼び出しと tests の `use super::*`
// がそのまま解決するよう、この use を温存する)。
use crate::rpc::params::param_str;
use crate::rpc::render::{
    bus_result_json, capabilities_result_json, dashboard_result_json, dropped_result_json,
    filesystem_result_json, schedules_result_json, sidecars_result_json,
};

/// ダッシュボードウィジェット向け SDK。`include_str!` でバイナリに埋め込み、
/// デーモン単体で(プラグイン側に SDK を同梱させずに)配信する。
const PLUGIN_UI_SDK: &str = include_str!("../plugin_ui_sdk.js");

/// ウィジェットアセットに付ける CSP。外部ネットワークへのサブリソース
/// 読み込み・fetch を遮断し、自ウィジェットのアセット(相対パス = この
/// デーモンのオリジン)のみ許可する。iframe 側は opaque origin
/// (sandbox="allow-scripts")だが、CSP の 'self' はドキュメント URL の
/// オリジンを指すため、相対パスのサブリソースは通る。
const WIDGET_CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; \
     style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'none'";

/// 拡張子ベースの Content-Type。ウィジェットアセットは信頼済みインストール
/// 物なので sniffing 対策よりも単純さを優先し、未知の拡張子は
/// octet-stream に倒す。
fn content_type_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// `GET /plugin-ui/{plugin}/{widget}/{*path}`。
///
/// grant チェック・トラバーサル拒否は `Registry::dashboard_asset_path`
/// (単体テスト済み)に集約してあり、HTTP 層は「失敗はすべて 404」に潰す
/// だけ。存在の有無・承認の有無を区別したステータスを返さないのは意図的
/// (未承認の外部者にウィジェット構成を探索させない)。
async fn plugin_ui_handler(
    axum::extract::State(state): axum::extract::State<ServerState>,
    axum::extract::Path((plugin, widget, path)): axum::extract::Path<(String, String, String)>,
) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    let Some(registry) = state.registry.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // ファイル IO はブロッキングなので spawn_blocking に逃がす。
    let result = tokio::task::spawn_blocking(move || {
        let file = registry.dashboard_asset_path(&plugin, &widget, &path)?;
        std::fs::read(&file)
            .map(|bytes| (bytes, content_type_for(&file)))
            .map_err(|_| crate::plugin::registry::RegistryError::UnknownDashboard(widget))
    })
    .await;
    match result {
        Ok(Ok((bytes, content_type))) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CONTENT_SECURITY_POLICY, WIDGET_CSP),
            ],
            bytes,
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn plugin_ui_sdk_handler() -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )],
        PLUGIN_UI_SDK,
    )
}

pub fn app(state: ServerState, ui_dir: Option<PathBuf>) -> axum::Router {
    let mut app = axum::Router::new()
        .route("/ws", get(ws_handler))
        .route("/plugin-ui-sdk.js", get(plugin_ui_sdk_handler))
        .route(
            "/plugin-ui/{plugin}/{widget}/{*path}",
            get(plugin_ui_handler),
        )
        .with_state(state);
    if let Some(dir) = ui_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)));
    }
    app
}

pub async fn serve(listener: TcpListener, state: ServerState, ui_dir: Option<PathBuf>) {
    if let Err(e) = axum::serve(listener, app(state, ui_dir)).await {
        tracing::warn!("http server terminated: {e}");
    }
}

/// `/ws` に許可される Origin かどうかを判定する。
///
/// ブラウザからの WS 接続は Origin ヘッダを送るため、任意の Web ページが
/// `ws://127.0.0.1:8137/ws` を開いてジャーナルストリームを読み取れてしまうのを防ぐ。
/// 許可するのは localhost 系(127.0.0.1 / localhost / [::1]、任意ポート・任意スキーム)と
/// Tauri のオリジン(`tauri://localhost`、`http(s)://tauri.localhost`)のみ。
/// Origin ヘッダ自体が無い接続(tokio-tungstenite や curl などブラウザ以外のクライアント)は許可する。
fn origin_allowed(origin: &str) -> bool {
    let Ok(uri) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    let Some(host) = uri.host() else {
        return false;
    };
    // http::Uri は IPv6 ホストをブラケット付き("[::1]")で返すため剥がしてから比較する
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "tauri.localhost")
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    if let Some(origin) = headers.get(axum::http::header::ORIGIN) {
        let allowed = origin.to_str().map(origin_allowed).unwrap_or(false);
        if !allowed {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    ws.on_upgrade(move |socket| client_loop(socket, state))
        .into_response()
}

async fn client_loop(mut socket: WebSocket, state: ServerState) {
    if socket
        .send(Message::Text(hello_json().into()))
        .await
        .is_err()
    {
        return;
    }
    let (snapshot, mut rx) = state.snapshot_and_subscribe();
    for json in snapshot {
        if socket
            .send(Message::Text(json.as_str().to_owned().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(json) => {
                    if socket
                        .send(Message::Text(json.as_str().to_owned().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("ws client lagged, dropped {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if let Some(response) = handle_client_message(&state, &text).await {
                        if socket.send(Message::Text(response.into())).await.is_err() {
                            return;
                        }
                    }
                }
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

/// クライアントからの生テキストメッセージを解析し、`{"type":"rpc",...}` で
/// あれば `handle_rpc` を呼んで応答 JSON 文字列を返す。
///
/// - JSON としてパースできない、または `type` が `"rpc"` でないメッセージは
///   `None`(無視)
/// - `id` が欠落・非数値の rpc メッセージも `None`(無視、応答しない)
/// - `method` が欠落・非文字列の場合も `None`(無視)。ここまでは仕様上の
///   「不正メッセージは無視」の範囲内で、`id` が取れない以上応答しようがない
///
/// `handle_rpc` 自体は同期関数だが、`SettingsStore` 経由のファイル I/O を
/// 行いうる(`plugins/list` はプラグイン数分のファイル読み取り)。この関数を
/// `client_loop` の `select!` 内で直接 await すると、その I/O が終わるまで
/// 同じループ反復がイベント配信アーム(`rx.recv()`)を一切ポーリングしなく
/// なり、他クライアントとワーカースレッドを共有している場合はイベント配信を
/// 遅延させてしまう。そのため実際の `handle_rpc` 呼び出しは
/// `tokio::task::spawn_blocking` に逃がし、ここでは `JoinHandle` を await
/// するだけにする。
async fn handle_client_message(state: &ServerState, text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("type").and_then(|v| v.as_str()) != Some("rpc") {
        return None;
    }
    let id = value.get("id")?.as_i64()?;
    let method = value.get("method")?.as_str()?.to_string();
    let params = value
        .get("params")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let registry = state.registry.clone();
    let drivers = state.drivers.clone();

    let result = tokio::task::spawn_blocking(move || {
        handle_rpc_with_drivers(registry.as_ref(), drivers.as_ref(), &method, &params)
    })
    .await;

    let response = match result {
        Ok(Ok(result)) => serde_json::json!({"type": "rpc-result", "id": id, "result": result}),
        Ok(Err(error)) => serde_json::json!({"type": "rpc-error", "id": id, "error": error}),
        Err(e) => {
            tracing::warn!("rpc handler task panicked: {e}");
            serde_json::json!({"type": "rpc-error", "id": id, "error": "internal error"})
        }
    };
    Some(response.to_string())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod origin_tests {
    use super::origin_allowed;

    #[test]
    fn allows_localhost_origins() {
        assert!(origin_allowed("http://localhost:5173"));
        assert!(origin_allowed("http://127.0.0.1:8137"));
        assert!(origin_allowed("http://[::1]:8137"));
    }

    #[test]
    fn allows_tauri_origins() {
        assert!(origin_allowed("tauri://localhost"));
        assert!(origin_allowed("http://tauri.localhost"));
        assert!(origin_allowed("https://tauri.localhost"));
    }

    #[test]
    fn rejects_other_origins() {
        assert!(!origin_allowed("https://evil.example"));
        assert!(!origin_allowed("http://localhost.evil.example"));
        assert!(!origin_allowed("not a uri"));
    }
}

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
    pub fn new(router: &Router, registry: Option<Registry>, drivers: Option<DriverRegistry>) -> Self {
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

    fn push(&self, json: String) {
        let json = Arc::new(json);
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
            Ok(capabilities_result_json(&manifest.capabilities, &grant_state))
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

/// `get-capabilities`/`set-capabilities` の result と `plugins/list` の各要素の
/// `capabilities` フィールドに使う共通の JSON 形: `{ requests, granted, staleGrant }`。
fn capabilities_result_json(
    requests: &[crate::plugin::CapabilityRequest],
    grant_state: &crate::plugin::GrantState,
) -> serde_json::Value {
    serde_json::json!({
        "requests": requests,
        "granted": grant_state.granted,
        "staleGrant": grant_state.stale,
    })
}

/// `plugins/set-bus-grant` の result と `plugins/list` の各要素の `bus`
/// フィールドに使う共通の JSON 形: `{ "bus": [...] }`(`sidecars_result_json`/
/// `filesystem_result_json` と同じ流儀 -- 1 件だけの grant state ではなく、
/// その plugin の bus 接続一覧全体を返す)。
///
/// `resolved` は渡された `bus: &[BusInfo]` の `BusInfo::resolved`
/// (`Registry::build_bus_infos` が `Registry` 自身の保持する
/// `DriverRegistry` から計算したもの)をそのまま使う -- 以前はここで
/// `ServerState` の `DriverRegistry` から独立に再計算していたが、それは
/// 同じ判定ロジックの二重管理になり、将来どちらか片方だけ直した変更が
/// サイレントに食い違ってしまう(コードレビュー指摘)。
fn bus_result_json(bus: &[crate::plugin::registry::BusInfo]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = bus
        .iter()
        .map(|info| {
            serde_json::json!({
                "driver": info.request.driver,
                "publish": info.request.publish,
                "subscribe": info.request.subscribe,
                "reason": info.request.reason,
                "granted": info.grant.granted,
                "staleGrant": info.grant.stale,
                "resolved": info.resolved,
            })
        })
        .collect();
    serde_json::json!({ "bus": items })
}

/// `params` から `key` の文字列値を取り出す。無い・文字列でない場合は
/// `Err`(RPC 層の流儀どおり panic しない)。
fn param_str<'a>(params: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("params.{key} must be a string"))
}

/// `get-sidecars` / `set-sidecar-*` / `sidecar-control` の共通 result 形と、
/// `plugins/list` の各要素の `sidecars` フィールドに使う JSON。
fn sidecars_result_json(sidecars: &[crate::plugin::SidecarInfo]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = sidecars
        .iter()
        .map(|info| {
            serde_json::json!({
                "name": info.request.name,
                "reason": info.request.reason,
                "args": info.request.args,
                "port": info.request.port,
                "scalable": info.request.scalable,
                "granted": info.grant.granted,
                "staleGrant": info.grant.stale,
                "config": info.config,
                "instances": info.instances.iter().map(|instance| serde_json::json!({
                    "index": instance.index,
                    "port": instance.port,
                    "state": if instance.running { "running" } else { "exited" },
                    "exitCode": instance.exit_code,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({ "sidecars": items })
}

/// `get-filesystem` / `set-filesystem-*` の共通 result 形と、`plugins/list`
/// の各要素の `filesystem` フィールドに使う JSON: `{ "roots": [...] }`。
fn filesystem_result_json(roots: &[crate::plugin::FilesystemInfo]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = roots
        .iter()
        .map(|info| {
            serde_json::json!({
                "name": info.request.name,
                "reason": info.request.reason,
                "mode": info.request.mode.as_str(),
                "granted": info.grant.granted,
                "staleGrant": info.grant.stale,
                "config": info.config,
            })
        })
        .collect();
    serde_json::json!({ "roots": items })
}

pub fn app(state: ServerState, ui_dir: Option<PathBuf>) -> axum::Router {
    let mut app = axum::Router::new()
        .route("/ws", get(ws_handler))
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
mod tests {
    use super::*;

    #[test]
    fn ws_json_carries_replay_for_journal_events_and_never_for_status() {
        let journal = Event::Journal {
            timestamp: "2026-07-27T12:00:00Z".into(),
            event: "FSDJump".into(),
            raw: serde_json::json!({"event": "FSDJump"}),
            replay: true,
        };
        let parsed: serde_json::Value = serde_json::from_str(&event_to_ws_json(&journal)).unwrap();
        assert_eq!(parsed["replay"], serde_json::json!(true));

        let status = Event::Status {
            raw: serde_json::json!({"Flags": 1}),
        };
        let parsed: serde_json::Value = serde_json::from_str(&event_to_ws_json(&status)).unwrap();
        assert_eq!(
            parsed.get("replay"),
            None,
            "status is a snapshot of the present; it has no replay notion"
        );
    }

    #[test]
    fn drivers_list_returns_the_dir_and_the_topics() {
        let (registry, drivers) = test_registries();
        let result = handle_rpc_with_drivers(
            Some(&registry),
            Some(&drivers),
            "drivers/list",
            &serde_json::json!({}),
        )
        .unwrap();
        assert!(result["driversDir"].is_string());
        assert_eq!(result["drivers"][0]["id"], "ed-state");
        assert_eq!(result["drivers"][0]["topics"][0]["name"], "current-system");
        assert_eq!(result["drivers"][0]["topics"][0]["retain"], true);
    }

    /// Regression test for a Minor review finding: `drivers/list` used to
    /// collapse `DriverState::Disabled { reason }` down to the bare string
    /// `"disabled"`, dropping `reason` entirely -- unlike `plugins/list`,
    /// which has always carried it. `ui/frontend/src/types/plugin.ts`
    /// declares `reason?: string` and `Drivers.tsx` renders
    /// `無効: {driver.reason}`, so a driver that failed to load showed a bare
    /// "無効" with no diagnostic. `drivers/list` must now carry `reason` too,
    /// mirroring `plugins/list`.
    #[test]
    fn drivers_list_carries_the_disabled_reason() {
        let bus = edlr_driver_channel::Bus::new();
        let drivers = crate::driver::registry::tests::test_registry_without_ed_state(bus);
        drivers.push(crate::driver::registry::DriverEntry {
            manifest: crate::driver::manifest::DriverManifest {
                id: "broken-driver".into(),
                name: "Broken Driver".into(),
                version: "0.1.0".into(),
                description: String::new(),
                entry: "driver.wasm".into(),
                topics: Vec::new(),
                settings: Vec::new(),
                capabilities: Vec::new(),
                sidecars: Vec::new(),
                filesystem: Vec::new(),
            },
            state: crate::driver::DriverState::Disabled {
                reason: "init() failed: boom".to_string(),
            },
            settings_json: std::sync::Arc::new(std::sync::Mutex::new("{}".to_string())),
            capabilities_json: std::sync::Arc::new(std::sync::Mutex::new(
                r#"{"hosts":[]}"#.to_string(),
            )),
            sidecars_json: std::sync::Arc::new(std::sync::Mutex::new("[]".to_string())),
            filesystem_json: std::sync::Arc::new(std::sync::Mutex::new("[]".to_string())),
        });

        let result = handle_drivers_rpc(&drivers, "list", &serde_json::json!({})).unwrap();
        let entry = &result["drivers"][0];
        assert_eq!(entry["state"], "disabled");
        assert_eq!(
            entry["reason"], "init() failed: boom",
            "drivers/list must carry the disabled reason like plugins/list does"
        );
    }

    #[test]
    fn drivers_rpc_without_a_driver_registry_reports_unavailable() {
        let (registry, _drivers) = test_registries();
        let err = handle_rpc_with_drivers(
            Some(&registry),
            None,
            "drivers/list",
            &serde_json::json!({}),
        )
        .unwrap_err();
        assert_eq!(err, "drivers unavailable");
    }

    #[test]
    fn plugins_rpc_without_a_registry_reports_unavailable_even_with_drivers_present() {
        // 逆方向のガード: `plugins/*` は `registry` の有無だけを見る必要が
        // あり、`drivers` が `Some` であることに引きずられて誤って通しては
        // ならない。
        let (_registry, drivers) = test_registries();
        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "plugins/list",
            &serde_json::json!({}),
        )
        .unwrap_err();
        assert_eq!(err, "plugins unavailable");
    }

    #[test]
    fn plugins_list_includes_bus_requests_with_their_resolution() {
        let (registry, drivers) = test_registries();
        let result = handle_rpc_with_drivers(
            Some(&registry),
            Some(&drivers),
            "plugins/list",
            &serde_json::json!({}),
        )
        .unwrap();
        let bus = &result["plugins"][0]["bus"][0];
        assert_eq!(bus["driver"], "ed-state");
        assert_eq!(bus["publish"][0], "ship-status");
        assert_eq!(bus["subscribe"][0], "current-system");
        assert_eq!(bus["granted"], false);
        assert_eq!(bus["resolved"], true);
    }

    /// 上のテストの裏付け: `registry` 自身が保持する `DriverRegistry`
    /// (`test_registries()` が焼き込むものとは別に、ここでは `ed-state` を
    /// 一切登録していないものを使う)に一致するドライバが無ければ、
    /// `resolved` は `false` に落ちる。`resolved` の計算
    /// (`crate::plugin::registry::Registry::build_bus_infos`、
    /// `bus_result_json` はそれをそのまま JSON にするだけ)自体が効いている
    /// ことを、true/false 両方を**別々の `Registry` インスタンス**(=
    /// 別々の `DriverRegistry` を焼き込んだもの)で示すことで確認する
    /// (常に `true` を返す実装でも通ってしまう単一ケースを避ける)。
    #[test]
    fn plugins_list_reports_unresolved_when_the_driver_is_not_installed() {
        let empty_drivers = crate::driver::registry::tests::test_registry_without_ed_state(
            edlr_driver_channel::Bus::new(),
        );
        let registry =
            crate::plugin::registry::tests::test_registry_with_bus_request_using(empty_drivers);
        let result = handle_rpc_with_drivers(
            Some(&registry),
            None,
            "plugins/list",
            &serde_json::json!({}),
        )
        .unwrap();
        let bus = &result["plugins"][0]["bus"][0];
        assert_eq!(bus["driver"], "ed-state");
        assert_eq!(bus["resolved"], false);
    }

    /// `translator`(`[[bus]]` を 1 件持つ)と `ed-state`(`current-system` を
    /// retain 付きで宣言)をそれぞれ 1 件だけ載せたレジストリの組。**プラグイン
    /// 側の `Registry` は返す `DriverRegistry` の `clone()` をそのままコンス
    /// トラクタに焼き込む**(`edlr.rs` の本番配線 -- 同じ `DriverRegistry` を
    /// `start_plugins` と `ServerState::new` の両方に配る -- を模したもの)。
    /// これにより `registry.bus(id)` の `resolved`(`Registry` 自身の
    /// `DriverRegistry` から計算)がこのテストファイルにも見える、単一の
    /// 情報源になる。プラグイン側は Task 10 の
    /// `test_registry_with_bus_request_using`、ドライバ側は Task 9 の
    /// `test_registry` を再利用する(どちらも wasm をロードせず `push` で
    /// 組み立てる)。
    fn test_registries() -> (Registry, DriverRegistry) {
        let drivers = crate::driver::registry::tests::test_registry(edlr_driver_channel::Bus::new());
        let registry =
            crate::plugin::registry::tests::test_registry_with_bus_request_using(drivers.clone());
        (registry, drivers)
    }

    #[test]
    fn set_bus_grant_requires_a_plugin_and_a_driver() {
        let (registry, drivers) = test_registries();
        assert!(handle_rpc_with_drivers(
            Some(&registry),
            Some(&drivers),
            "plugins/set-bus-grant",
            &serde_json::json!({"plugin": "translator"})
        )
        .is_err());
    }

    /// `plugins/set-bus-grant` は(`plugins/set-sidecar-grant`/
    /// `plugins/set-filesystem-grant` と同じ流儀で)1 件だけの grant state
    /// ではなく、その plugin の `bus[]` 一覧全体を返す。これにより UI は
    /// 1 往復でリスト全体を更新できる -- 呼び出し側でこの応答だけを見て
    /// 承認結果を判断できることを、`plugins/list` を経由せず確認する。
    #[test]
    fn set_bus_grant_returns_the_full_bus_array_for_that_plugin() {
        let (registry, drivers) = test_registries();
        let result = handle_rpc_with_drivers(
            Some(&registry),
            Some(&drivers),
            "plugins/set-bus-grant",
            &serde_json::json!({"plugin": "translator", "driver": "ed-state", "granted": true}),
        )
        .unwrap();
        assert_eq!(result["bus"][0]["driver"], "ed-state");
        assert_eq!(result["bus"][0]["granted"], true);
        assert_eq!(result["bus"][0]["resolved"], true);

        // 二重チェック: `plugins/list` を経由しても同じ承認状態が見える
        // (`Registry::set_bus_grant` が実際に永続化していることの裏付け)。
        let listed = handle_rpc_with_drivers(
            Some(&registry),
            Some(&drivers),
            "plugins/list",
            &serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(listed["plugins"][0]["bus"][0]["granted"], true);
    }

    #[test]
    fn drivers_get_and_set_settings_round_trip() {
        let (_registry, drivers) = test_registries();
        let updated = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-settings",
            &serde_json::json!({"driver": "ed-state", "values": {}}),
        )
        .unwrap();
        assert!(updated.is_object());

        let fetched = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/get-settings",
            &serde_json::json!({"driver": "ed-state"}),
        )
        .unwrap();
        assert!(fetched.is_object());
    }

    /// `ed-state` の fixture(`crate::driver::registry::tests::test_registry`)
    /// は http capability を 1 件宣言している(この review finding が入る
    /// までは宣言しておらず、`GrantsStore::set` の
    /// `capabilities_fingerprint` が常に `None` を返すため `granted` は
    /// 要求してもいつも `false` になり、承認の可否ではなく応答の形しか
    /// 確認できなかった)。ここでは承認が実際に切り替わり、`drivers/list`
    /// (`DriverRegistry::list` 経由、`set-capabilities` とは別の読み出し
    /// 経路)からも同じ状態が見える == ディスクに永続化されていることを
    /// 確認する。
    #[test]
    fn drivers_set_capabilities_persists_the_grant() {
        let (_registry, drivers) = test_registries();

        let granted = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-capabilities",
            &serde_json::json!({"driver": "ed-state", "granted": true}),
        )
        .unwrap();
        assert_eq!(granted["granted"], true);
        assert_eq!(granted["requests"][0]["kind"], "http");
        assert_eq!(granted["staleGrant"], false);

        let listed = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/list",
            &serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(listed["drivers"][0]["capabilities"]["granted"], true);

        let revoked = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-capabilities",
            &serde_json::json!({"driver": "ed-state", "granted": false}),
        )
        .unwrap();
        assert_eq!(revoked["granted"], false);

        let listed_again = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/list",
            &serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(listed_again["drivers"][0]["capabilities"]["granted"], false);
    }

    /// `drivers/set-sidecar-config` requires `driver`, `name`, and `config`
    /// (matching `plugins/set-sidecar-config`'s param names exactly, with
    /// `driver` in place of `plugin`). Missing any one of them must fail
    /// with the exact same wording `param_str`/the inline `config` check
    /// produce for the plugin arm.
    #[test]
    fn drivers_set_sidecar_config_requires_driver_name_and_config() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-config",
            &serde_json::json!({"name": "engine", "config": {"command": "/bin/sh"}}),
        )
        .unwrap_err();
        assert_eq!(err, "params.driver must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-config",
            &serde_json::json!({"driver": "voice", "config": {"command": "/bin/sh"}}),
        )
        .unwrap_err();
        assert_eq!(err, "params.name must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-config",
            &serde_json::json!({"driver": "voice", "name": "engine"}),
        )
        .unwrap_err();
        assert_eq!(err, "params.config must be an object");
    }

    #[test]
    fn drivers_set_sidecar_grant_requires_driver_name_and_granted() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-grant",
            &serde_json::json!({"name": "engine", "granted": true}),
        )
        .unwrap_err();
        assert_eq!(err, "params.driver must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-grant",
            &serde_json::json!({"driver": "voice", "granted": true}),
        )
        .unwrap_err();
        assert_eq!(err, "params.name must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-grant",
            &serde_json::json!({"driver": "voice", "name": "engine"}),
        )
        .unwrap_err();
        assert_eq!(err, "params.granted must be a bool");
    }

    #[test]
    fn drivers_sidecar_control_requires_driver_name_and_action() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/sidecar-control",
            &serde_json::json!({"name": "engine", "action": "stop"}),
        )
        .unwrap_err();
        assert_eq!(err, "params.driver must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/sidecar-control",
            &serde_json::json!({"driver": "voice", "action": "stop"}),
        )
        .unwrap_err();
        assert_eq!(err, "params.name must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/sidecar-control",
            &serde_json::json!({"driver": "voice", "name": "engine"}),
        )
        .unwrap_err();
        assert_eq!(err, "params.action must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/sidecar-control",
            &serde_json::json!({"driver": "voice", "name": "engine", "action": "jump"}),
        )
        .unwrap_err();
        assert_eq!(err, "unknown action: jump");
    }

    #[test]
    fn drivers_set_filesystem_config_requires_driver_name_and_config() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-filesystem-config",
            &serde_json::json!({"name": "cache", "config": {"path": "/tmp"}}),
        )
        .unwrap_err();
        assert_eq!(err, "params.driver must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-filesystem-config",
            &serde_json::json!({"driver": "voice", "config": {"path": "/tmp"}}),
        )
        .unwrap_err();
        assert_eq!(err, "params.name must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-filesystem-config",
            &serde_json::json!({"driver": "voice", "name": "cache"}),
        )
        .unwrap_err();
        assert_eq!(err, "params.config must be an object");
    }

    #[test]
    fn drivers_set_filesystem_grant_requires_driver_name_and_granted() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-filesystem-grant",
            &serde_json::json!({"name": "cache", "granted": true}),
        )
        .unwrap_err();
        assert_eq!(err, "params.driver must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-filesystem-grant",
            &serde_json::json!({"driver": "voice", "granted": true}),
        )
        .unwrap_err();
        assert_eq!(err, "params.name must be a string");

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-filesystem-grant",
            &serde_json::json!({"driver": "voice", "name": "cache"}),
        )
        .unwrap_err();
        assert_eq!(err, "params.granted must be a bool");
    }

    /// `drivers/set-sidecar-grant` must actually flip the approval and
    /// return it in the refreshed sidecar array (not just accept the call).
    /// Configures a real executable first (`drivers/set-sidecar-config`, the
    /// same round-trip the UI performs), then grants and checks the
    /// response array directly -- this is the RPC-level counterpart to
    /// `crate::driver::registry::tests::set_sidecar_config_and_grant_update_the_shared_sidecars_buffer`,
    /// which checks the underlying shared buffer.
    #[test]
    fn drivers_set_sidecar_grant_persists_and_returns_the_full_sidecar_array() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();

        handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-config",
            &serde_json::json!({
                "driver": "voice",
                "name": "engine",
                "config": {"command": "/bin/sh", "args": ["-c", "sleep 30"], "port": 51500, "replicas": 1},
            }),
        )
        .unwrap();

        let result = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-grant",
            &serde_json::json!({"driver": "voice", "name": "engine", "granted": true}),
        )
        .unwrap();
        assert_eq!(result["sidecars"][0]["name"], "engine");
        assert_eq!(result["sidecars"][0]["granted"], true);
        assert_eq!(result["sidecars"][0]["config"]["command"], "/bin/sh");
    }

    /// `drivers/set-filesystem-grant` must refuse to approve a root that has
    /// no directory configured, with the exact error the registry produces
    /// (`RegistryError::Filesystem`'s message) -- this is the negative-test
    /// the task brief singles out: pin the specific wording so the test
    /// cannot pass merely because some unrelated validation (e.g. an
    /// undeclared root) rejected the call first.
    #[test]
    fn drivers_set_filesystem_grant_rejects_granting_without_a_configured_path() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-filesystem-grant",
            &serde_json::json!({"driver": "voice", "name": "cache", "granted": true}),
        )
        .unwrap_err();
        assert_eq!(
            err,
            "filesystem root cache has no directory configured; cannot grant"
        );
    }

    /// `drivers/sidecar-control` `start` must refuse to launch a sidecar
    /// that has never been granted, even once a `command` is configured.
    #[test]
    fn drivers_sidecar_control_rejects_starting_an_ungranted_sidecar() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();

        handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-config",
            &serde_json::json!({
                "driver": "voice",
                "name": "engine",
                "config": {"command": "/bin/sh", "args": ["-c", "sleep 30"], "port": 51500, "replicas": 1},
            }),
        )
        .unwrap();

        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/sidecar-control",
            &serde_json::json!({"driver": "voice", "name": "engine", "action": "start"}),
        )
        .unwrap_err();
        assert_eq!(err, "sidecar engine is not granted");
    }

    // Regression coverage for a review finding: the five new `drivers/*`
    // sidecar/filesystem arms used to surface an unregistered driver id as
    // `RegistryError::UnknownPlugin` ("unknown plugin: {id}"), while the
    // pre-existing `drivers/set-capabilities` arm (via
    // `DriverRegistryError::UnknownDriver`) already says "unknown driver:
    // {id}" for the identical failure. Nothing exercised the unknown-driver
    // path for the five new arms, so the inconsistency went uncaught. Each
    // of the five gets its own test pinning the exact wording, one per arm
    // (rather than one combined test) so a future regression on any single
    // arm fails with an unambiguous test name.

    #[test]
    fn drivers_set_sidecar_config_reports_unknown_driver() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();
        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-config",
            &serde_json::json!({
                "driver": "not-a-driver",
                "name": "engine",
                "config": {"command": "/bin/sh", "args": [], "port": 51500, "replicas": 1},
            }),
        )
        .unwrap_err();
        assert_eq!(err, "unknown driver: not-a-driver");
    }

    #[test]
    fn drivers_set_sidecar_grant_reports_unknown_driver() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();
        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-sidecar-grant",
            &serde_json::json!({"driver": "not-a-driver", "name": "engine", "granted": true}),
        )
        .unwrap_err();
        assert_eq!(err, "unknown driver: not-a-driver");
    }

    #[test]
    fn drivers_sidecar_control_reports_unknown_driver() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();
        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/sidecar-control",
            &serde_json::json!({"driver": "not-a-driver", "name": "engine", "action": "start"}),
        )
        .unwrap_err();
        assert_eq!(err, "unknown driver: not-a-driver");
    }

    #[test]
    fn drivers_set_filesystem_config_reports_unknown_driver() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();
        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-filesystem-config",
            &serde_json::json!({
                "driver": "not-a-driver",
                "name": "cache",
                "config": {"path": "/tmp"},
            }),
        )
        .unwrap_err();
        assert_eq!(err, "unknown driver: not-a-driver");
    }

    #[test]
    fn drivers_set_filesystem_grant_reports_unknown_driver() {
        let (drivers, _tmp) = crate::driver::registry::tests::test_registry_with_sidecar_and_filesystem();
        let err = handle_rpc_with_drivers(
            None,
            Some(&drivers),
            "drivers/set-filesystem-grant",
            &serde_json::json!({"driver": "not-a-driver", "name": "cache", "granted": true}),
        )
        .unwrap_err();
        assert_eq!(err, "unknown driver: not-a-driver");
    }
}

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

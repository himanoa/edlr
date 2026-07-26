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
        } => serde_json::json!({
            "type": "event", "kind": "journal",
            "timestamp": timestamp, "event": event, "raw": raw,
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
}

struct ReplayBuffer {
    buf: VecDeque<Arc<String>>,
    tx: broadcast::Sender<Arc<String>>,
}

impl ServerState {
    pub fn new(router: &Router, registry: Option<Registry>) -> Self {
        let (tx, _) = broadcast::channel(256);
        let state = Self {
            inner: Arc::new(Mutex::new(ReplayBuffer {
                buf: VecDeque::new(),
                tx,
            })),
            registry,
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
///
/// `registry` が `None` の場合(プラグインホスト起動失敗などで
/// `ServerState` に `Registry` が渡されなかった場合)はどのメソッドも
/// `Err("plugins unavailable")` を返す。
pub fn handle_rpc(
    registry: Option<&Registry>,
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
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
        other => Err(format!("unknown method: {other}")),
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

    let result =
        tokio::task::spawn_blocking(move || handle_rpc(registry.as_ref(), &method, &params)).await;

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

//! axum/WS の配線(命令的モジュール)。
//!
//! `rpc/` を呼んで JSON を組み立てるだけの薄い層に保つ -- RPC の解釈・整形
//! ロジック自体は持たない。

use crate::event::Event;
use crate::registry::driver::DriverRegistry;
use crate::registry::plugin::Registry;
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

/// bus emit 1 件を WS フレーム JSON にする。UI は既に全ログ・全 journal
/// イベントを見られる承認サーフェスなので、bus フレームも全クライアントへ
/// 流す(設計書の承認モデル参照)。payload は UTF-8 文字列(lossy)。
pub fn bus_ws_frame(driver: &str, topic: &str, payload: &[u8]) -> String {
    serde_json::json!({
        "type": "event",
        "kind": "bus",
        "driver": driver,
        "topic": topic,
        "payload": String::from_utf8_lossy(payload),
    })
    .to_string()
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
        "plugins/list" => rpc_plugins::list(registry, params),
        "plugins/set-bus-grant" => rpc_plugins::set_bus_grant(registry, params),
        "plugins/set-dashboard-grant" => rpc_plugins::set_dashboard_grant(registry, params),
        "plugins/dashboard-action" => rpc_plugins::dashboard_action(registry, params),
        "dashboard/list" => rpc_plugins::dashboard_list(registry, params),
        "plugins/get-settings" => rpc_plugins::get_settings(registry, params),
        "plugins/set-settings" => rpc_plugins::set_settings(registry, params),
        "plugins/get-capabilities" => rpc_plugins::get_capabilities(registry, params),
        "plugins/set-capabilities" => rpc_plugins::set_capabilities(registry, params),
        "plugins/get-sidecars" => rpc_plugins::get_sidecars(registry, params),
        "plugins/set-sidecar-config" => rpc_plugins::set_sidecar_config(registry, params),
        "plugins/set-sidecar-grant" => rpc_plugins::set_sidecar_grant(registry, params),
        "plugins/sidecar-control" => rpc_plugins::sidecar_control(registry, params),
        "plugins/get-filesystem" => rpc_plugins::get_filesystem(registry, params),
        "plugins/set-filesystem-config" => rpc_plugins::set_filesystem_config(registry, params),
        "plugins/set-filesystem-grant" => rpc_plugins::set_filesystem_grant(registry, params),
        other => Err(format!("unknown method: {other}")),
    }
}

/// `drivers/*` RPC メソッドを処理する。呼び出し元([`handle_rpc_with_drivers`])
/// が `drivers/` プレフィックスを剥がして渡す(`method` はプレフィックス無し)。
/// `crate::registry::plugin::Registry` の `plugins/*` 系(`get-settings`/
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
        "list" => rpc_drivers::list(drivers, params),
        "bus-retained" => rpc_drivers::bus_retained(drivers, params),
        "get-settings" => rpc_drivers::get_settings(drivers, params),
        "set-settings" => rpc_drivers::set_settings(drivers, params),
        "set-capabilities" => rpc_drivers::set_capabilities(drivers, params),
        "set-sidecar-config" => rpc_drivers::set_sidecar_config(drivers, params),
        "set-sidecar-grant" => rpc_drivers::set_sidecar_grant(drivers, params),
        "sidecar-control" => rpc_drivers::sidecar_control(drivers, params),
        "set-filesystem-config" => rpc_drivers::set_filesystem_config(drivers, params),
        "set-filesystem-grant" => rpc_drivers::set_filesystem_grant(drivers, params),
        other => Err(format!("unknown method: drivers/{other}")),
    }
}

mod rpc_drivers;
mod rpc_plugins;

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
            .map_err(|_| crate::registry::plugin::RegistryError::UnknownDashboard(widget))
    })
    .await;
    match result {
        Ok(Ok((bytes, content_type))) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type.to_string()),
                // ウィジェットはホストページ(tauri://localhost や vite dev の
                // localhost:5173)から cross-origin の dynamic import で読み込む。
                // module 読み込みは CORS 必須なので許可を明示する。プラグインは
                // 信頼済みインストール物で、配信自体が grant 検証済み(未 grant は
                // 404)なので `*` でよい。
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".to_string()),
            ],
            bytes,
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

pub fn app(state: ServerState, ui_dir: Option<PathBuf>) -> axum::Router {
    let mut app = axum::Router::new()
        .route("/ws", get(ws_handler))
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

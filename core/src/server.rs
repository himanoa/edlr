use crate::event::Event;
use crate::router::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
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
}

struct ReplayBuffer {
    buf: VecDeque<Arc<String>>,
    tx: broadcast::Sender<Arc<String>>,
}

impl ServerState {
    pub fn new(router: &Router) -> Self {
        let (tx, _) = broadcast::channel(256);
        let state = Self {
            inner: Arc::new(Mutex::new(ReplayBuffer {
                buf: VecDeque::new(),
                tx,
            })),
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

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<ServerState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| client_loop(socket, state))
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
                // クライアント→サーバは将来の RPC 用に予約。現状は無視する
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

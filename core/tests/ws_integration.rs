use edlr_core::event::Event;
use edlr_core::router::Router;
use edlr_core::server::{self, ServerState};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn setup(ui_dir: Option<std::path::PathBuf>) -> (Router, SocketAddr) {
    let router = Router::new(64);
    let state = ServerState::new(&router, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server::serve(listener, state, ui_dir));
    (router, addr)
}

async fn connect(addr: SocketAddr) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("connect");
    ws
}

async fn recv_json(ws: &mut Ws) -> serde_json::Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timeout")
            .expect("stream ended")
            .expect("ws error");
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).expect("valid json");
        }
    }
}

fn journal(name: &str) -> Event {
    Event::Journal {
        timestamp: "2026-07-25T12:00:00Z".into(),
        event: name.into(),
        raw: serde_json::json!({"timestamp": "2026-07-25T12:00:00Z", "event": name}),
        replay: false,
    }
}

#[tokio::test]
async fn sends_hello_then_live_events() {
    let (router, addr) = setup(None).await;
    let mut ws = connect(addr).await;
    let hello = recv_json(&mut ws).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["protocol"], 1);
    router.publish(journal("FSDJump"));
    let ev = recv_json(&mut ws).await;
    assert_eq!(ev["type"], "event");
    assert_eq!(ev["kind"], "journal");
    assert_eq!(ev["event"], "FSDJump");
    assert_eq!(ev["raw"]["event"], "FSDJump");
    router.publish(Event::Status {
        raw: serde_json::json!({"Flags": 5}),
    });
    let ev = recv_json(&mut ws).await;
    assert_eq!(ev["kind"], "status");
    assert_eq!(ev["raw"]["Flags"], 5);
}

#[tokio::test]
async fn replays_buffered_events_to_new_connection() {
    let (router, addr) = setup(None).await;
    router.publish(journal("LoadGame"));
    router.publish(journal("Location"));
    // feeder かクライアント購読のどちらかが必ず拾う設計なので sleep 不要
    let mut ws = connect(addr).await;
    assert_eq!(recv_json(&mut ws).await["type"], "hello");
    assert_eq!(recv_json(&mut ws).await["event"], "LoadGame");
    assert_eq!(recv_json(&mut ws).await["event"], "Location");
}

#[tokio::test]
async fn client_messages_are_ignored_and_connection_survives() {
    let (router, addr) = setup(None).await;
    let mut ws = connect(addr).await;
    assert_eq!(recv_json(&mut ws).await["type"], "hello");
    ws.send(Message::Text("{\"type\":\"rpc\"}".into()))
        .await
        .unwrap();
    router.publish(journal("Music"));
    assert_eq!(recv_json(&mut ws).await["event"], "Music");
}

#[tokio::test]
async fn multiple_clients_receive_the_same_events() {
    let (router, addr) = setup(None).await;
    let mut a = connect(addr).await;
    let mut b = connect(addr).await;
    assert_eq!(recv_json(&mut a).await["type"], "hello");
    assert_eq!(recv_json(&mut b).await["type"], "hello");
    router.publish(journal("Docked"));
    assert_eq!(recv_json(&mut a).await["event"], "Docked");
    assert_eq!(recv_json(&mut b).await["event"], "Docked");
}

#[tokio::test]
async fn rejects_connection_with_disallowed_origin() {
    let (_router, addr) = setup(None).await;
    let mut req = format!("ws://{addr}/ws").into_client_request().unwrap();
    req.headers_mut().insert(
        "Origin",
        "https://evil.example".parse().expect("valid header value"),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("connection with disallowed Origin must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("403") || msg.contains("Forbidden"),
        "expected a 403 rejection, got: {msg}"
    );
}

async fn http_get(addr: SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).await.unwrap();
    buf
}

#[tokio::test]
async fn serves_static_files_with_spa_fallback() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<html>edlr-ui</html>").unwrap();
    std::fs::write(dir.path().join("app.js"), "console.log(1)").unwrap();
    let (_router, addr) = setup(Some(dir.path().to_path_buf())).await;
    let js = http_get(addr, "/app.js").await;
    assert!(js.contains("console.log(1)"), "got: {js}");
    let fallback = http_get(addr, "/logs").await;
    assert!(
        fallback.contains("edlr-ui"),
        "SPA fallback should serve index.html, got: {fallback}"
    );
}

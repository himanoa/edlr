use edlr_core::event::Event;
use edlr_core::plugin::grants::GrantsStore;
use edlr_core::plugin::host::PluginHost;
use edlr_core::plugin::runner::start_plugins;
use edlr_core::plugin::settings::SettingsStore;
use edlr_core::plugin::Registry;
use edlr_core::router::Router;
use edlr_core::server::{self, ServerState};
use futures_util::{SinkExt, StreamExt};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn hello_logger_wasm() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crate_path = manifest_dir
        .join("..")
        .join("examples/plugins/hello-logger");

    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2", "--release"])
        .current_dir(&crate_path)
        .status()
        .expect("failed to spawn cargo build for fixture plugin");
    assert!(status.success(), "hello-logger fixture build failed");

    crate_path
        .join("target/wasm32-wasip2/release")
        .join("hello_logger.wasm")
}

fn write_hello_logger(plugins_dir: &Path) {
    let dir = plugins_dir.join("hello-logger");
    fs::create_dir_all(&dir).unwrap();
    let wasm_src = hello_logger_wasm();
    fs::copy(&wasm_src, dir.join("hello_logger.wasm")).unwrap();
    fs::write(
        dir.join("manifest.toml"),
        r#"
id = "hello-logger"
name = "hello-logger"
version = "0.1.0"
entry = "hello_logger.wasm"
events = ["FSDJump"]

[[settings]]
key = "enabled"
label = "Enabled"
type = "boolean"
default = true
"#,
    )
    .unwrap();
}

/// Builds a `Registry` with a single running hello-logger plugin, under a
/// fresh tempdir for both plugins and settings storage.
fn hello_logger_registry() -> (tempfile::TempDir, Registry) {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    write_hello_logger(&plugins_dir);

    let settings_store = SettingsStore::new(tmp.path().join("settings"));
    let grants_store = GrantsStore::new(tmp.path().join("grants"));
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(&plugins_dir, settings_store, grants_store, &router, host);
    (tmp, registry)
}

async fn setup(registry: Option<Registry>) -> (Router, SocketAddr) {
    let router = Router::new(64);
    let state = ServerState::new(&router, registry);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server::serve(listener, state, None));
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

async fn recv_hello(ws: &mut Ws) {
    assert_eq!(recv_json(ws).await["type"], "hello");
}

async fn send_rpc(ws: &mut Ws, id: i64, method: &str, params: serde_json::Value) {
    let msg = serde_json::json!({
        "type": "rpc",
        "id": id,
        "method": method,
        "params": params,
    });
    ws.send(Message::Text(msg.to_string().into()))
        .await
        .unwrap();
}

fn journal(name: &str) -> Event {
    Event::Journal {
        timestamp: "2026-07-26T12:00:00Z".into(),
        event: name.into(),
        raw: serde_json::json!({"timestamp": "2026-07-26T12:00:00Z", "event": name}),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_list_returns_plugins_dir_and_running_plugin_with_settings_and_values() {
    let (_tmp, registry) = hello_logger_registry();
    let plugins_dir = registry.plugins_dir().to_path_buf();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(&mut ws, 1, "plugins/list", serde_json::json!({})).await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["id"], 1);
    assert_eq!(
        resp["result"]["pluginsDir"],
        plugins_dir.to_string_lossy().to_string()
    );
    let plugins = resp["result"]["plugins"].as_array().expect("plugins array");
    assert_eq!(plugins.len(), 1);
    let plugin = &plugins[0];
    assert_eq!(plugin["id"], "hello-logger");
    assert_eq!(plugin["state"], "running");
    assert!(plugin.get("reason").is_none());
    assert!(plugin["settings"].is_array());
    assert_eq!(plugin["values"]["enabled"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_get_settings_returns_values_and_errors_for_unknown_plugin() {
    let (_tmp, registry) = hello_logger_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(
        &mut ws,
        1,
        "plugins/get-settings",
        serde_json::json!({"plugin": "hello-logger"}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["result"]["enabled"], true);

    send_rpc(
        &mut ws,
        2,
        "plugins/get-settings",
        serde_json::json!({"plugin": "does-not-exist"}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-error");
    assert_eq!(resp["id"], 2);
    assert!(resp["error"].as_str().unwrap().contains("does-not-exist"));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_set_settings_persists_and_is_visible_to_subsequent_get() {
    let (_tmp, registry) = hello_logger_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(
        &mut ws,
        1,
        "plugins/set-settings",
        serde_json::json!({"plugin": "hello-logger", "values": {"enabled": false}}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["result"]["enabled"], false);

    send_rpc(
        &mut ws,
        2,
        "plugins/get-settings",
        serde_json::json!({"plugin": "hello-logger"}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["result"]["enabled"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_set_settings_with_unknown_key_errors_and_leaves_value_unchanged() {
    let (_tmp, registry) = hello_logger_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(
        &mut ws,
        1,
        "plugins/set-settings",
        serde_json::json!({"plugin": "hello-logger", "values": {"nope": 1}}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-error");
    assert_eq!(resp["id"], 1);

    send_rpc(
        &mut ws,
        2,
        "plugins/get-settings",
        serde_json::json!({"plugin": "hello-logger"}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["result"]["enabled"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_method_returns_rpc_error() {
    let (_tmp, registry) = hello_logger_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(&mut ws, 1, "plugins/nonsense", serde_json::json!({})).await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-error");
    assert_eq!(resp["id"], 1);
    assert!(resp["error"].as_str().unwrap().contains("plugins/nonsense"));
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_messages_are_ignored_and_connection_survives() {
    let (_tmp, registry) = hello_logger_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    ws.send(Message::Text("not json".into())).await.unwrap();
    ws.send(Message::Text("{\"type\":\"nonsense\"}".into()))
        .await
        .unwrap();
    // Missing/non-numeric id rpc messages must also be silently ignored.
    ws.send(Message::Text(
        "{\"type\":\"rpc\",\"method\":\"plugins/list\"}".into(),
    ))
    .await
    .unwrap();
    ws.send(Message::Text(
        "{\"type\":\"rpc\",\"id\":\"abc\",\"method\":\"plugins/list\"}".into(),
    ))
    .await
    .unwrap();

    send_rpc(&mut ws, 1, "plugins/list", serde_json::json!({})).await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["id"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_message_with_non_string_method_is_ignored_and_connection_survives() {
    let (_tmp, registry) = hello_logger_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    ws.send(Message::Text(
        "{\"type\":\"rpc\",\"id\":1,\"method\":123}".into(),
    ))
    .await
    .unwrap();

    send_rpc(&mut ws, 2, "plugins/list", serde_json::json!({})).await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["id"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_responses_and_events_are_multiplexed_on_the_same_socket() {
    let (_tmp, registry) = hello_logger_registry();
    let (router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    router.publish(journal("FSDJump"));
    send_rpc(&mut ws, 1, "plugins/list", serde_json::json!({})).await;
    router.publish(journal("Docked"));

    let mut saw_event_fsdjump = false;
    let mut saw_event_docked = false;
    let mut saw_rpc_result = false;
    for _ in 0..3 {
        let msg = recv_json(&mut ws).await;
        match msg["type"].as_str().unwrap() {
            "event" => {
                if msg["event"] == "FSDJump" {
                    saw_event_fsdjump = true;
                } else if msg["event"] == "Docked" {
                    saw_event_docked = true;
                }
            }
            "rpc-result" => {
                assert_eq!(msg["id"], 1);
                saw_rpc_result = true;
            }
            other => panic!("unexpected message type: {other}"),
        }
    }
    assert!(saw_event_fsdjump, "expected FSDJump event to be delivered");
    assert!(saw_event_docked, "expected Docked event to be delivered");
    assert!(saw_rpc_result, "expected rpc-result to be delivered");
}

#[tokio::test]
async fn plugins_list_errors_when_registry_is_none() {
    let (_router, addr) = setup(None).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(&mut ws, 1, "plugins/list", serde_json::json!({})).await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-error");
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["error"], "plugins unavailable");
}

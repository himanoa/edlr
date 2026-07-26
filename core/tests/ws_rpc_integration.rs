use edlr_core::event::Event;
use edlr_core::plugin::filesystem::FilesystemConfigStore;
use edlr_core::plugin::grants::GrantsStore;
use edlr_core::plugin::host::PluginHost;
use edlr_core::plugin::runner::start_plugins;
use edlr_core::plugin::settings::SettingsStore;
use edlr_core::plugin::sidecar::SidecarConfigStore;
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

mod support;

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
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(
        &plugins_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        &router,
        host,
    );
    (tmp, registry)
}

fn http_caller_wasm() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crate_path = manifest_dir.join("..").join("examples/plugins/http-caller");

    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2", "--release"])
        .current_dir(&crate_path)
        .status()
        .expect("failed to spawn cargo build for fixture plugin");
    assert!(status.success(), "http-caller fixture build failed");

    crate_path
        .join("target/wasm32-wasip2/release")
        .join("http_caller.wasm")
}

fn write_http_caller(plugins_dir: &Path) {
    let dir = plugins_dir.join("http-caller");
    fs::create_dir_all(&dir).unwrap();
    let wasm_src = http_caller_wasm();
    fs::copy(&wasm_src, dir.join("http_caller.wasm")).unwrap();
    fs::write(
        dir.join("manifest.toml"),
        r#"
id = "http-caller"
name = "http-caller"
version = "0.1.0"
entry = "http_caller.wasm"
events = ["FSDJump"]

[[capabilities]]
kind = "http"
hosts = ["https://api.example.com"]
reason = "test"
"#,
    )
    .unwrap();
}

/// Builds a `Registry` with a single running http-caller plugin (which
/// declares an http capability request), under a fresh tempdir for plugins,
/// settings, and grants storage.
fn http_caller_registry() -> (tempfile::TempDir, Registry) {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    write_http_caller(&plugins_dir);

    let settings_store = SettingsStore::new(tmp.path().join("settings"));
    let grants_store = GrantsStore::new(tmp.path().join("grants"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(
        &plugins_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        &router,
        host,
    );
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
        replay: false,
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

#[tokio::test(flavor = "multi_thread")]
async fn plugins_list_includes_capabilities_with_declared_requests_and_initial_grant_state() {
    let (_tmp, registry) = http_caller_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(&mut ws, 1, "plugins/list", serde_json::json!({})).await;
    let resp = recv_json(&mut ws).await;
    let plugins = resp["result"]["plugins"].as_array().expect("plugins array");
    let plugin = plugins
        .iter()
        .find(|p| p["id"] == "http-caller")
        .expect("http-caller plugin present");

    let capabilities = &plugin["capabilities"];
    assert_eq!(capabilities["granted"], false);
    assert_eq!(capabilities["staleGrant"], false);
    let requests = capabilities["requests"].as_array().expect("requests array");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["kind"], "http");
    assert_eq!(
        requests[0]["hosts"],
        serde_json::json!(["https://api.example.com"])
    );
    assert_eq!(requests[0]["reason"], "test");
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_list_reports_empty_requests_for_plugin_without_capabilities() {
    let (_tmp, registry) = hello_logger_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(&mut ws, 1, "plugins/list", serde_json::json!({})).await;
    let resp = recv_json(&mut ws).await;
    let plugins = resp["result"]["plugins"].as_array().expect("plugins array");
    let plugin = plugins
        .iter()
        .find(|p| p["id"] == "hello-logger")
        .expect("hello-logger plugin present");

    let capabilities = &plugin["capabilities"];
    assert_eq!(capabilities["granted"], false);
    assert_eq!(capabilities["staleGrant"], false);
    assert_eq!(
        capabilities["requests"]
            .as_array()
            .expect("requests array")
            .len(),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_get_capabilities_returns_requests_and_grant_state_and_errors_for_unknown_plugin() {
    let (_tmp, registry) = http_caller_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(
        &mut ws,
        1,
        "plugins/get-capabilities",
        serde_json::json!({"plugin": "http-caller"}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["result"]["granted"], false);
    assert_eq!(resp["result"]["staleGrant"], false);
    let requests = resp["result"]["requests"]
        .as_array()
        .expect("requests array");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["kind"], "http");

    send_rpc(
        &mut ws,
        2,
        "plugins/get-capabilities",
        serde_json::json!({"plugin": "does-not-exist"}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-error");
    assert_eq!(resp["id"], 2);
    assert!(resp["error"].as_str().unwrap().contains("does-not-exist"));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_set_capabilities_grants_and_revokes_and_is_reflected_in_get_and_list() {
    let (_tmp, registry) = http_caller_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(
        &mut ws,
        1,
        "plugins/set-capabilities",
        serde_json::json!({"plugin": "http-caller", "granted": true}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["result"]["granted"], true);
    assert_eq!(resp["result"]["staleGrant"], false);

    send_rpc(
        &mut ws,
        2,
        "plugins/get-capabilities",
        serde_json::json!({"plugin": "http-caller"}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["result"]["granted"], true);

    send_rpc(&mut ws, 3, "plugins/list", serde_json::json!({})).await;
    let resp = recv_json(&mut ws).await;
    let plugins = resp["result"]["plugins"].as_array().expect("plugins array");
    let plugin = plugins
        .iter()
        .find(|p| p["id"] == "http-caller")
        .expect("http-caller plugin present");
    assert_eq!(plugin["capabilities"]["granted"], true);

    // Revoke.
    send_rpc(
        &mut ws,
        4,
        "plugins/set-capabilities",
        serde_json::json!({"plugin": "http-caller", "granted": false}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["result"]["granted"], false);

    send_rpc(
        &mut ws,
        5,
        "plugins/get-capabilities",
        serde_json::json!({"plugin": "http-caller"}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["result"]["granted"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_set_capabilities_with_invalid_params_returns_rpc_error() {
    let (_tmp, registry) = http_caller_registry();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    // Missing plugin.
    send_rpc(
        &mut ws,
        1,
        "plugins/set-capabilities",
        serde_json::json!({"granted": true}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-error");
    assert_eq!(resp["id"], 1);

    // granted not a bool.
    send_rpc(
        &mut ws,
        2,
        "plugins/set-capabilities",
        serde_json::json!({"plugin": "http-caller", "granted": "yes"}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-error");
    assert_eq!(resp["id"], 2);

    // Missing granted.
    send_rpc(
        &mut ws,
        3,
        "plugins/set-capabilities",
        serde_json::json!({"plugin": "http-caller"}),
    )
    .await;
    let resp = recv_json(&mut ws).await;
    assert_eq!(resp["type"], "rpc-error");
    assert_eq!(resp["id"], 3);
}

/// WS 接続と RPC の往復をまとめたテスト用ヘルパ。`_sidecar_env` を保持する
/// のは、`Registry` が参照する `SidecarConfigStore`/`GrantsStore` の保存先
/// tempdir をテストの間ずっと生存させるため(drop すると保存先が消える)。
struct SidecarTestEnv {
    _sidecar_env: support::Env,
    ws: Ws,
    next_id: i64,
}

impl SidecarTestEnv {
    async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        send_rpc(&mut self.ws, id, method, params).await;
        let resp = recv_json(&mut self.ws).await;
        assert_eq!(resp["id"], id);
        match resp["type"].as_str().expect("type must be a string") {
            "rpc-result" => Ok(resp["result"].clone()),
            "rpc-error" => Err(resp["error"]
                .as_str()
                .expect("error must be a string")
                .to_string()),
            other => panic!("unexpected message type: {other}"),
        }
    }
}

/// `[[sidecar]]`(`name = "tts"`, `port = 50401`, `scalable = false`)を 1 件
/// 持つ `sc-plugin` の manifest を置いた plugins-dir でサーバを起動し、
/// 接続済みの `SidecarTestEnv` を返す。
async fn spawn_server_with_sidecar_plugin() -> SidecarTestEnv {
    let sidecar_env = support::sidecar_env("tts", 50401, false);
    let registry = sidecar_env.registry.clone();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;
    SidecarTestEnv {
        _sidecar_env: sidecar_env,
        ws,
        next_id: 0,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sidecar_rpc_round_trip() {
    let mut env = spawn_server_with_sidecar_plugin().await;

    let sidecars = env
        .call(
            "plugins/get-sidecars",
            serde_json::json!({"plugin": "sc-plugin"}),
        )
        .await
        .expect("get-sidecars should succeed");
    let list = sidecars["sidecars"].as_array().expect("sidecars array");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "tts");
    assert_eq!(list[0]["granted"], serde_json::json!(false));
    assert_eq!(list[0]["config"]["command"], serde_json::json!(""));

    let updated = env
        .call(
            "plugins/set-sidecar-config",
            serde_json::json!({
                "plugin": "sc-plugin",
                "name": "tts",
                "config": {"command": "/bin/sh", "args": ["-c", "sleep 30"], "port": 50401, "replicas": 1}
            }),
        )
        .await
        .expect("set-sidecar-config should succeed");
    assert_eq!(
        updated["sidecars"][0]["config"]["command"],
        serde_json::json!("/bin/sh")
    );

    let granted = env
        .call(
            "plugins/set-sidecar-grant",
            serde_json::json!({"plugin": "sc-plugin", "name": "tts", "granted": true}),
        )
        .await
        .expect("set-sidecar-grant should succeed");
    assert_eq!(granted["sidecars"][0]["granted"], serde_json::json!(true));

    let started = env
        .call(
            "plugins/sidecar-control",
            serde_json::json!({"plugin": "sc-plugin", "name": "tts", "action": "start"}),
        )
        .await
        .expect("start should succeed");
    assert_eq!(
        started["sidecars"][0]["instances"][0]["state"],
        serde_json::json!("running")
    );

    let stopped = env
        .call(
            "plugins/sidecar-control",
            serde_json::json!({"plugin": "sc-plugin", "name": "tts", "action": "stop"}),
        )
        .await
        .expect("stop should succeed");
    assert_eq!(
        stopped["sidecars"][0]["instances"][0]["state"],
        serde_json::json!("exited")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_sidecar_config_is_an_rpc_error_and_changes_nothing() {
    let mut env = spawn_server_with_sidecar_plugin().await;

    let err = env
        .call(
            "plugins/set-sidecar-config",
            serde_json::json!({
                "plugin": "sc-plugin",
                "name": "tts",
                // scalable = false のサイドカーに replicas > 1
                "config": {"command": "/bin/sh", "args": ["-c", "sleep 30"], "port": 50411, "replicas": 4}
            }),
        )
        .await
        .expect_err("invalid config must be rejected");
    assert!(err.contains("replicas"));

    let sidecars = env
        .call(
            "plugins/get-sidecars",
            serde_json::json!({"plugin": "sc-plugin"}),
        )
        .await
        .unwrap();
    assert_eq!(
        sidecars["sidecars"][0]["config"]["command"],
        serde_json::json!("")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_sidecar_name_is_an_rpc_error() {
    let mut env = spawn_server_with_sidecar_plugin().await;
    let err = env
        .call(
            "plugins/get-sidecars",
            serde_json::json!({"plugin": "nope"}),
        )
        .await
        .expect_err("unknown plugin must be rejected");
    assert!(err.contains("unknown plugin"));
}

/// `[[filesystem]]`(`name = "exports"`, `mode = "read-write"`)を 1 件持つ
/// `fs-plugin` の manifest を置いた plugins-dir でサーバを起動し、接続済みの
/// `SidecarTestEnv` を返す(`spawn_server_with_sidecar_plugin` と同じ流儀 --
/// この構造体は WS 接続と RPC 往復の足回りをまとめているだけで、
/// サイドカー固有ではない)。
async fn spawn_server_with_filesystem_plugin() -> SidecarTestEnv {
    let filesystem_env = support::filesystem_env("exports", "read-write");
    let registry = filesystem_env.registry.clone();
    let (_router, addr) = setup(Some(registry)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;
    SidecarTestEnv {
        _sidecar_env: filesystem_env,
        ws,
        next_id: 0,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn filesystem_rpc_round_trip() {
    let mut env = spawn_server_with_filesystem_plugin().await;

    let roots = env
        .call(
            "plugins/get-filesystem",
            serde_json::json!({"plugin": "fs-plugin"}),
        )
        .await
        .expect("get-filesystem should succeed");
    let list = roots["roots"].as_array().expect("roots array");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "exports");
    assert_eq!(list[0]["mode"], "read-write");
    assert_eq!(list[0]["granted"], serde_json::json!(false));
    assert_eq!(list[0]["config"]["path"], serde_json::json!(""));

    let dir = env._sidecar_env.tmp.path().join("exports");
    std::fs::create_dir(&dir).unwrap();
    let updated = env
        .call(
            "plugins/set-filesystem-config",
            serde_json::json!({
                "plugin": "fs-plugin",
                "name": "exports",
                "config": {"path": dir.to_string_lossy()}
            }),
        )
        .await
        .expect("set-filesystem-config should succeed");
    assert_eq!(
        updated["roots"][0]["config"]["path"],
        serde_json::json!(dir.to_string_lossy())
    );

    let granted = env
        .call(
            "plugins/set-filesystem-grant",
            serde_json::json!({"plugin": "fs-plugin", "name": "exports", "granted": true}),
        )
        .await
        .expect("set-filesystem-grant should succeed");
    assert_eq!(granted["roots"][0]["granted"], serde_json::json!(true));

    let revoked = env
        .call(
            "plugins/set-filesystem-grant",
            serde_json::json!({"plugin": "fs-plugin", "name": "exports", "granted": false}),
        )
        .await
        .expect("revoke should succeed");
    assert_eq!(revoked["roots"][0]["granted"], serde_json::json!(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn filesystem_grant_without_a_configured_directory_is_an_rpc_error() {
    let mut env = spawn_server_with_filesystem_plugin().await;

    let err = env
        .call(
            "plugins/set-filesystem-grant",
            serde_json::json!({"plugin": "fs-plugin", "name": "exports", "granted": true}),
        )
        .await
        .expect_err("granting without a configured directory must be rejected");
    assert!(err.contains("directory"));

    let roots = env
        .call(
            "plugins/get-filesystem",
            serde_json::json!({"plugin": "fs-plugin"}),
        )
        .await
        .unwrap();
    assert_eq!(
        roots["roots"][0]["granted"],
        serde_json::json!(false),
        "a rejected grant must not be persisted"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_plugin_is_an_rpc_error_for_filesystem_methods() {
    let mut env = spawn_server_with_filesystem_plugin().await;

    let err = env
        .call(
            "plugins/get-filesystem",
            serde_json::json!({"plugin": "nope"}),
        )
        .await
        .expect_err("unknown plugin must be rejected");
    assert!(err.contains("unknown plugin"));

    let err = env
        .call(
            "plugins/set-filesystem-config",
            serde_json::json!({
                "plugin": "nope",
                "name": "exports",
                "config": {"path": "/tmp"}
            }),
        )
        .await
        .expect_err("unknown plugin must be rejected");
    assert!(err.contains("unknown plugin"));

    let err = env
        .call(
            "plugins/set-filesystem-grant",
            serde_json::json!({"plugin": "nope", "name": "exports", "granted": true}),
        )
        .await
        .expect_err("unknown plugin must be rejected");
    assert!(err.contains("unknown plugin"));
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

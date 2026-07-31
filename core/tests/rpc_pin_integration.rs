//! Phase 2 の `server.rs` 分解に着手する前の pin テスト。
//!
//! 代表的な RPC 応答の**生 JSON 全体**を等値比較で固定する。分解後に応答の
//! 形が 1 フィールドでも変わればここが落ちる、というのが目的
//! (`.claude/skills/rpc-pin-tests/SKILL.md` 参照)。ハーネスは
//! `ws_rpc_integration.rs` のものをこのファイル内にコピーして使う(`core/tests/`
//! の各ファイルは独立クレートなので、既存ファイルへの追記は凍結違反になる)。

use edlr_core::host::driver::DriverHost;
use edlr_core::runner::driver::start_drivers;
use edlr_core::registry::driver::DriverRegistry;
use edlr_core::settings::filesystem::FilesystemConfigStore;
use edlr_core::capability::grants::GrantsStore;
use edlr_core::host::plugin::PluginHost;
use edlr_core::runner::plugin::start_plugins;
use edlr_core::schedule::store::ScheduleStore;
use edlr_core::settings::store::SettingsStore;
use edlr_core::settings::sidecar::SidecarConfigStore;
use edlr_core::registry::plugin::Registry;
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

async fn setup(registry: Option<Registry>, drivers: Option<DriverRegistry>) -> (Router, SocketAddr) {
    let router = Router::new(64);
    let state = ServerState::new(&router, registry, drivers);
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

/// `examples/plugins/http-caller` を `wasm32-wasip2` 向けにビルドし、できあがった
/// `.wasm` へのパスを返す(`ws_rpc_integration.rs` の `http_caller_wasm` と同じ)。
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

/// capability を宣言した http-caller プラグイン 1 件を持つ `Registry`
/// (`ws_rpc_integration.rs` の `http_caller_registry` と同じ流儀)。
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
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        support::empty_driver_registry(tmp.path()),
        host,
    );
    (tmp, registry)
}

/// `examples/drivers/ed-state` を `wasm32-wasip2` 向けにビルドし、できあがった
/// `.wasm` へのパスを返す(`support::valid_plugin_wasm` と同じ流儀)。
fn ed_state_driver_wasm() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crate_path = manifest_dir.join("..").join("examples/drivers/ed-state");

    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2", "--release"])
        .current_dir(&crate_path)
        .status()
        .expect("failed to spawn cargo build for the ed-state fixture driver");
    assert!(status.success(), "ed-state fixture driver build failed");

    crate_path
        .join("target/wasm32-wasip2/release")
        .join("ed_state.wasm")
}

fn write_ed_state_driver(drivers_dir: &Path) {
    let dir = drivers_dir.join("ed-state");
    fs::create_dir_all(&dir).unwrap();
    let wasm_src = ed_state_driver_wasm();
    fs::copy(&wasm_src, dir.join("driver.wasm")).unwrap();
    fs::write(
        dir.join("driver.toml"),
        r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
description = "受け取ったシステム名を retained トピックとして配る"
entry = "driver.wasm"

[[topics]]
name = "set-system"
retain = false
description = "プラグインからのシステム名の更新"

[[topics]]
name = "current-system"
retain = true
description = "現在のスターシステム"
"#,
    )
    .unwrap();
}

/// `examples/drivers/ed-state` を実際にビルドしてロードした `DriverRegistry`
/// (`core/src/server.rs` の `mod tests` にある `test_registries`/`test_registry`
/// と同型のフィクスチャを、公開 API だけで組み立てたもの)。
fn ed_state_driver_registry() -> (tempfile::TempDir, DriverRegistry) {
    let tmp = tempfile::tempdir().unwrap();
    let drivers_dir = tmp.path().join("drivers");
    fs::create_dir_all(&drivers_dir).unwrap();
    write_ed_state_driver(&drivers_dir);

    let settings_store = SettingsStore::new(tmp.path().join("driver-settings"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("driver-settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("driver-settings"), Vec::new());
    let grants_store = GrantsStore::new_for_drivers(tmp.path().join("driver-grants"));
    let host = DriverHost::new().expect("driver host should build");

    let drivers = start_drivers(
        &drivers_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        edlr_driver_channel::Bus::new(),
        host,
    );
    (tmp, drivers)
}

/// `examples/drivers/ed-state` の manifest に、secret 型 setting を1件
/// 足したもの(分析 §6 リスク4: driver の `values`/`set_values` は plugin と
/// 違い secret を剥がさない -- この挙動を固定するテストが今までなかった)。
fn write_secret_driver(drivers_dir: &Path) {
    let dir = drivers_dir.join("secret-driver");
    fs::create_dir_all(&dir).unwrap();
    let wasm_src = ed_state_driver_wasm();
    fs::copy(&wasm_src, dir.join("driver.wasm")).unwrap();
    fs::write(
        dir.join("driver.toml"),
        r#"
id = "secret-driver"
name = "Secret Driver"
version = "0.1.0"
description = "secret 型 setting を持つ fixture ドライバ"
entry = "driver.wasm"

[[topics]]
name = "set-system"
retain = false
description = "プラグインからのシステム名の更新"

[[topics]]
name = "current-system"
retain = true
description = "現在のスターシステム"

[[settings]]
type = "secret"
key = "api-key"
label = "API Key"
"#,
    )
    .unwrap();
}

/// secret 型 setting を持つ fixture ドライバ 1 件を持つ `DriverRegistry`
/// (`ed_state_driver_registry` と同じ流儀)。
fn secret_driver_registry() -> (tempfile::TempDir, DriverRegistry) {
    let tmp = tempfile::tempdir().unwrap();
    let drivers_dir = tmp.path().join("drivers");
    fs::create_dir_all(&drivers_dir).unwrap();
    write_secret_driver(&drivers_dir);

    let settings_store = SettingsStore::new(tmp.path().join("driver-settings"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("driver-settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("driver-settings"), Vec::new());
    let grants_store = GrantsStore::new_for_drivers(tmp.path().join("driver-grants"));
    let host = DriverHost::new().expect("driver host should build");

    let drivers = start_drivers(
        &drivers_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        edlr_driver_channel::Bus::new(),
        host,
    );
    (tmp, drivers)
}

#[tokio::test(flavor = "multi_thread")]
async fn pin_drivers_set_settings_does_not_strip_secret_value() {
    let (_tmp, drivers) = secret_driver_registry();
    let (_router, addr) = setup(None, Some(drivers)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(
        &mut ws,
        1,
        "drivers/set-settings",
        serde_json::json!({"driver": "secret-driver", "values": {"api-key": "sk-live-123"}}),
    )
    .await;
    let resp = recv_json(&mut ws).await;

    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["id"], 1);
    let expected = serde_json::json!({ "api-key": "sk-live-123" });
    assert_eq!(resp["result"], expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn pin_plugins_list_with_sidecar_plugin() {
    let sidecar_env = support::sidecar_env("svc", 50501, false);
    let registry = sidecar_env.registry.clone();
    let plugins_dir = registry.plugins_dir().to_path_buf();
    let (_router, addr) = setup(Some(registry), None).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(&mut ws, 1, "plugins/list", serde_json::json!({})).await;
    let resp = recv_json(&mut ws).await;

    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["id"], 1);

    let plugins_dir_str = plugins_dir.to_string_lossy().to_string();
    let expected = serde_json::json!({
        "pluginsDir": plugins_dir_str,
        "plugins": [
            {
                "id": "sc-plugin",
                "name": "SC",
                "version": "0.1.0",
                "description": "",
                "state": "running",
                "settings": [],
                "values": {},
                "capabilities": {
                    "granted": false,
                    "staleGrant": false,
                    "requests": [],
                },
                "sidecars": [
                    {
                        "name": "svc",
                        "port": 50501,
                        "args": ["-c", "sleep 30"],
                        "scalable": false,
                        "reason": "test sidecar",
                        "granted": false,
                        "staleGrant": false,
                        "config": {
                            "command": "",
                            "args": ["-c", "sleep 30"],
                            "port": 50501,
                            "replicas": 1,
                        },
                        "instances": [
                            {
                                "index": 0,
                                "port": 50501,
                                "state": "exited",
                                "exitCode": null,
                            }
                        ],
                    }
                ],
                "filesystem": [],
                "bus": [],
                "dashboard": [],
                "schedules": [],
                "dropped": {
                    "events": 0,
                    "busDeliveries": 0,
                },
                "secretsSet": [],
            }
        ],
    });
    assert_eq!(resp["result"], expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn pin_capabilities_get_then_set_granted_true() {
    let (_tmp, registry) = http_caller_registry();
    let (_router, addr) = setup(Some(registry), None).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(
        &mut ws,
        1,
        "plugins/get-capabilities",
        serde_json::json!({"plugin": "http-caller"}),
    )
    .await;
    let get_resp = recv_json(&mut ws).await;

    send_rpc(
        &mut ws,
        2,
        "plugins/set-capabilities",
        serde_json::json!({"plugin": "http-caller", "granted": true}),
    )
    .await;
    let set_resp = recv_json(&mut ws).await;

    assert_eq!(get_resp["type"], "rpc-result");
    assert_eq!(get_resp["id"], 1);
    let expected_get = serde_json::json!({
        "granted": false,
        "staleGrant": false,
        "requests": [
            {
                "kind": "http",
                "hosts": ["https://api.example.com"],
                "reason": "test",
            }
        ],
    });
    assert_eq!(get_resp["result"], expected_get);

    assert_eq!(set_resp["type"], "rpc-result");
    assert_eq!(set_resp["id"], 2);
    let expected_set = serde_json::json!({
        "granted": true,
        "staleGrant": false,
        "requests": [
            {
                "kind": "http",
                "hosts": ["https://api.example.com"],
                "reason": "test",
            }
        ],
    });
    assert_eq!(set_resp["result"], expected_set);
}

#[tokio::test(flavor = "multi_thread")]
async fn pin_drivers_list_with_fixture_driver() {
    let (_tmp, drivers) = ed_state_driver_registry();
    let drivers_dir = drivers.drivers_dir().to_path_buf();
    let (_router, addr) = setup(None, Some(drivers)).await;
    let mut ws = connect(addr).await;
    recv_hello(&mut ws).await;

    send_rpc(&mut ws, 1, "drivers/list", serde_json::json!({})).await;
    let resp = recv_json(&mut ws).await;

    assert_eq!(resp["type"], "rpc-result");
    assert_eq!(resp["id"], 1);

    let drivers_dir_str = drivers_dir.to_string_lossy().to_string();
    let expected = serde_json::json!({
        "driversDir": drivers_dir_str,
        "drivers": [
            {
                "id": "ed-state",
                "name": "ED State",
                "version": "0.1.0",
                "description": "受け取ったシステム名を retained トピックとして配る",
                "state": "running",
                "topics": [
                    {
                        "name": "set-system",
                        "retain": false,
                        "description": "プラグインからのシステム名の更新",
                    },
                    {
                        "name": "current-system",
                        "retain": true,
                        "description": "現在のスターシステム",
                    }
                ],
                "settings": [],
                "values": {},
                "capabilities": {
                    "granted": false,
                    "staleGrant": false,
                    "requests": [],
                },
                "sidecars": [],
                "filesystem": [],
            }
        ],
    });
    assert_eq!(resp["result"], expected);
}

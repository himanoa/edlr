//! Phase 2 の `server.rs` 分解に着手する前の pin テスト。
//!
//! 代表的な RPC 応答の**生 JSON 全体**を等値比較で固定する。分解後に応答の
//! 形が 1 フィールドでも変わればここが落ちる、というのが目的
//! (`.claude/skills/rpc-pin-tests/SKILL.md` 参照)。`ws_rpc_integration.rs`
//! と完全一致するハーネス(WS 接続・送受信・`http_caller_registry`)は
//! `support/` へ集約済み(issue yzyv)。`setup` はドライバレジストリも
//! 受け取るため両ファイルで別々に持つ。

use edlr_core::capability::grants::GrantsStore;
use edlr_core::host::driver::DriverHost;
use edlr_core::registry::driver::DriverRegistry;
use edlr_core::registry::plugin::Registry;
use edlr_core::router::Router;
use edlr_core::runner::driver::start_drivers;
use edlr_core::server::{self, ServerState};
use edlr_core::settings::filesystem::FilesystemConfigStore;
use edlr_core::settings::sidecar::SidecarConfigStore;
use edlr_core::settings::store::SettingsStore;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

mod support;

use support::{connect, recv_hello, recv_json, send_rpc};

async fn setup(
    registry: Option<Registry>,
    drivers: Option<DriverRegistry>,
) -> (Router, SocketAddr) {
    let router = Router::new(64);
    let state = ServerState::new(&router, registry, drivers);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server::serve(listener, state, None));
    (router, addr)
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
    let host = DriverHost::new(support::test_handle()).expect("driver host should build");

    let drivers = start_drivers(
        &drivers_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        edlr_driver_channel::Bus::new(),
        host,
        edlr_core::profiler::Profiler::noop(),
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
    let host = DriverHost::new(support::test_handle()).expect("driver host should build");

    let drivers = start_drivers(
        &drivers_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        edlr_driver_channel::Bus::new(),
        host,
        edlr_core::profiler::Profiler::noop(),
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
                "layout": null,
            }
        ],
    });
    assert_eq!(resp["result"], expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn pin_capabilities_get_then_set_granted_true() {
    let (_tmp, registry) = support::http_caller_registry();
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
                "layout": null,
            }
        ],
    });
    assert_eq!(resp["result"], expected);
}

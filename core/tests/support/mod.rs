//! サイドカー統合テスト用の足場。`plugins-dir` に、実際にロード・init が
//! 成功する(= `PluginState::Running` のまま保たれる)プラグインを 1 件
//! 作り、`Registry` を組み立てる。
//!
//! 以前はここで壊れた wasm バイト列(`\0asm\x01\x00\x00\x00` のみ)を書いて
//! いた。プラグインは常にロードに失敗して `Disabled` になるが、サイドカーの
//! 設定・承認・制御は `Registry` の API で完結するため、それでも大半のテスト
//! には十分だった。しかし再レビューで `control_sidecar` が `PluginState` を
//! 見るようになった(無効化されたプラグインの `start`/`restart` を拒否する)
//! ため、`Disabled` なままだとサイドカーの起動系のテストが (意図せず) 常に
//! 失敗するようになった。そのため `examples/plugins/hello-logger` の実際に
//! ビルドした wasm を使い、ロード・init が成功して `Running` のまま保たれる
//! ようにしている(`hello-logger` 自体は `[[sidecar]]` を一切知らず、
//! サイドカーはあくまで manifest の宣言と `Registry` 側の制御だけで完結する
//! ので、`entry` に流用しても問題ない)。

use std::path::{Path, PathBuf};
use std::process::Command;
#[allow(unused_imports)]
use std::sync::Arc;
use std::time::Duration;

use edlr_core::capability::grants::GrantsStore;
use edlr_core::host::driver::DriverHost;
use edlr_core::host::plugin::PluginHost;
use edlr_core::registry::driver::DriverRegistry;
use edlr_core::registry::plugin::Registry;
use edlr_core::router::Router;
use edlr_core::runner::driver::start_drivers;
use edlr_core::schedule::store::ScheduleStore;
use edlr_core::settings::filesystem::FilesystemConfigStore;
use edlr_core::settings::sidecar::SidecarConfigStore;
use edlr_core::settings::store::SettingsStore;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

/// ドライバを 1 件もロードしていない `DriverRegistry`(存在しないディレクトリ
/// を走査させることで空のまま作る)。バス機能を主眼にしないテストの
/// `start_plugins` 呼び出しはこれを渡しておけば十分。
#[allow(dead_code)]
pub fn empty_driver_registry(tmp_path: &Path) -> DriverRegistry {
    start_drivers(
        &tmp_path.join("drivers"),
        SettingsStore::new(tmp_path.join("driver-settings")),
        SidecarConfigStore::new(tmp_path.join("driver-settings")),
        FilesystemConfigStore::new(tmp_path.join("driver-settings"), Vec::new()),
        GrantsStore::new_for_drivers(tmp_path.join("driver-grants")),
        edlr_driver_channel::Bus::new(),
        DriverHost::new(test_handle()).expect("driver host should build"),
        edlr_core::profiler::Profiler::noop(),
    )
}

/// `rpc_pin_integration.rs` と `ws_rpc_integration.rs` の両方で使う WS
/// クライアントの型・接続・送受信ヘルパ(issue yzyv: 両ファイルで完全一致
/// していた重複を集約)。
#[allow(dead_code)]
pub type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[allow(dead_code)]
pub async fn connect(addr: std::net::SocketAddr) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("connect");
    ws
}

#[allow(dead_code)]
pub async fn recv_json(ws: &mut Ws) -> serde_json::Value {
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

#[allow(dead_code)]
pub async fn recv_hello(ws: &mut Ws) {
    assert_eq!(recv_json(ws).await["type"], "hello");
}

#[allow(dead_code)]
pub async fn send_rpc(ws: &mut Ws, id: i64, method: &str, params: serde_json::Value) {
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
/// `.wasm` へのパスを返す。
#[allow(dead_code)]
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

#[allow(dead_code)]
fn write_http_caller(plugins_dir: &Path) {
    let dir = plugins_dir.join("http-caller");
    std::fs::create_dir_all(&dir).unwrap();
    let wasm_src = http_caller_wasm();
    std::fs::copy(&wasm_src, dir.join("http_caller.wasm")).unwrap();
    std::fs::write(
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
/// (`rpc_pin_integration.rs`/`ws_rpc_integration.rs` の両方で使う)。
#[allow(dead_code)]
pub fn http_caller_registry() -> (tempfile::TempDir, Registry) {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    write_http_caller(&plugins_dir);

    let settings_store = SettingsStore::new(tmp.path().join("settings"));
    let grants_store = GrantsStore::new(tmp.path().join("grants"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
    let router = Router::new(16);
    let host = PluginHost::new(test_handle()).expect("host should start");

    let registry = edlr_core::runner::plugin::start_plugins(
        &plugins_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        host,
        edlr_core::profiler::Profiler::noop(),
    );
    (tmp, registry)
}

pub struct Env {
    #[allow(dead_code)]
    pub registry: Registry,
    /// テストが `env.tmp.path()` で参照する、`Registry` の各ストアが指す
    /// tempdir。フィールドを保持しているだけで drop すると保存先ごと消える
    /// (このフィールド自体は直接読まれなくても生存させる必要がある)。
    #[allow(dead_code)]
    pub tmp: tempfile::TempDir,
}

/// `examples/plugins/hello-logger` を `wasm32-wasip2` 向けにビルドし、
/// できあがった `.wasm` へのパスを返す。既にビルド済みならほぼ即座に戻る
/// (`cargo build` は成果物が新しければ再ビルドしない)。
#[allow(dead_code)]
pub fn valid_plugin_wasm() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crate_path = manifest_dir
        .join("..")
        .join("examples/plugins/hello-logger");

    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2", "--release"])
        .current_dir(&crate_path)
        .status()
        .expect("failed to spawn cargo build for the sidecar test fixture plugin");
    assert!(status.success(), "hello-logger fixture build failed");

    crate_path
        .join("target/wasm32-wasip2/release")
        .join("hello_logger.wasm")
}

#[allow(dead_code)]
pub fn sidecar_env(name: &str, port: u16, scalable: bool) -> Env {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugin_dir = plugins_dir.join("sc-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(valid_plugin_wasm(), plugin_dir.join("plugin.wasm")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "id = \"sc-plugin\"\nname = \"SC\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n\
             [[sidecar]]\nname = \"{name}\"\nreason = \"test sidecar\"\n\
             args = [\"-c\", \"sleep 30\"]\nport = {port}\nscalable = {scalable}\n"
        ),
    )
    .unwrap();

    let router = Router::new(16);
    let registry = edlr_core::runner::plugin::start_plugins(
        &plugins_dir,
        SettingsStore::new(tmp.path().join("settings")),
        SidecarConfigStore::new(tmp.path().join("settings")),
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new()),
        GrantsStore::new(tmp.path().join("grants")),
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        PluginHost::new(test_handle()).expect("plugin host"),
        edlr_core::profiler::Profiler::noop(),
    );

    Env { registry, tmp }
}

/// 当該プラグインの `capabilities_json` が現在載せている実効許可ホスト。
///
/// `#[allow(dead_code)]`: このモジュールはテストバイナリごとに個別に
/// コンパイルされる(`core/tests/*.rs` の各ファイルが `mod support;` する
/// たび)。このヘルパを使わないテストバイナリからは「未使用」警告が出るが、
/// 実際には他のテストバイナリで使われている正常な状態(Minor: 最終レビュー
/// で見つかったテスト出力のノイズ)。
#[allow(dead_code)]
pub fn effective_hosts(registry: &Registry, id: &str) -> Vec<String> {
    registry.effective_hosts(id).unwrap_or_default()
}

/// `plugins_dir` に独立した 2 プラグイン(`sc-plugin-a`/`sc-plugin-b`)を
/// 作り、それぞれに同名のサイドカーを 1 つずつ宣言した `Registry` を返す。
/// 「あるプラグインのサイドカー停止待ちが、別プラグインのサイドカー操作を
/// ブロックしないか」を検証するテスト専用のヘルパ。
#[allow(dead_code)]
pub fn two_plugin_sidecar_env(sidecar_name: &str, port_a: u16, port_b: u16) -> Env {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    write_sidecar_plugin(&plugins_dir, "sc-plugin-a", sidecar_name, port_a);
    write_sidecar_plugin(&plugins_dir, "sc-plugin-b", sidecar_name, port_b);

    let router = Router::new(16);
    let registry = edlr_core::runner::plugin::start_plugins(
        &plugins_dir,
        SettingsStore::new(tmp.path().join("settings")),
        SidecarConfigStore::new(tmp.path().join("settings")),
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new()),
        GrantsStore::new(tmp.path().join("grants")),
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        PluginHost::new(test_handle()).expect("plugin host"),
        edlr_core::profiler::Profiler::noop(),
    );

    Env { registry, tmp }
}

#[allow(dead_code)]
fn write_sidecar_plugin(plugins_dir: &Path, plugin_id: &str, sidecar_name: &str, port: u16) {
    let plugin_dir = plugins_dir.join(plugin_id);
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(valid_plugin_wasm(), plugin_dir.join("plugin.wasm")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "id = \"{plugin_id}\"\nname = \"{plugin_id}\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n\
             [[sidecar]]\nname = \"{sidecar_name}\"\nreason = \"test sidecar\"\n\
             args = [\"-c\", \"sleep 30\"]\nport = {port}\nscalable = false\n"
        ),
    )
    .unwrap();
}

/// `[[filesystem]]`(`name = name`, `mode = mode`)を 1 件持つ `fs-plugin` の
/// manifest を置いた plugins-dir でサーバを起動し、`Registry` を返す
/// (`sidecar_env` と同じ流儀)。
#[allow(dead_code)]
pub fn filesystem_env(name: &str, mode: &str) -> Env {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugin_dir = plugins_dir.join("fs-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(valid_plugin_wasm(), plugin_dir.join("plugin.wasm")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "id = \"fs-plugin\"\nname = \"FS\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n\
             [[filesystem]]\nname = \"{name}\"\nreason = \"test filesystem\"\nmode = \"{mode}\"\n"
        ),
    )
    .unwrap();

    let router = Router::new(16);
    let registry = edlr_core::runner::plugin::start_plugins(
        &plugins_dir,
        SettingsStore::new(tmp.path().join("settings")),
        SidecarConfigStore::new(tmp.path().join("settings")),
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new()),
        GrantsStore::new(tmp.path().join("grants")),
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        PluginHost::new(test_handle()).expect("plugin host"),
        edlr_core::profiler::Profiler::noop(),
    );

    Env { registry, tmp }
}

/// 当該プラグインの `filesystem_json` が現在載せている生の JSON 文字列
/// (テスト用アクセサ。`Registry::filesystem_buffer` をそのまま呼ぶ)。
#[allow(dead_code)]
pub fn filesystem_buffer(registry: &Registry, id: &str) -> String {
    registry.filesystem_buffer(id).unwrap_or_default()
}

/// テスト全体で共有する runtime の Handle。`PluginHost::new` /
/// `DriverHost::new` が要求する(`HttpDriver` の同期 `send` の `block_on` 先)。
/// 関数ローカルの Runtime だと drop 後の `block_on` で panic するため
/// static に生かす。
pub fn test_handle() -> tokio::runtime::Handle {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().expect("build test runtime"))
        .handle()
        .clone()
}

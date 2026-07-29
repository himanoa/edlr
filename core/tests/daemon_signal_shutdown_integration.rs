//! Critical 1 の回帰テスト: デーモン(`edlr` バイナリそのもの)が SIGTERM を
//! 受けたとき、稼働中のサイドカー(の孫プロセスまで)が確実に停止すること。
//!
//! `ws_rpc_integration.rs` の他のサイドカーテストと違い、これは
//! `core::server::serve` をテストプロセス内で直接起動するのではなく、
//! `edlr` バイナリを実プロセスとして spawn する -- SIGTERM ハンドラは
//! `core/src/bin/edlr.rs` の `main` にしかなく、そこを経由しない限り
//! 検証できないため。

use futures_util::{SinkExt, StreamExt};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

mod support;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// このテスト専用の listen アドレス。他の統合テストが使うポート帯
/// (5030x/5040x など)と衝突しないよう、明確に離れたポートを使う。
const DAEMON_ADDR: &str = "127.0.0.1:58501";
/// サイドカーに割り当てるポート。同様に他テストと衝突しない値。
const SIDECAR_PORT: u16 = 58601;

/// ドライバ側のシャットダウンテスト専用の listen アドレス・サイドカー
/// ポート。プラグイン側のテスト(`DAEMON_ADDR`/`SIDECAR_PORT`)と同時に
/// 実行されても衝突しないよう、別の値を使う。
const DRIVER_DAEMON_ADDR: &str = "127.0.0.1:58502";
const DRIVER_SIDECAR_PORT: u16 = 58602;

/// 実際にロード・init が成功する wasm を使う(`support::valid_plugin_wasm`
/// 参照)。`control_sidecar` が `PluginState` を見るようになった(無効化
/// されたプラグインの `start`/`restart` を拒否する)ため、以前のようにわざと
/// 壊れた wasm を置いて `Disabled` のままにすると、この統合テストが
/// (意図せず)サイドカーの起動そのものを検証できなくなってしまう。
fn write_sidecar_plugin(plugins_dir: &Path) {
    let dir = plugins_dir.join("sc-plugin");
    fs::create_dir_all(&dir).unwrap();
    fs::copy(support::valid_plugin_wasm(), dir.join("plugin.wasm")).unwrap();
    fs::write(
        dir.join("manifest.toml"),
        format!(
            "id = \"sc-plugin\"\nname = \"SC\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n\
             [[sidecar]]\nname = \"tts\"\nreason = \"test sidecar\"\n\
             args = [\"-c\", \"sleep 30\"]\nport = {SIDECAR_PORT}\nscalable = false\n"
        ),
    )
    .unwrap();
}

/// `examples/drivers/<dir>` をビルドした `.wasm` へのパスを返す。未ビルド
/// なら `None`(呼び出し側は skip する -- `core/tests/bus_integration.rs`
/// の `built_example` と同じ流儀。wasm ターゲットの無い環境で
/// `cargo test` を壊さないため)。
fn built_example_wasm(dir: &str, file: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(dir)
        .join("target/wasm32-wasip2/release")
        .join(file);
    path.exists().then_some(path)
}

/// `[[sidecar]]` を宣言する `driver.toml` を自分で書いた `drivers_dir/<id>`
/// を組み立てる(`examples/drivers/ed-state` はサイドカーを一切宣言しない
/// ので、既存の `driver.toml` はコピーせずここで直接書く -- Critical 1 の
/// 検証にはトピックは不要で、`load`/`init` が成功する実 wasm を流用したい
/// だけなので、`ed-state` の wasm を「サイドカー付きの適当なドライバ」に
/// 見せかける目的でこの形にしている)。
fn write_driver_with_sidecar(drivers_dir: &Path, wasm_src: &Path, port: u16) {
    let dir = drivers_dir.join("sc-driver");
    fs::create_dir_all(&dir).unwrap();
    fs::copy(wasm_src, dir.join("driver.wasm")).unwrap();
    fs::write(
        dir.join("driver.toml"),
        format!(
            "id = \"sc-driver\"\nname = \"SC Driver\"\nversion = \"0.1.0\"\nentry = \"driver.wasm\"\n\n\
             [[sidecar]]\nname = \"engine\"\nreason = \"test sidecar\"\n\
             args = [\"-c\", \"sleep 30\"]\nport = {port}\nscalable = false\n"
        ),
    )
    .unwrap();
}

fn daemon_running(addr: &str) -> bool {
    use std::net::TcpStream;
    match addr.parse::<std::net::SocketAddr>() {
        Ok(a) => TcpStream::connect_timeout(&a, Duration::from_millis(200)).is_ok(),
        Err(_) => false,
    }
}

async fn connect(addr: &str) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("connect to daemon ws");
    ws
}

async fn recv_json(ws: &mut Ws) -> serde_json::Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timeout waiting for ws message")
            .expect("stream ended")
            .expect("ws error");
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).expect("valid json");
        }
    }
}

/// spawn したデーモンを `Drop` で確実に回収するガード。
///
/// この統合テストはアサーション失敗のたびに実プロセスを spawn する。
/// ガード無しで途中のアサーションが panic すると、テスト末尾の
/// `daemon.kill()`/`daemon.wait()` まで到達できず、デーモンが
/// `DAEMON_ADDR`(固定ポート)を握ったまま生き残ってしまう -- 次にこの
/// テストを実行したときに「新しいデーモンを spawn したつもりが、実際には
/// 古い(既に消えた一時ディレクトリを指す)デーモンに接続してしまう」という、
/// 原因の分かりにくい失敗を招く(このテストを書く過程で実際に踏んだ)。
struct DaemonGuard(std::process::Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call(ws: &mut Ws, id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
    let msg = serde_json::json!({ "type": "rpc", "id": id, "method": method, "params": params });
    ws.send(Message::Text(msg.to_string().into()))
        .await
        .unwrap();
    // WS ストリームにはイベントフレーム(journal/status に加え、デーモンの
    // tracing ログ `kind:"log"` も)がいつでも交ざりうる。RPC 応答
    // (`rpc-result`/`rpc-error`)以外は読み飛ばす。
    let resp = loop {
        let msg = recv_json(ws).await;
        match msg["type"].as_str() {
            Some("rpc-result") | Some("rpc-error") => break msg,
            _ => continue,
        }
    };
    assert_eq!(resp["id"], id);
    assert_eq!(
        resp["type"], "rpc-result",
        "rpc call {method} failed: {resp:?}"
    );
    resp["result"].clone()
}

/// デーモンを実プロセスとして起動し、稼働中のサイドカーの孫プロセスまでが
/// SIGTERM で確実に消えることを確認する。
///
/// 手順: デーモンを spawn → RPC でサイドカーの設定・承認・起動 → 孫プロセスの
/// PID をファイル経由で確認 → デーモンへ SIGTERM → デーモンの終了を待つ →
/// 孫プロセスが死んでいることを確認する。
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_to_daemon_stops_running_sidecars_including_grandchildren() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let settings_dir = tmp.path().join("settings");
    let grants_dir = tmp.path().join("grants");
    let journal_dir = tmp.path().join("journal");
    fs::create_dir_all(&plugins_dir).unwrap();
    fs::create_dir_all(&journal_dir).unwrap();
    write_sidecar_plugin(&plugins_dir);

    let pidfile = tmp.path().join("grandchild.pid");
    let sidecar_script = tmp.path().join("sidecar.sh");
    fs::write(
        &sidecar_script,
        format!(
            "#!/bin/sh\n(sleep 60 & echo $! > {}); wait\n",
            pidfile.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sidecar_script, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut daemon = DaemonGuard(
        Command::new(env!("CARGO_BIN_EXE_edlr"))
            .arg("--journal-dir")
            .arg(&journal_dir)
            .arg("--listen")
            .arg(DAEMON_ADDR)
            .arg("--plugins-dir")
            .arg(&plugins_dir)
            .arg("--settings-dir")
            .arg(&settings_dir)
            .arg("--grants-dir")
            .arg(&grants_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn edlr daemon binary"),
    );

    for _ in 0..200 {
        if daemon_running(DAEMON_ADDR) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        daemon_running(DAEMON_ADDR),
        "daemon did not start listening in time"
    );

    let mut ws = connect(DAEMON_ADDR).await;
    // hello メッセージを読み飛ばす。
    let _ = recv_json(&mut ws).await;

    call(
        &mut ws,
        1,
        "plugins/set-sidecar-config",
        serde_json::json!({
            "plugin": "sc-plugin",
            "name": "tts",
            "config": {
                "command": "/bin/sh",
                "args": ["-c", format!("{}", sidecar_script.display())],
                "port": SIDECAR_PORT,
                "replicas": 1,
            },
        }),
    )
    .await;

    call(
        &mut ws,
        2,
        "plugins/set-sidecar-grant",
        serde_json::json!({"plugin": "sc-plugin", "name": "tts", "granted": true}),
    )
    .await;

    let started = call(
        &mut ws,
        3,
        "plugins/sidecar-control",
        serde_json::json!({"plugin": "sc-plugin", "name": "tts", "action": "start"}),
    )
    .await;
    assert_eq!(
        started["sidecars"][0]["instances"][0]["state"],
        serde_json::json!("running")
    );

    let mut grandchild_pid = None;
    for _ in 0..300 {
        if let Ok(content) = fs::read_to_string(&pidfile) {
            if let Ok(pid) = content.trim().parse::<i32>() {
                grandchild_pid = Some(pid);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let grandchild_pid = grandchild_pid.expect("grandchild should have reported its pid");

    // SAFETY: existence check only, no signal sent.
    assert!(
        unsafe { libc::kill(grandchild_pid, 0) } == 0,
        "grandchild should be alive before shutdown"
    );

    // SAFETY: `daemon.0.id()` is our own spawned, not-yet-waited-on child.
    let killed = unsafe { libc::kill(daemon.0.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(killed, 0, "failed to send SIGTERM to daemon");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while Instant::now() < deadline {
        match daemon.0.try_wait() {
            Ok(Some(_)) => {
                exited = true;
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    assert!(exited, "daemon did not exit within 10s of SIGTERM");

    // SAFETY: existence check only.
    let grandchild_alive = unsafe { libc::kill(grandchild_pid, 0) } == 0;
    assert!(
        !grandchild_alive,
        "grandchild {grandchild_pid} survived daemon SIGTERM; sidecars were orphaned"
    );

    // `daemon` (the `DaemonGuard`) drops here, reaping the already-exited
    // process; if any assertion above had panicked instead, the `Drop` impl
    // still runs during unwinding and kills the daemon, so `DAEMON_ADDR`
    // never stays occupied by a leaked process across test runs.
}

/// Critical 1 の回帰テスト、ドライバ側: 上の
/// `sigterm_to_daemon_stops_running_sidecars_including_grandchildren` の
/// プラグイン専用版と対をなす。最終レビューで見つかった取りこぼしは、
/// デーモンの shutdown シーケンス(`core/src/bin/edlr.rs`)が **プラグイン
/// 側の `Registry::stop_all_sidecars` しか呼んでおらず、`DriverRegistry`
/// (独自の `ProcessDriver` を持つ別インスタンス)の分は一切止めていなかっ
/// た**こと -- ドライバのサイドカー(設計書の動機そのものである VOICEVOX の
/// ような合成エンジン)は SIGTERM を送っても孤児として残り続けていた。
///
/// `examples/drivers/ed-state` はサイドカーを宣言しないため、
/// `write_driver_with_sidecar` で `[[sidecar]]` 付きの `driver.toml` を
/// 自分で書き、`ed-state` の実 wasm を entry として流用する(load/init が
/// 成功して `Running` のまま保たれることだけが目的で、ドライバ自体の
/// トピックは使わない)。
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_to_daemon_stops_running_driver_sidecars() {
    let Some(driver_wasm) = built_example_wasm("examples/drivers/ed-state", "ed_state.wasm") else {
        eprintln!("skipping: build the example driver first");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let drivers_dir = tmp.path().join("drivers");
    let plugins_dir = tmp.path().join("plugins");
    let settings_dir = tmp.path().join("settings");
    let grants_dir = tmp.path().join("grants");
    let journal_dir = tmp.path().join("journal");
    fs::create_dir_all(&drivers_dir).unwrap();
    fs::create_dir_all(&plugins_dir).unwrap();
    fs::create_dir_all(&journal_dir).unwrap();
    write_driver_with_sidecar(&drivers_dir, &driver_wasm, DRIVER_SIDECAR_PORT);

    let pidfile = tmp.path().join("grandchild.pid");
    let sidecar_script = tmp.path().join("sidecar.sh");
    fs::write(
        &sidecar_script,
        format!(
            "#!/bin/sh\n(sleep 60 & echo $! > {}); wait\n",
            pidfile.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sidecar_script, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut daemon = DaemonGuard(
        Command::new(env!("CARGO_BIN_EXE_edlr"))
            .arg("--journal-dir")
            .arg(&journal_dir)
            .arg("--listen")
            .arg(DRIVER_DAEMON_ADDR)
            .arg("--plugins-dir")
            .arg(&plugins_dir)
            .arg("--drivers-dir")
            .arg(&drivers_dir)
            .arg("--settings-dir")
            .arg(&settings_dir)
            .arg("--grants-dir")
            .arg(&grants_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn edlr daemon binary"),
    );

    for _ in 0..200 {
        if daemon_running(DRIVER_DAEMON_ADDR) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        daemon_running(DRIVER_DAEMON_ADDR),
        "daemon did not start listening in time"
    );

    let mut ws = connect(DRIVER_DAEMON_ADDR).await;
    // hello メッセージを読み飛ばす。
    let _ = recv_json(&mut ws).await;

    call(
        &mut ws,
        1,
        "drivers/set-sidecar-config",
        serde_json::json!({
            "driver": "sc-driver",
            "name": "engine",
            "config": {
                "command": "/bin/sh",
                "args": ["-c", format!("{}", sidecar_script.display())],
                "port": DRIVER_SIDECAR_PORT,
                "replicas": 1,
            },
        }),
    )
    .await;

    call(
        &mut ws,
        2,
        "drivers/set-sidecar-grant",
        serde_json::json!({"driver": "sc-driver", "name": "engine", "granted": true}),
    )
    .await;

    let started = call(
        &mut ws,
        3,
        "drivers/sidecar-control",
        serde_json::json!({"driver": "sc-driver", "name": "engine", "action": "start"}),
    )
    .await;
    assert_eq!(
        started["sidecars"][0]["instances"][0]["state"],
        serde_json::json!("running")
    );

    let mut grandchild_pid = None;
    for _ in 0..300 {
        if let Ok(content) = fs::read_to_string(&pidfile) {
            if let Ok(pid) = content.trim().parse::<i32>() {
                grandchild_pid = Some(pid);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let grandchild_pid = grandchild_pid.expect("grandchild should have reported its pid");

    // SAFETY: existence check only, no signal sent.
    assert!(
        unsafe { libc::kill(grandchild_pid, 0) } == 0,
        "grandchild should be alive before shutdown"
    );

    // SAFETY: `daemon.0.id()` is our own spawned, not-yet-waited-on child.
    let killed = unsafe { libc::kill(daemon.0.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(killed, 0, "failed to send SIGTERM to daemon");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while Instant::now() < deadline {
        match daemon.0.try_wait() {
            Ok(Some(_)) => {
                exited = true;
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    assert!(exited, "daemon did not exit within 10s of SIGTERM");

    // SAFETY: existence check only.
    let grandchild_alive = unsafe { libc::kill(grandchild_pid, 0) } == 0;
    assert!(
        !grandchild_alive,
        "grandchild {grandchild_pid} survived daemon SIGTERM; driver sidecars were orphaned"
    );
}

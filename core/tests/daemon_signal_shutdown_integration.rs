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
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// このテスト専用の listen アドレス。他の統合テストが使うポート帯
/// (5030x/5040x など)と衝突しないよう、明確に離れたポートを使う。
const DAEMON_ADDR: &str = "127.0.0.1:58501";
/// サイドカーに割り当てるポート。同様に他テストと衝突しない値。
const SIDECAR_PORT: u16 = 58601;

fn write_sidecar_plugin(plugins_dir: &Path) {
    let dir = plugins_dir.join("sc-plugin");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("plugin.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
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

async fn call(ws: &mut Ws, id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
    let msg = serde_json::json!({ "type": "rpc", "id": id, "method": method, "params": params });
    ws.send(Message::Text(msg.to_string().into())).await.unwrap();
    let resp = recv_json(ws).await;
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

    let mut daemon = Command::new(env!("CARGO_BIN_EXE_edlr"))
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
        .expect("failed to spawn edlr daemon binary");

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

    // SAFETY: `daemon.id()` is our own spawned, not-yet-waited-on child.
    let killed = unsafe { libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(killed, 0, "failed to send SIGTERM to daemon");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while Instant::now() < deadline {
        match daemon.try_wait() {
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

    let _ = daemon.kill();
    let _ = daemon.wait();
}

//! journal dir が CLI 引数・config.json・自動検出のどれでも解決できない
//! 環境(新規インストール直後の macOS など)で、デーモンがフォールバック
//! ディレクトリ(`$XDG_DATA_HOME/edlr/journal`)を作成して起動することの
//! 回帰テスト。以前はこの状況で `journal directory not found` を出して
//! exit(1) しており、初回の UI 起動が必ずデーモン不在で始まっていた。

use std::process::{Command, Stdio};
use std::time::Duration;

/// このテスト専用の listen アドレス。他の統合テストのポート帯
/// (28501-28503/5030x/5040x)と衝突せず、かつ ephemeral port range
/// (Linux 既定 32768-60999)の外の値
/// (→ `daemon_signal_shutdown_integration.rs` の `DAEMON_ADDR` のコメント)。
const DAEMON_ADDR: &str = "127.0.0.1:28504";

fn daemon_running(addr: &str) -> bool {
    use std::net::TcpStream;
    match addr.parse::<std::net::SocketAddr>() {
        Ok(a) => TcpStream::connect_timeout(&a, Duration::from_millis(200)).is_ok(),
        Err(_) => false,
    }
}

/// spawn したデーモンを `Drop` で確実に回収するガード
/// (`daemon_signal_shutdown_integration.rs` と同じ理由)。
struct DaemonGuard(std::process::Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn daemon_creates_fallback_journal_dir_when_nothing_resolves() {
    let tmp = tempfile::tempdir().unwrap();

    // HOME は Proton 既定パスも .config/edlr/config.json も含まない
    // 空ディレクトリ。XDG_DATA_HOME を tmp 配下に向けることで、
    // フォールバック先の作成をこのテスト内で観測できるようにする。
    let home = tmp.path().join("home");
    let data_home = tmp.path().join("data");
    std::fs::create_dir_all(&home).unwrap();

    let mut daemon = DaemonGuard(
        Command::new(env!("CARGO_BIN_EXE_edlr"))
            .arg("--listen")
            .arg(DAEMON_ADDR)
            .arg("--state-dir")
            .arg(tmp.path().join("state"))
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", tmp.path().join("config"))
            .env("XDG_DATA_HOME", &data_home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn edlr daemon binary"),
    );

    let mut listening = false;
    for _ in 0..200 {
        // フォールバックが働かず exit(1) した場合はここで即座に検出する
        // (ポート待ちの 4 秒を無駄に待たない)。
        if let Some(status) = daemon.0.try_wait().unwrap() {
            panic!(
                "daemon exited ({status}) instead of starting; \
                 it likely failed to create the fallback journal dir"
            );
        }
        if daemon_running(DAEMON_ADDR) {
            listening = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        listening,
        "daemon did not start listening; fallback journal dir was not used"
    );
    assert!(
        data_home.join("edlr").join("journal").is_dir(),
        "fallback journal directory was not created"
    );
}

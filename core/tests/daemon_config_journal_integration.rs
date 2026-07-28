//! デーモン(`edlr` バイナリ)が `--journal-dir` なしで起動されたとき、
//! `config.json` の `journalDir` へフォールバックすることの回帰テスト。
//!
//! 以前は CLI 引数 → Proton 既定パスの自動検出しかなく、Tauri シェルだけが
//! `config.json` を読んで `--journal-dir` に変換していた。そのため
//! `edlr` を単体で起動すると、UI で設定済みの journalDir があっても
//! `journal directory not found` で即終了し、UI 側は connection refused に
//! なっていた。

use std::fs;
use std::process::{Command, Stdio};
use std::time::Duration;

/// このテスト専用の listen アドレス。他の統合テストのポート帯
/// (5030x/5040x/5850x)と衝突しない値。
const DAEMON_ADDR: &str = "127.0.0.1:58503";

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
fn daemon_reads_journal_dir_from_config_json_when_no_cli_arg() {
    let tmp = tempfile::tempdir().unwrap();
    let journal_dir = tmp.path().join("journal");
    fs::create_dir_all(&journal_dir).unwrap();

    // XDG_CONFIG_HOME/edlr/config.json に journalDir を書く。HOME は
    // Proton 既定パスを含まない空ディレクトリにし、自動検出では絶対に
    // 解決できない状況を作る -- これで「起動できた = config.json を読んだ」
    // と言える。
    let config_home = tmp.path().join("config");
    let home = tmp.path().join("home");
    fs::create_dir_all(config_home.join("edlr")).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(
        config_home.join("edlr").join("config.json"),
        serde_json::json!({ "journalDir": journal_dir }).to_string(),
    )
    .unwrap();

    let mut daemon = DaemonGuard(
        Command::new(env!("CARGO_BIN_EXE_edlr"))
            .arg("--listen")
            .arg(DAEMON_ADDR)
            .arg("--state-dir")
            .arg(tmp.path().join("state"))
            .env("XDG_CONFIG_HOME", &config_home)
            .env("HOME", &home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn edlr daemon binary"),
    );

    let mut listening = false;
    for _ in 0..200 {
        // デーモンが journal ディレクトリを解決できず exit(1) した場合は
        // ここで即座に検出する(ポート待ちの 4 秒を無駄に待たない)。
        if let Some(status) = daemon.0.try_wait().unwrap() {
            panic!(
                "daemon exited ({status}) instead of starting; \
                 it likely failed to read journalDir from config.json"
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
        "daemon did not start listening; journalDir from config.json was not used"
    );
}

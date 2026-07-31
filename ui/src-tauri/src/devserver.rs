//! デバッグビルド専用: vite dev サーバ(フロントエンド)の道連れ起動。
//!
//! デバッグビルドの Tauri はフロントエンドを `devUrl`(`http://localhost:5173`)
//! から読み込むが、vite を起動するのは `tauri dev` の `beforeDevCommand` だけ
//! なので、素の `cargo run -p edlr-ui` では WebView が 5173 への接続拒否を
//! 表示していた。デーモンの道連れ起動(`autostart_daemon`)と同じ流儀で、
//! 5173 に誰もいなければここで vite を spawn し、終了時に片付ける。

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// vite dev サーバの listen アドレス(`tauri.conf.json` の `devUrl` と対)。
pub const DEV_SERVER_ADDR: &str = "127.0.0.1:5173";

/// vite の起動を待つ上限。pnpm の起動 + vite の立ち上がりを見込む。
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// 停止時に SIGTERM から SIGKILL へ昇格するまでの猶予。vite は SIGTERM で
/// 素直に終了するので、通常はこの猶予いっぱい待つことはない。
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// リポジトリ内のフロントエンドディレクトリ(`ui/frontend`)。
///
/// `CARGO_MANIFEST_DIR` はコンパイル時に焼き込まれるため、どのカレント
/// ディレクトリから `cargo run -p edlr-ui` されても同じ場所を指す。
/// dev fallback のデーモンバイナリ解決(`resolve_bin`)と同じ考え方。
pub fn frontend_dir() -> PathBuf {
    let raw = Path::new(env!("CARGO_MANIFEST_DIR")).join("../frontend");
    // `..` を含んだままでも動くが、テスト・ログでの見やすさのために
    // 正規化できるならしておく(失敗したらそのまま使う)。
    raw.canonicalize().unwrap_or(raw)
}

/// vite を起動するコマンドを組み立てる(`pnpm --dir <frontend> dev`)。
///
/// `tauri dev` の `beforeDevCommand` と同じ内容を、cwd に依存しない形で
/// 実行する。純関数にしてあるのはテストのため。
pub fn dev_server_command(frontend_dir: &Path) -> (&'static str, Vec<OsString>) {
    (
        "pnpm",
        vec![
            OsString::from("--dir"),
            frontend_dir.as_os_str().to_os_string(),
            OsString::from("dev"),
        ],
    )
}

/// 自分専用のプロセスグループで子を spawn する。
///
/// vite は `pnpm` → `node` の孫プロセスとして動くため、停止時に pnpm の
/// PID にだけシグナルを送ると node が孤児として 5173 を握り続けうる。
/// spawn 時にプロセスグループを分けておき、停止は `killpg` で行う。
pub fn spawn_in_own_process_group(program: &OsStr, args: &[OsString]) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;
    Command::new(program).args(args).process_group(0).spawn()
}

/// `addr` が listen し始めるまで待つ。`attempts` 回 × `delay` 間隔。
pub fn wait_until_listening(addr: &str, attempts: u32, delay: Duration) -> bool {
    for _ in 0..attempts {
        if crate::daemon::daemon_running(addr) {
            return true;
        }
        std::thread::sleep(delay);
    }
    false
}

/// dev サーバをプロセスグループごと停止する。SIGTERM → `grace` 待ち →
/// SIGKILL の順(`daemon::stop_child_gracefully` の killpg 版)。
pub fn stop_dev_server(child: &mut Child, grace: Duration) {
    let pgid = child.id() as libc::pid_t;
    // SAFETY: `child.id()` は自分が spawn した(まだ wait していない)子の
    // PID であり、`process_group(0)` により PGID と一致する。シグナル送信のみ。
    let sigterm_sent = unsafe { libc::killpg(pgid, libc::SIGTERM) } == 0;
    if sigterm_sent {
        let deadline = Instant::now() + grace;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {}
                Err(_) => break,
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // グループ内の生き残り(孫)ごと確実に片付ける。子が既に死んでいても
    // killpg は残りのメンバーに届く(全員死んでいれば ESRCH で無害)。
    // SAFETY: 上と同じ。自分の子のプロセスグループへのシグナル送信のみ。
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
    let _ = child.wait();
}

/// 5173 に誰もいなければ vite を spawn し、応答し始めるまで待つ。
///
/// 戻り値の `Child` は呼び出し元が保持し、終了時に `stop_dev_server` で
/// 片付けること。既に(`tauri dev` や手動起動の)vite がいる場合と、
/// spawn に失敗した場合(pnpm 不在など)は `None` を返し、起動は続行する
/// -- 後者は従来どおり WebView に接続エラーが出るだけで、デーモン運用には
/// 影響させない。
pub fn ensure_dev_server() -> Option<Child> {
    if crate::daemon::daemon_running(DEV_SERVER_ADDR) {
        eprintln!("vite dev server already running on {DEV_SERVER_ADDR}; leaving it alone");
        return None;
    }
    let dir = frontend_dir();
    let (program, args) = dev_server_command(&dir);
    let child = match spawn_in_own_process_group(OsStr::new(program), &args) {
        Ok(child) => child,
        Err(e) => {
            eprintln!("failed to spawn vite dev server ({program} --dir {} dev): {e}; the window will fail to load until you start it yourself", dir.display());
            return None;
        }
    };
    eprintln!(
        "spawned vite dev server (pid {}) for {}",
        child.id(),
        dir.display()
    );
    crate::signals::set_dev_server_pgid(child.id());
    let attempts = (START_TIMEOUT.as_millis() / 200) as u32;
    if !wait_until_listening(DEV_SERVER_ADDR, attempts, Duration::from_millis(200)) {
        eprintln!("vite dev server did not start listening on {DEV_SERVER_ADDR} within {START_TIMEOUT:?}; continuing anyway");
    }
    Some(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn frontend_dir_points_at_ui_frontend() {
        let dir = frontend_dir();
        assert!(
            dir.ends_with("ui/frontend"),
            "unexpected frontend dir: {dir:?}"
        );
        assert!(dir.is_dir(), "frontend dir must exist in the repo: {dir:?}");
    }

    #[test]
    fn dev_server_command_runs_pnpm_dev_against_the_frontend_dir() {
        let dir = std::path::Path::new("/repo/ui/frontend");
        let (program, args) = dev_server_command(dir);
        assert_eq!(program, "pnpm");
        assert_eq!(
            args,
            vec![
                std::ffi::OsString::from("--dir"),
                std::ffi::OsString::from("/repo/ui/frontend"),
                std::ffi::OsString::from("dev"),
            ]
        );
    }

    #[test]
    fn wait_until_listening_detects_a_listener_and_gives_up_without_one() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        assert!(wait_until_listening(&addr, 5, Duration::from_millis(10)));

        // 負のケースは「bind して閉じた直後のポート」を使ってはいけない:
        // 並行して走る他のテストの `bind("127.0.0.1:0")` が同じポートを
        // 再取得した瞬間に接続が成功してしまう(実際に flaky として踏んだ)。
        // ephemeral port range(Linux 既定 32768-60999)の外にある固定ポート
        // なら `bind(0)` に割り当てられることもないので、bind せずにそのまま
        // 使う。他の統合テストの固定ポート帯(285xx/286xx/5030x/5040x)とも
        // 被らない値。
        assert!(!wait_until_listening(
            "127.0.0.1:29993",
            2,
            Duration::from_millis(10)
        ));
    }

    /// vite は `pnpm` → `node` の孫プロセスとして動く。pnpm の PID にだけ
    /// SIGTERM を送ると node が孤児として 5173 を握り続けうるので、停止は
    /// プロセスグループごと行う。ここではその挙動を sh スクリプトの
    /// 孫プロセスで再現して検証する。
    #[test]
    fn stop_dev_server_kills_the_whole_process_group() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("grandchild.pid");
        let script = tmp.path().join("parent.sh");
        std::fs::write(
            &script,
            format!("(sleep 60 & echo $! > {}); wait\n", pidfile.display()),
        )
        .unwrap();

        // 書いたばかりのファイルを直接 exec すると、並列に走る他テストの
        // fork が書き込み fd を一瞬引き継いだ場合に ETXTBSY で落ちる
        // (issue aenf)。`/bin/sh <script>` の形なら exec するのは既存の
        // sh バイナリだけなので、この競合自体が起きない。
        let mut child = spawn_in_own_process_group(
            std::ffi::OsStr::new("/bin/sh"),
            &[script.as_os_str().to_os_string()],
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while !pidfile.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let grandchild: libc::pid_t = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        stop_dev_server(&mut child, Duration::from_millis(500));

        // SAFETY: existence check only, no signal sent.
        let parent_alive = unsafe { libc::kill(child.id() as libc::pid_t, 0) } == 0;
        assert!(!parent_alive, "parent must be dead");

        // 孫は殺された後もゾンビとして(init に回収されるまで)一瞬残り、
        // `kill(pid, 0)` はゾンビに対しても成功する。回収までポーリングで待つ。
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // SAFETY: existence check only, no signal sent.
            let grandchild_alive = unsafe { libc::kill(grandchild, 0) } == 0;
            if !grandchild_alive {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "grandchild must be dead (process group kill)"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

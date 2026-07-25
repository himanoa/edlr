use std::io;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

pub const DAEMON_ADDR: &str = "127.0.0.1:8137";

/// addr に TCP 接続できればデーモン生存とみなす。
pub fn daemon_running(addr: &str) -> bool {
    match addr.parse::<SocketAddr>() {
        Ok(a) => TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok(),
        Err(_) => false,
    }
}

/// PATH から実行ファイルを探す。
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// 探索順: env_bin(無条件)→ exe_dir の edlr → PATH ヒット → dev fallback(実在時のみ)。
/// PATH 探索・環境変数の読み取りは呼び出し側で行い、ここは順序決定のみを担う。
pub fn resolve_edlr_bin(
    env_bin: Option<PathBuf>,
    exe_dir: Option<&Path>,
    path_hit: Option<PathBuf>,
    dev_fallback: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(p) = env_bin {
        return Some(p);
    }
    if let Some(dir) = exe_dir {
        let candidate = dir.join("edlr");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Some(p) = path_hit {
        return Some(p);
    }
    dev_fallback.filter(|p| p.is_file())
}

/// デーモンを子プロセスとして起動する(stdout/stderr は継承)。
pub fn spawn_daemon(bin: &Path, journal_dir: Option<&Path>) -> io::Result<Child> {
    let mut cmd = Command::new(bin);
    if let Some(dir) = journal_dir {
        cmd.arg("--journal-dir").arg(dir);
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn daemon_running_detects_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        assert!(daemon_running(&addr));
        drop(listener);
        assert!(!daemon_running(&addr));
        assert!(!daemon_running("not an addr"));
    }

    fn make_exec(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn resolve_prefers_env_bin_unconditionally() {
        let p = std::path::PathBuf::from("/nonexistent/edlr");
        assert_eq!(resolve_edlr_bin(Some(p.clone()), None, None, None), Some(p));
    }

    #[test]
    fn resolve_order_is_exe_dir_then_path_then_dev_fallback() {
        let exe_dir = tempfile::tempdir().unwrap();
        let sibling = make_exec(exe_dir.path(), "edlr");
        let path_hit = std::path::PathBuf::from("/from/path/edlr");
        let dev = tempfile::tempdir().unwrap();
        let dev_bin = make_exec(dev.path(), "edlr");

        // exe_dir の edlr が最優先
        assert_eq!(
            resolve_edlr_bin(
                None,
                Some(exe_dir.path()),
                Some(path_hit.clone()),
                Some(dev_bin.clone())
            ),
            Some(sibling)
        );
        // exe_dir に無ければ PATH ヒット
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_edlr_bin(
                None,
                Some(empty.path()),
                Some(path_hit.clone()),
                Some(dev_bin.clone())
            ),
            Some(path_hit)
        );
        // PATH にも無ければ dev fallback(実在する場合のみ)
        assert_eq!(
            resolve_edlr_bin(None, Some(empty.path()), None, Some(dev_bin.clone())),
            Some(dev_bin)
        );
        assert_eq!(
            resolve_edlr_bin(
                None,
                Some(empty.path()),
                None,
                Some(std::path::PathBuf::from("/nonexistent"))
            ),
            None
        );
    }

    #[test]
    fn find_in_path_finds_sh() {
        let sh = find_in_path("sh").expect("sh should be on PATH");
        assert!(sh.is_file());
        assert_eq!(find_in_path("edlr-definitely-not-a-real-binary"), None);
    }

    #[test]
    fn spawn_daemon_passes_journal_dir_and_child_can_be_killed() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-edlr");
        // 引数をファイルに書いてから sleep する偽デーモン
        fs::write(
            &script,
            "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args.txt\"\nsleep 30\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let jdir = dir.path().join("journal");
        let mut child = spawn_daemon(&script, Some(&jdir)).unwrap();
        // args.txt が書かれるまで少し待つ
        for _ in 0..50 {
            if dir.path().join("args.txt").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let args = fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert!(args.contains("--journal-dir"));
        assert!(args.contains("journal"));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

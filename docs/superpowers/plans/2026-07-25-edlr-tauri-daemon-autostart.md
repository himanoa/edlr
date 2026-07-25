# Tauri デーモン自動起動 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tauri アプリ起動時に edlr デーモンを自動 spawn し、アプリ終了時に道連れ終了する(外部起動済みデーモンには不干渉)。

**Architecture:** `ui/src-tauri/src/daemon.rs` に生存確認・バイナリ探索・spawn を実装し、`main.rs` で起動時 spawn + `RunEvent::Exit` で kill。設計書: `docs/superpowers/specs/2026-07-25-edlr-tauri-daemon-autostart-design.md`

**Tech Stack:** Rust std のみ(`std::net::TcpStream`, `std::process::Command`)。tauri プラグイン追加なし。

## Global Constraints

- 既に `127.0.0.1:8137` が生きている場合は spawn しない・終了時に kill しない
- spawn 失敗・バイナリ不発見でも panic せず、stderr ログのみでウィンドウは表示する
- `ui/src-tauri` は独立 workspace(ルートの `cargo test --workspace` に影響しない)
- テストは `ui/src-tauri` 内で `cargo test`(システム依存導入済みの環境で実行可能)
- コミットメッセージ末尾に `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

---

### Task 1: daemon モジュール(生存確認・探索・spawn + ユニットテスト)

**Files:**
- Create: `ui/src-tauri/src/daemon.rs`
- Modify: `ui/src-tauri/src/main.rs`(`mod daemon;` 追加のみ、結線は Task 2)

**Interfaces:**
- Produces: `pub const DAEMON_ADDR: &str = "127.0.0.1:8137"` / `pub fn daemon_running(addr: &str) -> bool` / `pub fn find_in_path(name: &str) -> Option<PathBuf>` / `pub fn resolve_edlr_bin(env_bin: Option<PathBuf>, exe_dir: Option<&Path>, path_hit: Option<PathBuf>, dev_fallback: Option<PathBuf>) -> Option<PathBuf>` / `pub fn spawn_daemon(bin: &Path, journal_dir: Option<&Path>) -> io::Result<Child>`

- [ ] **Step 1: 失敗するテストを書く**(`ui/src-tauri/src/daemon.rs` 内 `#[cfg(test)]`)

```rust
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
        assert_eq!(
            resolve_edlr_bin(Some(p.clone()), None, None, None),
            Some(p)
        );
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
            resolve_edlr_bin(None, Some(exe_dir.path()), Some(path_hit.clone()), Some(dev_bin.clone())),
            Some(sibling)
        );
        // exe_dir に無ければ PATH ヒット
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_edlr_bin(None, Some(empty.path()), Some(path_hit.clone()), Some(dev_bin.clone())),
            Some(path_hit)
        );
        // PATH にも無ければ dev fallback(実在する場合のみ)
        assert_eq!(
            resolve_edlr_bin(None, Some(empty.path()), None, Some(dev_bin.clone())),
            Some(dev_bin)
        );
        assert_eq!(
            resolve_edlr_bin(None, Some(empty.path()), None, Some(std::path::PathBuf::from("/nonexistent"))),
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
        fs::write(&script, "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args.txt\"\nsleep 30\n").unwrap();
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
```

`ui/src-tauri/Cargo.toml` の `[dev-dependencies]` に `tempfile = "3"` を追加する。

- [ ] **Step 2: 失敗確認**

Run: `cd ui/src-tauri && cargo test`
Expected: FAIL(関数未定義のコンパイルエラー)

- [ ] **Step 3: 実装する**(`ui/src-tauri/src/daemon.rs` 冒頭に実装、`main.rs` に `mod daemon;`)

```rust
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
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cd ui/src-tauri && cargo test`
Expected: PASS(5 テスト)

- [ ] **Step 5: Commit**

```bash
git add ui/src-tauri && git commit -m "feat(ui): daemon liveness check, binary resolution, and spawn helpers"
```

---

### Task 2: main.rs 結線(起動時 spawn + Exit で kill)と手動スモーク

**Files:**
- Modify: `ui/src-tauri/src/main.rs`
- Modify: `README.md`(UI セクションに 1 行追記)

**Interfaces:**
- Consumes: `daemon::{DAEMON_ADDR, daemon_running, find_in_path, resolve_edlr_bin, spawn_daemon}`

- [ ] **Step 1: `main.rs` を書き換える**

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;

use std::path::PathBuf;

/// 未起動ならデーモンを spawn して Child を返す。起動済み・失敗時は None。
fn autostart_daemon() -> Option<std::process::Child> {
    if daemon::daemon_running(daemon::DAEMON_ADDR) {
        eprintln!("edlr daemon already running on {}; leaving it alone", daemon::DAEMON_ADDR);
        return None;
    }
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from));
    // 開発ビルドのみ: リポジトリ内の target/debug/edlr を最後の候補にする
    let dev_fallback = if cfg!(debug_assertions) {
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/edlr"))
    } else {
        None
    };
    let bin = daemon::resolve_edlr_bin(
        std::env::var_os("EDLR_BIN").map(PathBuf::from),
        exe_dir.as_deref(),
        daemon::find_in_path("edlr"),
        dev_fallback,
    );
    let Some(bin) = bin else {
        eprintln!("edlr binary not found (set EDLR_BIN or put edlr on PATH); starting UI without daemon");
        return None;
    };
    let journal_dir = std::env::var_os("EDLR_JOURNAL_DIR").map(PathBuf::from);
    match daemon::spawn_daemon(&bin, journal_dir.as_deref()) {
        Ok(child) => {
            eprintln!("spawned edlr daemon (pid {}) from {}", child.id(), bin.display());
            Some(child)
        }
        Err(e) => {
            eprintln!("failed to spawn edlr daemon from {}: {e}", bin.display());
            None
        }
    }
}

fn main() {
    // ウィンドウを出してフロントエンドを表示する薄い皮 + デーモンの道連れ起動。
    // 既に起動済みのデーモンには spawn も kill もしない。
    let mut child = autostart_daemon();
    tauri::Builder::default()
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        });
}
```

- [ ] **Step 2: ビルドとテスト**

Run: `cd ui/src-tauri && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt`
Expected: 全 PASS・警告ゼロ

- [ ] **Step 3: 手動スモーク(2 ケース)**

前提: ポート 8137 で何も listen していないこと(コントローラが既存プロセスを停止済み)。
`cargo build -p edlr-core` でワークスペース側の `target/debug/edlr` を最新にしておく。

1. **未起動 → spawn → 道連れ**: リポジトリルートで `EDLR_JOURNAL_DIR=<スクラッチの空ディレクトリ> timeout 25 pnpm dlx @tauri-apps/cli@^2 dev`(ui/src-tauri で実行)をバックグラウンド起動し、15 秒後に `pgrep -f 'edlr --journal-dir'`(または `ss -ltn | grep 8137`)でデーモン起動を確認。その後アプリプロセスを SIGTERM で終了させ、数秒後にデーモンプロセスも消えていることを確認
2. **起動済み → 不干渉**: 手動で `target/debug/edlr --journal-dir <dir>` を起動してから同様にアプリを起動し、「already running」ログが出ること・アプリ終了後もデーモンが生きていることを確認し、最後に手動デーモンを kill

結果(コマンドと観測)をレポートに記録する。

- [ ] **Step 4: README 追記**

`README.md` の UI セクション末尾に追記:

```markdown
    # Tauri アプリはデーモン未起動なら自動で spawn し、終了時に道連れで止める。
    # 既に起動済みのデーモンには手を出さない。EDLR_BIN / EDLR_JOURNAL_DIR で上書き可。
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(ui): auto-start edlr daemon from tauri app, kill on exit"
```

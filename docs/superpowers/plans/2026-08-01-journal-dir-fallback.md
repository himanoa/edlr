# journal dir フォールバック自動作成 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** journal dir が CLI 引数・config.json・Proton 自動検出のどれでも解決できないとき、デーモンが `$XDG_DATA_HOME/edlr/journal`(なければ `~/.local/share/edlr/journal`)を作成して使うようにし、初回起動が `exit(1)` で失敗しないようにする。

**Architecture:** パス計算は `edlr-config` crate の純粋関数 `fallback_journal_dir` に置き(mkdir はしない)、ディレクトリ作成という副作用は `core/src/bin/edlr.rs`(命令的側)で行う。既存の `daemon_journal_dir` のシグネチャ・挙動は変えない。

**Tech Stack:** Rust。テストは config crate の純粋テスト + core の統合テスト(実プロセス spawn)。

**Spec:** `docs/superpowers/specs/2026-08-01-journal-dir-fallback-design.md`

## Global Constraints

- フォールバックパス: `$XDG_DATA_HOME/edlr/journal`、`$XDG_DATA_HOME` が未設定/空文字列なら `<home>/.local/share/edlr/journal`、home も無ければフォールバック不能(`None`)
- CLI/config/自動検出で解決したパスが存在しない場合の `exit(1)` は従来どおり維持(自動作成の対象はフォールバックパスのみ)
- `.claude/rules/` 遵守: 純粋モジュール(config crate)に `std::fs` を持ち込まない・値の組み立てに `mut` を使わない・既存統合テストは消さない
- テスト実行前に `cargo fetch` 済みであること(CLAUDE.md の並列ビルド注意。単独実行なら不要)

---

### Task 1: `edlr-config` に純粋関数 `fallback_journal_dir` を追加

**Files:**
- Modify: `config/src/lib.rs`(`default_journal_dir` の直後、64行目付近に関数を追加。テストは既存の `mod tests` 内、`state_base` 系テストの近くに追加)

**Interfaces:**
- Consumes: なし(既存コードへの依存は `std::path::{Path, PathBuf}` のみ)
- Produces: `pub fn fallback_journal_dir(xdg_data_home: Option<&Path>, home: Option<&Path>) -> Option<PathBuf>` — Task 2 が `config::fallback_journal_dir` として呼ぶ

- [ ] **Step 1: 失敗するテストを書く**

`config/src/lib.rs` の `#[cfg(test)] mod tests` 内(`state_base_without_home_is_relative_to_the_current_directory` の後ろ)に追加:

```rust
    #[test]
    fn fallback_journal_dir_prefers_xdg_data_home() {
        let dir = fallback_journal_dir(Some(Path::new("/x/data")), Some(Path::new("/home/u")));
        assert_eq!(dir, Some(PathBuf::from("/x/data/edlr/journal")));
    }

    #[test]
    fn fallback_journal_dir_falls_back_to_local_share_under_home() {
        let dir = fallback_journal_dir(None, Some(Path::new("/home/u")));
        assert_eq!(dir, Some(PathBuf::from("/home/u/.local/share/edlr/journal")));
    }

    #[test]
    fn fallback_journal_dir_treats_empty_xdg_data_home_as_unset() {
        let dir = fallback_journal_dir(Some(Path::new("")), Some(Path::new("/home/u")));
        assert_eq!(dir, Some(PathBuf::from("/home/u/.local/share/edlr/journal")));
    }

    #[test]
    fn fallback_journal_dir_none_when_nothing_available() {
        assert_eq!(fallback_journal_dir(None, None), None);
        // 空 XDG + home なしもフォールバック不能
        assert_eq!(fallback_journal_dir(Some(Path::new("")), None), None);
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-config fallback_journal_dir`
Expected: コンパイルエラー `cannot find function fallback_journal_dir`

- [ ] **Step 3: 最小実装を書く**

`config/src/lib.rs` の `default_journal_dir`(62-65行目)の直後に追加:

```rust
/// journal ディレクトリの最終フォールバックパスを組み立てる。
///
/// CLI 引数・config.json・[`default_journal_dir`] の自動検出のどれでも
/// 解決できなかったときに、デーモンが「作成して使う」場所。パス計算のみで
/// ディレクトリの作成はしない(作成は `edlr` バイナリ側の仕事)。
///
/// `$XDG_DATA_HOME` が Some かつ非空ならそれを、そうでなければ
/// `<home>/.local/share` をデータベースディレクトリとして
/// `<base>/edlr/journal` を返す。home も無ければ `None`
/// (`config_base` と違いカレントディレクトリには落とさない —
/// 勝手に作る対象が CWD 相対になるのは危険なため)。
pub fn fallback_journal_dir(
    xdg_data_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    match (xdg_data_home, home) {
        (Some(data_home), _) if !data_home.as_os_str().is_empty() => {
            Some(data_home.join("edlr").join("journal"))
        }
        (_, Some(home)) => Some(
            home.join(".local")
                .join("share")
                .join("edlr")
                .join("journal"),
        ),
        _ => None,
    }
}
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-config`
Expected: 全テスト PASS(新規 4 本 + 既存に回帰なし)

- [ ] **Step 5: コミット**

```bash
git add config/src/lib.rs
git commit -m "feat(config): journal dir の最終フォールバックパス計算を追加"
```

---

### Task 2: デーモン起動時にフォールバックディレクトリを作成して使う

**Files:**
- Modify: `core/src/bin/edlr.rs:100-112`(journal dir 解決部)
- Create: `core/tests/daemon_journal_fallback_integration.rs`

**Interfaces:**
- Consumes: `config::fallback_journal_dir(Option<&Path>, Option<&Path>) -> Option<PathBuf>`(Task 1)、既存の `config::daemon_journal_dir`
- Produces: なし(バイナリの挙動変更のみ)

- [ ] **Step 1: 失敗する統合テストを書く**

`core/tests/daemon_journal_fallback_integration.rs` を新規作成。
ポートは既存統合テスト(28501-28503)と衝突しない 28504 を使う:

```rust
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
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-core --test daemon_journal_fallback_integration`
Expected: FAIL — `daemon exited (exit status: 1) instead of starting`

- [ ] **Step 3: `edlr.rs` の解決部を書き換える**

`core/src/bin/edlr.rs` の現在の 100-112 行目:

```rust
    let dir = config::daemon_journal_dir(args.journal_dir, configured, home.as_deref());
    let Some(dir) = dir else {
        eprintln!(
            "error: journal directory not found; specify one with --journal-dir <PATH> \
             or set journalDir in config.json"
        );
        std::process::exit(1);
    };

    if !dir.is_dir() {
        eprintln!("error: journal directory does not exist: {}", dir.display());
        std::process::exit(1);
    }
```

を以下へ置き換える(判断は `daemon_journal_dir` / `fallback_journal_dir` の
純関数側にあり、ここは結果に従って検証・作成するだけ):

```rust
    // CLI/config/自動検出で解決できたパスは存在検証のみ(設定ミスを勝手に
    // 直さない)。どれでも解決できない場合だけ、フォールバックパスを
    // 作成して使う — 新規環境で journal dir 未設定なだけでデーモンが
    // exit(1) すると、初回の UI 起動が必ずデーモン不在で始まるため。
    let dir = match config::daemon_journal_dir(args.journal_dir, configured, home.as_deref()) {
        Some(dir) => {
            if !dir.is_dir() {
                eprintln!("error: journal directory does not exist: {}", dir.display());
                std::process::exit(1);
            }
            dir
        }
        None => {
            let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
            let Some(dir) =
                config::fallback_journal_dir(xdg_data_home.as_deref(), home.as_deref())
            else {
                eprintln!(
                    "error: journal directory not found; specify one with --journal-dir <PATH> \
                     or set journalDir in config.json"
                );
                std::process::exit(1);
            };
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!(
                    "error: failed to create fallback journal directory {}: {e}",
                    dir.display()
                );
                std::process::exit(1);
            }
            tracing::info!("using fallback journal directory {}", dir.display());
            dir
        }
    };
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core --test daemon_journal_fallback_integration`
Expected: PASS

- [ ] **Step 5: 既存テストに回帰がないことを確認**

Run: `cargo test -p edlr-core --test daemon_config_journal_integration && cargo test -p edlr-core --bins`
Expected: PASS(config.json 経由の解決・CLI 引数の挙動は不変)

- [ ] **Step 6: コミット**

```bash
git add core/src/bin/edlr.rs core/tests/daemon_journal_fallback_integration.rs
git commit -m "feat(core): journal dir 未解決時にフォールバックを作成して起動する"
```

---

### Task 3: ドキュメント更新

**Files:**
- Modify: `docs/cli.md`(`--journal-dir` の解決順の記述)
- Modify: `README.md`(「If `--journal-dir` is omitted, edlr looks for the default Proton journal path.」の段落)

**Interfaces:**
- Consumes: なし
- Produces: なし

- [ ] **Step 1: `docs/cli.md` の解決順を更新**

`--journal-dir` の解決順の記述(「CLI 引数 → config.json → Proton 自動検出、
どれも無ければエラー終了」相当の箇所)を探し、最終段を追記する:

解決順: CLI 引数 → config.json の `journalDir` → Proton 既定パスの自動検出 →
`$XDG_DATA_HOME/edlr/journal`(`XDG_DATA_HOME` 未設定なら
`~/.local/share/edlr/journal`)を作成して使用。
「journal directory not found でエラー終了する」という記述が残っていれば
「HOME も XDG_DATA_HOME も無い場合のみ」に限定する。

- [ ] **Step 2: `README.md` の該当段落を更新**

「If `--journal-dir` is omitted, edlr looks for the default Proton journal
path.」の文に、見つからない場合はフォールバックディレクトリ
(`$XDG_DATA_HOME/edlr/journal`, default `~/.local/share/edlr/journal`)を
作成して使う旨を一文追記する。

- [ ] **Step 3: コミット**

```bash
git add docs/cli.md README.md
git commit -m "docs: journal dir のフォールバック自動作成を記載"
```

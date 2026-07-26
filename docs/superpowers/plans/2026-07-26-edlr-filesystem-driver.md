# ファイルアクセス capability(driver-fs)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** プラグインが `manifest.toml` で宣言し、ユーザーがディレクトリを割り当てて承認すると、そのディレクトリ配下に限ってファイルを読み書きできるようにする。

**Architecture:** 新クレート `drivers/fs` が「承認済みルート配下で安全にファイルを操作する能力」(パス検証 3 段・原子的書き込み・サイズ上限)を持ち、`core` は既存の capability 機構(manifest 宣言 → ユーザー設定 → 承認 → 呼び出し時照合)に載せる。`drivers/http` / `drivers/process` と対称。

**Tech Stack:** Rust 2021 / rustix(`openat2`)/ wasmtime component model + WIT / axum WebSocket RPC / React + TypeScript + vitest / Tauri 2。

**設計書:** `docs/superpowers/specs/2026-07-26-edlr-filesystem-driver-design.md`

## Global Constraints

- Rust edition 2021。ワークスペースの `members` に `drivers/fs` を追加する
- 新規依存は `rustix`(1.x、feature `fs`)のみ。非同期ランタイムを `drivers/fs` に持ち込まない(呼び出し元はプラグイン専用の同期 OS スレッド)
- ドキュメントコメントは既存コードにならい日本語。セキュリティ上の判断根拠は英語でも可
- **パス検証は 3 段**(構文 → 正規化後の配下チェック → `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`)。`openat2` が使えない環境では「正規化 + 配下チェック + `O_NOFOLLOW`」にフォールバックするが、1・2 段目は必ず通る
- **`RESOLVE_NO_SYMLINKS` はルート内のシンボリックリンクも拒否する**(外を指すものだけでなく)。これは意図した制約で、ドキュメントに明記する
- 書き込みは tmp + `rename` で原子的に行う。`append` のみ非原子的(追記オープン)
- `read` / `read-range` は 1 回 8 MiB まで。`write` / `append` にホスト由来の上限は設けない
- `list` は `prefix` 配下を再帰的に列挙しファイルのみ返す。上限 10,000 件、超過は `too-large`
- `mode = "read"` の要求からの `write` / `append` / `delete` は `permission-denied`(判定は core 側。`drivers/fs` は grants を知らない)
- 承認・取消は次の呼び出しから即座に効く。`path` 未設定では承認できない(`Registry` 側で強制)
- テスト実行: Rust は `cargo test`、フロントエンドは `cd ui/frontend && mise exec -- pnpm test`、Tauri 側は `cd ui/src-tauri && cargo test`
- 各タスクの最後にコミットする(Conventional Commits)

## File Structure

**新規**

| ファイル | 責務 |
|---|---|
| `drivers/fs/Cargo.toml` | `edlr-driver-fs` クレート定義 |
| `drivers/fs/src/lib.rs` | `FsDriver` — 各操作の入口、サイズ上限、原子的書き込み |
| `drivers/fs/src/path.rs` | パス検証(構文チェック + 配下チェック)。ここだけで完結させる |
| `drivers/fs/src/openat.rs` | `openat2` ラッパとフォールバック。プラットフォーム依存をここに閉じる |
| `core/src/plugin/filesystem.rs` | `FilesystemConfig` / `FilesystemConfigStore`(パス検証込み) |
| `core/src/plugin/fs_runtime.rs` | `filesystem_json` 共有バッファの組み立て/解釈 |
| `core/tests/driver_fs_integration.rs` | Registry 経由の設定・承認・脱出拒否の統合テスト |
| `ui/frontend/src/components/FilesystemSection.tsx` | ファイルアクセスの設定・承認 UI |
| `ui/frontend/src/components/FilesystemSection.test.tsx` | 同テスト |

**変更**

| ファイル | 変更内容 |
|---|---|
| `core/wit/plugin.wit` | `interface driver-fs` 追加、`world plugin` に `import driver-fs;` |
| `core/src/plugin/manifest.rs` | `[[filesystem]]` のパース・検証・フィンガープリント |
| `core/src/plugin/grants.rs` | エントリ単位の grant(`SavedGrant` に `filesystem` を追加、後方互換) |
| `core/src/plugin/host.rs` | `DriverFsHost` 実装、`HostCtx` に `filesystem_json` と `fs_driver` |
| `core/src/plugin/registry.rs` | 設定/承認 API、`PluginInfo` に filesystem 情報 |
| `core/src/plugin/runner.rs` | 起動時の `filesystem_json` 構築 |
| `core/src/plugin/mod.rs` | 新モジュールの `pub mod` / re-export |
| `core/src/server.rs` | RPC 3 メソッド + `plugins/list` への `filesystem` 追加 |
| `core/src/bin/edlr.rs` | `FilesystemConfigStore` を組み立てて `start_plugins` へ渡す |
| `ui/frontend/src/types/plugin.ts` | `FilesystemRoot` 型 |
| `ui/frontend/src/pages/Plugins.tsx` | `FilesystemSection` の配線 |
| `ui/src-tauri/src/main.rs` | `pick_directory` コマンド(`pick_journal_dir` の一般化) |
| `README.md` | ファイルアクセス capability の節 |

---

### Task 1: `drivers/fs` — パス検証

**Files:**
- Create: `drivers/fs/Cargo.toml`, `drivers/fs/src/lib.rs`, `drivers/fs/src/path.rs`
- Modify: `Cargo.toml`(ワークスペース `members`)

**Interfaces:**
- Consumes: なし(最初のタスク)
- Produces:
  - `edlr_driver_fs::FsError::{InvalidPath(String), NotFound(String), TooLarge(String), Io(String)}`(`Display + Error`)
  - `edlr_driver_fs::path::validate_relative(rel: &str) -> Result<Vec<String>, FsError>` — 構文検証を通った要素列を返す
  - `edlr_driver_fs::path::resolve_existing(root: &Path, rel: &str) -> Result<PathBuf, FsError>` — 既存パスを正規化し、ルート配下であることを確認して返す
  - `edlr_driver_fs::path::resolve_parent(root: &Path, rel: &str) -> Result<(PathBuf, String), FsError>` — 書き込み用。親ディレクトリを(必要なら作成しつつ)解決し、`(親の絶対パス, ファイル名)` を返す

- [ ] **Step 1: クレートを作りワークスペースに登録する**

`drivers/fs/Cargo.toml`:

```toml
[package]
name = "edlr-driver-fs"
version = "0.1.0"
edition = "2021"

[dependencies]
rustix = { version = "1", features = ["fs"] }

[dev-dependencies]
tempfile = "3"
```

ルート `Cargo.toml` の `members` に `"drivers/fs"` を追加する。

`drivers/fs/src/lib.rs` の先頭に:

```rust
//! 承認済みルートディレクトリ配下に限ってファイルを操作するドライバ。
//!
//! 呼び出し元(edlr のプラグインホスト)は「どのルートか」と「その配下の
//! 相対パス」だけを渡す。ルートの外へ出る経路が無いことをこのクレートが
//! 保証する。承認そのもの(誰がどのルートを使ってよいか)は呼び出し元の
//! 責務で、このクレートは grants を知らない。

pub mod path;

use std::fmt;

#[derive(Debug)]
pub enum FsError {
    /// 構文が不正、またはルート配下から出ている。
    InvalidPath(String),
    NotFound(String),
    TooLarge(String),
    Io(String),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsError::InvalidPath(m) => write!(f, "invalid path: {m}"),
            FsError::NotFound(m) => write!(f, "not found: {m}"),
            FsError::TooLarge(m) => write!(f, "too large: {m}"),
            FsError::Io(m) => write!(f, "io error: {m}"),
        }
    }
}

impl std::error::Error for FsError {}
```

- [ ] **Step 2: 構文検証の失敗するテストを書く**

`drivers/fs/src/path.rs` の末尾に:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_relative_paths_are_accepted() {
        assert_eq!(validate_relative("a.txt").unwrap(), vec!["a.txt".to_string()]);
        assert_eq!(
            validate_relative("logs/2026-07.csv").unwrap(),
            vec!["logs".to_string(), "2026-07.csv".to_string()]
        );
    }

    #[test]
    fn syntactically_dangerous_paths_are_rejected() {
        for bad in [
            "",                 // 空
            "/etc/passwd",      // 絶対パス
            "../secret",        // 親へ
            "a/../../secret",   // 途中で親へ
            "./a",              // カレント
            "a/./b",            // 途中にカレント
            "a//b",             // 空要素
            "a/",               // 末尾スラッシュ
            "a\\b",             // バックスラッシュ
            "a\0b",             // NUL
            "a\nb",             // 制御文字
        ] {
            assert!(
                validate_relative(bad).is_err(),
                "{bad:?} must be rejected by syntax validation"
            );
        }
    }
}
```

- [ ] **Step 3: テストが失敗することを確認する**

Run: `cargo test -p edlr-driver-fs`
Expected: FAIL(`validate_relative` が未定義でコンパイルエラー)

- [ ] **Step 4: 構文検証を実装する**

`drivers/fs/src/path.rs`(テストモジュールの上):

```rust
//! パス検証。**この機能のサンドボックス境界そのもの**なので、
//! ここを緩めると任意のファイルへの読み書きが通ってしまう。
//!
//! 検証は 3 段で、このモジュールは 1 段目(構文)と 2 段目(正規化後の
//! 配下チェック)を担う。3 段目(`openat2` によるカーネルレベルの拘束)は
//! `crate::openat` にある。

use std::path::{Component, Path, PathBuf};

use crate::FsError;

/// 相対パスを構文レベルで検証し、要素列に分解する。
///
/// ファイルシステムに一切触らないので、ここで弾けるものは必ずここで弾く
/// (触ってから判断する経路を減らすほど、競合の余地が減る)。
pub fn validate_relative(rel: &str) -> Result<Vec<String>, FsError> {
    if rel.is_empty() {
        return Err(FsError::InvalidPath("path must not be empty".into()));
    }
    if rel.contains('\0') {
        return Err(FsError::InvalidPath("path must not contain NUL".into()));
    }
    if rel.chars().any(|c| c.is_control()) {
        return Err(FsError::InvalidPath(
            "path must not contain control characters".into(),
        ));
    }
    if rel.contains('\\') {
        return Err(FsError::InvalidPath(
            "path must not contain a backslash".into(),
        ));
    }
    if rel.starts_with('/') {
        return Err(FsError::InvalidPath("path must be relative".into()));
    }

    let mut components = Vec::new();
    for part in rel.split('/') {
        match part {
            "" => {
                return Err(FsError::InvalidPath(
                    "path must not contain empty components".into(),
                ))
            }
            "." | ".." => {
                return Err(FsError::InvalidPath(format!(
                    "path must not contain a {part:?} component"
                )))
            }
            other => components.push(other.to_string()),
        }
    }
    Ok(components)
}
```

- [ ] **Step 5: テストを実行する**

Run: `cargo test -p edlr-driver-fs`
Expected: PASS(2 テスト)

- [ ] **Step 6: 配下チェックの失敗するテストを書く**

`path.rs` の `mod tests` に追記:

```rust
    use std::fs;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn existing_file_inside_the_root_resolves() {
        let dir = root();
        fs::write(dir.path().join("a.txt"), b"hi").unwrap();

        let resolved = resolve_existing(dir.path(), "a.txt").expect("inside the root");
        assert!(resolved.starts_with(dir.path().canonicalize().unwrap()));
    }

    #[test]
    fn symlink_pointing_outside_the_root_is_rejected() {
        let dir = root();
        let outside = root();
        fs::write(outside.path().join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();

        let err = resolve_existing(dir.path(), "link")
            .expect_err("a symlink escaping the root must be rejected");
        assert!(matches!(err, FsError::InvalidPath(_)));
    }

    #[test]
    fn symlinked_directory_component_pointing_outside_is_rejected() {
        let dir = root();
        let outside = root();
        fs::create_dir(outside.path().join("d")).unwrap();
        fs::write(outside.path().join("d").join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path().join("d"), dir.path().join("d")).unwrap();

        let err = resolve_existing(dir.path(), "d/secret")
            .expect_err("a symlinked directory component escaping the root must be rejected");
        assert!(matches!(err, FsError::InvalidPath(_)));
    }

    #[test]
    fn missing_file_reports_not_found_not_invalid_path() {
        let dir = root();
        let err = resolve_existing(dir.path(), "nope.txt").expect_err("missing file");
        assert!(matches!(err, FsError::NotFound(_)));
    }

    #[test]
    fn resolve_parent_creates_intermediate_directories_inside_the_root() {
        let dir = root();
        let (parent, name) =
            resolve_parent(dir.path(), "logs/2026/07.csv").expect("nested write target");
        assert_eq!(name, "07.csv");
        assert!(parent.starts_with(dir.path().canonicalize().unwrap()));
        assert!(parent.is_dir());
    }

    #[test]
    fn resolve_parent_refuses_to_follow_a_symlinked_directory_out_of_the_root() {
        let dir = root();
        let outside = root();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();

        let err = resolve_parent(dir.path(), "escape/evil.txt")
            .expect_err("writing through an escaping symlink must be rejected");
        assert!(matches!(err, FsError::InvalidPath(_)));
    }
```

- [ ] **Step 7: テストが失敗することを確認する**

Run: `cargo test -p edlr-driver-fs`
Expected: FAIL(`resolve_existing` / `resolve_parent` が未定義)

- [ ] **Step 8: 配下チェックを実装する**

`path.rs` に追記:

```rust
/// `root` を正規化する。設定時に一度だけ行い、以後の比較の基準にする。
pub fn canonical_root(root: &Path) -> Result<PathBuf, FsError> {
    root.canonicalize()
        .map_err(|e| FsError::InvalidPath(format!("root is unusable: {e}")))
}

/// `path` が `root`(正規化済み)の配下にあることを確認する。
fn ensure_inside(root: &Path, path: &Path) -> Result<(), FsError> {
    if path == root || path.starts_with(root) {
        return Ok(());
    }
    Err(FsError::InvalidPath(
        "resolved path escapes the granted root".into(),
    ))
}

/// 既存パスを解決する(読み取り・stat・delete 用)。
///
/// `canonicalize` はシンボリックリンクを解決するので、リンクで外を指して
/// いればこの時点で配下チェックに落ちる。存在しない場合は `NotFound`
/// (`InvalidPath` と区別する。呼び出し側が「無い」と「触ってはいけない」を
/// 取り違えないため)。
pub fn resolve_existing(root: &Path, rel: &str) -> Result<PathBuf, FsError> {
    let components = validate_relative(rel)?;
    let root = canonical_root(root)?;

    let mut joined = root.clone();
    for component in &components {
        joined.push(component);
    }

    let resolved = match joined.canonicalize() {
        Ok(resolved) => resolved,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FsError::NotFound(rel.to_string()))
        }
        Err(e) => return Err(FsError::Io(e.to_string())),
    };

    ensure_inside(&root, &resolved)?;
    Ok(resolved)
}

/// 書き込み先の親ディレクトリを解決する。無ければ 1 段ずつ作る。
///
/// 1 段作るごとに配下チェックを行うため、途中のディレクトリがシンボリック
/// リンクで外を指していればその時点で落ちる。戻り値は
/// `(正規化済みの親ディレクトリ, ファイル名)`。
pub fn resolve_parent(root: &Path, rel: &str) -> Result<(PathBuf, String), FsError> {
    let mut components = validate_relative(rel)?;
    let name = components
        .pop()
        .ok_or_else(|| FsError::InvalidPath("path must name a file".into()))?;
    let root = canonical_root(root)?;

    let mut current = root.clone();
    for component in &components {
        current.push(component);
        if !current.exists() {
            std::fs::create_dir(&current).map_err(|e| FsError::Io(e.to_string()))?;
        }
        current = current
            .canonicalize()
            .map_err(|e| FsError::Io(e.to_string()))?;
        ensure_inside(&root, &current)?;
        if !current.is_dir() {
            return Err(FsError::InvalidPath(format!(
                "{component:?} is not a directory"
            )));
        }
    }

    Ok((current, name))
}

/// `Component` の使用を強制しないためのダミー参照(未使用 import を避ける)。
#[allow(dead_code)]
fn _unused(_: Component<'_>) {}
```

`_unused` が不要なら `Component` の import ごと削ること。

- [ ] **Step 9: テストを実行する**

Run: `cargo test -p edlr-driver-fs`
Expected: PASS(8 テスト)

- [ ] **Step 10: コミット**

```bash
git add Cargo.toml Cargo.lock drivers/fs
git commit -m "feat(drivers): add filesystem path validation"
```

---

### Task 2: `drivers/fs` — 操作本体(openat2 / 原子的書き込み / 上限)

**Files:**
- Create: `drivers/fs/src/openat.rs`
- Modify: `drivers/fs/src/lib.rs`
- Test: `drivers/fs/src/lib.rs`(`mod tests`)

**Interfaces:**
- Consumes: `path::{validate_relative, resolve_existing, resolve_parent, canonical_root}`(Task 1)
- Produces:
  - `FsDriver::new(read_limit: usize, list_limit: usize) -> FsDriver`
  - `Entry { path: String, size: u64, modified: Option<u64> }`
  - `FsDriver::read(&self, root: &Path, rel: &str) -> Result<Vec<u8>, FsError>`
  - `FsDriver::read_range(&self, root: &Path, rel: &str, offset: u64, len: u32) -> Result<Vec<u8>, FsError>`
  - `FsDriver::stat(&self, root: &Path, rel: &str) -> Result<Entry, FsError>`
  - `FsDriver::list(&self, root: &Path, prefix: &str) -> Result<Vec<Entry>, FsError>`
  - `FsDriver::write(&self, root: &Path, rel: &str, bytes: &[u8]) -> Result<(), FsError>`
  - `FsDriver::append(&self, root: &Path, rel: &str, bytes: &[u8]) -> Result<(), FsError>`
  - `FsDriver::delete(&self, root: &Path, rel: &str) -> Result<(), FsError>`

- [ ] **Step 1: 失敗するテストを書く**

`drivers/fs/src/lib.rs` の末尾に:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    const READ_LIMIT: usize = 8 * 1024 * 1024;

    fn driver() -> FsDriver {
        FsDriver::new(READ_LIMIT, 10_000)
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();

        d.write(dir.path(), "notes/state.json", b"{\"seen\":1}").unwrap();
        let got = d.read(dir.path(), "notes/state.json").unwrap();

        assert_eq!(got, b"{\"seen\":1}");
        assert!(dir.path().join("notes").join("state.json").is_file());
    }

    #[test]
    fn write_is_atomic_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();

        d.write(dir.path(), "a.txt", b"first").unwrap();
        d.write(dir.path(), "a.txt", b"second").unwrap();

        assert_eq!(d.read(dir.path(), "a.txt").unwrap(), b"second");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name != "a.txt")
            .collect();
        assert!(leftovers.is_empty(), "unexpected leftovers: {leftovers:?}");
    }

    #[test]
    fn append_extends_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();

        d.append(dir.path(), "log.txt", b"one\n").unwrap();
        d.append(dir.path(), "log.txt", b"two\n").unwrap();

        assert_eq!(d.read(dir.path(), "log.txt").unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn read_range_returns_a_slice_and_tolerates_offsets_past_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        d.write(dir.path(), "a.txt", b"0123456789").unwrap();

        assert_eq!(d.read_range(dir.path(), "a.txt", 2, 3).unwrap(), b"234");
        assert_eq!(d.read_range(dir.path(), "a.txt", 8, 100).unwrap(), b"89");
        assert!(d.read_range(dir.path(), "a.txt", 50, 10).unwrap().is_empty());
    }

    #[test]
    fn read_over_the_limit_is_too_large_but_read_range_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let d = FsDriver::new(16, 10_000);
        d.write(dir.path(), "big.bin", &vec![7u8; 64]).unwrap();

        assert!(matches!(
            d.read(dir.path(), "big.bin").expect_err("over the read limit"),
            FsError::TooLarge(_)
        ));
        assert_eq!(d.read_range(dir.path(), "big.bin", 0, 16).unwrap().len(), 16);
        assert!(matches!(
            d.read_range(dir.path(), "big.bin", 0, 17)
                .expect_err("range longer than the limit"),
            FsError::TooLarge(_)
        ));
    }

    #[test]
    fn stat_reports_size_and_list_is_recursive_over_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        d.write(dir.path(), "a.txt", b"abc").unwrap();
        d.write(dir.path(), "logs/b.txt", b"de").unwrap();

        assert_eq!(d.stat(dir.path(), "a.txt").unwrap().size, 3);

        let mut listed: Vec<String> = d
            .list(dir.path(), "")
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        listed.sort();
        assert_eq!(listed, vec!["a.txt".to_string(), "logs/b.txt".to_string()]);

        let scoped: Vec<String> = d
            .list(dir.path(), "logs")
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(scoped, vec!["logs/b.txt".to_string()]);
    }

    #[test]
    fn list_over_the_entry_limit_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let d = FsDriver::new(READ_LIMIT, 3);
        for i in 0..4 {
            d.write(dir.path(), &format!("f{i}.txt"), b"x").unwrap();
        }

        assert!(matches!(
            d.list(dir.path(), "").expect_err("over the entry limit"),
            FsError::TooLarge(_)
        ));
    }

    #[test]
    fn delete_removes_a_file_and_missing_files_report_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        d.write(dir.path(), "a.txt", b"x").unwrap();

        d.delete(dir.path(), "a.txt").unwrap();
        assert!(!dir.path().join("a.txt").exists());
        assert!(matches!(
            d.delete(dir.path(), "a.txt").expect_err("already gone"),
            FsError::NotFound(_)
        ));
    }

    #[test]
    fn every_operation_refuses_to_escape_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let d = driver();

        assert!(d.read(dir.path(), "../secret").is_err());
        assert!(d.read(dir.path(), "link").is_err());
        assert!(d.stat(dir.path(), "link").is_err());
        assert!(d.write(dir.path(), "escape/evil.txt", b"x").is_err());
        assert!(d.append(dir.path(), "escape/evil.txt", b"x").is_err());
        assert!(d.delete(dir.path(), "link").is_err());
        assert!(d.list(dir.path(), "escape").is_err());

        // 外のファイルが一切変化していないこと。
        assert_eq!(fs::read(outside.path().join("secret")).unwrap(), b"secret");
        assert!(!outside.path().join("evil.txt").exists());
    }

    /// ルート内のシンボリックリンクは、外を指していなくても拒否する
    /// (`RESOLVE_NO_SYMLINKS` 相当の意図的な制約。設計書に明記済み)。
    #[test]
    fn symlinks_inside_the_root_are_rejected_too() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        d.write(dir.path(), "real.txt", b"x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("alias")).unwrap();

        assert!(d.read(dir.path(), "alias").is_err());
    }

    fn _assert_path_type(_: &Path) {}
}
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-driver-fs`
Expected: FAIL(`FsDriver` が未定義)

- [ ] **Step 3: `openat2` ラッパを実装する**

`drivers/fs/src/openat.rs`:

```rust
//! `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` によるパス解決の
//! 3 段目。検証と `open` の間にシンボリックリンクを差し替えられても、
//! カーネルがルート配下から出ることを拒否する(TOCTOU 対策)。
//!
//! `openat2` は Linux 5.6 以降。使えない環境では `O_NOFOLLOW` 付きの
//! 通常 open にフォールバックする。フォールバック経路でも、呼び出し側は
//! 事前に `path` モジュールの構文検証と配下チェックを通している。

use std::fs::File;
use std::path::Path;

use rustix::fs::{Mode, OFlags, ResolveFlags};

use crate::FsError;

/// ルート配下に拘束して開く。`create` が真なら存在しなければ作る。
pub fn open_beneath(root: &Path, rel: &str, write: bool, create: bool) -> Result<File, FsError> {
    let root_dir = File::open(root).map_err(|e| FsError::Io(e.to_string()))?;

    let mut oflags = if write {
        OFlags::WRONLY
    } else {
        OFlags::RDONLY
    };
    if create {
        oflags |= OFlags::CREATE;
    }
    oflags |= OFlags::NOFOLLOW;

    match rustix::fs::openat2(
        &root_dir,
        rel,
        oflags,
        Mode::from_bits_truncate(0o644),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    ) {
        Ok(fd) => Ok(File::from(fd)),
        Err(rustix::io::Errno::NOSYS) | Err(rustix::io::Errno::OPNOTSUPP) => {
            // カーネルが openat2 を持たない。事前検証済みのパスを
            // O_NOFOLLOW で開くフォールバック。
            open_fallback(root, rel, write, create)
        }
        Err(rustix::io::Errno::XDEV) | Err(rustix::io::Errno::LOOP) => Err(FsError::InvalidPath(
            "resolved path escapes the granted root".into(),
        )),
        Err(rustix::io::Errno::NOENT) => Err(FsError::NotFound(rel.to_string())),
        Err(e) => Err(FsError::Io(e.to_string())),
    }
}

fn open_fallback(root: &Path, rel: &str, write: bool, create: bool) -> Result<File, FsError> {
    let target = root.join(rel);
    let mut options = std::fs::OpenOptions::new();
    options.read(!write).write(write).create(create);
    // O_NOFOLLOW: 最終要素がシンボリックリンクなら開かない。
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc_o_nofollow());

    options.open(&target).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound(rel.to_string()),
        _ => FsError::Io(e.to_string()),
    })
}

/// `O_NOFOLLOW` の値。`libc` を直接依存に足さずに済ませるための定数。
/// Linux では 0o400000。
fn libc_o_nofollow() -> i32 {
    0o400_000
}
```

- [ ] **Step 4: `FsDriver` を実装する**

`drivers/fs/src/lib.rs`(`mod tests` の上、`pub mod path;` の下)に `pub mod openat;` を足し、続けて:

```rust
use std::path::Path;
use std::time::UNIX_EPOCH;

/// 1 ファイル分のメタデータ。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// ルートからの相対パス(区切りは `/`)。
    pub path: String,
    pub size: u64,
    /// Unix epoch 秒。取得できなければ `None`。
    pub modified: Option<u64>,
}

/// 承認済みルート配下でのファイル操作。
///
/// `root` は呼び出しごとに渡される(プラグインごと・エントリごとに違うため)。
/// このドライバは承認状態を知らない -- 呼び出してよいかの判断は呼び出し元
/// (`core` 側のホスト実装)が行う。
pub struct FsDriver {
    read_limit: usize,
    list_limit: usize,
}

impl FsDriver {
    pub fn new(read_limit: usize, list_limit: usize) -> FsDriver {
        FsDriver {
            read_limit,
            list_limit,
        }
    }

    pub fn read(&self, root: &Path, rel: &str) -> Result<Vec<u8>, FsError> {
        let resolved = path::resolve_existing(root, rel)?;
        let size = std::fs::metadata(&resolved)
            .map_err(|e| FsError::Io(e.to_string()))?
            .len();
        if size as usize > self.read_limit {
            return Err(FsError::TooLarge(format!(
                "{rel} is {size} bytes, over the {} byte read limit; use read-range",
                self.read_limit
            )));
        }

        let mut file = openat::open_beneath(root, rel, false, false)?;
        let mut buf = Vec::with_capacity(size as usize);
        use std::io::Read;
        file.read_to_end(&mut buf)
            .map_err(|e| FsError::Io(e.to_string()))?;
        Ok(buf)
    }

    pub fn read_range(
        &self,
        root: &Path,
        rel: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, FsError> {
        if len as usize > self.read_limit {
            return Err(FsError::TooLarge(format!(
                "requested {len} bytes, over the {} byte read limit",
                self.read_limit
            )));
        }
        path::resolve_existing(root, rel)?;

        let mut file = openat::open_beneath(root, rel, false, false)?;
        use std::io::{Read, Seek, SeekFrom};
        let size = file
            .metadata()
            .map_err(|e| FsError::Io(e.to_string()))?
            .len();
        if offset >= size {
            // 末尾より後ろは「読めるものが無い」だけでエラーにしない。
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| FsError::Io(e.to_string()))?;
        let mut buf = vec![0u8; len as usize];
        let read = file
            .read(&mut buf)
            .map_err(|e| FsError::Io(e.to_string()))?;
        buf.truncate(read);
        Ok(buf)
    }

    pub fn stat(&self, root: &Path, rel: &str) -> Result<Entry, FsError> {
        let resolved = path::resolve_existing(root, rel)?;
        let meta = std::fs::metadata(&resolved).map_err(|e| FsError::Io(e.to_string()))?;
        Ok(Entry {
            path: rel.to_string(),
            size: meta.len(),
            modified: modified_secs(&meta),
        })
    }

    /// `prefix` 配下を再帰的に列挙する。ファイルのみを返し、ディレクトリ
    /// 自体は含めない。`prefix` が空文字ならルート直下から。
    pub fn list(&self, root: &Path, prefix: &str) -> Result<Vec<Entry>, FsError> {
        let base = if prefix.is_empty() {
            path::canonical_root(root)?
        } else {
            path::resolve_existing(root, prefix)?
        };
        let root_canonical = path::canonical_root(root)?;

        let mut entries = Vec::new();
        let mut stack = vec![base];
        while let Some(dir) = stack.pop() {
            let read_dir = std::fs::read_dir(&dir).map_err(|e| FsError::Io(e.to_string()))?;
            for item in read_dir {
                let item = item.map_err(|e| FsError::Io(e.to_string()))?;
                let meta = item
                    .metadata()
                    .map_err(|e| FsError::Io(e.to_string()))?;
                // symlink_metadata ではなく metadata を使うとリンク先を
                // 見てしまうため、リンクは種別で弾く。
                let file_type = item.file_type().map_err(|e| FsError::Io(e.to_string()))?;
                if file_type.is_symlink() {
                    continue;
                }
                let path = item.path();
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                let relative = path
                    .strip_prefix(&root_canonical)
                    .map_err(|_| FsError::InvalidPath("entry escapes the granted root".into()))?
                    .to_string_lossy()
                    .to_string();
                entries.push(Entry {
                    path: relative,
                    size: meta.len(),
                    modified: modified_secs(&meta),
                });
                if entries.len() > self.list_limit {
                    return Err(FsError::TooLarge(format!(
                        "more than {} entries under {prefix:?}",
                        self.list_limit
                    )));
                }
            }
        }
        Ok(entries)
    }

    /// 原子的に書き込む。同一ディレクトリに tmp を作って `rename` する
    /// ので、読み手が半端な内容を見ることはない。
    pub fn write(&self, root: &Path, rel: &str, bytes: &[u8]) -> Result<(), FsError> {
        let (parent, name) = path::resolve_parent(root, rel)?;
        let tmp = parent.join(format!(".{name}.tmp.{}", std::process::id()));

        std::fs::write(&tmp, bytes).map_err(|e| FsError::Io(e.to_string()))?;
        std::fs::rename(&tmp, parent.join(&name)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            FsError::Io(e.to_string())
        })
    }

    /// 追記する。原子的ではない(ログ用途では途中で切れても後ろに足される
    /// だけなので許容する)。
    pub fn append(&self, root: &Path, rel: &str, bytes: &[u8]) -> Result<(), FsError> {
        let (parent, name) = path::resolve_parent(root, rel)?;
        let target = parent.join(&name);
        if target.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            return Err(FsError::InvalidPath(
                "refusing to append through a symlink".into(),
            ));
        }

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)
            .map_err(|e| FsError::Io(e.to_string()))?;
        file.write_all(bytes).map_err(|e| FsError::Io(e.to_string()))
    }

    pub fn delete(&self, root: &Path, rel: &str) -> Result<(), FsError> {
        let resolved = path::resolve_existing(root, rel)?;
        std::fs::remove_file(&resolved).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FsError::NotFound(rel.to_string()),
            _ => FsError::Io(e.to_string()),
        })
    }
}

fn modified_secs(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}
```

- [ ] **Step 5: テストを実行する**

Run: `cargo test -p edlr-driver-fs`
Expected: PASS(18 テスト)

テストが落ちた場合、**テストを緩めるのではなく実装を直す**こと。特に脱出系のテストは仕様そのものなので、落ちたまま先へ進んではいけない。

- [ ] **Step 6: TOCTOU のテストを追加する**

```rust
    /// 検証と open の間にシンボリックリンクへ差し替えられても、ルート外の
    /// ファイルを書き換えられないこと。`openat2` 経路の意義そのもの。
    #[test]
    fn swapping_the_target_for_a_symlink_cannot_write_outside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        fs::write(&victim, b"original").unwrap();
        let d = driver();

        // 事前に通常ファイルとして作らせ、その後リンクへ差し替える。
        d.write(dir.path(), "target.txt", b"x").unwrap();
        fs::remove_file(dir.path().join("target.txt")).unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join("target.txt")).unwrap();

        let _ = d.write(dir.path(), "target.txt", b"overwritten");

        assert_eq!(
            fs::read(&victim).unwrap(),
            b"original",
            "a symlink swap must never let a write reach outside the root"
        );
    }
```

Run: `cargo test -p edlr-driver-fs`
Expected: PASS

**注意**: `write` は tmp + `rename` なので、リンクを踏むのは `rename` の宛先側。`rename` はシンボリックリンクを追わずリンク自体を置き換えるため、このテストは通るはず。通らない場合は `write` の実装を直すこと(リンクだった場合は `InvalidPath` を返す)。

- [ ] **Step 7: コミット**

```bash
git add drivers/fs Cargo.lock
git commit -m "feat(drivers): add filesystem operations with openat2 confinement"
```

---

### Task 3: manifest の `[[filesystem]]` パースと検証

**Files:**
- Modify: `core/src/plugin/manifest.rs`, `core/src/plugin/mod.rs`
- Test: `core/src/plugin/manifest.rs`(既存 `mod tests` に追記)

**Interfaces:**
- Consumes: なし
- Produces:
  - `pub enum FilesystemMode { Read, ReadWrite }`(`serde` で `"read"` / `"read-write"`)
  - `pub struct FilesystemRequest { pub name: String, pub reason: String, pub mode: FilesystemMode }`
  - `Manifest.filesystem: Vec<FilesystemRequest>`
  - `Manifest::filesystem_root(&self, name: &str) -> Option<&FilesystemRequest>`
  - `Manifest::filesystem_fingerprint(&self, name: &str) -> Option<String>`
  - `ManifestError::BadFilesystem(String)`

- [ ] **Step 1: 失敗するテストを書く**

既存の `[[sidecar]]` テストが使っている `parse_sidecar_manifest` と同じ流儀のヘルパを足し(名前は `parse_fs_manifest`)、次を書く:

```rust
    fn parse_fs_manifest(body: &str) -> Result<Manifest, ManifestError> {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("fs-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            format!(
                "id = \"fs-plugin\"\nname = \"FS\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n{body}"
            ),
        )
        .unwrap();
        load_manifest(&plugin_dir)
    }

    #[test]
    fn filesystem_block_is_parsed() {
        let manifest = parse_fs_manifest(
            "[[filesystem]]\nname = \"exports\"\nreason = \"CSV を書き出すため\"\nmode = \"read-write\"\n",
        )
        .expect("valid filesystem manifest should load");

        assert_eq!(manifest.filesystem.len(), 1);
        assert_eq!(manifest.filesystem[0].name, "exports");
        assert_eq!(manifest.filesystem[0].mode, FilesystemMode::ReadWrite);
    }

    #[test]
    fn read_only_mode_is_parsed() {
        let manifest = parse_fs_manifest(
            "[[filesystem]]\nname = \"input\"\nreason = \"読むだけ\"\nmode = \"read\"\n",
        )
        .unwrap();
        assert_eq!(manifest.filesystem[0].mode, FilesystemMode::Read);
    }

    #[test]
    fn unknown_mode_duplicate_name_and_blank_reason_are_rejected() {
        assert!(matches!(
            parse_fs_manifest(
                "[[filesystem]]\nname = \"a\"\nreason = \"r\"\nmode = \"write\"\n"
            )
            .expect_err("unknown mode"),
            ManifestError::Parse(_) | ManifestError::BadFilesystem(_)
        ));
        assert!(matches!(
            parse_fs_manifest(
                "[[filesystem]]\nname = \"a\"\nreason = \"r\"\nmode = \"read\"\n\n[[filesystem]]\nname = \"a\"\nreason = \"r2\"\nmode = \"read\"\n"
            )
            .expect_err("duplicate name"),
            ManifestError::BadFilesystem(_)
        ));
        assert!(matches!(
            parse_fs_manifest(
                "[[filesystem]]\nname = \"a\"\nreason = \"  \"\nmode = \"read\"\n"
            )
            .expect_err("blank reason"),
            ManifestError::BadFilesystem(_)
        ));
        assert!(matches!(
            parse_fs_manifest(
                "[[filesystem]]\nname = \"Exports\"\nreason = \"r\"\nmode = \"read\"\n"
            )
            .expect_err("uppercase name"),
            ManifestError::BadFilesystem(_)
        ));
    }

    #[test]
    fn filesystem_fingerprint_is_stable_and_changes_with_the_request() {
        let manifest = parse_fs_manifest(
            "[[filesystem]]\nname = \"exports\"\nreason = \"r\"\nmode = \"read\"\n",
        )
        .unwrap();
        let first = manifest.filesystem_fingerprint("exports").unwrap();
        assert_eq!(first, manifest.filesystem_fingerprint("exports").unwrap());
        assert_eq!(manifest.filesystem_fingerprint("nope"), None);

        let changed = parse_fs_manifest(
            "[[filesystem]]\nname = \"exports\"\nreason = \"r\"\nmode = \"read-write\"\n",
        )
        .unwrap();
        assert_ne!(first, changed.filesystem_fingerprint("exports").unwrap());
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core filesystem`
Expected: FAIL(`FilesystemMode` などが未定義)

- [ ] **Step 3: 実装する**

`manifest.rs` に追加(既存の `SidecarRequest` の隣):

```rust
/// `[[filesystem]]` の `mode`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FilesystemMode {
    Read,
    ReadWrite,
}

impl FilesystemMode {
    /// フィンガープリント・RPC 応答で使う安定した文字列表現。
    pub fn as_str(&self) -> &'static str {
        match self {
            FilesystemMode::Read => "read",
            FilesystemMode::ReadWrite => "read-write",
        }
    }

    pub fn allows_write(&self) -> bool {
        matches!(self, FilesystemMode::ReadWrite)
    }
}

/// プラグインが要求するファイルアクセス 1 件。
///
/// **ディレクトリの実パスはここに書けない** -- 必ずユーザーが UI で選ぶ。
/// 承認画面に出る内容と実際にアクセスされる場所を、ユーザー自身の指定に
/// よって一致させるため(`[[sidecar]]` の `command` と同じ原則)。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FilesystemRequest {
    pub name: String,
    pub reason: String,
    pub mode: FilesystemMode,
}
```

`Manifest` に `#[serde(default)] pub filesystem: Vec<FilesystemRequest>,` を足す。

`impl Manifest` に:

```rust
    pub fn filesystem_root(&self, name: &str) -> Option<&FilesystemRequest> {
        self.filesystem.iter().find(|r| r.name == name)
    }

    /// ファイルアクセス要求 1 件の安定フィンガープリント。
    /// `capabilities_fingerprint` と同じ長さ接頭辞エンコード + SHA-256。
    /// **ユーザーが選ぶ path は含めない**(パス変更は再承認を要さない)。
    pub fn filesystem_fingerprint(&self, name: &str) -> Option<String> {
        let request = self.filesystem_root(name)?;
        let mut canonical = encode_field("filesystem");
        canonical.push_str(&encode_field(&request.name));
        canonical.push_str(&encode_field(&request.reason));
        canonical.push_str(&encode_field(request.mode.as_str()));
        Some(sha256_hex(&canonical))
    }
```

`ManifestError` に `BadFilesystem(String)` を足し、`Display` に
`ManifestError::BadFilesystem(msg) => write!(f, "invalid filesystem request: {msg}"),` を足す。

検証関数(`validate_sidecars` の隣):

```rust
fn validate_filesystem(requests: &mut [FilesystemRequest]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for request in requests.iter_mut() {
        if !is_valid_id(&request.name) {
            return Err(ManifestError::BadFilesystem(format!(
                "filesystem name must match [a-z0-9-]+: {}",
                request.name
            )));
        }
        if !seen.insert(request.name.clone()) {
            return Err(ManifestError::BadFilesystem(format!(
                "duplicate filesystem name: {}",
                request.name
            )));
        }

        let trimmed = request.reason.trim().to_string();
        if trimmed.is_empty() {
            return Err(ManifestError::BadFilesystem(
                "filesystem request requires a non-empty reason".to_string(),
            ));
        }
        reject_invisible_chars("reason", &trimmed).map_err(ManifestError::BadFilesystem)?;
        request.reason = trimmed;
    }
    Ok(())
}
```

`load_manifest` の `validate_sidecars(...)` の直後に `validate_filesystem(&mut manifest.filesystem)?;` を足す。

`core/src/plugin/mod.rs` の re-export に `FilesystemMode` / `FilesystemRequest` を追加。`Manifest` をリテラル構築している既存テスト(`grants.rs` / `registry.rs` / `runner.rs` / `settings.rs` / `sidecar.rs` / `manifest.rs` 自身)に `filesystem: vec![]` を足してコンパイルを通す。**それ以外の既存テストの意味は変えないこと。**

- [ ] **Step 4: テストを実行する**

Run: `cargo test -p edlr-core`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add core/src/plugin
git commit -m "feat(plugin): parse and validate [[filesystem]] manifest blocks"
```

---

### Task 4: 設定ストア(ディレクトリの検証込み)

**Files:**
- Create: `core/src/plugin/filesystem.rs`
- Modify: `core/src/plugin/mod.rs`
- Test: `core/src/plugin/filesystem.rs`(同ファイル内 `mod tests`)

**Interfaces:**
- Consumes: `FilesystemRequest`(Task 3)
- Produces:
  - `pub struct FilesystemConfig { pub path: String }`(`Serialize + Deserialize + Clone + PartialEq + Debug`)
  - `pub enum FilesystemConfigError { UnknownRoot(String), NotAbsolute(String), NotADirectory(String), ProtectedDirectory(String), Io(std::io::Error), Serialize(serde_json::Error) }`(`Display + Error`)
  - `pub struct FilesystemConfigStore`(`new(dir: PathBuf)`)
  - `FilesystemConfigStore::effective(&self, manifest: &Manifest) -> BTreeMap<String, FilesystemConfig>`
  - `FilesystemConfigStore::update_and_effective(&self, manifest: &Manifest, name: &str, config: &FilesystemConfig) -> Result<BTreeMap<String, FilesystemConfig>, FilesystemConfigError>`

- [ ] **Step 1: 失敗するテストを書く**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::{FilesystemMode, FilesystemRequest};

    fn manifest_with(requests: Vec<FilesystemRequest>) -> Manifest {
        Manifest {
            id: "fs-plugin".into(),
            name: "FS".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: requests,
        }
    }

    fn request(name: &str) -> FilesystemRequest {
        FilesystemRequest {
            name: name.into(),
            reason: "reason".into(),
            mode: FilesystemMode::ReadWrite,
        }
    }

    #[test]
    fn effective_defaults_to_an_empty_path() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("exports")]);

        assert_eq!(store.effective(&manifest)["exports"].path, "");
    }

    #[test]
    fn update_persists_and_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        let target = tmp.path().join("exports");
        std::fs::create_dir(&target).unwrap();
        let store = FilesystemConfigStore::new(dir.clone());
        let manifest = manifest_with(vec![request("exports")]);

        let updated = store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig {
                    path: target.to_string_lossy().to_string(),
                },
            )
            .expect("valid directory should be accepted");
        assert_eq!(updated["exports"].path, target.to_string_lossy());
        assert!(dir.join("fs-plugin.filesystem.json").is_file());

        let reread = FilesystemConfigStore::new(dir).effective(&manifest);
        assert_eq!(reread["exports"].path, target.to_string_lossy());
    }

    #[test]
    fn relative_paths_missing_paths_and_files_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("exports")]);
        let file = tmp.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();

        for (path, expect_not_a_dir) in [
            ("relative/dir".to_string(), false),
            (tmp.path().join("nope").to_string_lossy().to_string(), true),
            (file.to_string_lossy().to_string(), true),
        ] {
            let err = store
                .update_and_effective(&manifest, "exports", &FilesystemConfig { path })
                .expect_err("invalid directory must be rejected");
            if expect_not_a_dir {
                assert!(matches!(err, FilesystemConfigError::NotADirectory(_)));
            } else {
                assert!(matches!(err, FilesystemConfigError::NotAbsolute(_)));
            }
        }
    }

    #[test]
    fn protected_directories_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("exports")]);

        for path in ["/", "/etc", "/home", "/usr", "/var"] {
            let err = store
                .update_and_effective(
                    &manifest,
                    "exports",
                    &FilesystemConfig { path: path.to_string() },
                )
                .expect_err("a protected directory must be rejected");
            assert!(
                matches!(err, FilesystemConfigError::ProtectedDirectory(_)),
                "{path} should be protected"
            );
        }
    }

    #[test]
    fn a_rejected_update_leaves_the_stored_value_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        let target = tmp.path().join("exports");
        std::fs::create_dir(&target).unwrap();
        let store = FilesystemConfigStore::new(dir);
        let manifest = manifest_with(vec![request("exports")]);
        store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig { path: target.to_string_lossy().to_string() },
            )
            .unwrap();

        let _ = store.update_and_effective(
            &manifest,
            "exports",
            &FilesystemConfig { path: "/etc".to_string() },
        );

        assert_eq!(store.effective(&manifest)["exports"].path, target.to_string_lossy());
    }

    #[test]
    fn unknown_root_is_rejected_and_broken_json_falls_back_to_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fs-plugin.filesystem.json"), "not json {{{").unwrap();
        let store = FilesystemConfigStore::new(dir);
        let manifest = manifest_with(vec![request("exports")]);

        assert_eq!(store.effective(&manifest)["exports"].path, "");
        assert!(matches!(
            store
                .update_and_effective(&manifest, "nope", &FilesystemConfig { path: "/tmp".into() })
                .expect_err("unknown root"),
            FilesystemConfigError::UnknownRoot(_)
        ));
    }
}
```

- [ ] **Step 2: テストが失敗することを確認する**

`core/src/plugin/mod.rs` に `pub mod filesystem;` と re-export を足したうえで、

Run: `cargo test -p edlr-core filesystem::tests`
Expected: FAIL(未実装)

- [ ] **Step 3: 実装する**

`core/src/plugin/filesystem.rs`。構造は `core/src/plugin/sidecar.rs`(`SidecarConfigStore`)をそのまま踏襲する: 内部 `Mutex<()>` で read-merge-write を直列化し、tmp + `fs::rename` で原子的に書き、壊れたファイルは既定値へフォールバックする。保存先は `<settings-dir>/<plugin-id>.filesystem.json`。

検証は次の順に行い、いずれかで失敗したら**何も書き込まない**:

```rust
/// ユーザーが選んだディレクトリを検証する。
///
/// システム上重要なディレクトリ「そのもの」は拒否する。承認画面での確認だけに
/// 頼らず、明らかな事故を 1 段止めるため(配下の任意のディレクトリは許可する
/// -- `/home/alice/Documents` は通り、`/home` は通らない)。
fn validate_path(name: &str, path: &str) -> Result<(), FilesystemConfigError> {
    if path.is_empty() {
        return Ok(()); // 未設定は許す(承認できないだけ)
    }

    let candidate = std::path::Path::new(path);
    if !candidate.is_absolute() {
        return Err(FilesystemConfigError::NotAbsolute(name.to_string()));
    }

    let canonical = candidate
        .canonicalize()
        .map_err(|_| FilesystemConfigError::NotADirectory(name.to_string()))?;
    if !canonical.is_dir() {
        return Err(FilesystemConfigError::NotADirectory(name.to_string()));
    }

    if is_protected(&canonical) {
        return Err(FilesystemConfigError::ProtectedDirectory(name.to_string()));
    }
    Ok(())
}

/// 「そのものは選ばせない」ディレクトリ。配下は許可する。
fn is_protected(canonical: &std::path::Path) -> bool {
    const PROTECTED: &[&str] = &[
        "/", "/home", "/etc", "/usr", "/var", "/boot", "/dev", "/proc", "/sys", "/root", "/bin",
        "/sbin", "/lib",
    ];
    if PROTECTED.iter().any(|p| canonical == std::path::Path::new(p)) {
        return true;
    }
    // ユーザーのホームディレクトリ「そのもの」も拒否する。
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() && canonical == std::path::Path::new(&home) {
            return true;
        }
    }
    false
}
```

- [ ] **Step 4: テストを実行する**

Run: `cargo test -p edlr-core filesystem`
Expected: PASS(6 テスト)

- [ ] **Step 5: コミット**

```bash
git add core/src/plugin
git commit -m "feat(plugin): add filesystem config store with directory validation"
```

---

### Task 5: エントリ単位の grant

**Files:**
- Modify: `core/src/plugin/grants.rs`
- Test: `core/src/plugin/grants.rs`(既存 `mod tests` に追記)

**Interfaces:**
- Consumes: `Manifest::filesystem_fingerprint`(Task 3)
- Produces:
  - `GrantsStore::filesystem_state(&self, manifest: &Manifest, name: &str) -> GrantState`
  - `GrantsStore::set_filesystem(&self, manifest: &Manifest, name: &str, granted: bool) -> Result<GrantState, GrantsError>`
  - ディスク形式: `SavedGrant` に `filesystem: BTreeMap<String, SavedEntryGrant>` を追加(既存ファイルは欠落として読める)

**この実装は既存の `sidecar_state` / `set_sidecar` と構造が同一。** `SavedSidecarGrant` と同じ形の値型を使い、`write_saved` ヘルパを共用する。`granted`/`fingerprint`/`sidecars` を保持したまま `filesystem` だけを更新すること(HTTP・サイドカーの承認を消さない)。

- [ ] **Step 1: 失敗するテストを書く**

```rust
    fn manifest_with_fs(mode: crate::plugin::manifest::FilesystemMode) -> Manifest {
        Manifest {
            id: "fs-plugin".into(),
            name: "FS".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![crate::plugin::manifest::FilesystemRequest {
                name: "exports".into(),
                reason: "reason".into(),
                mode,
            }],
        }
    }

    #[test]
    fn filesystem_grant_defaults_to_ungranted_and_persists() {
        use crate::plugin::manifest::FilesystemMode;
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_fs(FilesystemMode::ReadWrite);

        assert_eq!(
            store.filesystem_state(&manifest, "exports"),
            GrantState { granted: false, stale: false }
        );
        store.set_filesystem(&manifest, "exports", true).unwrap();
        assert_eq!(
            store.filesystem_state(&manifest, "exports"),
            GrantState { granted: true, stale: false }
        );
    }

    #[test]
    fn changing_the_mode_makes_the_filesystem_grant_stale() {
        use crate::plugin::manifest::FilesystemMode;
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        store
            .set_filesystem(&manifest_with_fs(FilesystemMode::Read), "exports", true)
            .unwrap();

        assert_eq!(
            store.filesystem_state(&manifest_with_fs(FilesystemMode::ReadWrite), "exports"),
            GrantState { granted: false, stale: true }
        );
    }

    #[test]
    fn filesystem_http_and_sidecar_grants_are_independent() {
        use crate::plugin::manifest::FilesystemMode;
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let mut manifest = manifest_with_fs(FilesystemMode::ReadWrite);
        manifest.capabilities = vec![CapabilityRequest::Http {
            hosts: vec!["https://api.example.com".into()],
            reason: "fetch".into(),
        }];

        store.set_filesystem(&manifest, "exports", true).unwrap();
        store.set(&manifest, true).unwrap();

        assert!(store.state(&manifest).granted);
        assert!(
            store.filesystem_state(&manifest, "exports").granted,
            "granting http must not clobber the filesystem grant"
        );
    }

    #[test]
    fn unknown_filesystem_root_is_never_granted() {
        use crate::plugin::manifest::FilesystemMode;
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_fs(FilesystemMode::Read);

        assert_eq!(
            store.filesystem_state(&manifest, "nope"),
            GrantState { granted: false, stale: false }
        );
        assert_eq!(
            store.set_filesystem(&manifest, "nope", true).unwrap(),
            GrantState { granted: false, stale: false }
        );
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core grants`
Expected: FAIL(`filesystem_state` / `set_filesystem` が未定義)

- [ ] **Step 3: 実装する**

`SavedGrant` に `#[serde(default)] filesystem: BTreeMap<String, SavedSidecarGrant>` を足す(値型は既存の `SavedSidecarGrant` をそのまま流用してよい。名前が紛らわしければ `SavedEntryGrant` にリネームし、`sidecars` 側の型も合わせて変更する — その場合ディスク形式は変わらないことを確認すること)。

`filesystem_state` / `set_filesystem` は `sidecar_state` / `set_sidecar` と同じ判定規則(未保存 → 未承認 / fingerprint 不一致 → stale / 一致 → 保存値)で実装する。`set_filesystem` は既存の `granted` / `fingerprint` / `sidecars` を読み出してから書き戻すこと。

- [ ] **Step 4: テストを実行する**

Run: `cargo test -p edlr-core grants`
Expected: PASS(既存 13 + 新規 4)

- [ ] **Step 5: コミット**

```bash
git add core/src/plugin/grants.rs
git commit -m "feat(plugin): add per-root filesystem grants"
```

---

### Task 6: WIT `driver-fs` とホスト実装

**Files:**
- Modify: `core/wit/plugin.wit`, `core/src/plugin/host.rs`, `core/Cargo.toml`, `core/src/plugin/mod.rs`
- Create: `core/src/plugin/fs_runtime.rs`
- Test: `core/src/plugin/host.rs`、`core/src/plugin/fs_runtime.rs`

**Interfaces:**
- Consumes: `edlr_driver_fs::{FsDriver, FsError, Entry}`(Task 1・2)、`FilesystemConfig`(Task 4)
- Produces:
  - `pub struct FsRuntimeEntry { pub name: String, pub granted: bool, pub mode: String, pub path: String }`
  - `fs_runtime::filesystem_json_string(entries: &[FsRuntimeEntry]) -> String`(未承認エントリは `path` を落とす)
  - `fs_runtime::parse_filesystem(raw: &str) -> BTreeMap<String, FsRuntimeEntry>`
  - `HostCtx::new(plugin_id, settings_json, capabilities_json, sidecars_json, filesystem_json, http_driver, process_driver, fs_driver)`
  - `host::FS_READ_LIMIT: usize = 8 * 1024 * 1024`、`host::FS_LIST_LIMIT: usize = 10_000`

- [ ] **Step 1: WIT を追加する**

`core/wit/plugin.wit` の `driver-process` の下に、設計書「WIT 追加」の `interface driver-fs` をそのまま追加する。`world plugin` に `import driver-fs;` を足す(`world plugin-guest` は `include plugin` なので自動で追随する)。

`core/Cargo.toml` に `edlr-driver-fs = { path = "../drivers/fs" }` を追加する。

- [ ] **Step 2: `fs_runtime` の失敗するテストを書く**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(granted: bool) -> FsRuntimeEntry {
        FsRuntimeEntry {
            name: "exports".into(),
            granted,
            mode: "read-write".into(),
            path: "/home/u/exports".into(),
        }
    }

    #[test]
    fn ungranted_entries_carry_no_path() {
        let parsed = parse_filesystem(&filesystem_json_string(&[entry(false)]));
        let root = parsed.get("exports").expect("entry survives serialization");
        assert!(!root.granted);
        assert_eq!(root.path, "");
    }

    #[test]
    fn granted_entries_round_trip() {
        let parsed = parse_filesystem(&filesystem_json_string(&[entry(true)]));
        let root = parsed.get("exports").unwrap();
        assert!(root.granted);
        assert_eq!(root.path, "/home/u/exports");
        assert_eq!(root.mode, "read-write");
    }

    #[test]
    fn broken_json_parses_as_no_roots() {
        assert!(parse_filesystem("not json {{{").is_empty());
    }
}
```

- [ ] **Step 3: テストが失敗することを確認する**

Run: `cargo test -p edlr-core fs_runtime`
Expected: FAIL(未実装)

- [ ] **Step 4: `fs_runtime` を実装する**

`core/src/plugin/sidecar_runtime.rs` と同じ構造(未承認エントリは `path` を空にして直列化する redaction 付き)で実装する。`mode` は承認状態に関わらず載せてよい(承認画面に出る情報であり、秘密ではない)。

- [ ] **Step 5: `host.rs` の失敗するテストを書く**

```rust
    fn fs_ctx(filesystem_json: &str) -> HostCtx {
        HostCtx::new(
            "test-plugin".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(capabilities_json_string(&[]))),
            Arc::new(Mutex::new("[]".to_string())),
            Arc::new(Mutex::new(filesystem_json.to_string())),
            test_http_driver(),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                Duration::from_millis(200),
                Duration::from_secs(1),
            )),
            Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT)),
        )
    }

    fn fs_entry(granted: bool, mode: &str, path: &str) -> crate::plugin::fs_runtime::FsRuntimeEntry {
        crate::plugin::fs_runtime::FsRuntimeEntry {
            name: "exports".to_string(),
            granted,
            mode: mode.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn fs_calls_without_grant_are_permission_denied() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = fs_ctx(&filesystem_json_string(&[fs_entry(
            false,
            "read-write",
            &dir.path().to_string_lossy(),
        )]));

        let err = ctx
            .read("exports".to_string(), "a.txt".to_string())
            .expect_err("ungranted root must be denied");
        assert!(matches!(err, WitFsError::PermissionDenied(_)));
    }

    #[test]
    fn unknown_root_is_reported_as_such() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = fs_ctx(&filesystem_json_string(&[fs_entry(
            true,
            "read-write",
            &dir.path().to_string_lossy(),
        )]));

        let err = ctx
            .read("nope".to_string(), "a.txt".to_string())
            .expect_err("unknown root");
        assert!(matches!(err, WitFsError::UnknownRoot(_)));
    }

    #[test]
    fn granted_but_unconfigured_root_is_not_configured() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let mut ctx = fs_ctx(&filesystem_json_string(&[fs_entry(true, "read-write", "")]));

        let err = ctx
            .read("exports".to_string(), "a.txt".to_string())
            .expect_err("no directory configured");
        assert!(matches!(err, WitFsError::NotConfigured(_)));
    }

    #[test]
    fn read_mode_rejects_every_mutating_call() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut ctx = fs_ctx(&filesystem_json_string(&[fs_entry(
            true,
            "read",
            &dir.path().to_string_lossy(),
        )]));

        assert!(ctx.read("exports".to_string(), "a.txt".to_string()).is_ok());
        assert!(matches!(
            ctx.write("exports".to_string(), "a.txt".to_string(), vec![1])
                .expect_err("write under read mode"),
            WitFsError::PermissionDenied(_)
        ));
        assert!(matches!(
            ctx.append("exports".to_string(), "a.txt".to_string(), vec![1])
                .expect_err("append under read mode"),
            WitFsError::PermissionDenied(_)
        ));
        assert!(matches!(
            ctx.delete("exports".to_string(), "a.txt".to_string())
                .expect_err("delete under read mode"),
            WitFsError::PermissionDenied(_)
        ));
    }

    #[test]
    fn granted_read_write_root_round_trips_and_still_refuses_escapes() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = fs_ctx(&filesystem_json_string(&[fs_entry(
            true,
            "read-write",
            &dir.path().to_string_lossy(),
        )]));

        ctx.write("exports".to_string(), "a.txt".to_string(), b"hi".to_vec())
            .expect("write");
        assert_eq!(
            ctx.read("exports".to_string(), "a.txt".to_string()).unwrap(),
            b"hi".to_vec()
        );
        assert!(matches!(
            ctx.read("exports".to_string(), "../secret".to_string())
                .expect_err("escape attempt"),
            WitFsError::InvalidPath(_)
        ));
    }
```

- [ ] **Step 6: テストが失敗することを確認する**

Run: `cargo test -p edlr-core host`
Expected: FAIL(`HostCtx::new` の引数不足、`DriverFsHost` 未実装)

- [ ] **Step 7: `host.rs` を実装する**

定数:

```rust
/// `driver-fs` の 1 回の読み取り上限。`HTTP_MAX_BODY` と同値。ホスト側の
/// バッファを無制限にしないためのもので、扱えるファイルサイズの上限では
/// ない(超えるものは `stat` + `read-range` で分割して読む)。
pub const FS_READ_LIMIT: usize = HTTP_MAX_BODY;

/// `list` が返すエントリ数の上限。呼び出し期限(`CALL_DEADLINE`)を
/// 食い潰さないための保護。
pub const FS_LIST_LIMIT: usize = 10_000;
```

`HostCtx` に `pub filesystem_json: Arc<Mutex<String>>` と `fs_driver: Arc<edlr_driver_fs::FsDriver>` を足し、`HostCtx::new` の引数を拡張する。

解決ヘルパ(判定順は `driver-process` の `resolve_sidecar` と同じ「存在 → 承認 → 設定」):

```rust
impl HostCtx {
    /// `filesystem_json` から当該ルートの実パスと mode を解決する。
    ///
    /// `driver-http` / `driver-process` と同じく、判定材料は全て `HostCtx`
    /// 側にあり、ゲストが渡すのはルート名と相対パスだけ。
    fn resolve_root(&self, root: &str, need_write: bool) -> Result<PathBuf, WitFsError> {
        let raw = self
            .filesystem_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let entries = crate::plugin::fs_runtime::parse_filesystem(&raw);

        let Some(entry) = entries.get(root) else {
            return Err(WitFsError::UnknownRoot(format!("no such root: {root}")));
        };
        if !entry.granted {
            return Err(WitFsError::PermissionDenied(format!(
                "filesystem root not granted: {root}"
            )));
        }
        if entry.path.is_empty() {
            return Err(WitFsError::NotConfigured(format!(
                "root {root} has no directory configured"
            )));
        }
        if need_write && entry.mode != "read-write" {
            return Err(WitFsError::PermissionDenied(format!(
                "root {root} is read-only"
            )));
        }
        Ok(PathBuf::from(&entry.path))
    }
}
```

`DriverFsHost for HostCtx` の 7 関数は、`resolve_root` → `fs_driver` の対応メソッド呼び出し → `FsError` を WIT の variant へ写像するだけにする:

```rust
fn to_wit_fs_error(e: edlr_driver_fs::FsError) -> WitFsError {
    match e {
        edlr_driver_fs::FsError::InvalidPath(m) => WitFsError::InvalidPath(m),
        edlr_driver_fs::FsError::NotFound(m) => WitFsError::NotFound(m),
        edlr_driver_fs::FsError::TooLarge(m) => WitFsError::TooLarge(m),
        edlr_driver_fs::FsError::Io(m) => WitFsError::Io(m),
    }
}
```

`PluginHost` に `fs_driver: Arc<FsDriver>` を持たせ、`new()` で 1 つ生成して `pub fn fs_driver(&self) -> Arc<FsDriver>` を足す。`runner.rs` / `registry.rs` の `HostCtx::new` 呼び出しは、コンパイルが通る最小限の修正(`filesystem_json` に `"[]"` を渡す)に留める — 実際の配線は次タスク。

- [ ] **Step 8: テストを実行する**

Run: `cargo test -p edlr-core`
Expected: PASS

- [ ] **Step 9: コミット**

```bash
git add core/wit core/Cargo.toml Cargo.lock core/src/plugin
git commit -m "feat(plugin): add driver-fs WIT interface and host implementation"
```

---

### Task 7: Registry 配線と RPC

**Files:**
- Modify: `core/src/plugin/registry.rs`, `core/src/plugin/runner.rs`, `core/src/bin/edlr.rs`, `core/src/server.rs`
- Create: `core/tests/driver_fs_integration.rs`
- Test: `core/tests/ws_rpc_integration.rs`(追記)

**Interfaces:**
- Consumes: Task 3〜6 の全て
- Produces:
  - `pub struct FilesystemInfo { pub request: FilesystemRequest, pub config: FilesystemConfig, pub grant: GrantState }`
  - `Registry::filesystem(&self, id: &str) -> Result<Vec<FilesystemInfo>, RegistryError>`
  - `Registry::set_filesystem_config(&self, id: &str, name: &str, config: &FilesystemConfig) -> Result<Vec<FilesystemInfo>, RegistryError>`
  - `Registry::set_filesystem_grant(&self, id: &str, name: &str, granted: bool) -> Result<Vec<FilesystemInfo>, RegistryError>`
  - `PluginInfo.filesystem: Vec<FilesystemInfo>`
  - `start_plugins(plugins_dir, settings_store, sidecar_config_store, filesystem_config_store, grants_store, router, host) -> Registry`(**引数追加**)
  - RPC: `plugins/get-filesystem` / `plugins/set-filesystem-config` / `plugins/set-filesystem-grant`、`plugins/list` に `filesystem` 追加

- [ ] **Step 1: 失敗する統合テストを書く**

`core/tests/driver_fs_integration.rs`。`core/tests/support/mod.rs` の `sidecar_env` と同じ流儀で、`[[filesystem]]` を持つプラグインを 1 件置いた `Registry` を組み立てるヘルパ `filesystem_env(name, mode)` を足す。

```rust
#[test]
fn granting_requires_a_configured_directory() {
    let env = support::filesystem_env("exports", "read-write");

    let err = env
        .registry
        .set_filesystem_grant("fs-plugin", "exports", true)
        .expect_err("granting without a directory must be rejected");
    assert!(err.to_string().contains("directory"));

    let dir = env.tmp.path().join("exports");
    std::fs::create_dir(&dir).unwrap();
    env.registry
        .set_filesystem_config(
            "fs-plugin",
            "exports",
            &FilesystemConfig { path: dir.to_string_lossy().to_string() },
        )
        .expect("config");
    let roots = env
        .registry
        .set_filesystem_grant("fs-plugin", "exports", true)
        .expect("grant after configuring");
    assert!(roots[0].grant.granted);
}

#[test]
fn revoking_removes_the_path_from_the_shared_buffer() {
    let env = support::filesystem_env("exports", "read-write");
    let dir = env.tmp.path().join("exports");
    std::fs::create_dir(&dir).unwrap();
    env.registry
        .set_filesystem_config(
            "fs-plugin",
            "exports",
            &FilesystemConfig { path: dir.to_string_lossy().to_string() },
        )
        .unwrap();
    env.registry.set_filesystem_grant("fs-plugin", "exports", true).unwrap();
    assert!(support::filesystem_buffer(&env.registry, "fs-plugin").contains(&dir.to_string_lossy().to_string()));

    env.registry.set_filesystem_grant("fs-plugin", "exports", false).unwrap();
    assert!(
        !support::filesystem_buffer(&env.registry, "fs-plugin")
            .contains(&dir.to_string_lossy().to_string()),
        "a revoked root must not leave its path in the buffer plugins read"
    );
}

#[test]
fn changing_the_directory_takes_effect_without_reapproval() {
    let env = support::filesystem_env("exports", "read-write");
    let first = env.tmp.path().join("one");
    let second = env.tmp.path().join("two");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    env.registry
        .set_filesystem_config("fs-plugin", "exports", &FilesystemConfig { path: first.to_string_lossy().to_string() })
        .unwrap();
    env.registry.set_filesystem_grant("fs-plugin", "exports", true).unwrap();

    let roots = env
        .registry
        .set_filesystem_config("fs-plugin", "exports", &FilesystemConfig { path: second.to_string_lossy().to_string() })
        .expect("path change");

    assert!(roots[0].grant.granted, "changing the path must not revoke the grant");
    assert!(support::filesystem_buffer(&env.registry, "fs-plugin")
        .contains(&second.to_string_lossy().to_string()));
}
```

`support::filesystem_buffer(&registry, id)` は共有バッファ(`filesystem_json`)の中身を返すテスト用アクセサ(`Registry::filesystem_buffer` として実装する)。

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core --test driver_fs_integration`
Expected: FAIL(未実装)

- [ ] **Step 3: `Registry` に実装する**

既存の `refresh_sidecar_runtime` と同じ流儀:

- `entries` ロックは manifest と共有ハンドルの取得の間だけ保持し、ファイル I/O はロック解放後
- プラグイン単位のロック(`sidecar_runtime_lock_for` と同じマップを再利用してよい。その場合は「サイドカーとファイルアクセスで同じ id 別ロックを共有する」ことをコメントで明示する)の下で、**永続化と `filesystem_json` の更新を不可分に**行う
- `set_filesystem_grant` は `granted == true` のとき `config.path` が空なら `RegistryError::Filesystem` で拒否する(取消は常に許す)
- `set_filesystem_config` は `FilesystemConfigStore::update_and_effective`(検証込み)→ バッファ再構築。**承認は維持する**(パスはフィンガープリントに含まれない)
- `filesystem_buffer(&self, id) -> Result<String, RegistryError>` を足す(テスト用アクセサ)

`PluginInfo` に `filesystem: Vec<FilesystemInfo>` を足し、`list()` で埋める。

- [ ] **Step 4: `runner.rs` と `edlr.rs` を配線する**

`start_plugins` に `filesystem_config_store: FilesystemConfigStore` 引数を足し、`load_and_run_plugin` で `FsRuntimeEntry` の配列を組み立てて `filesystem_json` を作る(承認済みのエントリだけが `path` を持つ)。`core/src/bin/edlr.rs` で `FilesystemConfigStore::new(settings_dir.clone())` を作って渡す。

- [ ] **Step 5: RPC を実装する**

`handle_rpc` に 3 分岐を追加し、既存の `sidecars_result_json` と同じ流儀で `filesystem_result_json(&[FilesystemInfo]) -> serde_json::Value` を書く(`{ "roots": [...] }`)。`plugins/list` の各要素に `filesystem` を追加する。検証は `Registry` 側の責務で、RPC 層は `RegistryError` を文字列にして返すだけ。

`core/tests/ws_rpc_integration.rs` に、3 メソッドの往復・未設定での承認拒否・未知プラグインのエラーを確認するテストを足す。

- [ ] **Step 6: テストを実行する**

Run: `cargo test -p edlr-core`
Expected: PASS

- [ ] **Step 7: コミット**

```bash
git add core/src core/tests
git commit -m "feat(plugin): wire filesystem config, grants, and RPC into the registry"
```

---

### Task 8: UI と README

**Files:**
- Create: `ui/frontend/src/components/FilesystemSection.tsx`, `ui/frontend/src/components/FilesystemSection.test.tsx`
- Modify: `ui/frontend/src/types/plugin.ts`, `ui/frontend/src/pages/Plugins.tsx`, `ui/frontend/src/pages/Plugins.test.tsx`, `ui/frontend/src/index.css`, `ui/src-tauri/src/main.rs`, `README.md`

**Interfaces:**
- Consumes: Task 7 の RPC
- Produces: `FilesystemSection` コンポーネント、`pick_directory` Tauri コマンド

- [ ] **Step 1: 型を足す**

```ts
export interface FilesystemConfig {
  path: string;
}

export interface FilesystemRoot {
  name: string;
  reason: string;
  mode: "read" | "read-write";
  granted: boolean;
  staleGrant: boolean;
  config: FilesystemConfig;
}

export interface FilesystemRoots {
  roots: FilesystemRoot[];
}
```

`PluginInfo` に `filesystem: FilesystemRoot[];` を追加する。

- [ ] **Step 2: 失敗するコンポーネントテストを書く**

`FilesystemSection.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import FilesystemSection from "./FilesystemSection";
import type { FilesystemRoot } from "../types/plugin";

function root(overrides: Partial<FilesystemRoot> = {}): FilesystemRoot {
  return {
    name: "exports",
    reason: "巡回した星系の一覧を CSV で書き出すため",
    mode: "read-write",
    granted: false,
    staleGrant: false,
    config: { path: "" },
    ...overrides,
  };
}

const noop = async () => {};

describe("FilesystemSection", () => {
  it("renders nothing when the plugin declares no roots", () => {
    const { container } = render(
      <FilesystemSection roots={[]} onConfigChange={noop} onGrantChange={noop} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("shows the reason and the ungranted notice", () => {
    render(<FilesystemSection roots={[root()]} onConfigChange={noop} onGrantChange={noop} />);
    expect(screen.getByText(/CSV で書き出すため/)).toBeInTheDocument();
    expect(
      screen.getByText(/未承認 — このプラグインはファイルにアクセスできません/),
    ).toBeInTheDocument();
  });

  it("warns about write access for read-write roots", () => {
    render(<FilesystemSection roots={[root()]} onConfigChange={noop} onGrantChange={noop} />);
    expect(screen.getByText(/読み取り・作成・上書き・削除できます/)).toBeInTheDocument();
  });

  it("warns about read-only access for read roots", () => {
    render(
      <FilesystemSection roots={[root({ mode: "read" })]} onConfigChange={noop} onGrantChange={noop} />,
    );
    expect(screen.getByText(/読み取れます/)).toBeInTheDocument();
    expect(screen.queryByText(/上書き・削除/)).not.toBeInTheDocument();
  });

  it("disables the grant toggle until a directory is saved", async () => {
    render(<FilesystemSection roots={[root()]} onConfigChange={noop} onGrantChange={noop} />);
    const toggle = screen.getByRole("checkbox", { name: /このフォルダへのアクセスを承認する/ });
    expect(toggle).toBeDisabled();

    await userEvent.type(screen.getByLabelText(/フォルダ/), "/home/u/exports");
    expect(toggle).toBeDisabled();
  });

  it("enables the grant toggle once the daemon has the directory", async () => {
    const onGrantChange = vi.fn(async () => {});
    render(
      <FilesystemSection
        roots={[root({ config: { path: "/home/u/exports" } })]}
        onConfigChange={noop}
        onGrantChange={onGrantChange}
      />,
    );
    const toggle = screen.getByRole("checkbox", { name: /このフォルダへのアクセスを承認する/ });
    expect(toggle).toBeEnabled();
    await userEvent.click(toggle);
    expect(onGrantChange).toHaveBeenCalledWith("exports", true);
  });

  it("shows a stale-grant warning", () => {
    render(
      <FilesystemSection roots={[root({ staleGrant: true })]} onConfigChange={noop} onGrantChange={noop} />,
    );
    expect(screen.getByText(/要求が変わったため再承認が必要/)).toBeInTheDocument();
  });

  it("surfaces an error from a rejected config save", async () => {
    const onConfigChange = vi.fn(async () => {
      throw new Error("protected directory");
    });
    render(
      <FilesystemSection roots={[root()]} onConfigChange={onConfigChange} onGrantChange={noop} />,
    );
    await userEvent.type(screen.getByLabelText(/フォルダ/), "/etc");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByText(/protected directory/)).toBeInTheDocument();
  });
});
```

- [ ] **Step 3: テストが失敗することを確認する**

Run: `cd ui/frontend && mise exec -- pnpm test`
Expected: FAIL(`FilesystemSection` が存在しない)

- [ ] **Step 4: `FilesystemSection.tsx` を実装する**

`SidecarSection.tsx` の流儀に合わせる:

- `roots.length === 0` なら `null`
- ルートごとに `reason`、mode バッジ(「読み取りのみ」/「読み書き」)、フォルダパス入力(`aria-label="フォルダ"`、Tauri 環境では「選択…」ボタンで `pick_directory`)、「保存」ボタン
- 承認チェックボックス(`aria-label="このフォルダへのアクセスを承認する"`)は **`root.config.path === ""` の間 disabled**(サーバが確認したパスで判定する。ローカルの未保存入力では有効化しない)
- `checked` は `root.granted` のみで駆動(楽観的更新をしない)
- 警告文は mode で出し分け(設計書の文言をそのまま使う)
- 未承認時・失効時の注記
- ローカル state は `useEffect` で `root.config` の変化に追随させる

`index.css` に `.filesystem-section` 等を `.sidecar-*` に倣って足す。

- [ ] **Step 5: テストを実行する**

Run: `cd ui/frontend && mise exec -- pnpm test`
Expected: PASS(8 テスト)

- [ ] **Step 6: `Plugins.tsx` に配線する**

`handleFilesystemConfig` / `handleFilesystemGrant` を `plugins/set-filesystem-config` / `plugins/set-filesystem-grant` を呼ぶ形で足し(応答の `roots` で `p.filesystem` を差し替える)、`<FilesystemSection ... />` を `SidecarSection` の下に置く。`Plugins.test.tsx` に、`plugins/list` のモック応答へ `filesystem` を含めたケースと、承認トグルが正しい params で RPC を呼ぶテストを 2 本足す。

Run: `cd ui/frontend && mise exec -- pnpm test`
Expected: PASS

- [ ] **Step 7: `pick_directory` を足す**

`ui/src-tauri/src/main.rs` に、`pick_journal_dir` と同じ実装のジェネリックなコマンドを足す:

```rust
/// ネイティブのディレクトリ選択ダイアログを開く(プラグインのファイル
/// アクセス設定用)。キャンセル時は None。
#[tauri::command]
async fn pick_directory(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten().map(|path| path.to_string())
}
```

`invoke_handler!` に追加する。既存の `pick_journal_dir` はそのまま残す(Settings 画面が使っている)。

Run: `cd ui/src-tauri && cargo test`
Expected: PASS

- [ ] **Step 8: README を更新する**

`README.md` の capability 節に「ファイルアクセス(driver-fs)」を足す:

- `[[filesystem]]` の書式(`name` / `reason` / `mode`)と、ディレクトリはユーザーが UI で指定すること
- 承認の粒度(ルート単位)、`path` 未設定では承認できないこと
- パス検証(構文 → 配下チェック → `openat2`)と、**ルート内のシンボリックリンクも拒否する**こと
- `read` / `read-range` の 8 MiB 上限と、超えるファイルは分割して読むこと
- `list` は再帰・ファイルのみ・上限 10,000 件
- 書き込みは原子的、`append` は非原子的
- 設定の保存先 `<settings-dir>/<id>.filesystem.json`、承認の保存先 `<grants-dir>/<id>.json`

- [ ] **Step 9: 全テストを実行する**

```bash
cargo test --workspace
cd ui/frontend && mise exec -- pnpm test
cd ../src-tauri && cargo test
```
Expected: 全て PASS

- [ ] **Step 10: コミット**

```bash
git add ui README.md
git commit -m "feat(ui): add filesystem access configuration and approval UI"
```

---

## 自己レビューメモ

- **設計書の全項目に対応するタスクがある**: パス検証 3 段(Task 1・2)、原子的書き込みと上限(Task 2)、manifest(Task 3)、設定と保護ディレクトリ(Task 4)、grants(Task 5)、WIT とホスト実装・mode 強制(Task 6)、Registry/RPC(Task 7)、UI と README(Task 8)
- **`RESOLVE_NO_SYMLINKS` の副作用**(ルート内のシンボリックリンクも拒否)は設計書の「パス検証」節の帰結。Task 2 のテストで固定し、README に明記する
- **`mode` の強制は core 側**(Task 6)。`drivers/fs` は grants を知らないので、ドライバ単体テストには mode の概念が出てこない
- **Task 5 の値型**: 既存 `SavedSidecarGrant` を流用するか `SavedEntryGrant` にリネームするかは実装者判断。リネームする場合、**ディスク上の JSON 形式が変わらないこと**をテストで確認すること(既存の grant ファイルが読めなくなると承認が全部飛ぶ)
- **プラグイン単位ロック**: Task 7 でサイドカーと同じロックマップを共有してよい。共有する場合、`sidecar_runtime_lock_for` の名前が実態と合わなくなるので `runtime_lock_for` へ改名し、ドキュメントコメントを両方の用途に合わせて直すこと
- **実 wasm 統合テストは入れていない**。判定は全て `HostCtx` 側にあり wasm 経由でも同じ関数を通るため(`driver_http_integration.rs` / サイドカーと同じ論拠)

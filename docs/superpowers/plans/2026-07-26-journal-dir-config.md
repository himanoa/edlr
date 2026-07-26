# Journal ディレクトリ設定 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Journal ディレクトリを設定ファイルで指定できるようにし、Tauri の Settings ページから編集・即時反映できるようにする。

**Architecture:** `core/src/config.rs` を軽量な新クレート `edlr-config`(依存は serde/serde_json のみ)へ抽出し、`edlr-core` と `edlr-ui` の両方が参照する単一の情報源にする。`edlr-ui`(Tauri シェル)が `config.json` を読み、値があればデーモンへ `--journal-dir` として渡す。デーモン自身は一切変更しない。設定変更時は Tauri が自分で spawn したデーモンのみ再起動する。

**Tech Stack:** Rust (Cargo workspace, serde/serde_json, Tauri 2, tauri-plugin-dialog), TypeScript (React 18, Vite 5, Vitest 2, @tauri-apps/api 2)

設計書: `docs/superpowers/specs/2026-07-26-edlr-journal-dir-config-design.md`

## Global Constraints

- Rust edition 2021。workspace はリポジトリルートの `Cargo.toml`(members に `core`, `drivers/http`, `drivers/channel`, `ui/src-tauri`)
- Node は mise 管理。**すべての pnpm コマンドは `mise exec -- pnpm ...` で実行する**(素の `pnpm` は PATH に無い場合がある)
- パッケージマネージャは pnpm 10.x。フロントエンドの作業ディレクトリは `ui/frontend`
- **デーモン(`core/src/bin/edlr.rs`)の journal_dir 決定ロジックは変更しない。** CLI 引数 → 自動検出 → 無ければ `exit(1)` のまま
- 設定ファイルのキーは camelCase(`journalDir`)。Rust 側は `#[serde(rename_all = "camelCase")]`
- 壊れた JSON は既定値へ倒さず `Err` を返す(`SettingsStore` とは意図的に異なる方針)
- ファイル書き込みは tmp + `rename` の atomic write に統一する
- UI の文言は日本語(既存 `Plugins.tsx` に合わせる)
- 着手時点のベースラインは Rust 169 テスト / フロントエンド 46 テストで、いずれも全パス。これを壊さないこと
  (capability + HTTP ドライバのマージ後に計測。着手前に `cargo test` と
  `cd ui/frontend && mise exec -- pnpm test` で再確認すること)

---

### Task 1: `edlr-config` クレートの抽出

`core/src/config.rs` を新クレートへ移設する。関数の中身とテストは一切変更しない純粋な移設で、`edlr-core` からは re-export して呼び出し側を無変更に保つ。

**Files:**
- Create: `config/Cargo.toml`
- Create: `config/src/lib.rs`
- Delete: `core/src/config.rs`
- Modify: `core/src/lib.rs:1`
- Modify: `core/Cargo.toml`(`[dependencies]` に追加)
- Modify: `Cargo.toml`(members に追加)

**Interfaces:**
- Consumes: なし(最初のタスク)
- Produces: クレート `edlr_config` が公開する
  `default_journal_dir(home: &Path) -> Option<PathBuf>`、
  `default_config_subdir(home: &Path, sub: &str) -> PathBuf`、
  `config_subdir(xdg_config_home: Option<&Path>, home: Option<&Path>, sub: &str) -> PathBuf`。
  `edlr_core::config` は同クレートのエイリアスとして解決される。

- [ ] **Step 1: 現状のテストが通ることを確認(ベースライン)**

Run: `cargo test 2>&1 | grep -E "^test result"`
Expected: すべて `ok`、失敗ゼロ。この時点の合計を控えておく(移設後に同数であることを確認するため)。

- [ ] **Step 2: `config/Cargo.toml` を作成**

```toml
[package]
name = "edlr-config"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: ルート `Cargo.toml` の members に追加**

```toml
[workspace]
resolver = "2"
members = ["config", "core", "drivers/http", "drivers/channel", "ui/src-tauri"]
```

- [ ] **Step 4: `core/src/config.rs` を `config/src/lib.rs` へ移動**

```bash
git mv core/src/config.rs config/src/lib.rs
```

内容は一切編集しない。`use std::path::{Path, PathBuf};` から始まり `#[cfg(test)] mod tests` までそのまま。

- [ ] **Step 5: `core/Cargo.toml` に依存を追加**

`[dependencies]` の先頭に 1 行足すだけ。**既存の依存行は一切消さないこと**
(capability + HTTP ドライバのマージで `url` / `sha2` / `edlr-driver-http` などが
増えているため、古いスニペットで丸ごと置き換えると壊れる)。

```toml
[dependencies]
edlr-config = { path = "../config" }
```

この 1 行を `[dependencies]` の直後に挿入し、以降の既存行はそのまま残す。

- [ ] **Step 6: `core/src/lib.rs` の 1 行目を re-export に差し替え**

変更前:

```rust
pub mod config;
```

変更後:

```rust
pub use edlr_config as config;
```

これにより `edlr_core::config::default_journal_dir(...)`(`core/src/bin/edlr.rs:49`)も
`crate::config::config_subdir(...)` も呼び出し側は無変更で解決される。

- [ ] **Step 7: 移設したテストが新クレートで通ることを確認**

Run: `cargo test -p edlr-config 2>&1 | grep -E "^test result|^error"`
Expected: `test result: ok. 9 passed; 0 failed`

- [ ] **Step 8: workspace 全体がビルド・テストを通ることを確認**

Run: `cargo build && cargo test 2>&1 | grep -E "^test result|^error"`
Expected: ビルド成功。テスト合計が Step 1 と同数(内訳は core から 9 が config へ移動している)。失敗ゼロ。

- [ ] **Step 9: コミット**

```bash
git add Cargo.toml Cargo.lock core/Cargo.toml core/src/lib.rs config/
git commit -m "refactor: extract config path resolution into edlr-config crate"
```

---

### Task 2: `AppConfig` と `config_file_path` の追加

設定ファイルの読み書きを `edlr-config` に実装する。ベース解決を `config_base` に括り出し、`config_subdir` と `config_file_path` が共有する。

**Files:**
- Modify: `config/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 の `edlr_config` クレート
- Produces:
  - `config_file_path(xdg_config_home: Option<&Path>, home: Option<&Path>) -> PathBuf` — `<base>/edlr/config.json`
  - `struct AppConfig { pub journal_dir: Option<PathBuf> }`(`Debug + Clone + Default + PartialEq + Serialize + Deserialize`)
  - `AppConfig::load(path: &Path) -> Result<AppConfig, ConfigError>`
  - `AppConfig::save(&self, path: &Path) -> Result<(), ConfigError>`
  - `enum ConfigError { Io(std::io::Error), Parse(serde_json::Error), Serialize(serde_json::Error) }`(`Display + std::error::Error` 実装済み)

- [ ] **Step 1: 失敗するテストを書く**

`config/src/lib.rs` の `mod tests` の末尾(最後の `}` の直前)に追記:

```rust
    #[test]
    fn config_file_path_uses_xdg_when_set() {
        assert_eq!(
            config_file_path(Some(Path::new("/xdg/config")), Some(Path::new("/home/pilot"))),
            PathBuf::from("/xdg/config/edlr/config.json")
        );
    }

    #[test]
    fn config_file_path_falls_back_to_home_dot_config() {
        assert_eq!(
            config_file_path(None, Some(Path::new("/home/pilot"))),
            PathBuf::from("/home/pilot/.config/edlr/config.json")
        );
    }

    #[test]
    fn load_returns_default_when_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        assert_eq!(AppConfig::load(&path).unwrap(), AppConfig::default());
    }

    #[test]
    fn load_reads_journal_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, r#"{"journalDir":"/mnt/game/ED"}"#).unwrap();

        let loaded = AppConfig::load(&path).unwrap();

        assert_eq!(loaded.journal_dir, Some(PathBuf::from("/mnt/game/ED")));
    }

    #[test]
    fn load_returns_err_on_broken_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, "not valid json {{{").unwrap();

        let err = AppConfig::load(&path).expect_err("broken json must not fall back to default");

        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn save_creates_dir_and_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("edlr").join("config.json");
        let config = AppConfig {
            journal_dir: Some(PathBuf::from("/mnt/game/ED")),
        };

        config.save(&path).unwrap();

        assert!(path.is_file());
        assert_eq!(AppConfig::load(&path).unwrap(), config);
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("edlr");
        let path = dir.join("config.json");

        AppConfig::default().save(&path).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name != "config.json")
            .collect();
        assert!(leftovers.is_empty(), "unexpected leftovers: {leftovers:?}");
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-config 2>&1 | tail -20`
Expected: コンパイルエラー。`cannot find function config_file_path` および `cannot find type AppConfig` / `ConfigError`。

- [ ] **Step 3: `config_base` を括り出す**

`config/src/lib.rs` の `config_subdir` を次で置き換える(ドキュメントコメントは既存のものを残す):

```rust
/// 設定ベースディレクトリ(`<base>/edlr`)を解決する。
/// `config_subdir` と `config_file_path` が共有する。
fn config_base(xdg_config_home: Option<&Path>, home: Option<&Path>) -> PathBuf {
    match xdg_config_home {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join("edlr"),
        _ => {
            let home = home
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".config").join("edlr")
        }
    }
}

pub fn config_subdir(xdg_config_home: Option<&Path>, home: Option<&Path>, sub: &str) -> PathBuf {
    config_base(xdg_config_home, home).join(sub)
}

/// 設定ファイル `<base>/edlr/config.json` の絶対パスを組み立てる。
pub fn config_file_path(xdg_config_home: Option<&Path>, home: Option<&Path>) -> PathBuf {
    config_base(xdg_config_home, home).join("config.json")
}
```

`default_config_subdir` は既存テストが参照しているためそのまま残す。

- [ ] **Step 4: `AppConfig` と `ConfigError` を実装**

`config/src/lib.rs` の先頭 `use` を差し替え:

```rust
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
```

`#[cfg(test)] mod tests` の直前に追記:

```rust
/// アプリ全体の設定(`<base>/edlr/config.json`)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// Journal ディレクトリ。`None` ならデーモンの自動検出に委ねる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_dir: Option<PathBuf>,
}

/// `AppConfig` の読み書きが返しうるエラー。
#[derive(Debug)]
pub enum ConfigError {
    /// 読み書き自体の失敗(ファイル不在を除く)。
    Io(io::Error),
    /// JSON として解釈できなかった。既定値へは倒さない。
    Parse(serde_json::Error),
    /// 保存直前のシリアライズに失敗した。
    Serialize(serde_json::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "failed to access config file: {e}"),
            ConfigError::Parse(e) => write!(f, "config file is not valid JSON: {e}"),
            ConfigError::Serialize(e) => write!(f, "failed to serialize config: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl AppConfig {
    /// 設定を読み込む。ファイルが存在しない場合のみ既定値を返す。
    ///
    /// JSON が壊れている場合は `Err(ConfigError::Parse)` を返し、既定値へは
    /// 倒さない。黙って倒すと「設定したのに反映されない」という、本機能が
    /// 解決しようとしている症状そのものになるため。
    pub fn load(path: &Path) -> Result<AppConfig, ConfigError> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(AppConfig::default()),
            Err(e) => return Err(ConfigError::Io(e)),
        };
        serde_json::from_str(&content).map_err(ConfigError::Parse)
    }

    /// 設定を保存する。親ディレクトリが無ければ作る。
    ///
    /// tmp ファイルへ書いてから `rename` することで、書き込み途中のファイルを
    /// 読まれることを防ぐ(`SettingsStore::update_and_effective` と同じ手口)。
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let dir = path.parent().ok_or_else(|| {
            ConfigError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "config path has no parent directory",
            ))
        })?;
        fs::create_dir_all(dir).map_err(ConfigError::Io)?;

        let serialized = serde_json::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        let tmp_path = dir.join(format!("config.json.tmp.{}", std::process::id()));
        fs::write(&tmp_path, serialized).map_err(ConfigError::Io)?;
        fs::rename(&tmp_path, path).map_err(ConfigError::Io)
    }
}
```

- [ ] **Step 5: テストが通ることを確認**

Run: `cargo test -p edlr-config 2>&1 | grep -E "^test result|^error"`
Expected: `test result: ok. 16 passed; 0 failed`(既存 9 + 新規 7)

- [ ] **Step 6: workspace 全体が壊れていないことを確認**

Run: `cargo test 2>&1 | grep -E "^test result: FAILED|^error" || echo "ALL GREEN"`
Expected: `ALL GREEN`

- [ ] **Step 7: コミット**

```bash
git add config/src/lib.rs
git commit -m "feat(config): add AppConfig with journalDir persistence"
```

---

### Task 3: Tauri が設定を読んでデーモンへ渡す

Tauri 起動時に `config.json` を読み、`resolve_journal_dir` で優先順位を決めてデーモンへ `--journal-dir` を渡す。UI はまだ無く、この時点では「手で `config.json` を置けば起動する」状態になる。

**Files:**
- Modify: `ui/src-tauri/Cargo.toml`
- Create: `ui/src-tauri/src/config.rs`
- Modify: `ui/src-tauri/src/main.rs`

**Interfaces:**
- Consumes: Task 2 の `edlr_config::{AppConfig, ConfigError, config_file_path}`
- Produces:
  - `config::resolve_journal_dir(env: Option<PathBuf>, config: Option<PathBuf>) -> Option<PathBuf>`
  - `config::LoadedConfig { pub path: PathBuf, pub config: AppConfig, pub error: Option<String> }`
  - `config::load_from_env() -> LoadedConfig` — `$XDG_CONFIG_HOME` / `$HOME` を読んでパスを解決し、`AppConfig::load` の失敗を `error` に文字列で保持する(起動は止めない)

- [ ] **Step 1: 失敗するテストを書く**

`ui/src-tauri/src/config.rs` を新規作成:

```rust
use edlr_config::{config_file_path, AppConfig};
use std::path::PathBuf;

/// 読み込み結果。JSON が壊れていても起動は止めず、`error` に理由を持って
/// UI へ見せる(黙って既定値に倒すと原因が分からなくなるため)。
pub struct LoadedConfig {
    pub path: PathBuf,
    pub config: AppConfig,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_wins_over_config() {
        let resolved = resolve_journal_dir(
            Some(PathBuf::from("/from/env")),
            Some(PathBuf::from("/from/config")),
        );
        assert_eq!(resolved, Some(PathBuf::from("/from/env")));
    }

    #[test]
    fn config_used_when_env_absent() {
        let resolved = resolve_journal_dir(None, Some(PathBuf::from("/from/config")));
        assert_eq!(resolved, Some(PathBuf::from("/from/config")));
    }

    #[test]
    fn none_when_neither_set() {
        // None は「--journal-dir を渡さない」= デーモンの自動検出に委ねるを意味する
        assert_eq!(resolve_journal_dir(None, None), None);
    }
}
```

- [ ] **Step 2: テストが失敗することを確認**

`ui/src-tauri/src/main.rs` の `mod daemon;` の下に `mod config;` を追加してから:

Run: `cargo test -p edlr-ui 2>&1 | tail -20`
Expected: コンパイルエラー `cannot find function resolve_journal_dir`。

- [ ] **Step 3: `ui/src-tauri/Cargo.toml` に依存を追加**

```toml
[dependencies]
edlr-config = { path = "../../config" }
tauri = { version = "2", features = [] }
```

- [ ] **Step 4: 最小実装を書く**

`ui/src-tauri/src/config.rs` の `#[cfg(test)] mod tests` の直前に追記:

```rust
/// 優先順位は env → 設定ファイル → None。
///
/// `None` は「`--journal-dir` を渡さない」を意味し、デーモンが従来どおり
/// Proton 既定パスの自動検出を行う。これにより「設定 > 自動検出」が成立し、
/// 自動検出が当たる環境では設定不要のままとなる。
pub fn resolve_journal_dir(env: Option<PathBuf>, config: Option<PathBuf>) -> Option<PathBuf> {
    env.or(config)
}

/// `$XDG_CONFIG_HOME` / `$HOME` からパスを解決して設定を読む。
pub fn load_from_env() -> LoadedConfig {
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = config_file_path(xdg.as_deref(), home.as_deref());

    match AppConfig::load(&path) {
        Ok(config) => LoadedConfig {
            path,
            config,
            error: None,
        },
        Err(e) => LoadedConfig {
            path,
            config: AppConfig::default(),
            error: Some(e.to_string()),
        },
    }
}
```

- [ ] **Step 5: テストが通ることを確認**

Run: `cargo test -p edlr-ui 2>&1 | grep -E "^test result|^error"`
Expected: `test result: ok. 8 passed; 0 failed`(既存 5 + 新規 3)

- [ ] **Step 6: `main.rs` の `autostart_daemon` を配線する**

`ui/src-tauri/src/main.rs` の `autostart_daemon` は現在シグネチャが引数なしで、
`EDLR_JOURNAL_DIR` だけを見ている(`main.rs:37`)。設定を受け取る形に変える。

変更前(`main.rs:37-38`):

```rust
    let journal_dir = std::env::var_os("EDLR_JOURNAL_DIR").map(PathBuf::from);
    match daemon::spawn_daemon(&bin, journal_dir.as_deref()) {
```

変更後:

```rust
    let journal_dir = config::resolve_journal_dir(
        std::env::var_os("EDLR_JOURNAL_DIR").map(PathBuf::from),
        config_journal_dir,
    );
    match daemon::spawn_daemon(&bin, journal_dir.as_deref()) {
```

あわせて関数シグネチャを変更:

```rust
fn autostart_daemon(config_journal_dir: Option<PathBuf>) -> Option<std::process::Child> {
```

`main()` の先頭を変更:

```rust
fn main() {
    let loaded = config::load_from_env();
    if let Some(error) = &loaded.error {
        eprintln!("failed to load {}: {error}", loaded.path.display());
    }
    let mut child = autostart_daemon(loaded.config.journal_dir.clone());
```

(以降の `tauri::Builder` 以下は現状のまま)

- [ ] **Step 7: ビルドと全テストを確認**

Run: `cargo build && cargo test 2>&1 | grep -E "^test result: FAILED|^error" || echo "ALL GREEN"`
Expected: `ALL GREEN`

- [ ] **Step 8: 手動で動作確認**

```bash
mkdir -p ~/.config/edlr
cat > ~/.config/edlr/config.json <<'JSON'
{"journalDir":"/mnt/game/SteamLibrary/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous"}
JSON
cargo run -p edlr-ui
```

Expected: `spawned edlr daemon (pid ...)` が出て、`error: journal directory not found` が**出ない**こと。
確認できたら Ctrl-C で終了する。

- [ ] **Step 9: コミット**

```bash
git add ui/src-tauri/Cargo.toml ui/src-tauri/src/config.rs ui/src-tauri/src/main.rs Cargo.lock
git commit -m "feat(ui): read journal dir from config.json at startup"
```

---

### Task 4: デーモンの再起動と IPC コマンド

デーモンハンドルを managed state へ移し、設定を保存してデーモンを再起動する IPC を追加する。

**Files:**
- Modify: `ui/src-tauri/src/main.rs`
- Modify: `ui/src-tauri/src/config.rs`(DTO 追加)

**Interfaces:**
- Consumes: Task 3 の `config::{load_from_env, resolve_journal_dir, LoadedConfig}`
- Produces: IPC コマンド 2 つ
  - `get_config() -> ConfigDto`
  - `set_journal_dir(path: String) -> Result<ConfigDto, String>`
  - `struct ConfigDto { journalDir: Option<String>, daemonManaged: bool, configError: Option<String> }`(camelCase でシリアライズ)

- [ ] **Step 1: 失敗するテストを書く**

`ui/src-tauri/Cargo.toml` の `[dev-dependencies]` に追加:

```toml
serde_json = "1"
```

`ui/src-tauri/src/config.rs` の `mod tests` に追記:

```rust
    #[test]
    fn dto_serializes_to_camel_case() {
        let dto = ConfigDto {
            journal_dir: Some("/mnt/game/ED".to_string()),
            daemon_managed: true,
            config_error: None,
        };

        let json = serde_json::to_value(&dto).unwrap();

        assert_eq!(json["journalDir"], "/mnt/game/ED");
        assert_eq!(json["daemonManaged"], true);
        assert!(json["configError"].is_null());
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-ui 2>&1 | tail -20`
Expected: コンパイルエラー `cannot find struct, variant or union type ConfigDto`。

- [ ] **Step 3: DTO を実装**

`ui/src-tauri/Cargo.toml` の `[dependencies]` に追加:

```toml
serde = { version = "1", features = ["derive"] }
```

`ui/src-tauri/src/config.rs` の `LoadedConfig` の下に追記:

```rust
use serde::Serialize;

/// フロントエンドへ返す設定のスナップショット。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDto {
    pub journal_dir: Option<String>,
    /// Tauri が spawn したデーモンを保持しているか。`false` の場合は
    /// 外部起動のデーモンなので再起動できない(勝手に殺さない)。
    pub daemon_managed: bool,
    pub config_error: Option<String>,
}
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-ui 2>&1 | grep -E "^test result|^error"`
Expected: `test result: ok. 9 passed; 0 failed`

- [ ] **Step 5: managed state と `restart_daemon` を実装**

`ui/src-tauri/src/main.rs` の `use` に追加:

```rust
use std::sync::{Arc, Mutex};
```

`autostart_daemon` の下に追記:

```rust
/// Tauri が保持するアプリ状態。
///
/// `daemon` を `Arc` で包むのは、`tauri::Builder::build` が失敗したときに
/// `main` 側からもデーモンを停止できるようにするため。`state` は `manage` へ
/// ムーブされてしまうので、同じ `Arc` のクローンを `main` に残しておく。
struct AppState {
    /// Tauri が spawn したデーモン。外部起動のデーモンを掴んでいる場合は None。
    daemon: Arc<Mutex<Option<std::process::Child>>>,
    config_path: PathBuf,
    config: Mutex<edlr_config::AppConfig>,
    config_error: Mutex<Option<String>>,
}

/// 保持しているデーモンを停止し、`journal_dir` で再 spawn する。
///
/// kill と re-spawn はこの関数だけが行う。将来サイドカーを導入する際に
/// SIGTERM + プロセスグループ化へ移行する変更箇所をここ 1 つに絞るため
/// (設計書「スコープ外」の前提条件を参照)。
fn restart_daemon(
    slot: &Mutex<Option<std::process::Child>>,
    journal_dir: Option<&Path>,
) -> Result<(), String> {
    let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
    let Some(mut old) = guard.take() else {
        return Err("daemon is not managed by this app".to_string());
    };
    let _ = old.kill();
    let _ = old.wait();

    let bin = resolve_bin().ok_or_else(|| "edlr binary not found".to_string())?;
    let child = daemon::spawn_daemon(&bin, journal_dir)
        .map_err(|e| format!("failed to spawn edlr daemon: {e}"))?;
    *guard = Some(child);
    Ok(())
}
```

`autostart_daemon` 内のバイナリ解決部分(`main.rs:23-35`)を `resolve_bin` として括り出し、
`autostart_daemon` と `restart_daemon` の双方から呼ぶ:

```rust
/// デーモンのバイナリを探す。探索順は daemon::resolve_edlr_bin に委ねる。
fn resolve_bin() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from));
    let dev_fallback = if cfg!(debug_assertions) {
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/edlr"))
    } else {
        None
    };
    daemon::resolve_edlr_bin(
        std::env::var_os("EDLR_BIN").map(PathBuf::from),
        exe_dir.as_deref(),
        daemon::find_in_path("edlr"),
        dev_fallback,
    )
}
```

`autostart_daemon` 側はこれを呼ぶだけにする:

```rust
    let Some(bin) = resolve_bin() else {
        eprintln!(
            "edlr binary not found (set EDLR_BIN or put edlr on PATH); starting UI without daemon"
        );
        return None;
    };
```

`use std::path::Path;` を `use` に足すこと。

- [ ] **Step 6: IPC コマンドを実装**

`main.rs` の `restart_daemon` の下に追記:

```rust
fn snapshot(state: &AppState) -> config::ConfigDto {
    let config = state.config.lock().unwrap_or_else(|p| p.into_inner());
    let error = state.config_error.lock().unwrap_or_else(|p| p.into_inner());
    let managed = state
        .daemon
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some();
    config::ConfigDto {
        journal_dir: config
            .journal_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        daemon_managed: managed,
        config_error: error.clone(),
    }
}

#[tauri::command]
fn get_config(state: tauri::State<'_, AppState>) -> config::ConfigDto {
    snapshot(&state)
}

/// journal_dir を検証・保存し、Tauri 管理下のデーモンを再起動する。
///
/// 外部起動のデーモンを掴んでいる場合は保存のみ行い、再起動しない
/// (`daemonManaged: false` を返して UI 側に反映を促す)。
/// 再起動に失敗しても保存はロールバックしない。ユーザーが入力した正しい値まで
/// 失われるため。
#[tauri::command]
fn set_journal_dir(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<config::ConfigDto, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("ディレクトリが存在しません: {path}"));
    }

    let updated = edlr_config::AppConfig {
        journal_dir: Some(dir.clone()),
    };
    updated
        .save(&state.config_path)
        .map_err(|e| format!("設定の保存に失敗しました: {e}"))?;

    {
        let mut guard = state.config.lock().unwrap_or_else(|p| p.into_inner());
        *guard = updated;
    }
    {
        let mut guard = state.config_error.lock().unwrap_or_else(|p| p.into_inner());
        *guard = None;
    }

    let managed = state
        .daemon
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some();
    if managed {
        restart_daemon(&state.daemon, Some(&dir))?;
    }

    Ok(snapshot(&state))
}
```

- [ ] **Step 7: `main()` を state 管理へ移行**

`main()` を次で置き換える。

**`AppState` そのものは `Arc` で包まず、そのまま `manage` する。** こうすると
Step 6 のコマンドが宣言している `tauri::State<'_, AppState>` と `manage` した
型が一致する(`Arc<AppState>` を manage すると、コマンド側も
`tauri::State<'_, Arc<AppState>>` にしなければ実行時に取得へ失敗する)。

かわりに **`daemon` フィールドだけ `Arc`** にして、そのクローンを `main` に
残す。これで `state` が `manage` へムーブされた後でも、ビルド失敗時と終了時の
両方からデーモンを停止できる(現行 `main.rs:62,78` の挙動を維持する)。

```rust
fn main() {
    // ウィンドウを出してフロントエンドを表示する薄い皮 + デーモンの道連れ起動。
    // 既に起動済みのデーモンには spawn も kill もしない。
    let loaded = config::load_from_env();
    if let Some(error) = &loaded.error {
        eprintln!("failed to load {}: {error}", loaded.path.display());
    }
    let child = autostart_daemon(loaded.config.journal_dir.clone());

    // state は manage へムーブされるため、停止用にこのハンドルを手元へ残す。
    let daemon = Arc::new(Mutex::new(child));

    let state = AppState {
        daemon: Arc::clone(&daemon),
        config_path: loaded.path,
        config: Mutex::new(loaded.config),
        config_error: Mutex::new(loaded.error),
    };

    let app = match tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![get_config, set_journal_dir])
        .build(tauri::generate_context!())
    {
        Ok(app) => app,
        Err(e) => {
            kill_daemon(&daemon);
            eprintln!("error while building tauri application: {e}");
            std::process::exit(1);
        }
    };

    app.run(move |_app, event| {
        if let tauri::RunEvent::Exit = event {
            kill_daemon(&daemon);
        }
    });
}

/// 保持しているデーモンがあれば停止する(終了時・ビルド失敗時)。
fn kill_daemon(slot: &Mutex<Option<std::process::Child>>) {
    let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(mut child) = guard.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}
```

- [ ] **Step 8: capability ファイルを作成**

**このタスクがこのアプリ初の IPC コマンドを追加する。** Tauri 2 ではフロントエンドが
`invoke` を呼ぶために capability の許可が要る。現在 `ui/src-tauri/capabilities/` は
**存在しない**(コマンドが 1 つも無かったため不要だった)。これを作らないと
コマンドはビルドは通るが実行時に拒否される。

`ui/src-tauri/capabilities/default.json` を新規作成:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "edlr のウィンドウが使う権限",
  "windows": ["main"],
  "permissions": ["core:default"]
}
```

`"windows"` の値は `tauri.conf.json` のウィンドウのラベルと一致する必要がある。
`ui/src-tauri/tauri.conf.json` の `app.windows[0]` に `label` が無い場合、Tauri の
既定ラベルは `"main"` なので上記のままでよい。`label` が明示されていればその値を使うこと。

- [ ] **Step 9: ビルドと全テストを確認**

Run: `cargo build && cargo test 2>&1 | grep -E "^test result: FAILED|^error" || echo "ALL GREEN"`
Expected: `ALL GREEN`

- [ ] **Step 10: コミット**

```bash
git add ui/src-tauri/
git commit -m "feat(ui): add get_config/set_journal_dir IPC with daemon restart"
```

---

### Task 5: ディレクトリ選択ダイアログ

`tauri-plugin-dialog` を導入し、ネイティブのディレクトリ選択を IPC で公開する。

**Files:**
- Modify: `ui/src-tauri/Cargo.toml`
- Modify: `ui/src-tauri/src/main.rs`
- Modify: `ui/src-tauri/capabilities/default.json`(存在しなければ作成)

**Interfaces:**
- Consumes: Task 4 の `AppState`
- Produces: IPC コマンド `pick_journal_dir() -> Option<String>`(キャンセル時 `None`)

- [ ] **Step 1: 依存を追加**

`ui/src-tauri/Cargo.toml`:

```toml
tauri-plugin-dialog = "2"
```

- [ ] **Step 2: プラグインを登録しコマンドを実装**

`main.rs` の `tauri::Builder::default()` チェーンに 1 行足す:

```rust
    let app = match tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_journal_dir,
            pick_journal_dir
        ])
        .build(tauri::generate_context!())
```

コマンド本体を追記:

```rust
/// ネイティブのディレクトリ選択ダイアログを開く。キャンセル時は None。
#[tauri::command]
async fn pick_journal_dir(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await
        .ok()
        .flatten()
        .map(|path| path.to_string())
}
```

`ui/src-tauri/Cargo.toml` の `[dependencies]` に追加:

```toml
tokio = { version = "1", features = ["sync"] }
```

- [ ] **Step 3: capability を許可**

Task 4 Step 8 で作成済みの `ui/src-tauri/capabilities/default.json` の
`permissions` 配列に `"dialog:allow-open"` を追加する。ファイル全体は次になる:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "edlr のウィンドウが使う権限",
  "windows": ["main"],
  "permissions": ["core:default", "dialog:allow-open"]
}
```

- [ ] **Step 4: ビルドを確認**

Run: `cargo build 2>&1 | tail -5`
Expected: `Finished`。エラーなし。

- [ ] **Step 5: 全テストを確認**

Run: `cargo test 2>&1 | grep -E "^test result: FAILED|^error" || echo "ALL GREEN"`
Expected: `ALL GREEN`

- [ ] **Step 6: コミット**

```bash
git add ui/src-tauri/ Cargo.lock
git commit -m "feat(ui): add native directory picker for journal dir"
```

---

### Task 6: フロントエンドの Tauri IPC ラッパ

`window.__TAURI_INTERNALS__` の有無で Tauri 環境を判定する薄いラッパを作る。ブラウザと vitest/jsdom は同じ「非 Tauri」経路を通る。

**Files:**
- Create: `ui/frontend/src/lib/tauri.ts`
- Create: `ui/frontend/src/lib/tauri.test.ts`
- Modify: `ui/frontend/package.json`

**Interfaces:**
- Consumes: Task 4・5 の IPC コマンド名
- Produces:
  - `isTauri(): boolean`
  - `invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>`
  - `type AppConfigDto = { journalDir: string | null; daemonManaged: boolean; configError: string | null }`

- [ ] **Step 1: 依存を追加**

```bash
cd ui/frontend && mise exec -- pnpm add @tauri-apps/api@^2
```

- [ ] **Step 2: 失敗するテストを書く**

`ui/frontend/src/lib/tauri.test.ts`:

```ts
import { afterEach, describe, expect, it } from "vitest";
import { isTauri } from "./tauri";

afterEach(() => {
  delete (globalThis as Record<string, unknown>).__TAURI_INTERNALS__;
});

describe("isTauri", () => {
  it("returns false in a plain browser or jsdom", () => {
    expect(isTauri()).toBe(false);
  });

  it("returns true when the Tauri internals bridge is present", () => {
    (globalThis as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    expect(isTauri()).toBe(true);
  });
});
```

- [ ] **Step 3: テストが失敗することを確認**

Run: `cd ui/frontend && mise exec -- pnpm vitest run src/lib/tauri.test.ts`
Expected: FAIL — `Failed to resolve import "./tauri"`

- [ ] **Step 4: 最小実装を書く**

`ui/frontend/src/lib/tauri.ts`:

```ts
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/** Tauri の webview 内かどうか。ブラウザと vitest/jsdom では false になる。 */
export function isTauri(): boolean {
  return typeof globalThis !== "undefined" && "__TAURI_INTERNALS__" in globalThis;
}

export type AppConfigDto = {
  journalDir: string | null;
  /** false なら外部起動のデーモン。設定は保存できるが再起動はされない。 */
  daemonManaged: boolean;
  configError: string | null;
};

/** Tauri コマンドを呼ぶ。非 Tauri 環境では呼び出し自体が誤りなので明示的に失敗させる。 */
export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    throw new Error("Tauri 環境ではありません");
  }
  return tauriInvoke<T>(cmd, args);
}
```

- [ ] **Step 5: テストが通ることを確認**

Run: `cd ui/frontend && mise exec -- pnpm vitest run src/lib/tauri.test.ts`
Expected: PASS、2 tests

- [ ] **Step 6: 既存のフロントエンドテストが壊れていないことを確認**

Run: `cd ui/frontend && mise exec -- pnpm test`
Expected: 全 PASS

- [ ] **Step 7: コミット**

```bash
git add ui/frontend/src/lib/tauri.ts ui/frontend/src/lib/tauri.test.ts ui/frontend/package.json ui/frontend/pnpm-lock.yaml
git commit -m "feat(ui): add tauri ipc wrapper with non-tauri detection"
```

---

### Task 7: Settings ページ

設定ページを追加し、未設定時は初期タブをそこにする。非 Tauri 環境では読み取り専用表示にする。

**Files:**
- Create: `ui/frontend/src/pages/Settings.tsx`
- Create: `ui/frontend/src/pages/Settings.test.tsx`
- Modify: `ui/frontend/src/App.tsx`
- Modify: `ui/frontend/src/App.test.tsx`

**Interfaces:**
- Consumes: Task 6 の `isTauri`、`invoke`、`AppConfigDto`
- Produces: `Settings` コンポーネント(default export、props なし)

- [ ] **Step 1: 失敗するテストを書く**

`ui/frontend/src/pages/Settings.test.tsx`:

```tsx
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import Settings from "./Settings";

vi.mock("../lib/tauri", () => ({
  isTauri: vi.fn(),
  invoke: vi.fn(),
}));

import { invoke, isTauri } from "../lib/tauri";

const mockIsTauri = vi.mocked(isTauri);
const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  vi.resetAllMocks();
});

describe("非 Tauri 環境", () => {
  it("読み取り専用の案内を出し、IPC を呼ばない", async () => {
    mockIsTauri.mockReturnValue(false);

    render(<Settings />);

    expect(
      await screen.findByText(/デスクトップアプリから変更してください/),
    ).toBeInTheDocument();
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});

describe("Tauri 環境", () => {
  it("現在の journalDir を表示する", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockResolvedValue({
      journalDir: "/mnt/game/ED",
      daemonManaged: true,
      configError: null,
    });

    render(<Settings />);

    expect(await screen.findByDisplayValue("/mnt/game/ED")).toBeInTheDocument();
  });

  it("保存に成功したら成功メッセージを出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return { journalDir: null, daemonManaged: true, configError: null };
      }
      return { journalDir: "/mnt/game/ED", daemonManaged: true, configError: null };
    });

    render(<Settings />);
    const input = await screen.findByLabelText("Journal ディレクトリ");
    await userEvent.type(input, "/mnt/game/ED");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));

    expect(await screen.findByText(/保存しました/)).toBeInTheDocument();
  });

  it("パスが不正ならエラーを出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return { journalDir: null, daemonManaged: true, configError: null };
      }
      throw new Error("ディレクトリが存在しません: /nope");
    });

    render(<Settings />);
    const input = await screen.findByLabelText("Journal ディレクトリ");
    await userEvent.type(input, "/nope");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));

    expect(await screen.findByText(/ディレクトリが存在しません/)).toBeInTheDocument();
  });

  it("外部起動デーモンなら再起動されない旨を出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return { journalDir: null, daemonManaged: false, configError: null };
      }
      return { journalDir: "/mnt/game/ED", daemonManaged: false, configError: null };
    });

    render(<Settings />);
    const input = await screen.findByLabelText("Journal ディレクトリ");
    await userEvent.type(input, "/mnt/game/ED");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => {
      expect(screen.getByText(/外部で起動中のデーモン/)).toBeInTheDocument();
    });
  });

  it("設定ファイルが壊れている場合は警告を出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockResolvedValue({
      journalDir: null,
      daemonManaged: true,
      configError: "config file is not valid JSON: expected value at line 1 column 1",
    });

    render(<Settings />);

    expect(await screen.findByText(/設定ファイルを読み込めませんでした/)).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cd ui/frontend && mise exec -- pnpm vitest run src/pages/Settings.test.tsx`
Expected: FAIL — `Failed to resolve import "./Settings"`

- [ ] **Step 3: 最小実装を書く**

`ui/frontend/src/pages/Settings.tsx`:

```tsx
import { useEffect, useRef, useState } from "react";
import { invoke, isTauri, type AppConfigDto } from "../lib/tauri";

type Status = "loading" | "ready" | "unavailable";

export default function Settings() {
  // Plugins.tsx と同じく、await をまたぐ setState をアンマウント後に撃たないよう守る
  const mountedRef = useRef(true);
  const [status, setStatus] = useState<Status>("loading");
  const [config, setConfig] = useState<AppConfigDto | null>(null);
  const [draft, setDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    mountedRef.current = true;

    if (!isTauri()) {
      setStatus("unavailable");
      return () => {
        mountedRef.current = false;
      };
    }

    invoke<AppConfigDto>("get_config")
      .then((res) => {
        if (!mountedRef.current) return;
        setConfig(res);
        setDraft(res.journalDir ?? "");
        setStatus("ready");
      })
      .catch((err) => {
        if (!mountedRef.current) return;
        setError(err instanceof Error ? err.message : String(err));
        setStatus("ready");
      });

    return () => {
      mountedRef.current = false;
    };
  }, []);

  const handlePick = async () => {
    const picked = await invoke<string | null>("pick_journal_dir");
    if (!mountedRef.current || picked === null) return;
    setDraft(picked);
  };

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      const updated = await invoke<AppConfigDto>("set_journal_dir", { path: draft });
      if (!mountedRef.current) return;
      setConfig(updated);
      setNotice(
        updated.daemonManaged
          ? "保存しました。デーモンを再起動しました。"
          : "保存しました。外部で起動中のデーモンには反映されていません。手動で再起動してください。",
      );
    } catch (err) {
      if (!mountedRef.current) return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (mountedRef.current) setSaving(false);
    }
  };

  return (
    <section>
      <h1>Settings</h1>

      {status === "loading" && <p className="note">読み込み中…</p>}

      {status === "unavailable" && (
        <p className="note">
          設定はデスクトップアプリから変更してください。ブラウザからは変更できません。
        </p>
      )}

      {status === "ready" && (
        <>
          {config?.configError && (
            <p className="form-error">
              設定ファイルを読み込めませんでした: {config.configError}
              <br />
              保存すると新しい内容で上書きされます。
            </p>
          )}

          <label htmlFor="journal-dir">Journal ディレクトリ</label>
          <input
            id="journal-dir"
            type="text"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            disabled={saving}
          />
          <button type="button" onClick={handlePick} disabled={saving}>
            選択…
          </button>
          <button type="button" onClick={handleSave} disabled={saving || draft === ""}>
            保存
          </button>

          <p className="note">
            未設定の場合は Proton の既定パスを自動検出します。自動検出が当たらない環境
            (セカンダリ Steam ライブラリなど)では、ここで明示的に指定してください。
          </p>

          {error && <p className="form-error">{error}</p>}
          {notice && <p className="note">{notice}</p>}
        </>
      )}
    </section>
  );
}
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cd ui/frontend && mise exec -- pnpm vitest run src/pages/Settings.test.tsx`
Expected: PASS、6 tests

- [ ] **Step 5: App.tsx にタブを追加し、未設定時の誘導を書く**

`ui/frontend/src/App.tsx` を次で置き換える:

```tsx
import { useEffect, useState } from "react";
import { invoke, isTauri, type AppConfigDto } from "./lib/tauri";
import Dashboard from "./pages/Dashboard";
import Logs from "./pages/Logs";
import Plugins from "./pages/Plugins";
import Settings from "./pages/Settings";

const TABS = ["Dashboard", "Logs", "Plugins", "Settings"] as const;
type Tab = (typeof TABS)[number];

export default function App() {
  const [tab, setTab] = useState<Tab>("Dashboard");

  // journal_dir が未設定なら Settings から始める。デーモンが居ない状態で
  // Dashboard を出しても何も表示できないため。
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    invoke<AppConfigDto>("get_config")
      .then((config) => {
        if (active && config.journalDir === null) setTab("Settings");
      })
      .catch(() => {
        // 取得に失敗しても既定タブのまま続行する
      });
    return () => {
      active = false;
    };
  }, []);

  return (
    <div className="app">
      <nav className="tabs">
        {TABS.map((t) => (
          <button
            key={t}
            className={t === tab ? "tab active" : "tab"}
            onClick={() => setTab(t)}
          >
            {t}
          </button>
        ))}
      </nav>
      <main className="page">
        {tab === "Dashboard" && <Dashboard />}
        {tab === "Logs" && <Logs />}
        {tab === "Plugins" && <Plugins />}
        {tab === "Settings" && <Settings />}
      </main>
    </div>
  );
}
```

- [ ] **Step 6: App のタブ誘導テストを追加**

`ui/frontend/src/App.test.tsx` の末尾に追記:

```tsx
describe("初期タブ", () => {
  it("journalDir が未設定なら Settings から始まる", async () => {
    vi.resetModules();
    vi.doMock("./lib/tauri", () => ({
      isTauri: () => true,
      invoke: vi.fn().mockResolvedValue({
        journalDir: null,
        daemonManaged: true,
        configError: null,
      }),
    }));
    const { default: FreshApp } = await import("./App");

    render(<FreshApp />);

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Settings" })).toHaveClass("active");
    });
    vi.doUnmock("./lib/tauri");
  });
});
```

必要な import(`describe` / `it` / `expect` / `vi` / `waitFor` / `screen` / `render`)が
ファイル冒頭に揃っているか確認し、足りないものを追加すること。

- [ ] **Step 7: フロントエンドの全テストを確認**

Run: `cd ui/frontend && mise exec -- pnpm test`
Expected: 全 PASS、失敗ゼロ

- [ ] **Step 8: 型チェックとビルドを確認**

Run: `cd ui/frontend && mise exec -- pnpm build`
Expected: `tsc -b` と `vite build` が成功

- [ ] **Step 9: 手動で通しの動作確認**

```bash
rm -f ~/.config/edlr/config.json
cd ui/src-tauri && mise exec -- pnpm dlx @tauri-apps/cli@^2 dev
```

Expected:
1. ウィンドウが開き、**Settings タブが選択された状態**で始まる
2. 「選択…」でディレクトリ選択ダイアログが開く
3. `/mnt/game/SteamLibrary/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous` を選んで「保存」
4. 「保存しました。デーモンを再起動しました。」が出る
5. Dashboard タブに切り替えると journal イベントが流れている
6. `cat ~/.config/edlr/config.json` に `journalDir` が入っている

- [ ] **Step 10: コミット**

```bash
git add ui/frontend/src/
git commit -m "feat(ui): add settings page for journal dir"
```

---

## 完了条件

- `cargo test` がルートから全パス(ベースライン 169 + Task 2 の 7 + Task 3 の 3 + Task 4 の 1 = 180)
- `cd ui/frontend && mise exec -- pnpm test` が全パス(ベースライン 46 + Task 6 の 2 + Task 7 の 7 = 55)
- `config.json` を消した状態で `tauri dev` すると Settings タブから始まり、
  ディレクトリを選んで保存するとデーモンが再起動して journal が流れる
- `config.json` に有効な `journalDir` がある状態で起動すると、最初からデーモンが動く
- `EDLR_JOURNAL_DIR` を設定すると `config.json` より優先される

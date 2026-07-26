# Journal 読み取り位置の永続化 + replay フラグ 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** デーモン再起動のたびに Journal を先頭から読み直す挙動をやめ、読み取り位置を永続化して再開する。あわせて「デーモンが動き出す前に既に書かれていたイベント」に `replay` フラグを立てる。

**Architecture:** 位置の永続化は新しい `PositionStore` が担い、`JournalTailer` はディスクに触らず「復元された位置から読む / 現在位置を教える」だけにする。`monitor` が両者を繋ぐ。`replay` は `Event::Journal` → WS JSON → WIT `event` の 3 箇所を同じ 1 ビットが通る。

**Tech Stack:** Rust 2021 / wasmtime component model + WIT / axum WebSocket。

**設計書:** `docs/superpowers/specs/2026-07-27-edlr-journal-position-design.md`

## Global Constraints

- Rust edition 2021。ドキュメントコメントは既存コードにならい日本語
- 新規依存なし
- **tailer はディスクに触らない**(位置の出し入れは `monitor` の責務)
- **保存は配信の後**(at-least-once。逆順にするとクラッシュ時にイベントが失われる)
- **保存するのは `pos - partial.len()`**(最後の完全な行の直後。partial を含む位置を保存しない)
- **replay の定義**: デーモンが動き出す前に既にファイルへ書かれていたイベント。実装上は「起動後の最初の poll で読み切った分までが `true`、それ以降の追記が `false`」
- **`Status` イベントの `replay` は常に `false`**
- state ディレクトリに書けなくても**デーモンは止めない**(warn を 1 度出して永続化なしで継続)
- ファイルの同一性は**名前だけ**で判断する(inode は見ない)
- WIT パッケージのバージョンを `edlr:plugin@0.1.0` → `@0.2.0` に上げる
- テスト実行: `cargo test`(ワークスペース)。フロントエンドは変更しない
- 各タスクの最後にコミットする(Conventional Commits)

## File Structure

**新規**

| ファイル | 責務 |
|---|---|
| `core/src/journal/position.rs` | `Position` / `PositionStore` — `<state-dir>/journal-position.json` の読み書き |
| `core/tests/journal_position_integration.rs` | 起動 → 保存 → 再起動で重複しないことの統合テスト |

**変更**

| ファイル | 変更内容 |
|---|---|
| `config/src/lib.rs` | `state_base` — `$XDG_STATE_HOME/edlr` の解決 |
| `core/src/journal/tailer.rs` | `resume_from` / `position()` / 返り値を `JournalLine`(`replay` 付き)に |
| `core/src/journal/mod.rs` | `pub mod position;` と re-export |
| `core/src/event.rs` | `Event::Journal` に `replay: bool` |
| `core/src/journal/parser.rs` | `parse_line(line, replay)` |
| `core/src/server.rs` | WS JSON に `replay` |
| `core/wit/plugin.wit` | パッケージを `@0.2.0` に、`record event` に `replay: bool` |
| `core/src/plugin/host.rs` | `call_on_event` に `replay` を渡す |
| `core/src/plugin/runner.rs` | `event_params` から `replay` を取り出す |
| `core/src/monitor.rs` | `PositionStore` を受け取り、復元と保存を行う |
| `core/src/bin/edlr.rs` | `--state-dir` の解決と `PositionStore` の組み立て |
| `examples/plugins/hello-logger/src/lib.rs` | 新 world でのビルド確認(必要なら追随) |
| `examples/plugins/inara-uploader/` | バインディング再生成 + `replay` の扱い |
| `README.md` | `--state-dir`、位置の永続化、`replay` の意味と使い分け |

---

### Task 1: `PositionStore`

**Files:**
- Create: `core/src/journal/position.rs`
- Modify: `config/src/lib.rs`, `core/src/journal/mod.rs`
- Test: 同ファイル内 `mod tests`、`config/src/lib.rs` の `mod tests`

**Interfaces:**
- Consumes: なし
- Produces:
  - `edlr_config::state_base(xdg_state_home: Option<&Path>, home: Option<&Path>) -> PathBuf`
  - `pub struct Position { pub file: String, pub offset: u64 }`(`Serialize + Deserialize + Clone + PartialEq + Debug`)
  - `pub struct PositionStore`(`new(dir: PathBuf)`)
  - `PositionStore::load(&self, journal_dir: &Path) -> Option<Position>`
  - `PositionStore::save(&self, journal_dir: &Path, position: &Position) -> std::io::Result<()>`

- [ ] **Step 1: `state_base` の失敗するテストを書く**

`config/src/lib.rs` の `mod tests` に:

```rust
    #[test]
    fn state_base_prefers_xdg_state_home() {
        let base = state_base(Some(Path::new("/x/state")), Some(Path::new("/home/u")));
        assert_eq!(base, Path::new("/x/state/edlr"));
    }

    #[test]
    fn state_base_falls_back_to_local_state_under_home() {
        let base = state_base(None, Some(Path::new("/home/u")));
        assert_eq!(base, Path::new("/home/u/.local/state/edlr"));
    }

    #[test]
    fn state_base_without_home_is_relative_to_the_current_directory() {
        // HOME も XDG_STATE_HOME も無い環境でも panic しない。
        let base = state_base(None, None);
        assert!(base.ends_with("edlr"));
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-config state_base`
Expected: FAIL(`state_base` が未定義)

- [ ] **Step 3: `state_base` を実装する**

`config/src/lib.rs` に、既存の `config_base` の隣へ:

```rust
/// 状態ファイルの置き場所 `<base>/edlr` を解決する。
///
/// XDG 的に「状態」(再作成できるが消えると不便なもの)は config ではなく
/// state に置く。`$XDG_STATE_HOME` が無ければ `~/.local/state`。
pub fn state_base(xdg_state_home: Option<&Path>, home: Option<&Path>) -> PathBuf {
    let base = match (xdg_state_home, home) {
        (Some(state_home), _) => state_home.to_path_buf(),
        (None, Some(home)) => home.join(".local").join("state"),
        (None, None) => PathBuf::from(".local").join("state"),
    };
    base.join("edlr")
}
```

- [ ] **Step 4: テストを実行する**

Run: `cargo test -p edlr-config`
Expected: PASS

- [ ] **Step 5: `PositionStore` の失敗するテストを書く**

`core/src/journal/position.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn round_trips_a_position_per_journal_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PositionStore::new(tmp.path().join("state"));
        let dir_a = Path::new("/journals/a");
        let dir_b = Path::new("/journals/b");

        assert_eq!(store.load(dir_a), None);

        store
            .save(
                dir_a,
                &Position { file: "Journal.a.log".into(), offset: 42 },
            )
            .expect("save should succeed");
        store
            .save(
                dir_b,
                &Position { file: "Journal.b.log".into(), offset: 7 },
            )
            .expect("save should succeed");

        assert_eq!(
            store.load(dir_a),
            Some(Position { file: "Journal.a.log".into(), offset: 42 })
        );
        assert_eq!(
            store.load(dir_b),
            Some(Position { file: "Journal.b.log".into(), offset: 7 })
        );
    }

    #[test]
    fn saving_the_same_directory_twice_overwrites_rather_than_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PositionStore::new(tmp.path().join("state"));
        let dir = Path::new("/journals/a");

        store.save(dir, &Position { file: "Journal.a.log".into(), offset: 1 }).unwrap();
        store.save(dir, &Position { file: "Journal.a.log".into(), offset: 99 }).unwrap();

        assert_eq!(store.load(dir).unwrap().offset, 99);
    }

    #[test]
    fn a_broken_file_reads_as_no_position() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("journal-position.json"), "not json {{{").unwrap();

        let store = PositionStore::new(dir);
        assert_eq!(store.load(Path::new("/journals/a")), None);
    }

    #[test]
    fn saving_creates_the_state_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("state");
        let store = PositionStore::new(dir.clone());

        store
            .save(Path::new("/journals/a"), &Position { file: "j.log".into(), offset: 1 })
            .expect("save should create the directory");

        assert!(dir.join("journal-position.json").is_file());
    }

    #[test]
    fn saving_into_an_unwritable_location_returns_an_error_instead_of_panicking() {
        // 既存のファイルをディレクトリとして使わせることで、確実に失敗させる。
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();

        let store = PositionStore::new(file.join("state"));
        assert!(store
            .save(Path::new("/journals/a"), &Position { file: "j.log".into(), offset: 1 })
            .is_err());
    }
}
```

- [ ] **Step 6: テストが失敗することを確認する**

`core/src/journal/mod.rs` に `pub mod position;` を足したうえで、

Run: `cargo test -p edlr-core position`
Expected: FAIL(`PositionStore` が未定義)

- [ ] **Step 7: `PositionStore` を実装する**

```rust
//! Journal の読み取り位置の永続化。
//!
//! デーモンを再起動したときに現行 Journal を先頭から読み直さないためのもの。
//! 保存先は `<state-dir>/journal-position.json` で、**Journal ディレクトリを
//! キーにしたマップ**。Settings でディレクトリを切り替えても、別ディレクトリの
//! 位置を誤って適用しないようにするため。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// あるディレクトリで「どのファイルをどこまで読んだか」。
///
/// `offset` は**最後の完全な行の直後**を指す(読み込み途中の不完全な行を
/// 含む位置を保存すると、再起動時にその行が頭を欠いた状態で読まれる)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Position {
    /// Journal ディレクトリ内のファイル名(ディレクトリはキー側が持つ)。
    pub file: String,
    pub offset: u64,
}

/// `<state-dir>/journal-position.json` を読み書きするストア。
///
/// `SettingsStore` などと同じく内部に `Mutex<()>` を持ち、read-merge-write を
/// 直列化する。書き込みは tmp + `rename` で原子的に行う。
pub struct PositionStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl PositionStore {
    pub fn new(dir: PathBuf) -> Self {
        PositionStore {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("journal-position.json")
    }

    fn read_all(&self) -> BTreeMap<String, Position> {
        fs::read_to_string(self.path())
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// 保存済みの位置を返す。ファイルが無い・壊れている・その
    /// ディレクトリの記録が無い場合はいずれも `None`(panic しない)。
    pub fn load(&self, journal_dir: &Path) -> Option<Position> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.read_all()
            .get(&journal_dir.to_string_lossy().to_string())
            .cloned()
    }

    /// 位置を保存する。他ディレクトリの記録は保持する。
    pub fn save(&self, journal_dir: &Path, position: &Position) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let mut all = self.read_all();
        all.insert(journal_dir.to_string_lossy().to_string(), position.clone());

        fs::create_dir_all(&self.dir)?;
        let serialized = serde_json::to_string_pretty(&all)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = self.dir.join(format!(
            "journal-position.json.tmp.{}",
            std::process::id()
        ));
        fs::write(&tmp, serialized)?;
        fs::rename(&tmp, self.path())
    }
}
```

- [ ] **Step 8: テストを実行する**

Run: `cargo test -p edlr-core position && cargo test -p edlr-config`
Expected: PASS(5 + 3 テスト)

- [ ] **Step 9: コミット**

```bash
git add core/src/journal/position.rs core/src/journal/mod.rs config/src/lib.rs
git commit -m "feat(journal): add a store for the journal read position"
```

---

### Task 2: Tailer の再開・行境界・replay

**Files:**
- Modify: `core/src/journal/tailer.rs`
- Test: 同ファイル内 `mod tests`

**Interfaces:**
- Consumes: `Position`(Task 1)
- Produces:
  - `pub struct JournalLine { pub text: String, pub replay: bool }`
  - `JournalTailer::resume_from(dir: PathBuf, position: Option<Position>) -> JournalTailer`
  - `JournalTailer::poll(&mut self) -> io::Result<Vec<JournalLine>>`(**返り値の型が変わる**)
  - `JournalTailer::position(&self) -> Option<Position>` — 最後の完全な行の直後を指す

- [ ] **Step 1: 失敗するテストを書く**

既存の `mod tests` に追記(既存テストは `poll()` が `Vec<String>` を返す前提なので、`Vec<JournalLine>` に合わせて `.text` を見るよう直す。**アサートしている内容は変えないこと**):

```rust
    #[test]
    fn resumes_from_a_saved_position_without_re_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Journal.2026-07-27T120000.01.log");
        append(&path, "{\"a\":1}\n{\"b\":2}\n");

        let mut first = JournalTailer::resume_from(dir.path().to_path_buf(), None);
        let lines = first.poll().unwrap();
        assert_eq!(lines.len(), 2);
        let saved = first.position().expect("position after reading");

        append(&path, "{\"c\":3}\n");

        let mut second = JournalTailer::resume_from(dir.path().to_path_buf(), Some(saved));
        let lines = second.poll().unwrap();
        assert_eq!(lines.len(), 1, "must not re-read what was already consumed");
        assert!(lines[0].text.contains("\"c\""));
    }

    #[test]
    fn the_saved_position_never_includes_a_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Journal.2026-07-27T120000.01.log");
        append(&path, "{\"a\":1}\n{\"partial\":");

        let mut tailer = JournalTailer::resume_from(dir.path().to_path_buf(), None);
        let lines = tailer.poll().unwrap();
        assert_eq!(lines.len(), 1);
        let saved = tailer.position().expect("position");

        // 途中で切れた行を書き足してから、保存位置で再開する。
        append(&path, "2}\n");
        let mut resumed = JournalTailer::resume_from(dir.path().to_path_buf(), Some(saved));
        let lines = resumed.poll().unwrap();

        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].text, "{\"partial\":2}",
            "resuming must not lose the head of a line that was incomplete"
        );
    }

    #[test]
    fn everything_read_in_the_first_poll_is_replay_and_later_appends_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Journal.2026-07-27T120000.01.log");
        append(&path, "{\"a\":1}\n{\"b\":2}\n");

        let mut tailer = JournalTailer::resume_from(dir.path().to_path_buf(), None);
        let first = tailer.poll().unwrap();
        assert!(first.iter().all(|l| l.replay), "pre-existing lines are replay");

        append(&path, "{\"c\":3}\n");
        let second = tailer.poll().unwrap();
        assert!(
            second.iter().all(|l| !l.replay),
            "lines appended after startup are live"
        );
    }

    #[test]
    fn resumed_catch_up_lines_are_also_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Journal.2026-07-27T120000.01.log");
        append(&path, "{\"a\":1}\n");

        let mut first = JournalTailer::resume_from(dir.path().to_path_buf(), None);
        first.poll().unwrap();
        let saved = first.position().unwrap();

        // デーモンが止まっている間に書かれたぶん。
        append(&path, "{\"b\":2}\n");

        let mut second = JournalTailer::resume_from(dir.path().to_path_buf(), Some(saved));
        let lines = second.poll().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].replay,
            "lines written while the daemon was down were already in the file at startup"
        );
    }

    #[test]
    fn a_saved_offset_past_the_end_restarts_that_file_from_the_top() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Journal.2026-07-27T120000.01.log");
        append(&path, "{\"a\":1}\n");

        let mut tailer = JournalTailer::resume_from(
            dir.path().to_path_buf(),
            Some(Position {
                file: "Journal.2026-07-27T120000.01.log".into(),
                offset: 9_999,
            }),
        );
        let lines = tailer.poll().unwrap();

        assert_eq!(lines.len(), 1, "a truncated/replaced file is read from the top");
    }

    #[test]
    fn a_saved_file_that_no_longer_exists_resumes_at_the_next_file() {
        let dir = tempfile::tempdir().unwrap();
        let newer = dir.path().join("Journal.2026-07-27T130000.01.log");
        append(&newer, "{\"b\":2}\n");

        let mut tailer = JournalTailer::resume_from(
            dir.path().to_path_buf(),
            Some(Position {
                file: "Journal.2026-07-27T120000.01.log".into(), // 既に消えている
                offset: 10,
            }),
        );
        let lines = tailer.poll().unwrap();

        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.contains("\"b\""));
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core tailer`
Expected: FAIL(`resume_from` / `JournalLine` / `position` が未定義)

- [ ] **Step 3: 実装する**

`tailer.rs` を次の形に変える:

```rust
/// tail で読み取った 1 行と、その行がデーモン起動前に既に書かれていたか。
#[derive(Debug, Clone, PartialEq)]
pub struct JournalLine {
    pub text: String,
    /// デーモンが動き出す前に既にファイルへ書かれていた行。
    pub replay: bool,
}

pub struct JournalTailer {
    dir: PathBuf,
    current: Option<PathBuf>,
    pos: u64,
    partial: String,
    /// 最初の poll を終えたか。起動直後の 1 回目で読み切った分までを
    /// `replay` とし、それ以降の追記を live とするための境界。
    caught_up: bool,
}
```

- `new(dir)` は `resume_from(dir, None)` に委譲する(既存の呼び出し側を壊さない)
- `resume_from(dir, position)` は、`position` があれば `current = dir.join(file)` と
  `pos = offset` を設定する。**ファイルの存在確認はここではしない**(`poll` の
  ローテーション処理に任せる)
- `poll()` の先頭で、`current` が設定されているのに実ファイルが存在しない場合は、
  `next_journal_after(&dir, &current)`(既存関数)で次のファイルへ進み、
  `pos = 0` にする。次が無ければ `latest_journal` にフォールバックする
- `read_new` は既存のまま(`len < self.pos` で先頭に戻す truncate 検出を含む)。
  収集した行を `JournalLine { text, replay: !self.caught_up }` に包む
- `poll()` の末尾で `self.caught_up = true` にする
- `position()` は次を返す:

```rust
    /// 保存すべき位置(最後の完全な行の直後)。まだ何も読んでいなければ `None`。
    ///
    /// `pos` は読み込んだバイト数ぶん進んでおり、行の途中で切れた分は
    /// `partial` にメモリ上で保持している。`pos` をそのまま保存すると、
    /// 再起動時に `partial` が失われ、その行が頭を欠いた状態で読まれて
    /// しまう(パーサが警告して捨てる = イベントが 1 つ消える)。
    pub fn position(&self) -> Option<Position> {
        let current = self.current.as_ref()?;
        let file = current.file_name()?.to_str()?.to_string();
        Some(Position {
            file,
            offset: self.pos - self.partial.len() as u64,
        })
    }
```

- [ ] **Step 4: テストを実行する**

Run: `cargo test -p edlr-core tailer`
Expected: PASS(既存テスト + 新規 6 テスト)

- [ ] **Step 5: コミット**

```bash
git add core/src/journal/tailer.rs
git commit -m "feat(journal): resume the tailer from a saved position and mark replayed lines"
```

---

### Task 3: `replay` をイベント・WS・WIT に通す

**Files:**
- Modify: `core/src/event.rs`, `core/src/journal/parser.rs`, `core/src/server.rs`, `core/wit/plugin.wit`, `core/src/plugin/host.rs`, `core/src/plugin/runner.rs`
- Test: 各ファイルの `mod tests`、`core/tests/ws_integration.rs`

**Interfaces:**
- Consumes: `JournalLine`(Task 2)
- Produces:
  - `Event::Journal { timestamp, event, raw, replay: bool }`
  - `parser::parse_line(line: &str, replay: bool) -> Option<Event>`(**引数追加**)
  - WS JSON の journal イベントに `"replay": bool`
  - WIT `record event` に `replay: bool`、パッケージ `edlr:plugin@0.2.0`
  - `PluginInstance::call_on_event(&mut self, kind, timestamp, name, payload_json, replay: bool)`(**引数追加**)

- [ ] **Step 1: 失敗するテストを書く**

`parser.rs` の `mod tests` に:

```rust
    #[test]
    fn carries_the_replay_flag_through() {
        let line = r#"{"timestamp":"2026-07-27T12:00:00Z","event":"FSDJump"}"#;
        let Some(Event::Journal { replay, .. }) = parse_line(line, true) else {
            panic!("expected Journal event");
        };
        assert!(replay);

        let Some(Event::Journal { replay, .. }) = parse_line(line, false) else {
            panic!("expected Journal event");
        };
        assert!(!replay);
    }
```

`server.rs` の `mod tests` に:

```rust
    #[test]
    fn ws_json_carries_replay_for_journal_events_and_never_for_status() {
        let journal = Event::Journal {
            timestamp: "2026-07-27T12:00:00Z".into(),
            event: "FSDJump".into(),
            raw: serde_json::json!({"event": "FSDJump"}),
            replay: true,
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&event_to_ws_json(&journal)).unwrap();
        assert_eq!(parsed["replay"], serde_json::json!(true));

        let status = Event::Status {
            raw: serde_json::json!({"Flags": 1}),
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&event_to_ws_json(&status)).unwrap();
        assert_eq!(
            parsed.get("replay"),
            None,
            "status is a snapshot of the present; it has no replay notion"
        );
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core replay`
Expected: FAIL(`Event::Journal` に `replay` が無い)

- [ ] **Step 3: 型と WS を変える**

`core/src/event.rs`:

```rust
    Journal {
        timestamp: String,
        event: String,
        raw: Value,
        /// デーモンが動き出す前に既に Journal へ書かれていたイベント。
        /// 通知・読み上げ系のプラグインはこれを無視し、アップローダ・集計系は
        /// 処理する、という使い分けを想定している。
        replay: bool,
    },
```

`parser.rs` の `parse_line` に `replay: bool` 引数を足し、`Event::Journal` に載せる。

`server.rs` の `event_to_ws_json` の journal 分岐に `"replay": replay` を足す。
`Status` 分岐は変更しない。

`Event::Journal` をリテラル構築している既存テスト・コード(`monitor.rs`、
`core/tests/*`、`plugin/runner.rs` のテスト等)に `replay: false` を足して
コンパイルを通す。**既存テストの意味は変えないこと。**

- [ ] **Step 4: WIT を上げる**

`core/wit/plugin.wit`:

- 1 行目を `package edlr:plugin@0.2.0;` に変更
- `world plugin-guest` の `include wasi:cli/imports@0.2.0;` は**そのまま**
  (WASI のバージョンであって edlr のバージョンではない)
- `record event` に `replay: bool,` を追加(コメントは「デーモンが動き出す前に
  既に書かれていたイベント」)

- [ ] **Step 5: ホスト側を追随させる**

`core/src/plugin/host.rs` の `call_on_event` に `replay: bool` 引数を足し、
生成された `WitEvent` に渡す。`core/src/plugin/runner.rs` の `event_params` を
`replay` も返す形にし、呼び出し側で渡す。

- [ ] **Step 6: テストを実行する**

Run: `cargo test -p edlr-core`
Expected: PASS

- [ ] **Step 7: コミット**

```bash
git add core/src core/wit
git commit -m "feat(core): carry a replay flag from the journal to plugins"
```

---

### Task 4: `monitor` と CLI の配線

**Files:**
- Modify: `core/src/monitor.rs`, `core/src/bin/edlr.rs`
- Create: `core/tests/journal_position_integration.rs`

**Interfaces:**
- Consumes: `PositionStore` / `Position`(Task 1)、`JournalTailer::{resume_from, position}`(Task 2)、`parse_line(line, replay)`(Task 3)
- Produces:
  - `monitor::run(dir: PathBuf, router: Router, interval: Duration, positions: Option<Arc<PositionStore>>)`(**引数追加**)
  - `edlr` に `--state-dir`

- [ ] **Step 1: 失敗する統合テストを書く**

`core/tests/journal_position_integration.rs`:

```rust
//! 位置の永続化が「再起動しても同じイベントを配り直さない」ことを、
//! monitor::run を実際に回して確認する。

use std::sync::Arc;
use std::time::Duration;

use edlr_core::journal::position::PositionStore;
use edlr_core::monitor;
use edlr_core::router::Router;

fn append(path: &std::path::Path, s: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(s.as_bytes()).unwrap();
}

/// `monitor::run` を短時間回し、その間に配信されたイベントを集める。
async fn collect_for(
    dir: &std::path::Path,
    positions: Arc<PositionStore>,
    millis: u64,
) -> Vec<edlr_core::event::Event> {
    let router = Router::new();
    let mut rx = router.subscribe();
    let handle = tokio::spawn(monitor::run(
        dir.to_path_buf(),
        router,
        Duration::from_millis(20),
        Some(positions),
    ));

    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(millis);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => seen.push((*event).clone()),
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    handle.abort();
    seen
}

#[tokio::test]
async fn a_restart_does_not_redeliver_what_was_already_read() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = tmp.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    let path = journal.join("Journal.2026-07-27T120000.01.log");
    append(
        &path,
        "{\"timestamp\":\"2026-07-27T12:00:00Z\",\"event\":\"FSDJump\"}\n",
    );

    let positions = Arc::new(PositionStore::new(tmp.path().join("state")));

    let first = collect_for(&journal, positions.clone(), 300).await;
    assert_eq!(first.len(), 1, "the pre-existing line is delivered once");

    // 2 回目の起動。ファイルは変わっていない。
    let second = collect_for(&journal, positions.clone(), 300).await;
    assert!(
        second.is_empty(),
        "a restart must not redeliver lines that were already consumed"
    );
}

#[tokio::test]
async fn lines_written_while_the_daemon_was_down_arrive_exactly_once_as_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = tmp.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    let path = journal.join("Journal.2026-07-27T120000.01.log");
    append(
        &path,
        "{\"timestamp\":\"2026-07-27T12:00:00Z\",\"event\":\"FSDJump\"}\n",
    );

    let positions = Arc::new(PositionStore::new(tmp.path().join("state")));
    collect_for(&journal, positions.clone(), 300).await;

    // デーモンが止まっている間に追記される。
    append(
        &path,
        "{\"timestamp\":\"2026-07-27T12:05:00Z\",\"event\":\"Docked\"}\n",
    );

    let second = collect_for(&journal, positions.clone(), 300).await;
    assert_eq!(second.len(), 1);
    match &second[0] {
        edlr_core::event::Event::Journal { event, replay, .. } => {
            assert_eq!(event, "Docked");
            assert!(replay, "it was already in the file when the daemon started");
        }
        other => panic!("expected a journal event, got {other:?}"),
    }
}

#[tokio::test]
async fn without_a_position_store_the_old_behaviour_is_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = tmp.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    append(
        &journal.join("Journal.2026-07-27T120000.01.log"),
        "{\"timestamp\":\"2026-07-27T12:00:00Z\",\"event\":\"FSDJump\"}\n",
    );

    let first = collect_for_without_store(&journal, 300).await;
    let second = collect_for_without_store(&journal, 300).await;

    assert_eq!(first.len(), 1);
    assert_eq!(
        second.len(),
        1,
        "with no store there is nothing to resume from, so the file is re-read"
    );
}

/// `positions = None`(state ディレクトリに書けない環境の劣化動作)。
async fn collect_for_without_store(
    dir: &std::path::Path,
    millis: u64,
) -> Vec<edlr_core::event::Event> {
    let router = Router::new();
    let mut rx = router.subscribe();
    let handle = tokio::spawn(monitor::run(
        dir.to_path_buf(),
        router,
        Duration::from_millis(20),
        None,
    ));

    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(millis);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => seen.push((*event).clone()),
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    handle.abort();
    seen
}
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core --test journal_position_integration`
Expected: FAIL(`monitor::run` の引数が合わない)

- [ ] **Step 3: `monitor` を実装する**

```rust
pub async fn run(
    dir: PathBuf,
    router: Router,
    interval: Duration,
    positions: Option<Arc<PositionStore>>,
) {
    let saved = positions.as_ref().and_then(|store| store.load(&dir));
    let mut tailer = JournalTailer::resume_from(dir.clone(), saved);
    // ... 既存の status / wake はそのまま

    while wake.rx.recv().await.is_some() {
        match tailer.poll() {
            Ok(lines) => {
                for line in lines {
                    match parser::parse_line(&line.text, line.replay) {
                        Some(event) => router.publish(event),
                        None => tracing::warn!("skipping unparsable journal line: {}", line.text),
                    }
                }
                // 配信した後に保存する(at-least-once)。保存に失敗しても
                // デーモンは止めず、警告を 1 度だけ出して続行する。
                if let (Some(store), Some(position)) = (positions.as_ref(), tailer.position()) {
                    if let Err(e) = store.save(&dir, &position) {
                        if !warned_about_saving {
                            tracing::warn!(
                                "failed to persist the journal position ({e}); \
                                 continuing without persistence"
                            );
                            warned_about_saving = true;
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("journal poll failed (will retry): {e}"),
        }
        // ... status の既存処理はそのまま
    }
}
```

`warned_about_saving` はループの外で `let mut warned_about_saving = false;` として持つ。

- [ ] **Step 4: CLI を足す**

`core/src/bin/edlr.rs` の `Args` に:

```rust
    /// Journal 読み取り位置の保存先ディレクトリ(未指定時は
    /// $XDG_STATE_HOME/edlr、未設定なら ~/.local/state/edlr)
    #[arg(long)]
    state_dir: Option<PathBuf>,
```

解決は既存の `plugins_dir` 等と同じ流儀で:

```rust
    let state_dir = args.state_dir.clone().unwrap_or_else(|| {
        let xdg_state_home = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
        config::state_base(xdg_state_home.as_deref(), home.as_deref())
    });
    let positions = Arc::new(edlr_core::journal::position::PositionStore::new(state_dir));
```

`monitor::run` の呼び出しに `Some(positions)` を渡す。

- [ ] **Step 5: テストを実行する**

Run: `cargo test -p edlr-core`
Expected: PASS

- [ ] **Step 6: コミット**

```bash
git add core/src/monitor.rs core/src/bin/edlr.rs core/tests/journal_position_integration.rs
git commit -m "feat(core): persist and resume the journal read position"
```

---

### Task 5: サンプルプラグインとドキュメント

**Files:**
- Modify: `examples/plugins/hello-logger/src/lib.rs`(必要なら)、`examples/plugins/inara-uploader/`(`gen/` 再生成、`main.go`)、`README.md`、`examples/plugins/inara-uploader/README.md`
- Test: `cargo build --release --target wasm32-wasip2`(hello-logger)、`./build.sh`(inara-uploader)

**Interfaces:**
- Consumes: WIT `@0.2.0` の `event.replay`(Task 3)
- Produces: 新 world でビルドできる 2 つのサンプル

- [ ] **Step 1: `hello-logger` を新 world でビルドする**

```bash
cd examples/plugins/hello-logger
rustup target add wasm32-wasip2   # 未追加なら
cargo build --release --target wasm32-wasip2
```

`wit_bindgen::generate!` はパス指定なのでバージョン変更に自動追随する。生成型に
`replay` が増えるだけで、`Guest` 実装のシグネチャは変わらないためコード変更は
不要のはず。ビルドが通らなければ、生成物に合わせて `src/lib.rs` を直す。

ログ出力に `replay` を含めると動作確認しやすいので、`on_event` のログ行を
次に変える:

```rust
            edlr::plugin::host_log::log(
                edlr::plugin::host_log::Level::Info,
                &format!(
                    "{}:{}{} {}",
                    ev.kind,
                    name,
                    if ev.replay { " (replay)" } else { "" },
                    ev.payload_json
                ),
            );
```

- [ ] **Step 2: `inara-uploader` のバインディングを再生成してビルドする**

```bash
cd examples/plugins/inara-uploader
go install go.bytecodealliance.org/cmd/wit-bindgen-go@v0.6.2   # 未導入なら
wit-bindgen-go generate --world plugin --out gen ../../../core/wit
./build.sh
```

`wit-bindgen-go` は `wasm-tools` を PATH に要求する。

- [ ] **Step 3: `inara-uploader` を `replay` ベースに切り替える**

`main.go` の `isReplay(timestamp, startedAt)` による時刻比較の回避策を捨て、
ホストから渡る `ev.Replay` を使う:

```go
	if !cfg.UploadHistorical && ev.Replay {
		st.skippedOld++
		if st.skippedOld == 1 || st.skippedOld%100 == 0 {
			logf(hostlog.LevelInfo,
				"skipping %d replayed journal event(s) (set uploadHistorical to send them)",
				st.skippedOld)
		}
		return
	}
```

`isReplay` 関数と `state.startedAt` の時刻比較用途を削除する(`startedAt` は
ログ表示に使っているならそのまま残してよい)。設定 `uploadHistorical` の説明も
「プラグイン起動より前」から「デーモン起動前に既に書かれていた(replay)」に直す。

**位置の永続化が入ったので、`uploadHistorical = true` にしても再起動での重複は
起きなくなった**ことを README に明記する。

- [ ] **Step 4: ドキュメントを更新する**

ルート `README.md`:

- `--state-dir` を CLI の一覧に追加(既定 `$XDG_STATE_HOME/edlr`、未設定なら `~/.local/state/edlr`)
- 「Journal の読み取り位置を `<state-dir>/journal-position.json` に保存し、再起動時は
  そこから再開する」旨
- `replay` フラグの意味(デーモンが動き出す前に既に書かれていたイベント)と、
  プラグイン側の使い分け(通知系は無視 / アップローダは処理)
- WS の journal イベントに `replay` が載ること
- WIT パッケージが `edlr:plugin@0.2.0` になったこと、**旧 world でビルドされた
  プラグインはロードに失敗する**こと
- state ディレクトリに書けない場合は警告を出して永続化なしで動くこと
- 同じ Journal ディレクトリを複数のデーモンで見る構成はサポートしないこと

`examples/plugins/inara-uploader/README.md`:

- 「不足している実装」の 2 番(tailer が位置を永続化しない)を**解決済みとして削除**し、
  以降を繰り上げる。設定表の `uploadHistorical` の説明も `replay` ベースに直す

- [ ] **Step 5: 全テストを実行する**

```bash
cargo test --workspace
cd ui/frontend && mise exec -- pnpm test
cd ../src-tauri && cargo test
```
Expected: 全て PASS

- [ ] **Step 6: コミット**

```bash
git add examples README.md
git commit -m "feat(examples): use the replay flag and document position persistence"
```

---

## 自己レビューメモ

- 設計書の全項目に対応するタスクがある: 保存先と形式(Task 1)、行境界(Task 2)、replay の境界(Task 2)、型と WIT と ABI(Task 3)、異常系の各行(Task 1 の壊れた JSON / Task 2 の offset 超過・ファイル消失 / Task 4 の書き込み失敗)、テスト方針(各タスク)、ドキュメント(Task 5)
- **`monitor::run` の引数に `Option<Arc<PositionStore>>` を選んだ理由**: state ディレクトリに書けない環境で「永続化なしで動き続ける」を型で表現でき、既存のテスト(位置を気にしないもの)が `None` を渡すだけで済むため
- **`parse_line` に引数を足す形にした理由**: `Event::Journal` を構築するのはパーサなので、replay を後から差し込むより素直。呼び出し元は `monitor` の 1 箇所だけ
- Task 5 の hello-logger は「ビルドが通れば変更不要」の可能性が高いが、ログに `replay` を出す変更は動作確認に効くので入れている
- **Task 3 で `Event::Journal` のリテラル構築箇所を直す作業が広く薄く発生する**(既存テスト多数)。機械的だが、`replay: false` を足すだけで意味が変わらないことを実装者が確認すること

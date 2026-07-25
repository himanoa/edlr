# edlr モノレポ + 監視コア Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** edlr モノレポ(Cargo workspace)を構築し、`core` に Journal tail・Status.json 監視・broadcast 配信の監視コアを実装する。

**Architecture:** `core`(edlr-core)が Journal の JSON Lines を position 追跡で tail し、Status.json の変更を読み、`tokio::sync::broadcast` で `Arc<Event>` を配信する。ファイル検知は inotify(notify クレート)+ 常時 1 秒ポーリングのハイブリッドで、読み取りが冪等なので inotify 故障判定は不要。`drivers/http`・`drivers/channel` は空スケルトン、`ui/` は README のみ。

**Tech Stack:** Rust (edition 2021), tokio, serde_json, notify 8, clap 4, tracing。dev-dependency: tempfile。

## Global Constraints

- crate 名: `edlr-core` / `edlr-driver-http` / `edlr-driver-channel`(設計書どおり)
- バイナリ名: `edlr`
- 監視ループはファイル消失・パース失敗で panic せず、ログして継続する
- 壊れた JSON 行・不完全な Status.json はスキップ(次回リトライ)
- Journal 既定パス(Proton): `~/.steam/steam/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous`
- コミットは各タスク末尾で行い、メッセージ末尾に `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` を付ける

---

### Task 1: Cargo workspace の骨組み

**Files:**
- Create: `Cargo.toml`(workspace ルート)
- Create: `core/Cargo.toml`, `core/src/lib.rs`
- Create: `drivers/http/Cargo.toml`, `drivers/http/src/lib.rs`
- Create: `drivers/channel/Cargo.toml`, `drivers/channel/src/lib.rs`
- Create: `ui/README.md`
- Modify: `.gitignore`(`/target` 追加)

**Interfaces:**
- Produces: workspace メンバ `edlr-core`(後続タスクは全て `core/src/` 配下に実装)

- [ ] **Step 1: ルート `Cargo.toml` を書く**

```toml
[workspace]
resolver = "2"
members = ["core", "drivers/http", "drivers/channel"]
```

- [ ] **Step 2: `core/Cargo.toml` を書く**

```toml
[package]
name = "edlr-core"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "edlr"
path = "src/bin/edlr.rs"

[dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }
serde_json = "1"
notify = "8"
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = "0.3"

[dev-dependencies]
tempfile = "3"
```

`core/src/lib.rs` は空ファイル、`core/src/bin/edlr.rs` は暫定で `fn main() {}` を置く。

- [ ] **Step 3: ドライバスケルトンを書く**

`drivers/http/Cargo.toml`:

```toml
[package]
name = "edlr-driver-http"
version = "0.1.0"
edition = "2021"
```

`drivers/http/src/lib.rs`:

```rust
//! HTTP driver for edlr. Placeholder until the capability model is designed.
```

`drivers/channel/Cargo.toml` は name を `edlr-driver-channel` にして同様。`drivers/channel/src/lib.rs`:

```rust
//! Inter-plugin channel driver for edlr. Placeholder until the capability model is designed.
```

- [ ] **Step 4: `ui/README.md` を書く**

```markdown
# edlr ui

デーモンに WebSocket で接続する GUI クライアント。実装順序(spec.md 参照)に従い、
まずブラウザ版ダッシュボードを作り、その後 Tauri の薄い皮を被せる。現時点では未実装。
```

- [ ] **Step 5: `.gitignore` に `/target` を追記し、ビルド確認**

Run: `cargo build --workspace`
Expected: 3 crate すべてコンパイル成功

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: scaffold cargo workspace (core, drivers, ui placeholder)"
```

---

### Task 2: Event 型と Journal 行パーサ

**Files:**
- Create: `core/src/event.rs`
- Create: `core/src/journal/mod.rs`, `core/src/journal/parser.rs`
- Modify: `core/src/lib.rs`

**Interfaces:**
- Produces: `pub enum Event { Journal { timestamp: String, event: String, raw: serde_json::Value }, Status { raw: serde_json::Value } }`(`core/src/event.rs`, `Clone + Debug + PartialEq` 導出)
- Produces: `pub fn parse_line(line: &str) -> Option<Event>`(`core/src/journal/parser.rs`)— 壊れた行・timestamp/event 欠落行は `None`

- [ ] **Step 1: 失敗するテストを書く**(`core/src/journal/parser.rs` 内の `#[cfg(test)]`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

    #[test]
    fn parses_journal_line() {
        let line = r#"{"timestamp":"2026-07-25T12:00:00Z","event":"FSDJump","StarSystem":"Sol"}"#;
        let Some(Event::Journal { timestamp, event, raw }) = parse_line(line) else {
            panic!("expected Journal event");
        };
        assert_eq!(timestamp, "2026-07-25T12:00:00Z");
        assert_eq!(event, "FSDJump");
        assert_eq!(raw["StarSystem"], "Sol");
    }

    #[test]
    fn rejects_broken_or_incomplete_lines() {
        assert_eq!(parse_line("{not json"), None);
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line(r#"{"timestamp":"t"}"#), None); // event 欠落
        assert_eq!(parse_line(r#"{"event":"e"}"#), None); // timestamp 欠落
    }
}
```

- [ ] **Step 2: テストが失敗する(コンパイルエラーになる)ことを確認**

Run: `cargo test -p edlr-core`
Expected: FAIL(`event` モジュール・`parse_line` 未定義)

- [ ] **Step 3: 実装する**

`core/src/event.rs`:

```rust
use serde_json::Value;

/// カーネルが配信するイベント。生 JSON を保持し、型付けは下流に委ねる。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Journal {
        timestamp: String,
        event: String,
        raw: Value,
    },
    Status {
        raw: Value,
    },
}
```

`core/src/journal/parser.rs`:

```rust
use crate::event::Event;
use serde_json::Value;

/// Journal の JSON Lines 1 行をパースする。壊れた行や必須フィールド欠落は None。
pub fn parse_line(line: &str) -> Option<Event> {
    let raw: Value = serde_json::from_str(line.trim()).ok()?;
    let timestamp = raw.get("timestamp")?.as_str()?.to_string();
    let event = raw.get("event")?.as_str()?.to_string();
    Some(Event::Journal { timestamp, event, raw })
}
```

`core/src/journal/mod.rs`:

```rust
pub mod parser;
```

`core/src/lib.rs`:

```rust
pub mod event;
pub mod journal;
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core`
Expected: PASS(2 テスト)

- [ ] **Step 5: Commit**

```bash
git add core/src && git commit -m "feat(core): add Event type and journal line parser"
```

---

### Task 3: 最新 Journal ファイルの発見

**Files:**
- Create: `core/src/journal/discovery.rs`
- Modify: `core/src/journal/mod.rs`

**Interfaces:**
- Produces: `pub fn latest_journal(dir: &Path) -> std::io::Result<Option<PathBuf>>` — `Journal.*.log` のうちファイル名の辞書順で最大のもの(ED のファイル名はタイムスタンプ形式なので辞書順 = 新しい順)。該当なしなら `Ok(None)`

- [ ] **Step 1: 失敗するテストを書く**(`core/src/journal/discovery.rs` 内)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_lexicographically_latest_journal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Journal.2026-07-24T090000.01.log"), "").unwrap();
        std::fs::write(dir.path().join("Journal.2026-07-25T120000.01.log"), "").unwrap();
        std::fs::write(dir.path().join("Status.json"), "").unwrap();
        let latest = latest_journal(dir.path()).unwrap().unwrap();
        assert_eq!(
            latest.file_name().unwrap().to_str().unwrap(),
            "Journal.2026-07-25T120000.01.log"
        );
    }

    #[test]
    fn returns_none_when_no_journal() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(latest_journal(dir.path()).unwrap(), None);
    }
}
```

- [ ] **Step 2: 失敗確認**

Run: `cargo test -p edlr-core journal::discovery`
Expected: FAIL(`latest_journal` 未定義)

- [ ] **Step 3: 実装する**

```rust
use std::io;
use std::path::{Path, PathBuf};

/// dir 内の最新 Journal ファイルを返す。ファイル名はタイムスタンプを含むため辞書順最大が最新。
pub fn latest_journal(dir: &Path) -> io::Result<Option<PathBuf>> {
    let mut latest: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(name.starts_with("Journal.") && name.ends_with(".log")) {
            continue;
        }
        if latest.as_ref().is_none_or(|l| path > *l) {
            latest = Some(path);
        }
    }
    Ok(latest)
}
```

`mod.rs` に `pub mod discovery;` を追加。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core journal::discovery`
Expected: PASS(2 テスト)

- [ ] **Step 5: Commit**

```bash
git add core/src/journal && git commit -m "feat(core): discover latest journal file"
```

---

### Task 4: Journal tailer(position 追跡 + ローテーション追従)

**Files:**
- Create: `core/src/journal/tailer.rs`
- Modify: `core/src/journal/mod.rs`

**Interfaces:**
- Consumes: `discovery::latest_journal`
- Produces: `pub struct JournalTailer` / `pub fn new(dir: PathBuf) -> JournalTailer` / `pub fn poll(&mut self) -> std::io::Result<Vec<String>>` — 前回以降に追記された**完全な行**(改行で終わった行)を返す。改行前の書きかけ行は内部バッファに保持。より新しい Journal ファイルが現れたら旧ファイルの残りを読み切ってから切り替える。ファイル短縮(truncate)時は先頭から読み直す

- [ ] **Step 1: 失敗するテストを書く**(`core/src/journal/tailer.rs` 内)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;

    fn append(path: &std::path::Path, s: &str) {
        let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn reads_only_appended_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let j = dir.path().join("Journal.2026-07-25T120000.01.log");
        append(&j, "line1\nline2\n");
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        assert_eq!(t.poll().unwrap(), vec!["line1", "line2"]);
        assert_eq!(t.poll().unwrap(), Vec::<String>::new()); // 追記なし → 空
        append(&j, "line3\npart"); // 書きかけ行は返さない
        assert_eq!(t.poll().unwrap(), vec!["line3"]);
        append(&j, "ial\n"); // 書きかけの続き
        assert_eq!(t.poll().unwrap(), vec!["partial"]);
    }

    #[test]
    fn follows_rotation_to_newer_file() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("Journal.2026-07-25T120000.01.log");
        append(&old, "old1\n");
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        assert_eq!(t.poll().unwrap(), vec!["old1"]);
        append(&old, "old2\n"); // 新ファイル出現と同時に旧ファイルにも追記済みのケース
        let new = dir.path().join("Journal.2026-07-25T130000.01.log");
        append(&new, "new1\n");
        assert_eq!(t.poll().unwrap(), vec!["old2", "new1"]);
    }

    #[test]
    fn restarts_from_top_on_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let j = dir.path().join("Journal.2026-07-25T120000.01.log");
        append(&j, "aaaa\nbbbb\n");
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        t.poll().unwrap();
        std::fs::write(&j, "cc\n").unwrap(); // 短縮
        assert_eq!(t.poll().unwrap(), vec!["cc"]);
    }

    #[test]
    fn empty_dir_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        assert_eq!(t.poll().unwrap(), Vec::<String>::new());
    }
}
```

- [ ] **Step 2: 失敗確認**

Run: `cargo test -p edlr-core journal::tailer`
Expected: FAIL(`JournalTailer` 未定義)

- [ ] **Step 3: 実装する**

```rust
use super::discovery::latest_journal;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

/// Journal ディレクトリを tail する。position 追跡により読み取りは冪等。
pub struct JournalTailer {
    dir: PathBuf,
    current: Option<PathBuf>,
    pos: u64,
    partial: String,
}

impl JournalTailer {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, current: None, pos: 0, partial: String::new() }
    }

    /// 追記された完全な行を返す。新しい Journal が現れたら旧ファイルを読み切って切り替える。
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        let mut lines = Vec::new();
        let latest = latest_journal(&self.dir)?;
        if let Some(cur) = self.current.clone() {
            self.read_new(&cur, &mut lines)?;
        }
        if latest != self.current {
            // 切り替え: 旧ファイルは読み切り済みなので新ファイルを先頭から
            self.current = latest;
            self.pos = 0;
            self.partial.clear();
            if let Some(cur) = self.current.clone() {
                self.read_new(&cur, &mut lines)?;
            }
        }
        Ok(lines)
    }

    fn read_new(&mut self, path: &std::path::Path, lines: &mut Vec<String>) -> io::Result<()> {
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return Ok(()), // 消えた/一時的に開けない → 次回リトライ
        };
        let len = file.metadata()?.len();
        if len < self.pos {
            // truncate された → 先頭から読み直す
            self.pos = 0;
            self.partial.clear();
        }
        file.seek(SeekFrom::Start(self.pos))?;
        let mut chunk = String::new();
        file.read_to_string(&mut chunk)?;
        self.pos = len;
        self.partial.push_str(&chunk);
        while let Some(nl) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=nl).collect();
            let line = line.trim_end();
            if !line.is_empty() {
                lines.push(line.to_string());
            }
        }
        Ok(())
    }
}
```

`mod.rs` に `pub mod tailer;` を追加。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core journal::tailer`
Expected: PASS(4 テスト)

- [ ] **Step 5: Commit**

```bash
git add core/src/journal && git commit -m "feat(core): journal tailer with rotation and partial-line handling"
```

---

### Task 5: Status.json リーダー(重複排除 + 不完全書き込み耐性)

**Files:**
- Create: `core/src/status.rs`
- Modify: `core/src/lib.rs`

**Interfaces:**
- Produces: `pub struct StatusReader` / `pub fn new(path: PathBuf) -> StatusReader` / `pub fn poll(&mut self) -> Option<serde_json::Value>` — 内容が前回から変化した場合のみ `Some(raw)`。ファイル不在・空・不完全 JSON は `None`(次回リトライ)

- [ ] **Step 1: 失敗するテストを書く**(`core/src/status.rs` 内)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_only_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Status.json");
        std::fs::write(&path, r#"{"Flags":1}"#).unwrap();
        let mut r = StatusReader::new(path.clone());
        assert_eq!(r.poll().unwrap()["Flags"], 1);
        assert_eq!(r.poll(), None); // 同一内容 → 配信しない
        std::fs::write(&path, r#"{"Flags":2}"#).unwrap();
        assert_eq!(r.poll().unwrap()["Flags"], 2);
    }

    #[test]
    fn tolerates_missing_empty_and_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Status.json");
        let mut r = StatusReader::new(path.clone());
        assert_eq!(r.poll(), None); // 不在
        std::fs::write(&path, "").unwrap();
        assert_eq!(r.poll(), None); // 空
        std::fs::write(&path, r#"{"Flags"#).unwrap();
        assert_eq!(r.poll(), None); // 書き込み途中
        std::fs::write(&path, r#"{"Flags":3}"#).unwrap();
        assert_eq!(r.poll().unwrap()["Flags"], 3); // 完成後に配信
    }
}
```

- [ ] **Step 2: 失敗確認**

Run: `cargo test -p edlr-core status`
Expected: FAIL(`StatusReader` 未定義)

- [ ] **Step 3: 実装する**

```rust
use serde_json::Value;
use std::path::PathBuf;

/// Status.json を読む。同一内容は重複配信せず、不完全な書き込みは次回リトライ。
pub struct StatusReader {
    path: PathBuf,
    last: Option<String>,
}

impl StatusReader {
    pub fn new(path: PathBuf) -> Self {
        Self { path, last: None }
    }

    pub fn poll(&mut self) -> Option<Value> {
        let content = std::fs::read_to_string(&self.path).ok()?;
        if content.trim().is_empty() || self.last.as_deref() == Some(content.as_str()) {
            return None;
        }
        let raw: Value = serde_json::from_str(&content).ok()?;
        self.last = Some(content);
        Some(raw)
    }
}
```

`lib.rs` に `pub mod status;` を追加。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core status`
Expected: PASS(2 テスト)

- [ ] **Step 5: Commit**

```bash
git add core/src && git commit -m "feat(core): Status.json reader with dedup and partial-write tolerance"
```

---

### Task 6: Router(broadcast pub/sub)と WakeSource(inotify + ポーリング)

**Files:**
- Create: `core/src/router.rs`
- Create: `core/src/watch.rs`
- Modify: `core/src/lib.rs`

**Interfaces:**
- Consumes: `Event`
- Produces: `pub struct Router` / `pub fn new(capacity: usize) -> Router` / `pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>>` / `pub fn publish(&self, event: Event)`(購読者ゼロでもエラーにしない)。`Router: Clone`
- Produces: `pub struct WakeSource { pub rx: tokio::sync::mpsc::Receiver<()> }` / `pub fn wake_source(dir: &Path, interval: Duration) -> WakeSource` — inotify イベントまたはインターバル経過で `()` が届く。inotify 初期化失敗でもポーリングだけで動く(tracing で warn)

- [ ] **Step 1: 失敗するテストを書く**

`core/src/router.rs` 内:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

    #[tokio::test]
    async fn delivers_to_all_subscribers() {
        let router = Router::new(16);
        let mut a = router.subscribe();
        let mut b = router.subscribe();
        router.publish(Event::Status { raw: serde_json::json!({"Flags": 1}) });
        assert!(matches!(*a.recv().await.unwrap(), Event::Status { .. }));
        assert!(matches!(*b.recv().await.unwrap(), Event::Status { .. }));
    }

    #[test]
    fn publish_without_subscribers_does_not_panic() {
        let router = Router::new(16);
        router.publish(Event::Status { raw: serde_json::json!({}) });
    }
}
```

`core/src/watch.rs` 内:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn wakes_on_file_write() {
        let dir = tempfile::tempdir().unwrap();
        // インターバルを長くして inotify 経路で起きることを確認
        let mut ws = wake_source(dir.path(), Duration::from_secs(60));
        std::fs::write(dir.path().join("Journal.x.log"), "x\n").unwrap();
        tokio::time::timeout(Duration::from_secs(5), ws.rx.recv())
            .await
            .expect("should wake within 5s")
            .expect("channel should be open");
    }

    #[tokio::test]
    async fn wakes_on_interval_without_fs_events() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = wake_source(dir.path(), Duration::from_millis(50));
        tokio::time::timeout(Duration::from_secs(5), ws.rx.recv())
            .await
            .expect("should tick")
            .expect("channel should be open");
    }
}
```

- [ ] **Step 2: 失敗確認**

Run: `cargo test -p edlr-core router watch`
Expected: FAIL(`Router` / `wake_source` 未定義)

- [ ] **Step 3: 実装する**

`core/src/router.rs`:

```rust
use crate::event::Event;
use std::sync::Arc;
use tokio::sync::broadcast;

/// イベントを全購読者に配る pub/sub ルーター。
#[derive(Clone)]
pub struct Router {
    tx: broadcast::Sender<Arc<Event>>,
}

impl Router {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.tx.subscribe()
    }

    /// 購読者がいない場合の送信エラーは無視する。
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(Arc::new(event));
    }
}
```

`core/src/watch.rs`:

```rust
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;

/// ファイル変更の「起きろ」シグナル源。inotify + 常時インターバルのハイブリッド。
/// 読み取り側が冪等なので、シグナルは coalesce(容量 1、あふれは破棄)する。
pub struct WakeSource {
    _watcher: Option<RecommendedWatcher>,
    pub rx: mpsc::Receiver<()>,
}

pub fn wake_source(dir: &Path, interval: Duration) -> WakeSource {
    let (tx, rx) = mpsc::channel(1);

    let notify_tx = tx.clone();
    let watcher = (|| -> notify::Result<RecommendedWatcher> {
        let mut w = notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
            let _ = notify_tx.try_send(());
        })?;
        w.watch(dir, RecursiveMode::NonRecursive)?;
        Ok(w)
    })();
    let watcher = match watcher {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("inotify unavailable, relying on polling only: {e}");
            None
        }
    };

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if tx.is_closed() {
                break;
            }
            let _ = tx.try_send(());
        }
    });

    WakeSource { _watcher: watcher, rx }
}
```

`lib.rs` に `pub mod router;` と `pub mod watch;` を追加。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core router watch`
Expected: PASS(4 テスト)

- [ ] **Step 5: Commit**

```bash
git add core/src && git commit -m "feat(core): broadcast router and hybrid inotify+polling wake source"
```

---

### Task 7: 監視ループの結線(monitor)

**Files:**
- Create: `core/src/monitor.rs`
- Create: `core/tests/monitor_integration.rs`
- Modify: `core/src/lib.rs`

**Interfaces:**
- Consumes: `JournalTailer`, `StatusReader`, `Router`, `wake_source`, `journal::parser::parse_line`
- Produces: `pub async fn run(dir: PathBuf, router: Router, interval: Duration)` — wake のたびに tailer と status を poll し、パース結果を router に publish し続ける(WakeSource が閉じるまで戻らない)。パース失敗行は `tracing::warn!` してスキップ

- [ ] **Step 1: 失敗する統合テストを書く**(`core/tests/monitor_integration.rs`)

```rust
use edlr_core::event::Event;
use edlr_core::monitor;
use edlr_core::router::Router;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

fn append(path: &std::path::Path, s: &str) {
    let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
    f.write_all(s.as_bytes()).unwrap();
}

async fn next_event(
    rx: &mut tokio::sync::broadcast::Receiver<std::sync::Arc<Event>>,
) -> std::sync::Arc<Event> {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("event within 5s")
        .expect("channel open")
}

#[tokio::test]
async fn routes_journal_and_status_events() {
    let dir = tempfile::tempdir().unwrap();
    let router = Router::new(64);
    let mut rx = router.subscribe();
    let _task = tokio::spawn(monitor::run(
        dir.path().to_path_buf(),
        router.clone(),
        Duration::from_millis(50),
    ));

    append(
        &dir.path().join("Journal.2026-07-25T120000.01.log"),
        "{\"timestamp\":\"2026-07-25T12:00:00Z\",\"event\":\"FSDJump\"}\nbroken line\n",
    );
    let ev = next_event(&mut rx).await;
    assert!(matches!(&*ev, Event::Journal { event, .. } if event == "FSDJump"));

    std::fs::write(dir.path().join("Status.json"), r#"{"Flags":16777240}"#).unwrap();
    let ev = next_event(&mut rx).await;
    assert!(matches!(&*ev, Event::Status { raw } if raw["Flags"] == 16777240));
}
```

- [ ] **Step 2: 失敗確認**

Run: `cargo test -p edlr-core --test monitor_integration`
Expected: FAIL(`monitor` モジュール未定義)

- [ ] **Step 3: 実装する**(`core/src/monitor.rs`)

```rust
use crate::journal::{parser, tailer::JournalTailer};
use crate::router::Router;
use crate::status::StatusReader;
use crate::watch::wake_source;
use std::path::PathBuf;
use std::time::Duration;

/// 監視ループ本体。wake のたびに Journal と Status.json を poll して配信する。
/// エラーで panic せず、ログして継続する。
pub async fn run(dir: PathBuf, router: Router, interval: Duration) {
    let mut tailer = JournalTailer::new(dir.clone());
    let mut status = StatusReader::new(dir.join("Status.json"));
    let mut wake = wake_source(&dir, interval);

    while wake.rx.recv().await.is_some() {
        match tailer.poll() {
            Ok(lines) => {
                for line in lines {
                    match parser::parse_line(&line) {
                        Some(event) => router.publish(event),
                        None => tracing::warn!("skipping unparsable journal line: {line}"),
                    }
                }
            }
            Err(e) => tracing::warn!("journal poll failed (will retry): {e}"),
        }
        if let Some(raw) = status.poll() {
            router.publish(crate::event::Event::Status { raw });
        }
    }
}
```

`lib.rs` に `pub mod monitor;` を追加。

- [ ] **Step 4: 全テストが通ることを確認**

Run: `cargo test --workspace`
Expected: PASS(全テスト)

- [ ] **Step 5: Commit**

```bash
git add core && git commit -m "feat(core): wire monitor loop from wake source to router"
```

---

### Task 8: デーモンバイナリ(CLI + 既定パス探索 + stdout 出力)

**Files:**
- Create: `core/src/config.rs`
- Modify: `core/src/bin/edlr.rs`
- Modify: `core/src/lib.rs`

**Interfaces:**
- Consumes: `monitor::run`, `Router`
- Produces: `pub fn default_journal_dir(home: &Path) -> Option<PathBuf>`(`core/src/config.rs`)— Proton 既定パスが存在すれば返す。バイナリは `--journal-dir <PATH>` 指定(未指定時は既定探索、どちらも無ければエラーメッセージを出して exit 1)、受信イベントを 1 行 1 JSON で stdout に出力

- [ ] **Step 1: 失敗するテストを書く**(`core/src/config.rs` 内)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_proton_dir_when_it_exists() {
        let home = tempfile::tempdir().unwrap();
        let proton = home.path().join(
            ".steam/steam/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous",
        );
        std::fs::create_dir_all(&proton).unwrap();
        assert_eq!(default_journal_dir(home.path()), Some(proton));
    }

    #[test]
    fn returns_none_when_absent() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(default_journal_dir(home.path()), None);
    }
}
```

- [ ] **Step 2: 失敗確認**

Run: `cargo test -p edlr-core config`
Expected: FAIL(`default_journal_dir` 未定義)

- [ ] **Step 3: 実装する**

`core/src/config.rs`:

```rust
use std::path::{Path, PathBuf};

const PROTON_JOURNAL_DIR: &str = ".steam/steam/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous";

/// 既知の Journal ディレクトリを探す。現状は Proton 既定パスのみ。
pub fn default_journal_dir(home: &Path) -> Option<PathBuf> {
    let candidate = home.join(PROTON_JOURNAL_DIR);
    candidate.is_dir().then_some(candidate)
}
```

`core/src/bin/edlr.rs`:

```rust
use clap::Parser;
use edlr_core::{config, monitor, router::Router};
use std::path::PathBuf;
use std::time::Duration;

/// EliteDangerousLogRouter daemon
#[derive(Parser)]
#[command(name = "edlr", version)]
struct Args {
    /// Journal ディレクトリ(未指定時は既知パスを探索)
    #[arg(long)]
    journal_dir: Option<PathBuf>,

    /// ポーリング間隔(ミリ秒)
    #[arg(long, default_value_t = 1000)]
    poll_interval_ms: u64,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    let args = Args::parse();

    let dir = args.journal_dir.or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        config::default_journal_dir(&home)
    });
    let Some(dir) = dir else {
        eprintln!("error: journal directory not found; specify one with --journal-dir <PATH>");
        std::process::exit(1);
    };

    tracing::info!("watching {}", dir.display());
    let router = Router::new(256);
    let mut rx = router.subscribe();
    tokio::spawn(monitor::run(
        dir,
        router.clone(),
        Duration::from_millis(args.poll_interval_ms),
    ));

    loop {
        match rx.recv().await {
            Ok(event) => {
                let json = match &*event {
                    edlr_core::event::Event::Journal { raw, .. } => raw.to_string(),
                    edlr_core::event::Event::Status { raw } => raw.to_string(),
                };
                println!("{json}");
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("stdout consumer lagged, dropped {n} events");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}
```

`lib.rs` に `pub mod config;` を追加。

- [ ] **Step 4: テストと手動スモークで確認**

Run: `cargo test --workspace`
Expected: PASS(全テスト)

Run(スモーク): `mkdir -p /tmp/claude-1000/-mnt-game-caches-src-github-com-himanoa-edlr/*/scratchpad/edlr-smoke 2>/dev/null; DIR=$(echo /tmp/claude-1000/-mnt-game-caches-src-github-com-himanoa-edlr/*/scratchpad)/edlr-smoke; mkdir -p "$DIR"; timeout 5 cargo run -p edlr-core --bin edlr -- --journal-dir "$DIR" --poll-interval-ms 100 & sleep 2; printf '{"timestamp":"2026-07-25T12:00:00Z","event":"Music"}\n' >> "$DIR/Journal.2026-07-25T120000.01.log"; wait`
Expected: stdout に `{"timestamp":"2026-07-25T12:00:00Z","event":"Music"}` が出力される

- [ ] **Step 5: Commit**

```bash
git add core && git commit -m "feat(core): edlr daemon binary with journal dir discovery"
```

---

### Task 9: 仕上げ(clippy / fmt / README)

**Files:**
- Create: `README.md`

**Interfaces:**
- Consumes: 全タスクの成果物

- [ ] **Step 1: fmt と clippy を通す**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: 警告ゼロ。指摘があれば修正する

- [ ] **Step 2: ルート `README.md` を書く**

```markdown
# edlr — EliteDangerousLogRouter

Elite Dangerous の Journal / Status.json を監視し、イベントをドライバとプラグインへ配るルーター。
設計の全体像は [spec.md](spec.md) を参照。

## 構成

- `core/` — Rust 製カーネル。Journal tail(inotify + ポーリング常時併用)、JSON Lines パース、
  Status.json 監視、broadcast によるイベント配信。バイナリ名 `edlr`
- `drivers/` — 特権 capability を持つドライバ層(http / channel、現在はスケルトン)
- `ui/` — GUI クライアント(未実装。ブラウザ版 → Tauri の順で実装予定)

## 使い方

    cargo run -p edlr-core --bin edlr -- --journal-dir <JournalディレクトリのPATH>

`--journal-dir` 省略時は Proton の既定パスを探索する。イベントは 1 行 1 JSON で stdout に流れる。
```

- [ ] **Step 3: 全テスト最終確認**

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "chore: fmt, clippy, add README"
```

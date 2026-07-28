# デーモンログの GUI 表示 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** デーモンの tracing ログ(INFO 以上)を WS 経由で GUI の Logs 画面に kind=log として流す。

**Architecture:** カスタム tracing Layer(`core/src/logs.rs`)が INFO 以上のイベントを JSON フレームに整形して `tokio::sync::broadcast` へ送り、`ServerState::attach_log_stream` が既存の ReplayBuffer + WS ブロードキャストへ合流させる。フロントは `parseWsMessage` / `LogEntry` に kind=log を追加し、Logs 画面に種別フィルタと level 表示を足す。

**Tech Stack:** Rust (tracing-subscriber Layer, tokio broadcast, chrono), React + vitest。

**Spec:** `docs/superpowers/specs/2026-07-28-edlr-daemon-logs-in-gui-design.md`

## Global Constraints

- 転送は INFO 以上のみ。Layer 内では一切ログを出さない(ループ防止)。送信は非ブロッキング(受信者なし・詰まりは黙って捨てる)。
- フレーム形: `{"type":"event","kind":"log","timestamp":"<RFC3339 millis UTC>","level":"info|warn|error","target":"<module path>","message":"<message + key=value...>"}`
- stderr への既存 fmt 出力は現状維持。
- Dashboard の `edlr:event` 転送に log は流さない(`matchesEvent` は journal/status のみ。変更しないことをテストで担保済み — events.test.ts の既存テストがカバー)。
- TDD。cargo は並走させない。作業ブランチ `daemon-logs-gui`。

---

### Task 1: LogLayer(tracing → JSON フレーム)

**Files:**
- Create: `core/src/logs.rs`
- Modify: `core/src/lib.rs`(`pub mod logs;` 追加)、`core/Cargo.toml`(`chrono = "0.4"` 追加。Cargo.lock に transitive で既存)

**Interfaces:**
- Produces:
  - `pub fn log_channel() -> (LogLayer, tokio::sync::broadcast::Receiver<Arc<String>>)`
  - `pub struct LogLayer`(`tracing_subscriber::Layer<S>` 実装、`Clone` 不要)
  - `pub(crate) fn format_log_frame(level: &str, target: &str, timestamp: &str, message: &str) -> String`(純関数、テスト用に分離)

- [ ] **Step 1: 失敗するテストを書く**(`core/src/logs.rs` 内 `mod tests`)

```rust
#[test]
fn format_log_frame_produces_the_wire_shape() {
    let frame = format_log_frame("info", "edlr_core::plugin", "2026-07-28T12:00:00.000Z", "hello");
    let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(v["type"], "event");
    assert_eq!(v["kind"], "log");
    assert_eq!(v["level"], "info");
    assert_eq!(v["target"], "edlr_core::plugin");
    assert_eq!(v["timestamp"], "2026-07-28T12:00:00.000Z");
    assert_eq!(v["message"], "hello");
}

/// Layer を scoped subscriber として使い、INFO 以上だけがフレーム化される
/// ことを確認する(グローバル subscriber は汚さない)。
#[test]
fn log_layer_forwards_info_and_above_with_fields() {
    use tracing_subscriber::layer::SubscriberExt;
    let (layer, mut rx) = log_channel();
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        tracing::debug!("dropped");
        tracing::info!(plugin_id = "widgety", "plugin started");
        tracing::warn!("watch out");
    });

    let first: serde_json::Value =
        serde_json::from_str(&rx.try_recv().expect("info frame")).unwrap();
    assert_eq!(first["level"], "info");
    let msg = first["message"].as_str().unwrap();
    assert!(msg.contains("plugin started"));
    assert!(msg.contains("plugin_id=\"widgety\""), "fields must be appended: {msg}");
    assert!(first["timestamp"].as_str().unwrap().contains("T"));

    let second: serde_json::Value =
        serde_json::from_str(&rx.try_recv().expect("warn frame")).unwrap();
    assert_eq!(second["level"], "warn");
    // debug は転送されない
    assert!(rx.try_recv().is_err());
}
```

- [ ] **Step 2: 失敗を確認** — `cargo test -p edlr-core --lib logs` → モジュール不在でコンパイルエラー

- [ ] **Step 3: 実装**

```rust
//! tracing ログを GUI(WS クライアント)へ流すためのブリッジ。
//!
//! `LogLayer` は INFO 以上のイベントを JSON フレームに整形して broadcast へ
//! 送るだけ。**この Layer の中では絶対にログしない**(自分のイベントを
//! 自分で拾う無限ループになる)。送信は `broadcast::Sender::send` のみで
//! 非ブロッキング。受信者がいなければ Err になるが黙って捨てる
//! (ログ表示はベストエフォートで、デーモン本体の動作に影響させない)。

use std::fmt::Write as _;
use std::sync::Arc;

use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// フレームのバッファ容量。WS 側の ReplayBuffer(1000)への合流前の
/// 一時経路なので小さめでよい。
const CHANNEL_CAPACITY: usize = 256;

pub struct LogLayer {
    tx: broadcast::Sender<Arc<String>>,
}

pub fn log_channel() -> (LogLayer, broadcast::Receiver<Arc<String>>) {
    let (tx, rx) = broadcast::channel(CHANNEL_CAPACITY);
    (LogLayer { tx }, rx)
}

pub(crate) fn format_log_frame(level: &str, target: &str, timestamp: &str, message: &str) -> String {
    serde_json::json!({
        "type": "event",
        "kind": "log",
        "timestamp": timestamp,
        "level": level,
        "target": target,
        "message": message,
    })
    .to_string()
}

/// `message` フィールドを本文、それ以外を ` key=value` として畳み込む visitor。
struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > Level::INFO {
            return; // DEBUG/TRACE は転送しない
        }
        let mut visitor = MessageVisitor { message: String::new(), fields: String::new() };
        event.record(&mut visitor);
        let message = format!("{}{}", visitor.message, visitor.fields);
        let level_str = match level {
            Level::ERROR => "error",
            Level::WARN => "warn",
            _ => "info",
        };
        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let frame = format_log_frame(level_str, event.metadata().target(), &timestamp, &message);
        // 受信者不在・ラグは黙って捨てる(doc コメント参照)
        let _ = self.tx.send(Arc::new(frame));
    }
}
```

`core/src/lib.rs` に `pub mod logs;`、`core/Cargo.toml` の `[dependencies]` に `chrono = "0.4"`。

- [ ] **Step 4: パスを確認** — `cargo test -p edlr-core --lib logs` 全パス

- [ ] **Step 5: Commit** — `git commit -m "feat(core): tracing layer that formats log frames for the GUI"`

---

### Task 2: ServerState への合流とデーモン配線

**Files:**
- Modify: `core/src/server.rs`(`attach_log_stream` 追加 + テスト)
- Modify: `core/src/bin/edlr.rs`(tracing 初期化に Layer を重ね、ServerState に接続)

**Interfaces:**
- Consumes: `logs::log_channel()` の `broadcast::Receiver<Arc<String>>`
- Produces: `ServerState::attach_log_stream(&self, rx: broadcast::Receiver<Arc<String>>)` — 受け取ったフレームを ReplayBuffer に push する tokio タスクを spawn する

- [ ] **Step 1: 失敗するテストを書く**(server.rs `mod tests`)

```rust
/// attach_log_stream で流し込んだフレームが、journal/status イベントと同じ
/// 経路(ReplayBuffer + broadcast)で新規クライアントに届くことを確認する。
#[tokio::test]
async fn attached_log_frames_reach_the_replay_buffer_and_broadcast() {
    let router = crate::router::Router::new(8);
    let state = ServerState::new(&router, None, None);
    let (tx, rx) = tokio::sync::broadcast::channel::<std::sync::Arc<String>>(8);
    state.attach_log_stream(rx);

    tx.send(std::sync::Arc::new(
        crate::logs::format_log_frame("info", "t", "2026-07-28T00:00:00.000Z", "hello"),
    ))
    .unwrap();

    // feeder タスクが処理するまで少し待ってから snapshot を取る
    let mut found = false;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let (snapshot, _rx) = state.snapshot_and_subscribe();
        if snapshot.iter().any(|f| f.contains("\"kind\":\"log\"") && f.contains("hello")) {
            found = true;
            break;
        }
    }
    assert!(found, "log frame must appear in the replay snapshot");
}
```

(`snapshot_and_subscribe` の実際の可視性・シグネチャに合わせて調整。private なら `pub(crate)` 化してよい。)

- [ ] **Step 2: 失敗を確認** — メソッド不在でコンパイルエラー

- [ ] **Step 3: 実装**(server.rs)

```rust
    /// tracing ログのフレーム(`logs::LogLayer` 産)をイベントと同じ
    /// ReplayBuffer + broadcast に合流させる。受信ラグ(Lagged)は捨てて
    /// 続行する -- ログはベストエフォート。
    pub fn attach_log_stream(&self, mut rx: broadcast::Receiver<Arc<String>>) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(frame) => push(&inner, frame),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
```

(既存の `push` ヘルパー/フィールド名に合わせる。)

edlr.rs の初期化を変更:

```rust
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let (log_layer, log_rx) = edlr_core::logs::log_channel();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(log_layer)
        .init();
```

(従来の `fmt().init()` と同じ INFO 既定を保つ — fmt::layer は既定でフィルタを持たないため、`with(tracing_subscriber::filter::LevelFilter::INFO)` をレジストリに付けて従来挙動に合わせる。)

`ServerState::new(...)` の直後に `state.attach_log_stream(log_rx);`。

- [ ] **Step 4: パスを確認** — `cargo test -p edlr-core` 全パス + `./target/debug/edlr` を手で起動し、WS で `{"kind":"log"}` フレームが届くこと(Task 4 の E2E でも確認)

- [ ] **Step 5: Commit** — `git commit -m "feat(core): stream daemon logs to websocket clients"`

---

### Task 3: フロントの型・パース・フィルタ

**Files:**
- Modify: `ui/frontend/src/lib/filter.ts`(`LogEntry` に kind "log" + level/message フィールド、`filterEntries` が message にマッチ)
- Modify: `ui/frontend/src/ws.ts`(`WsMessage` / `parseWsMessage` / `useEventStream` に log 対応)
- Test: `ui/frontend/src/ws.test.ts`(既存があれば追記、無ければ新規)・`ui/frontend/src/lib/filter.test.ts`(既存に追記)

**Interfaces:**
- Produces:
  - `LogEntry`: `kind: "journal" | "status" | "log"`、log のとき `level?: string; message?: string; target?: string`
  - `parseWsMessage`: `{type:"event", kind:"log", timestamp, level, target, message}` → `{ type: "event", kind: "log", timestamp, level, target, message }`

- [ ] **Step 1: 失敗するテストを書く**

```ts
// ws.test.ts(parseWsMessage の既存テストファイルに追記。無ければ新規)
import { describe, expect, it } from "vitest";
import { parseWsMessage } from "./ws";

describe("parseWsMessage log frames", () => {
  it("parses a log frame", () => {
    const msg = parseWsMessage(
      JSON.stringify({
        type: "event", kind: "log", timestamp: "2026-07-28T00:00:00.000Z",
        level: "warn", target: "edlr_core::x", message: "watch out",
      }),
    );
    expect(msg).toEqual({
      type: "event", kind: "log", timestamp: "2026-07-28T00:00:00.000Z",
      level: "warn", target: "edlr_core::x", message: "watch out",
    });
  });

  it("rejects log frames without level or message", () => {
    expect(
      parseWsMessage(JSON.stringify({ type: "event", kind: "log", timestamp: "t" })),
    ).toBeNull();
  });
});
```

```ts
// filter.test.ts に追記
it("matches log entries by message text", () => {
  const log: LogEntry = {
    id: 1, kind: "log", timestamp: "t", level: "info",
    message: "plugin widgety started", raw: {},
  };
  expect(filterEntries([log], "widgety")).toHaveLength(1);
  expect(filterEntries([log], "nomatch")).toHaveLength(0);
});
```

- [ ] **Step 2: 失敗を確認** — `pnpm --dir ui/frontend test`

- [ ] **Step 3: 実装** — `LogEntry` に `kind: "log"` と `level?/message?/target?` を追加。`parseWsMessage` に log 分岐(`kind === "log"` で `timestamp`/`level`/`message` が string であること検証、`target` は任意)。`useEventStream` の onmessage で log を `{id, kind:"log", timestamp, level, target, message, raw: {level, target, message}}` として entries に積む。`filterEntries` は `e.kind === "log"` のとき `e.message` にもマッチさせる(既存の raw JSON マッチはそのまま)。

- [ ] **Step 4: パスを確認** — `pnpm --dir ui/frontend test` + `pnpm --dir ui/frontend exec tsc -b`

- [ ] **Step 5: Commit** — `git commit -m "feat(ui): parse and filter daemon log frames"`

---

### Task 4: Logs 画面(表示 + 種別フィルタ)+ E2E

**Files:**
- Modify: `ui/frontend/src/pages/Logs.tsx`(Row の log 表示、種別フィルタトグル)
- Modify: `ui/frontend/src/index.css`(level バッジ色)
- Test: `ui/frontend/src/pages/Logs.test.tsx`(既存に追記)

**Interfaces:**
- Consumes: Task 3 の `LogEntry`(kind "log")
- Produces: Logs 画面に journal/status/log のチェックボックス(既定すべて ON)。log 行は `時刻 / logバッジ(level色) / message` 表示

- [ ] **Step 1: 失敗するテストを書く**(Logs.test.tsx の既存モック流儀 — `vi.mock("../ws")` で `useEventStream` を差し替え — に合わせる)

```tsx
test("renders log entries with level badge and message", () => {
  entries = [
    { id: 1, kind: "log", timestamp: "t1", level: "warn", message: "watch out", raw: {} },
  ];
  render(<Logs />);
  expect(screen.getByText("watch out")).toBeInTheDocument();
  expect(screen.getByText("warn")).toBeInTheDocument();
});

test("kind filter checkboxes hide unchecked kinds", async () => {
  entries = [
    { id: 1, kind: "journal", timestamp: "t", event: "FSDJump", raw: {} },
    { id: 2, kind: "log", timestamp: "t", level: "info", message: "daemon log line", raw: {} },
  ];
  render(<Logs />);
  expect(screen.getByText("daemon log line")).toBeInTheDocument();
  await userEvent.click(screen.getByRole("checkbox", { name: "log" }));
  expect(screen.queryByText("daemon log line")).not.toBeInTheDocument();
  expect(screen.getByText("FSDJump")).toBeInTheDocument();
});
```

- [ ] **Step 2: 失敗を確認** — `pnpm --dir ui/frontend test Logs`

- [ ] **Step 3: 実装** — `Logs.tsx` に `const [kinds, setKinds] = useState({journal: true, status: true, log: true})`、ツールバーに 3 つのチェックボックス(`aria-label` を kind 名に)。`shown = filterEntries(entries, query).filter((e) => kinds[e.kind])`。Row: `entry.kind === "log"` のとき event 列の代わりに `<span className={"log-level log-level-" + entry.level}>{entry.level}</span><span className="log-message">{entry.message}</span>`。CSS: `.log-level-warn { color: #e0b429; } .log-level-error { color: #e05555; } .log-level-info { color: #7fb3d8; }`。

- [ ] **Step 4: パスを確認** — `pnpm --dir ui/frontend test` + `tsc -b` + `pnpm --dir ui/frontend build`

- [ ] **Step 5: E2E** — scratch の journal/plugins dir でデーモン起動 → WS を叩いて `kind:"log"` フレーム(例: `starting plugins`)が replay に含まれることを確認。`cargo test --workspace` + フロント全テスト green。

- [ ] **Step 6: Commit** — `git commit -m "feat(ui): show daemon logs in the Logs page with kind filter"`

---

## Self-Review 結果

- **Spec coverage:** Layer+整形(Task 1)、合流+配線(Task 2)、パース/フィルタ(Task 3)、表示+種別フィルタ+E2E(Task 4)。エラーハンドリング(捨てる/null 無視)は Task 1/3 に内包。Dashboard 非転送は既存テストで担保(Global Constraints に明記)。
- **Placeholder:** なし。Task 2 の snapshot_and_subscribe 可視性のみ実装時判断(選択肢を明記済み)。
- **型整合:** `log_channel` / `format_log_frame` / `attach_log_stream` / `LogEntry.kind "log"` を全タスクで統一。

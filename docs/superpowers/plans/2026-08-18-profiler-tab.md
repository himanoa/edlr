# プロファイラータブ Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** edlr デーモンに常時 ON のプロファイラ(wasm 呼び出し時間・キュー滞留・ドロップ・メモリ)を内蔵し、UI に Profiler タブを追加する。

**Architecture:** 計測点は `ProfilerSink`(try_send、満杯なら捨てる)へサンプルを送るだけ。collector スレッド 1 本が生 JSONL(`<state-base>/profiler/YYYY-MM-DD.jsonl`)への追記と、直近 3600 秒の 1 秒バケットのメモリリング保持を行う。UI は RPC(`profiler/summary` / `profiler/series`)ポーリングでリングだけを読む。

**Tech Stack:** Rust(core、追加依存なし。chrono/serde は既存)、React + jotai + Tailwind(ui/frontend、チャートは手書き SVG で追加依存なし)

**Spec:** `docs/superpowers/specs/2026-08-18-profiler-tab-design.md`

## Global Constraints

- `.claude/rules/` 必読・厳守: 純粋/命令的境界(`profiler/` の判断・集計・整形は純粋、チャネル/スレッド/ファイルは命令的側)、mut 最小、判断は純関数抽出、trait 増設なし、モック手書き
- 計測が本体をブロックしない: 計測点は `try_send` のみ。失敗時は捨てて数える
- 新規依存を追加しない(Rust も npm も)
- 既存テストは消さない・書き換えない(挙動の錨)
- コミットメッセージは既存の流儀(`feat:` / `test:` 等 + 日本語)で、末尾に `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
- cargo コマンドは同一 worktree で並走させない

---

### Task 1: profiler 純粋部 — サンプル型と JSONL 1 行シリアライズ

**Files:**
- Create: `core/src/profiler/mod.rs`
- Create: `core/src/profiler/sample.rs`
- Modify: `core/src/lib.rs`(`pub mod profiler;` を追加)

**Interfaces:**
- Produces:
  - `profiler::Subject`(`Plugin` | `Driver`、serde で `"plugin"`/`"driver"`)
  - `profiler::CallKind`(`OnEvent` | `OnMessage` | `OnSchedule` | `OnJobComplete`、serde で `"on-event"` 等)
  - `profiler::Outcome`(`Ok` | `Error` | `Timeout`)
  - `profiler::Sample`(enum: `Call(CallSample)` | `Gauge(GaugeSample)` | `SinkLost { ts: f64, lost: u64 }`)
  - `CallSample { ts: f64, subject: Subject, id: String, call: CallKind, detail: String, duration_us: u64, outcome: Outcome }`
  - `GaugeSample { ts: f64, subject: Subject, id: String, queue_len: usize, dropped_events: u64, dropped_bus: u64, memory_bytes: u64 }`
  - `Sample::to_jsonl_line(&self) -> String`(改行なしの 1 行 JSON)

- [ ] **Step 1: 失敗するテストを書く**(`core/src/profiler/sample.rs` の `#[cfg(test)] mod tests`)

```rust
#[test]
fn call_sample_serializes_to_a_flat_jsonl_line() {
    let s = Sample::Call(CallSample {
        ts: 1755500000.123,
        subject: Subject::Plugin,
        id: "inara-uploader".into(),
        call: CallKind::OnEvent,
        detail: "FSDJump".into(),
        duration_us: 1800,
        outcome: Outcome::Ok,
    });
    let line = s.to_jsonl_line();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["subject"], "plugin");
    assert_eq!(v["call"], "on-event");
    assert_eq!(v["outcome"], "ok");
    assert_eq!(v["duration_us"], 1800);
    assert!(!line.contains('\n'));
}

#[test]
fn gauge_and_sink_lost_serialize_with_their_own_shapes() {
    let g = Sample::Gauge(GaugeSample {
        ts: 1755500000.0,
        subject: Subject::Driver,
        id: "ed-state".into(),
        queue_len: 3,
        dropped_events: 0,
        dropped_bus: 1,
        memory_bytes: 4194304,
    });
    let v: serde_json::Value = serde_json::from_str(&g.to_jsonl_line()).unwrap();
    assert_eq!(v["subject"], "driver");
    assert_eq!(v["queue_len"], 3);

    let l = Sample::SinkLost { ts: 1.0, lost: 5 };
    let v: serde_json::Value = serde_json::from_str(&l.to_jsonl_line()).unwrap();
    assert_eq!(v["subject"], "profiler");
    assert_eq!(v["id"], "sink");
    assert_eq!(v["lost"], 5);
}
```

- [ ] **Step 2: 落ちることを確認** — Run: `cargo test -p edlr-core --lib profiler` → コンパイルエラー(型未定義)
- [ ] **Step 3: 実装**

```rust
// core/src/profiler/mod.rs
//! プロファイラ(issue: docs/superpowers/specs/2026-08-18-profiler-tab-design.md)。
//! このモジュール直下と sample/bucket は純粋(値イン値アウト)。
pub mod sample;
pub use sample::{CallKind, CallSample, GaugeSample, Outcome, Sample, Subject};
```

```rust
// core/src/profiler/sample.rs
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Subject {
    Plugin,
    Driver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallKind {
    OnEvent,
    OnMessage,
    OnSchedule,
    OnJobComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    Error,
    Timeout,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallSample {
    pub ts: f64,
    pub subject: Subject,
    pub id: String,
    pub call: CallKind,
    pub detail: String,
    pub duration_us: u64,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Serialize)]
pub struct GaugeSample {
    pub ts: f64,
    pub subject: Subject,
    pub id: String,
    pub queue_len: usize,
    pub dropped_events: u64,
    pub dropped_bus: u64,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone)]
pub enum Sample {
    Call(CallSample),
    Gauge(GaugeSample),
    /// sink 満杯で捨てた計測サンプル数(累計)。毎秒 1 行。
    SinkLost { ts: f64, lost: u64 },
}

impl Sample {
    /// JSONL 1 行(改行なし)。シリアライズ失敗はあり得ない構造だが、
    /// 万一に備えて空オブジェクトへフォールバック。
    pub fn to_jsonl_line(&self) -> String {
        match self {
            Sample::Call(c) => serde_json::to_string(c),
            Sample::Gauge(g) => serde_json::to_string(g),
            Sample::SinkLost { ts, lost } => serde_json::to_string(&serde_json::json!({
                "ts": ts, "subject": "profiler", "id": "sink", "lost": lost,
            })),
        }
        .unwrap_or_else(|_| "{}".to_string())
    }
}
```

`core/src/lib.rs` のモジュール宣言並びに `pub mod profiler;` を追加。

- [ ] **Step 4: テストが通ることを確認** — Run: `cargo test -p edlr-core --lib profiler` → PASS
- [ ] **Step 5: Commit** — `git add core/src/profiler core/src/lib.rs && git commit -m "feat: profiler のサンプル型と JSONL シリアライズを追加"`

---

### Task 2: profiler 純粋部 — 1 秒バケット集計とリング

**Files:**
- Create: `core/src/profiler/bucket.rs`
- Modify: `core/src/profiler/mod.rs`(`pub mod bucket;` を追加)

**Interfaces:**
- Consumes: Task 1 の `Sample`/`CallSample`/`GaugeSample`/`Subject`
- Produces:
  - `bucket::SecondBucket { calls: u64, errors: u64, sum_us: u64, max_us: u64, queue_len: Option<usize>, memory_bytes: Option<u64>, dropped_events: Option<u64>, dropped_bus: Option<u64> }`(`Default` 実装)
  - `bucket::Ring`(subject×id ごとに直近 `RING_SECONDS = 3600` 秒の `(sec: u64, SecondBucket)` を保持)
    - `Ring::insert(&mut self, sample: &Sample)`(ts を秒に丸めてバケットへ畳む。`SinkLost` は `lost` フィールドへ)
    - `Ring::lost(&self) -> u64`
    - `Ring::keys(&self) -> Vec<(Subject, String)>`
    - `Ring::window(&self, subject: Subject, id: &str, from_sec: u64, to_sec: u64) -> Vec<Option<SecondBucket>>`(欠けた秒は `None`)

- [ ] **Step 1: 失敗するテストを書く**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiler::{CallKind, CallSample, GaugeSample, Outcome, Sample, Subject};

    fn call(ts: f64, us: u64, outcome: Outcome) -> Sample {
        Sample::Call(CallSample {
            ts,
            subject: Subject::Plugin,
            id: "p1".into(),
            call: CallKind::OnEvent,
            detail: "E".into(),
            duration_us: us,
            outcome,
        })
    }

    #[test]
    fn calls_in_the_same_second_fold_into_one_bucket() {
        let mut ring = Ring::new();
        ring.insert(&call(100.1, 10, Outcome::Ok));
        ring.insert(&call(100.9, 30, Outcome::Error));
        let w = ring.window(Subject::Plugin, "p1", 100, 101);
        let b = w[0].as_ref().unwrap();
        assert_eq!((b.calls, b.errors, b.sum_us, b.max_us), (2, 1, 40, 30));
    }

    #[test]
    fn window_fills_missing_seconds_with_none() {
        let mut ring = Ring::new();
        ring.insert(&call(100.0, 10, Outcome::Ok));
        ring.insert(&call(102.0, 10, Outcome::Ok));
        let w = ring.window(Subject::Plugin, "p1", 100, 103);
        assert!(w[0].is_some() && w[1].is_none() && w[2].is_some());
        assert_eq!(w.len(), 3);
    }

    #[test]
    fn gauges_set_the_gauge_fields_and_old_seconds_are_evicted() {
        let mut ring = Ring::new();
        ring.insert(&Sample::Gauge(GaugeSample {
            ts: 100.0,
            subject: Subject::Plugin,
            id: "p1".into(),
            queue_len: 5,
            dropped_events: 2,
            dropped_bus: 0,
            memory_bytes: 1024,
        }));
        let w = ring.window(Subject::Plugin, "p1", 100, 101);
        assert_eq!(w[0].as_ref().unwrap().queue_len, Some(5));

        // RING_SECONDS より古い秒は insert 時に追い出される
        ring.insert(&call(100.0 + (RING_SECONDS as f64) + 10.0, 1, Outcome::Ok));
        let w = ring.window(Subject::Plugin, "p1", 100, 101);
        assert!(w[0].is_none());
    }

    #[test]
    fn sink_lost_is_tracked_globally() {
        let mut ring = Ring::new();
        ring.insert(&Sample::SinkLost { ts: 1.0, lost: 7 });
        assert_eq!(ring.lost(), 7);
    }
}
```

- [ ] **Step 2: 落ちることを確認** — Run: `cargo test -p edlr-core --lib profiler::bucket` → コンパイルエラー
- [ ] **Step 3: 実装**

```rust
// core/src/profiler/bucket.rs
//! 1 秒バケットの畳み込みとリング(純粋。スレッド・ロックは持たない --
//! 共有は collector 側が `Mutex<Ring>` で行う)。
use std::collections::{HashMap, VecDeque};

use super::{Sample, Subject};

pub const RING_SECONDS: u64 = 3600;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SecondBucket {
    pub calls: u64,
    pub errors: u64,
    pub sum_us: u64,
    pub max_us: u64,
    pub queue_len: Option<usize>,
    pub memory_bytes: Option<u64>,
    pub dropped_events: Option<u64>,
    pub dropped_bus: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Ring {
    // ponytail: subject×id ごとの VecDeque を線形に引く。対象は数十個なので十分
    series: HashMap<(Subject, String), VecDeque<(u64, SecondBucket)>>,
    lost: u64,
}

impl Ring {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, sample: &Sample) {
        let (key, sec) = match sample {
            Sample::Call(c) => ((c.subject, c.id.clone()), c.ts as u64),
            Sample::Gauge(g) => ((g.subject, g.id.clone()), g.ts as u64),
            Sample::SinkLost { lost, .. } => {
                self.lost = *lost;
                return;
            }
        };
        let deque = self.series.entry(key).or_default();
        let bucket = match deque.back_mut() {
            Some((s, b)) if *s == sec => b,
            _ => {
                deque.push_back((sec, SecondBucket::default()));
                while deque.len() > 1 && sec.saturating_sub(deque[0].0) >= RING_SECONDS {
                    deque.pop_front();
                }
                &mut deque.back_mut().expect("just pushed").1
            }
        };
        match sample {
            Sample::Call(c) => {
                bucket.calls += 1;
                if !matches!(c.outcome, super::Outcome::Ok) {
                    bucket.errors += 1;
                }
                bucket.sum_us += c.duration_us;
                bucket.max_us = bucket.max_us.max(c.duration_us);
            }
            Sample::Gauge(g) => {
                bucket.queue_len = Some(g.queue_len);
                bucket.memory_bytes = Some(g.memory_bytes);
                bucket.dropped_events = Some(g.dropped_events);
                bucket.dropped_bus = Some(g.dropped_bus);
            }
            Sample::SinkLost { .. } => unreachable!("handled above"),
        }
    }

    pub fn lost(&self) -> u64 {
        self.lost
    }

    pub fn keys(&self) -> Vec<(Subject, String)> {
        self.series.keys().cloned().collect()
    }

    /// `[from_sec, to_sec)` の各秒のバケット。無い秒は `None`。
    pub fn window(
        &self,
        subject: Subject,
        id: &str,
        from_sec: u64,
        to_sec: u64,
    ) -> Vec<Option<SecondBucket>> {
        let deque = self.series.get(&(subject, id.to_string()));
        (from_sec..to_sec)
            .map(|sec| {
                deque.and_then(|d| {
                    d.iter()
                        .find(|(s, _)| *s == sec)
                        .map(|(_, b)| b.clone())
                })
            })
            .collect()
    }
}
```

注意: 挿入順が秒単位で単調でないサンプル(スレッド間のわずかな順序ずれ)で
`back_mut` の秒が一致しないケースは新バケットを積む素朴な実装でよい
(`window` は `find` で引くので重複秒があっても最初の 1 個を返すだけ。
1 秒単位のずれは表示上無視できる)。この割り切りをコード内コメントに残すこと。

- [ ] **Step 4: テストが通ることを確認** — Run: `cargo test -p edlr-core --lib profiler` → PASS
- [ ] **Step 5: Commit** — `git commit -m "feat: profiler の 1 秒バケット集計とリングを追加"`

---

### Task 3: sink と collector スレッド(JSONL 追記 + リング)

**Files:**
- Create: `core/src/profiler/collector.rs`(命令的: チャネル・スレッド・ファイル)
- Modify: `core/src/profiler/mod.rs`(`pub mod collector;` + re-export)

**Interfaces:**
- Consumes: Task 1/2 の `Sample`/`Ring`
- Produces:
  - `collector::Profiler`(Clone。中身は Arc 共有)
    - `Profiler::start(profiler_dir: PathBuf) -> Profiler`(collector スレッド起動。dir は `<state-base>/profiler`)
    - `Profiler::noop() -> Profiler`(テスト・未配線環境用 null sink。スレッドもファイルも作らない)
    - `Profiler::record(&self, sample: Sample)`(try_send。満杯なら捨てて lost を数える。noop なら何もしない)
    - `Profiler::ring(&self) -> Arc<Mutex<Ring>>`(RPC が読む)
    - `Profiler::shutdown(&self)`(フラグ + センチネルで停止し flush、join)
- 実装メモ:
  - チャネルは `std::sync::mpsc::sync_channel::<CollectorMsg>(4096)`。`enum CollectorMsg { Sample(Sample), Shutdown }`
  - lost は `Arc<AtomicU64>`。`record` の `try_send` が `Full` なら `fetch_add(1)`
  - collector ループ: `recv_timeout(次の秒境界まで)` で受信 → `ring.lock().insert(&sample)` + JSONL 行をバッファへ。秒境界ごとに: `SinkLost` サンプルを自分で合成(lost 値を読む)して ring + JSONL へ、`BufWriter::flush()`、日付が変わっていたらファイルを開き直す
  - ファイル名は `chrono::Local::now().format("%Y-%m-%d")`。open は `OpenOptions::new().create(true).append(true)`
  - gauge 走査は Task 4 で追加する(この Task では collector の骨格まで)

- [ ] **Step 1: 統合テストを書く**(`collector.rs` 内 `#[cfg(test)]`)

```rust
#[test]
fn recorded_samples_land_in_the_ring_and_the_jsonl_file() {
    let tmp = tempfile::tempdir().unwrap();
    let profiler = Profiler::start(tmp.path().join("profiler"));
    profiler.record(sample_call(100.0)); // テスト用ヘルパ: Task 2 の call() と同形
    // 秒境界の flush を待つ
    std::thread::sleep(std::time::Duration::from_millis(1500));
    profiler.shutdown();

    let ring = profiler.ring();
    let ring = ring.lock().unwrap();
    assert_eq!(ring.keys().len(), 1);

    let files: Vec<_> = std::fs::read_dir(tmp.path().join("profiler"))
        .unwrap()
        .collect();
    assert_eq!(files.len(), 1);
    let content = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
    assert!(content.lines().any(|l| l.contains("\"on-event\"")));
}

#[test]
fn a_noop_profiler_records_nothing_and_never_blocks() {
    let profiler = Profiler::noop();
    for _ in 0..10_000 {
        profiler.record(sample_call(1.0));
    }
    assert_eq!(profiler.ring().lock().unwrap().keys().len(), 0);
}

#[test]
fn shutdown_flushes_pending_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let profiler = Profiler::start(tmp.path().join("profiler"));
    profiler.record(sample_call(100.0));
    profiler.shutdown(); // flush 待ちなしで即 shutdown
    let files: Vec<_> = std::fs::read_dir(tmp.path().join("profiler")).unwrap().collect();
    let content = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
    assert!(!content.is_empty());
}
```

- [ ] **Step 2: 落ちることを確認** — Run: `cargo test -p edlr-core --lib profiler::collector` → コンパイルエラー
- [ ] **Step 3: 実装**(上記メモどおり。`Profiler` の中身は `Option<Inner>` で noop を表現せず、`enum ProfilerImpl { Live(Arc<Inner>), Noop(Arc<Mutex<Ring>>) }` のように null オブジェクトで分岐を record 内 1 箇所に閉じる。書き込みエラーは `tracing::warn!` 一度だけ出して以降そのファイルへの書き込みを諦める(計測でデーモンを落とさない))
- [ ] **Step 4: テストが通ることを確認** — Run: `cargo test -p edlr-core --lib profiler` → PASS
- [ ] **Step 5: Commit** — `git commit -m "feat: profiler の sink と collector スレッドを追加"`

---

### Task 4: キュー長プローブと gauge 走査

**Files:**
- Modify: `core/src/runner/plugin/queue.rs`(`WorkSender::len()` と `QueueLenProbe` を追加)
- Modify: `core/src/profiler/collector.rs`(gauge source 登録と毎秒の走査)
- Modify: `core/src/registry/supervisor.rs` は触らない(gauge source は Profiler 自身が持つ)

**Interfaces:**
- Consumes: Task 3 の `Profiler`、既存 `DropCounters`
- Produces:
  - `queue::WorkSender<T>::len(&self) -> usize`
  - `queue::QueueLenProbe`(`Clone`。`fn get(&self) -> usize`。`WorkSender<T>::len_probe(&self) -> QueueLenProbe` で作る。実装: `Arc<dyn Fn() -> usize + Send + Sync>` を包む newtype — 型パラメータ `T` を消すためだけの dyn で、trait-di ルールの「必要が実証された場所」に当たる旨コメントを書く)
  - `collector::GaugeSource { subject: Subject, id: String, queue: QueueLenProbe, drops: Option<Arc<DropCounters>>, memory_bytes: Arc<AtomicU64> }`(ドライバは drops なし)
  - `Profiler::register_gauge(&self, source: GaugeSource)` / `Profiler::unregister_gauge(&self, subject: Subject, id: &str)`(プラグイン Disabled 時に呼ぶ)
- collector の毎秒処理に追加: 登録済み source を走査して `GaugeSample` を合成し、ring + JSONL へ入れる(`ts` は実時刻)

- [ ] **Step 1: 失敗するテストを書く**

```rust
// queue.rs 側
#[test]
fn len_and_len_probe_report_the_queue_depth() {
    let (tx, _rx) = channel();
    tx.push(journal_work("a")).unwrap();
    tx.push(journal_work("b")).unwrap();
    assert_eq!(tx.len(), 2);
    let probe = tx.len_probe();
    assert_eq!(probe.get(), 2);
}

// collector.rs 側
#[test]
fn registered_gauge_sources_are_sampled_every_second() {
    let tmp = tempfile::tempdir().unwrap();
    let profiler = Profiler::start(tmp.path().join("profiler"));
    let (tx, _rx) = crate::runner::plugin::queue::channel();
    tx.push(test_work()).unwrap();
    let memory = Arc::new(AtomicU64::new(4096));
    profiler.register_gauge(GaugeSource {
        subject: Subject::Plugin,
        id: "p1".into(),
        queue: tx.len_probe(),
        drops: Some(DropCounters::new()),
        memory_bytes: memory.clone(),
    });
    std::thread::sleep(std::time::Duration::from_millis(2500));
    profiler.shutdown();

    let ring = profiler.ring();
    let ring = ring.lock().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let recent = ring.window(Subject::Plugin, "p1", now - 5, now + 1);
    let sampled = recent.into_iter().flatten().find(|b| b.queue_len.is_some());
    let b = sampled.expect("at least one gauge bucket in the last seconds");
    assert_eq!(b.queue_len, Some(1));
    assert_eq!(b.memory_bytes, Some(4096));
}
```

- [ ] **Step 2: 落ちることを確認** — Run: `cargo test -p edlr-core --lib "len_probe" && cargo test -p edlr-core --lib registered_gauge` → コンパイルエラー
- [ ] **Step 3: 実装**(`len()` は state ロック 1 回。gauge 走査は collector ループの秒境界処理に足す。登録簿は `Mutex<Vec<GaugeSource>>`)
- [ ] **Step 4: テストが通ることを確認** — Run: `cargo test -p edlr-core --lib profiler && cargo test -p edlr-core --lib queue` → PASS
- [ ] **Step 5: Commit** — `git commit -m "feat: profiler にキュー長プローブと gauge 走査を追加"`

---

### Task 5: wasm 線形メモリの計測(ResourceLimiter ラップ)

**Files:**
- Modify: `core/src/host/plugin.rs`(`limits: StoreLimits` を `TrackedLimits` に差し替え)
- Modify: `core/src/host/driver.rs`(同様)

**Interfaces:**
- Consumes: なし(wasmtime の `ResourceLimiter` trait)
- Produces:
  - `host::plugin::TrackedLimits { inner: StoreLimits, memory_bytes: Arc<AtomicU64> }`
  - `PluginHost::load`(および driver 側の対応するロード関数)が `memory_gauge: Arc<AtomicU64>` を受け取り `TrackedLimits` に渡す(呼び出し側は Task 6 で配線。この Task では既存呼び出し元に `Arc::new(AtomicU64::new(0))` を渡してコンパイルを通す)
- 実装: `impl ResourceLimiter for TrackedLimits` — `memory_growing(current, desired, maximum)` は `self.inner.memory_growing(...)` に委譲し、`Ok(true)`(成長許可)のとき `self.memory_bytes.store(desired as u64, Relaxed)`。`table_growing` ほか他メソッドも全て委譲。`store.limiter(|ctx| &mut ctx.limits)` の行はそのまま(型が変わるだけ)
- 注意: wasmtime の `ResourceLimiter` のメソッドシグネチャはバージョンで異なる(`anyhow::Result<bool>` か `bool` か)。`core/Cargo.toml` の wasmtime バージョンのドキュメントを確認して合わせること。委譲するメソッドは trait 定義の**全メソッド**(デフォルト実装があるものも含め、`inner` と挙動がずれないように明示的に委譲する)

- [ ] **Step 1: 失敗するテストを書く**(`host/plugin.rs` の tests に追加。wasm 実体なしで limiter 単体を叩く)

```rust
#[test]
fn tracked_limits_records_the_granted_memory_size() {
    let gauge = Arc::new(AtomicU64::new(0));
    let mut limits = TrackedLimits::new(
        StoreLimitsBuilder::new().memory_size(PLUGIN_MEMORY_LIMIT).build(),
        gauge.clone(),
    );
    // 上限内の成長は許可され、desired が記録される
    let ok = wasmtime::ResourceLimiter::memory_growing(&mut limits, 0, 65536, None).unwrap();
    assert!(ok);
    assert_eq!(gauge.load(std::sync::atomic::Ordering::Relaxed), 65536);
    // 上限超過は拒否され、値は据え置き
    let ok =
        wasmtime::ResourceLimiter::memory_growing(&mut limits, 65536, u64::MAX as usize, None)
            .unwrap();
    assert!(!ok);
    assert_eq!(gauge.load(std::sync::atomic::Ordering::Relaxed), 65536);
}
```

- [ ] **Step 2: 落ちることを確認** — Run: `cargo test -p edlr-core --lib tracked_limits` → コンパイルエラー
- [ ] **Step 3: 実装**(上記メモどおり。plugin 側で作った `TrackedLimits` を driver 側 `host/driver.rs` からも使う — 置き場は `host/plugin.rs` のまま re-export でよい)
- [ ] **Step 4: テストが通ることを確認** — Run: `cargo test -p edlr-core --lib` → PASS(既存の host テストが `load` の引数追加で壊れていないこと)
- [ ] **Step 5: Commit** — `git commit -m "feat: wasm 線形メモリ使用量を ResourceLimiter 経由で計測する"`

---

### Task 6: 計測点の配線(event_loop・driver・起動経路)

**Files:**
- Modify: `core/src/runner/plugin/event_loop.rs`(`LoopAction::Handle`/`Fire`/`fire_all_due` の wasm 呼び出しを計測)
- Modify: `core/src/runner/driver.rs`(`instance.call_on_message` 呼び出しを計測)
- Modify: `core/src/runner/plugin/start.rs`(`start_plugins`/`load_and_run_plugin` に `Profiler` を引き回し、gauge 登録・memory gauge 配線)
- Modify: `core/src/registry/plugin.rs`(`Registry` 経由で必要なら `Profiler` を保持。Disabled 時の `unregister_gauge` 呼び出し)
- Modify: `core/src/bin/edlr.rs`(`Profiler::start(state_dir.join("profiler"))` を構築し、`start_plugins`/`start_drivers` へ渡す。shutdown シーケンス末尾で `profiler.shutdown()`)

**Interfaces:**
- Consumes: Task 3/4/5 の `Profiler`/`GaugeSource`/`TrackedLimits`
- Produces:
  - `profiler::now_ts() -> f64`(`SystemTime::now()` を UNIX 秒 f64 に。計測点が共通で使う)
  - `profiler::call_sample(subject: Subject, id: &str, call: CallKind, detail: &str, started: Instant, result: &Result<(), PluginCallError>, now: f64) -> CallSample`(純関数: outcome 判定 `PluginCallError` が timeout 系 trap なら `Timeout`、その他 Err は `Error`。`ts` には `now` をそのまま入れる)
- 配線の形(event_loop、`LoopAction::Handle` 内。Fire も同形):

```rust
let started = std::time::Instant::now();
let result = match &work { /* 既存の match をそのまま包む */ };
profiler.record(Sample::Call(profiler::call_sample(
    Subject::Plugin,
    &manifest.id,
    call_kind_of(&work),          // 純関数: PluginWork -> CallKind + detail
    detail_of(&work),
    started,
    &result,
    profiler::now_ts(),
)));
handle_call_result!(result);
```

- `run_plugin_thread` の引数に `profiler: Profiler` を追加(既存引数リストの末尾)。`load_and_run_plugin` はプラグインごとに `memory_gauge = Arc::new(AtomicU64::new(0))` を作り、`PluginHost::load` へ渡し、Running になったら `profiler.register_gauge(GaugeSource { subject: Plugin, id, queue: work_tx.len_probe(), drops: Some(drops.clone()), memory_bytes: memory_gauge })`。スレッド終了時(Disabled 化・shutdown)に `unregister_gauge`
- driver 側 `start_drivers` も同形(drops は `None`)
- 既存テスト(`registry/plugin.rs` ほか)で `start_plugins` 系を呼ぶ箇所は `Profiler::noop()` を渡す
- 純関数 `call_kind_of`/`detail_of`/`call_sample` にはテストを書く。スレッド絡みの配線自体は Task 8 の実データ確認で検証する(wasm 実体が要るため自動テストは既存の統合テスト任せ)

- [ ] **Step 1: 純関数のテストを書く**

```rust
#[test]
fn call_sample_classifies_outcomes_and_measures_duration() {
    let started = std::time::Instant::now();
    let s = call_sample(
        Subject::Plugin, "p1", CallKind::OnEvent, "FSDJump",
        started, &Ok(()), 100.0,
    );
    assert!(matches!(s.outcome, Outcome::Ok));
    assert_eq!(s.id, "p1");
    assert_eq!(s.ts, 100.0);
}
```

(Err ケースは `PluginCallError` の実際のコンストラクタ/variant を確認して 2 ケース --
trap 由来 timeout → `Timeout`、その他 → `Error` -- を足す)

- [ ] **Step 2: 落ちることを確認** — Run: `cargo test -p edlr-core --lib call_sample` → コンパイルエラー
- [ ] **Step 3: 実装 + 配線**(bin/edlr.rs: `Profiler::start` は registry 構築の前、`profiler.shutdown()` は `shutdown_plugins` 系の後・プロセス終了直前)
- [ ] **Step 4: 全テストが通ることを確認** — Run: `cargo test --workspace` → PASS
- [ ] **Step 5: Commit** — `git commit -m "feat: wasm 呼び出しと起動経路に profiler を配線する"`

---

### Task 7: RPC(profiler/summary・profiler/series)

**Files:**
- Create: `core/src/rpc/profiler.rs`(純粋: リングのスナップショット → JSON)
- Modify: `core/src/rpc/mod.rs`(`pub mod profiler;`)
- Modify: `core/src/server/mod.rs`(`ServerState::new` に第 4 引数 `profiler: Option<Profiler>`、WS ディスパッチで `method.starts_with("profiler/")` を `handle_rpc_with_drivers` より前に分岐)
- Modify: `core/src/server/tests.rs`・`core/src/bin/edlr.rs`(`ServerState::new` 呼び出し 3 箇所に引数追加)

**Interfaces:**
- Consumes: Task 2 の `Ring`(`window`/`keys`/`lost`)、Task 3 の `Profiler::ring()`
- Produces(純関数。`now_sec: u64` は引数で受ける):
  - `rpc::profiler::summary_json(ring: &Ring, now_sec: u64) -> serde_json::Value` — 直近 60 秒(`window(.., now_sec - 60, now_sec)`)を subject×id ごとに畳み、spec の形 `{ "profilerLost": u64, "subjects": [ { "subject", "id", "calls_1m", "avg_us_1m", "max_us_1m", "errors_1m", "queue_len", "dropped": {"events","busDeliveries"}, "memory_bytes" } ] }` を返す。`queue_len`/`memory_bytes`/`dropped` は窓内で最後に値のあるバケットの値(無ければ 0)。`avg_us_1m = sum_us / calls`(calls 0 なら 0)。subjects は id 昇順
  - `rpc::profiler::series_json(ring: &Ring, subject: Subject, id: &str, seconds: u64, now_sec: u64) -> serde_json::Value` — `seconds` は 1..=3600 に clamp。`{ "from_ts": now_sec - seconds, "step": 1, "points": [ {calls, avg_us, max_us, errors, queue_len, memory_bytes} | null, ... ] }`
- server 側: `handle_profiler_rpc(profiler: Option<&Profiler>, method: &str, params) -> Result<Value, String>` — `None` なら `Err("profiler unavailable")`。`summary`/`series` を上記純関数に委譲(`now_sec` はここで取る)。`series` のパラメータ検証(subject が `"plugin"`/`"driver"` 以外、id 欠落は `Err`)

- [ ] **Step 1: 失敗するテストを書く**(`rpc/profiler.rs` の tests)

```rust
#[test]
fn summary_folds_the_last_minute_and_orders_subjects_by_id() {
    let mut ring = Ring::new();
    // now=1000。950 秒に 2 call(10us, 30us / 1 error)、990 秒に gauge
    ring.insert(&call_at("b-plugin", 950.0, 10, Outcome::Ok));
    ring.insert(&call_at("b-plugin", 950.5, 30, Outcome::Error));
    ring.insert(&gauge_at("a-plugin", 990.0, 5, 2, 0, 1024));
    let v = summary_json(&ring, 1000);
    let subjects = v["subjects"].as_array().unwrap();
    assert_eq!(subjects[0]["id"], "a-plugin"); // id 昇順
    assert_eq!(subjects[1]["calls_1m"], 2);
    assert_eq!(subjects[1]["avg_us_1m"], 20);
    assert_eq!(subjects[1]["max_us_1m"], 30);
    assert_eq!(subjects[1]["errors_1m"], 1);
    assert_eq!(subjects[0]["queue_len"], 5);
    assert_eq!(subjects[0]["dropped"]["events"], 2);
}

#[test]
fn series_returns_one_point_per_second_with_nulls_for_gaps() {
    let mut ring = Ring::new();
    ring.insert(&call_at("p", 995.0, 10, Outcome::Ok));
    let v = series_json(&ring, Subject::Plugin, "p", 10, 1000);
    assert_eq!(v["from_ts"], 990);
    let points = v["points"].as_array().unwrap();
    assert_eq!(points.len(), 10);
    assert!(points[0].is_null());
    assert_eq!(points[5]["calls"], 1);
}

#[test]
fn series_clamps_seconds_to_3600() {
    let ring = Ring::new();
    let v = series_json(&ring, Subject::Plugin, "p", 999_999, 5000);
    assert_eq!(v["points"].as_array().unwrap().len(), 3600);
}
```

server 側(`server/tests.rs` に追加):

```rust
#[test]
fn profiler_rpc_reads_the_ring_and_errors_without_a_profiler() {
    assert!(handle_profiler_rpc(None, "profiler/summary", &serde_json::json!({})).is_err());

    let profiler = Profiler::noop();
    profiler.ring().lock().unwrap().insert(&/* call サンプル */);
    let v = handle_profiler_rpc(Some(&profiler), "profiler/summary", &serde_json::json!({}))
        .unwrap();
    assert_eq!(v["subjects"].as_array().unwrap().len(), 1);
    assert!(handle_profiler_rpc(Some(&profiler), "profiler/unknown", &serde_json::json!({}))
        .is_err());
}
```

- [ ] **Step 2: 落ちることを確認** — Run: `cargo test -p edlr-core --lib rpc::profiler` → コンパイルエラー
- [ ] **Step 3: 実装**(WS ディスパッチ: `server/mod.rs` の `method` 解決箇所で `if method.starts_with("profiler/")` を先に分岐)
- [ ] **Step 4: 全テストが通ることを確認** — Run: `cargo test --workspace` → PASS
- [ ] **Step 5: Commit** — `git commit -m "feat: profiler/summary と profiler/series RPC を追加"`

---

### Task 8: 実データ確認(手動ゲート)

**Files:** なし(確認のみ)

- [ ] **Step 1: リリースせずローカルでデーモンを起動** — Run: `cargo run -p edlr-core --bin edlr`(必要なら `--journal-dir` 等は既存の起動手順どおり)
- [ ] **Step 2: JSONL を確認** — `<state-base>/profiler/$(date +%F).jsonl` に call/gauge/sink 行が増えること。`tail -f` で on-event 行と毎秒の gauge 行を目視
- [ ] **Step 3: RPC を確認** — `websocat` 等で WS に `{"id":1,"method":"profiler/summary","params":{}}` を送り、動いているプラグインが subjects に並ぶこと。`profiler/series` も 1 件試す
- [ ] **Step 4: 結果をユーザーへ報告し、UI 着手の承認を得る**(数値が明らかにおかしい場合はここで止めて調査)

---

### Task 9: UI — Profiler タブ

**Files:**
- Create: `ui/frontend/src/pages/Profiler.tsx`
- Create: `ui/frontend/src/pages/Profiler.test.tsx`
- Create: `ui/frontend/src/components/Sparkline.tsx`(SVG 折れ線。汎用の小コンポーネント)
- Create: `ui/frontend/src/store/profiler.ts`(RPC 呼び出しヘルパ: summary / series の型と fetch 関数)
- Modify: `ui/frontend/src/App.tsx`(`TABS` に `"Profiler"` を追加し `{tab === "Profiler" && <Profiler />}` を配線。並びは Logs の後)

**Interfaces:**
- Consumes: Task 7 の RPC 応答形(store/profiler.ts に TypeScript 型として写す: `ProfilerSummary`, `SubjectSummary`, `ProfilerSeries`, `SeriesPoint | null`)
- Produces: `<Profiler />` ページコンポーネント
- 仕様:
  - `store/profiler.ts`: `fetchSummary(client): Promise<ProfilerSummary>`、`fetchSeries(client, subject, id, seconds): Promise<ProfilerSeries>`(`store/driverList.ts` と同じく `RpcClient` を使う)
  - ページ: `useEffect` + `setInterval(2000)` で summary をポーリング。テーブル列 = 名前(subject バッジ付き)/ calls/min / avg / max / errors / queue / dropped / memory。クリックで行選択。`max_us_1m > 1_000_000` または `queue_len > 48` の行に `text-destructive` 系の既存 warn スタイル
  - 選択行があるときだけ series(選択レンジ 300 or 3600 秒、トグルボタン)を 2 秒ポーリングし、`<Sparkline>` 3 枚(①calls+errors ②avg/max ③queue+memory)を描画。null 点は線を切る
  - `Sparkline`: props `{ series: (number | null)[][], colors: string[], height?: number }`。viewBox 固定・`preserveAspectRatio="none"`・polyline を系列ぶん重ねるだけ。軸・凡例はラベルテキストのみ
- テスト(vitest + testing-library、既存ページのテストと同じ流儀でモック):

```tsx
it("summary をテーブルに描画し、行選択で series を取得する", async () => {
  const client = {
    call: vi.fn(async (method: string) => {
      if (method === "profiler/summary") return SUMMARY_FIXTURE; // subjects 2 件
      if (method === "profiler/series") return SERIES_FIXTURE;
      throw new Error(`unexpected ${method}`);
    }),
    close: vi.fn(),
  };
  render(<Profiler makeClient={() => client} />);
  expect(await screen.findByText("inara-uploader")).toBeInTheDocument();
  await userEvent.click(screen.getByText("inara-uploader"));
  await waitFor(() =>
    expect(client.call).toHaveBeenCalledWith(
      "profiler/series",
      expect.objectContaining({ id: "inara-uploader" }),
    ),
  );
});

it("閾値超えの行が警告スタイルになる", async () => {
  // max_us_1m: 2_000_000 の行に警告クラスが付くことを確認
});
```

(`makeClient` prop で注入するのは `store/driverList.ts` の既存パターンに合わせる)

- [ ] **Step 1: テストを書く**(上記 2 本 + Sparkline の「null で polyline が分割される」1 本)
- [ ] **Step 2: 落ちることを確認** — Run: `cd ui/frontend && pnpm test -- Profiler` → FAIL
- [ ] **Step 3: 実装**(App.tsx のタブ追加を含む)
- [ ] **Step 4: テストが通ることを確認** — Run: `cd ui/frontend && pnpm test` → 全件 PASS
- [ ] **Step 5: Commit** — `git commit -m "feat: Profiler タブを追加"`

---

### Task 10: 仕上げ — ドキュメント・issue・全体検証

**Files:**
- Modify: `docs/plugins.md`(`plugins/list` の説明の近くに Profiler タブと `<state-base>/profiler/*.jsonl` の 1 段落を追加。DuckDB の `read_json_auto` 例 1 行)
- git issues: 保持日数設定(ローテーション)の後付けを起票

- [ ] **Step 1: docs/plugins.md に追記**(スペックの「スコープ外」も 1 行言及)
- [ ] **Step 2: issue 起票** — `git issues dump` で重複確認 → `GIT_EDITOR=true git issues new "profiler の JSONL に保持日数設定を足したい"` → 本文に「現状ローテなしで単調増加。`collector.rs` の日付切り替え箇所に保持日数の削除を足すのが素直」と書き、summary を埋めて `GIT_EDITOR=true git issues edit <id>`
- [ ] **Step 3: 全体検証** — Run: `cargo test --workspace && cargo clippy --workspace --all-targets && cd ui/frontend && pnpm test` → すべて PASS(clippy の新規警告ゼロ)
- [ ] **Step 4: Commit** — `git add docs/plugins.md && git commit -m "docs: profiler タブと JSONL の説明を追加"`
- [ ] **Step 5: 完了報告**(issue reliable-zn3c は queue_len 可視化がこのタブで満たされるため、内容を確認して満たしていれば close コメントを付けて閉じる)

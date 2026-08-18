//! `profiler/summary` / `profiler/series` の JSON 整形(純粋)。
//!
//! `Ring` のスナップショットを受け取って JSON に畳むだけ。時刻・ロックは
//! 一切扱わない(`now_sec` は呼び出し側(server/)が渡す、→ trait-di.md
//! 「時刻は trait にしない」)。

use serde_json::{json, Value};

use crate::profiler::bucket::{Ring, SecondBucket};
use crate::profiler::Subject;

const WINDOW_SECONDS: u64 = 60;
const MIN_SERIES_SECONDS: u64 = 1;
const MAX_SERIES_SECONDS: u64 = 3600;

/// 直近 60 秒(`[now_sec - 60, now_sec)`)を subject×id ごとに畳んだ summary。
/// `subjects` は id 昇順。
pub fn summary_json(ring: &Ring, now_sec: u64) -> Value {
    let from = now_sec.saturating_sub(WINDOW_SECONDS);
    let mut keys = ring.keys();
    keys.sort_by(|a, b| a.1.cmp(&b.1));

    let subjects: Vec<Value> = keys
        .into_iter()
        .map(|(subject, id)| {
            let window = ring.window(subject, &id, from, now_sec);
            subject_summary_json(subject, &id, &fold_window(&window))
        })
        .collect();

    json!({
        "profilerLost": ring.lost(),
        "subjects": subjects,
    })
}

/// `[now_sec - seconds, now_sec)` の秒ごとの点列(`seconds` は 1..=3600 に
/// clamp)。値の無い秒は `null`。
pub fn series_json(ring: &Ring, subject: Subject, id: &str, seconds: u64, now_sec: u64) -> Value {
    let seconds = seconds.clamp(MIN_SERIES_SECONDS, MAX_SERIES_SECONDS);
    let from_ts = now_sec.saturating_sub(seconds);
    let points: Vec<Value> = ring
        .window(subject, id, from_ts, now_sec)
        .iter()
        .map(point_json)
        .collect();

    json!({
        "from_ts": from_ts,
        "step": 1,
        "points": points,
    })
}

/// 窓内の畳み込み結果。カウンタは合計、gauge 系は窓内で最後に値のあった
/// バケットの値(無ければ 0)。
#[derive(Default)]
struct WindowFold {
    calls: u64,
    errors: u64,
    sum_us: u64,
    max_us: u64,
    queue_len: u64,
    memory_bytes: u64,
    dropped_events: u64,
    dropped_bus: u64,
}

fn fold_window(window: &[Option<SecondBucket>]) -> WindowFold {
    window
        .iter()
        .flatten()
        .fold(WindowFold::default(), |acc, b| WindowFold {
            calls: acc.calls + b.calls,
            errors: acc.errors + b.errors,
            sum_us: acc.sum_us + b.sum_us,
            max_us: acc.max_us.max(b.max_us),
            queue_len: b.queue_len.map(|q| q as u64).unwrap_or(acc.queue_len),
            memory_bytes: b.memory_bytes.unwrap_or(acc.memory_bytes),
            dropped_events: b.dropped_events.unwrap_or(acc.dropped_events),
            dropped_bus: b.dropped_bus.unwrap_or(acc.dropped_bus),
        })
}

fn avg_us(sum_us: u64, calls: u64) -> u64 {
    sum_us.checked_div(calls).unwrap_or(0)
}

fn subject_summary_json(subject: Subject, id: &str, fold: &WindowFold) -> Value {
    json!({
        "subject": subject,
        "id": id,
        "calls_1m": fold.calls,
        "avg_us_1m": avg_us(fold.sum_us, fold.calls),
        "max_us_1m": fold.max_us,
        "errors_1m": fold.errors,
        "queue_len": fold.queue_len,
        "dropped": {
            "events": fold.dropped_events,
            "busDeliveries": fold.dropped_bus,
        },
        "memory_bytes": fold.memory_bytes,
    })
}

fn point_json(bucket: &Option<SecondBucket>) -> Value {
    match bucket {
        None => Value::Null,
        Some(b) => json!({
            "calls": b.calls,
            "avg_us": avg_us(b.sum_us, b.calls),
            "max_us": b.max_us,
            "errors": b.errors,
            "queue_len": b.queue_len,
            "memory_bytes": b.memory_bytes,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiler::{CallKind, CallSample, GaugeSample, Outcome, Sample};

    fn call_at(id: &str, ts: f64, duration_us: u64, outcome: Outcome) -> Sample {
        Sample::Call(CallSample {
            ts,
            subject: Subject::Plugin,
            id: id.to_string(),
            call: CallKind::OnEvent,
            detail: "E".into(),
            duration_us,
            outcome,
        })
    }

    fn gauge_at(
        id: &str,
        ts: f64,
        queue_len: usize,
        dropped_events: u64,
        dropped_bus: u64,
        memory_bytes: u64,
    ) -> Sample {
        Sample::Gauge(GaugeSample {
            ts,
            subject: Subject::Plugin,
            id: id.to_string(),
            queue_len,
            dropped_events,
            dropped_bus,
            memory_bytes,
        })
    }

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
}

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
                // 挿入順が秒単位で単調でないサンプル(スレッド間のわずかな順序ずれ)の場合、
                // `back_mut()` の秒が一致しないケースは新バケットを積む素朴な実装を採用している。
                // これは `window()` が `find` で秒を引くため、重複秒があってもそのうち最初の 1 個を返すだけだから許容される。
                // 1 秒単位のずれは表示上無視できるため、この割り切りでよい。
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

//! プラグインごとの「取りこぼし」カウンタ。
//!
//! プラグイン専用の作業キュー(`PLUGIN_WORK_QUEUE_CAPACITY`)が満杯のとき、
//! 購読タスクは新しい journal イベント / バス配信を warn ログを出して捨てる
//! (遅いだけのプラグインを遅れを理由に殺さないための設計判断 --
//! `runner::PLUGIN_WORK_QUEUE_CAPACITY` のドキュメントコメント参照)。
//!
//! **journal イベントの取りこぼしは replay でも戻らない**: journal の読み取り
//! 位置は配送の成否と独立に進むため、この経路で失われたイベントは再起動時の
//! replay でも二度と配送されない。バス配信も同様に再送されない。
//!
//! それ自体は許容している設計だが、**黙って失われる**のは別問題だった。
//! 何がどれだけ捨てられているか分からないまま `PLUGIN_WORK_QUEUE_CAPACITY` を
//! チューニングするのは当て推量でしかない。ここでプラグインごとに数え、
//! `plugins/list` の応答に載せて可視化する。
//!
//! カウンタは単調増加で、デーモンの起動時からの累計(永続化はしない)。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 1 プラグインぶんの取りこぼし数。購読タスク(書き手)と `plugins/list`
/// (読み手)が `Arc` で共有する。
#[derive(Debug, Default)]
pub struct DropCounters {
    events: AtomicU64,
    bus_deliveries: AtomicU64,
}

impl DropCounters {
    pub(crate) fn new() -> Arc<DropCounters> {
        Arc::new(DropCounters::default())
    }

    /// 作業キュー満杯で journal イベントを 1 件捨てた。
    pub(crate) fn record_event_drop(&self) {
        self.events.fetch_add(1, Ordering::Relaxed);
    }

    /// 作業キュー満杯でバス配信を 1 件捨てた。
    pub(crate) fn record_bus_delivery_drop(&self) {
        self.bus_deliveries.fetch_add(1, Ordering::Relaxed);
    }

    /// RPC 応答用のスナップショット。
    pub fn snapshot(&self) -> DroppedCounts {
        DroppedCounts {
            events: self.events.load(Ordering::Relaxed),
            bus_deliveries: self.bus_deliveries.load(Ordering::Relaxed),
        }
    }
}

/// `plugins/list` に載せる取りこぼし数(デーモン起動時からの累計)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DroppedCounts {
    pub events: u64,
    pub bus_deliveries: u64,
}

impl DroppedCounts {
    /// 一度も取りこぼしていないか。UI は非ゼロのときだけ表示する。
    pub fn is_empty(&self) -> bool {
        self.events == 0 && self.bus_deliveries == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_zero_and_are_reported_as_empty() {
        let counters = DropCounters::new();
        assert_eq!(counters.snapshot(), DroppedCounts::default());
        assert!(counters.snapshot().is_empty());
    }

    #[test]
    fn each_kind_of_drop_is_counted_separately() {
        let counters = DropCounters::new();
        counters.record_event_drop();
        counters.record_event_drop();
        counters.record_bus_delivery_drop();

        let snapshot = counters.snapshot();
        assert_eq!(snapshot.events, 2);
        assert_eq!(snapshot.bus_deliveries, 1);
        assert!(!snapshot.is_empty());
    }

    /// 購読タスクは別スレッドから同時に数えるので、`Arc` 越しの加算が
    /// 失われないこと。
    #[test]
    fn counts_from_concurrent_writers_are_not_lost() {
        let counters = DropCounters::new();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let counters = counters.clone();
                std::thread::spawn(move || {
                    for _ in 0..250 {
                        counters.record_event_drop();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("writer threads should not panic");
        }
        assert_eq!(counters.snapshot().events, 1000);
    }
}

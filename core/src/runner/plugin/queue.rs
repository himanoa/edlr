//! プラグイン/ドライバ専用スレッドの作業キュー。
//!
//! 実体はジェネリックな `WorkSender<T>` / `WorkReceiver<T>`。中身は
//! `Mutex<VecDeque>` と `Condvar` で、受け入れ判定(admit)を種別ごとの
//! 純関数として注入する。プラグイン用(`PluginWork`)の口はこのモジュールが
//! 持ち、ドライバ用の `DriverWork` は `crate::runner::driver` が自分の
//! admit で `channel_for` を呼ぶ。歴史的経緯でこの場所にあるが、中身は共有物。

use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use crate::runner::plugin::{PluginWork, PLUGIN_WORK_QUEUE_CAPACITY};

#[derive(Eq, PartialEq, Debug, Clone)]
pub(crate) enum Admit {
    Accept,
    DropNewest,
}

fn admit(queue_len: usize, work: &PluginWork) -> Admit {
    match work {
        // 完了通知は捨てない: 捨てるとプラグインから見て「submit は成功
        // したのに結果が永遠に来ない」になる。総量は容量 64 +
        // `SUBMIT_IN_FLIGHT_LIMIT`(未完了 submit の上限)で有界のまま。
        PluginWork::JobComplete { .. } => Admit::Accept,
        PluginWork::Event(_) | PluginWork::Message(_) | PluginWork::Stop => {
            if queue_len >= PLUGIN_WORK_QUEUE_CAPACITY {
                Admit::DropNewest
            } else {
                Admit::Accept
            }
        }
    }
}

/// プラグイン用の作業キュー(admit はこのモジュールの `PluginWork` 用)。
pub fn channel() -> (PluginWorkSender, PluginWorkReceiver) {
    channel_for(admit)
}

/// 任意の作業型 `T` のキューを、種別ごとの受け入れ判定 `admit` 付きで作る。
pub(crate) fn channel_for<T>(admit: fn(usize, &T) -> Admit) -> (WorkSender<T>, WorkReceiver<T>) {
    let shared_state = Arc::new(SharedState {
        state: Mutex::new(QueueState {
            queue: VecDeque::new(),
            senders: 1, // この channel_for() が返す Sender の分
            receiver_alive: true,
        }),
        cond: Condvar::new(),
        admit,
    });
    (
        WorkSender(shared_state.clone()),
        WorkReceiver(shared_state),
    )
}

struct QueueState<T> {
    queue: VecDeque<T>,
    senders: usize,
    receiver_alive: bool,
}

struct SharedState<T> {
    state: Mutex<QueueState<T>>,
    cond: Condvar,
    admit: fn(usize, &T) -> Admit,
}

pub struct WorkSender<T>(Arc<SharedState<T>>);

/// 既存の公開名(プラグイン用)。中身はジェネリック版の別名。
pub type PluginWorkSender = WorkSender<PluginWork>;
pub type PluginWorkReceiver = WorkReceiver<PluginWork>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushError {
    Dropped,
    Disconnected,
}

impl<T> WorkSender<T> {
    pub fn push(&self, work: T) -> Result<(), PushError> {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());

        if !state.receiver_alive {
            return Err(PushError::Disconnected);
        }

        match (self.0.admit)(state.queue.len(), &work) {
            Admit::Accept => {
                state.queue.push_back(work);
                self.0.cond.notify_one();
                Ok(())
            }
            Admit::DropNewest => Err(PushError::Dropped),
        }
    }
}

impl<T> Clone for WorkSender<T> {
    fn clone(&self) -> Self {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .senders += 1;
        Self(self.0.clone())
    }
}

impl<T> Drop for WorkSender<T> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        state.senders -= 1;
        if state.senders == 0 {
            self.0.cond.notify_all();
        }
    }
}

pub struct WorkReceiver<T>(Arc<SharedState<T>>);

impl<T> Drop for WorkReceiver<T> {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .receiver_alive = false
    }
}
impl<T> WorkReceiver<T> {
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, std::sync::mpsc::RecvTimeoutError> {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());

        loop {
            if let Some(work) = state.queue.pop_front() {
                return Ok(work);
            }
            if state.senders == 0 {
                return Err(std::sync::mpsc::RecvTimeoutError::Disconnected);
            }
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return Err(std::sync::mpsc::RecvTimeoutError::Timeout);
            };

            let (guard, _) = self
                .0
                .cond
                .wait_timeout(state, remaining)
                .unwrap_or_else(|p| p.into_inner());
            state = guard
        }
    }

    /// 仕事が来る(または全 Sender が切断される)まで待つブロッキング受信。
    /// スケジュールを持たないドライバのループ用(プラグイン側は
    /// `recv_timeout` でスケジュール発火の期限を待つ)。
    pub fn recv(&self) -> Result<T, std::sync::mpsc::RecvError> {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(work) = state.queue.pop_front() {
                return Ok(work);
            }
            if state.senders == 0 {
                return Err(std::sync::mpsc::RecvError);
            }
            state = self
                .0
                .cond
                .wait(state)
                .unwrap_or_else(|p| p.into_inner());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Instant;

    fn journal_work(name: &str) -> PluginWork {
        PluginWork::Event(Arc::new(Event::Journal {
            timestamp: "2026-08-05T00:00:00Z".into(),
            event: name.into(),
            raw: serde_json::json!({}),
            replay: false,
        }))
    }

    fn event_name(work: &PluginWork) -> String {
        match work {
            PluginWork::Event(event) => match event.as_ref() {
                Event::Journal { event: name, .. } => name.clone(),
                other => panic!("expected a journal event, got {other:?}"),
            },
            other => panic!("expected PluginWork::Event, got {other:?}"),
        }
    }

    mod admit_tests {
        use super::*;

        #[test]
        fn below_capacity_accepts() {
            let verdict = admit(PLUGIN_WORK_QUEUE_CAPACITY - 1, &PluginWork::Stop);
            assert_eq!(verdict, Admit::Accept);
        }

        #[test]
        fn at_capacity_drops_newest() {
            let verdict = admit(PLUGIN_WORK_QUEUE_CAPACITY, &PluginWork::Stop);
            assert_eq!(verdict, Admit::DropNewest);
        }

        /// `JobComplete` だけは満杯でも受け入れる(issue-sizx 決定 2)。
        /// 捨てると「submit は成功したのに結果が永遠に来ない」になるため。
        #[test]
        fn job_complete_is_accepted_even_at_capacity() {
            let verdict = admit(
                PLUGIN_WORK_QUEUE_CAPACITY,
                &PluginWork::JobComplete {
                    generation: 0,
                    job_id: 1,
                    result_json: "{}".to_string(),
                },
            );
            assert_eq!(verdict, Admit::Accept);
        }
    }

    #[test]
    fn push_then_recv_is_fifo() {
        let (tx, rx) = channel();
        tx.push(journal_work("first")).unwrap();
        tx.push(journal_work("second")).unwrap();

        let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let second = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event_name(&first), "first");
        assert_eq!(event_name(&second), "second");
    }

    #[test]
    fn push_to_a_full_queue_returns_dropped_and_keeps_existing_work() {
        let (tx, rx) = channel();
        for i in 0..PLUGIN_WORK_QUEUE_CAPACITY {
            tx.push(journal_work(&format!("Evt{i}"))).unwrap();
        }

        assert_eq!(
            tx.push(journal_work("overflow")),
            Err(PushError::Dropped),
            "the {PLUGIN_WORK_QUEUE_CAPACITY}+1th push must be rejected, not block"
        );

        // 既存の仕事は失われない(捨てるのは新入りだけ)。
        let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event_name(&first), "Evt0");
    }

    /// std mpsc と同じセマンティクス: 全 Sender が drop 済みでも、キューに
    /// 残った仕事(shutdown 時の `Stop` など)は返し切ってから
    /// `Disconnected` になる。pop と senders チェックの順序を入れ替えると
    /// このテストが落ちる。
    #[test]
    fn remaining_work_is_drained_before_disconnected() {
        let (tx, rx) = channel();
        tx.push(journal_work("leftover")).unwrap();
        tx.push(PluginWork::Stop).unwrap();
        drop(tx);

        assert_eq!(
            event_name(&rx.recv_timeout(Duration::from_secs(1)).unwrap()),
            "leftover"
        );
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)),
            Ok(PluginWork::Stop)
        ));
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(10)),
            Err(RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn push_after_receiver_drop_is_disconnected() {
        let (tx, rx) = channel();
        drop(rx);
        assert_eq!(tx.push(PluginWork::Stop), Err(PushError::Disconnected));
    }

    #[test]
    fn recv_on_an_empty_queue_times_out_while_senders_are_alive() {
        let (tx, rx) = channel();
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(10)),
            Err(RecvTimeoutError::Timeout)
        ));
        drop(tx);
    }

    #[test]
    fn cloned_sender_keeps_the_channel_alive() {
        let (tx, rx) = channel();
        let tx2 = tx.clone();
        drop(tx);

        // clone が生きている間は Disconnected にならない。
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(10)),
            Err(RecvTimeoutError::Timeout)
        ));

        drop(tx2);
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(10)),
            Err(RecvTimeoutError::Disconnected)
        ));
    }

    /// notify 漏れの検出。単一スレッドのテストは受信側が実際に待ちへ入る
    /// 前に push が済んでしまうため、`notify_one` を消しても通ってしまう。
    /// ここでは受信側を本当にブロックさせ、push で速やかに(タイムアウトの
    /// 5 秒ではなく)起こされることを確認する。
    #[test]
    fn a_blocked_receiver_is_woken_by_push_promptly() {
        let (tx, rx) = channel();
        let pusher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            tx.push(journal_work("wake")).unwrap();
            tx // 受信完了まで Sender を生かしておく
        });

        let started = Instant::now();
        let work = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(event_name(&work), "wake");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "receiver must be woken by the push, not by the 5s timeout"
        );
        drop(pusher.join().unwrap());
    }

    /// 同上の切断版: 最後の Sender の drop(`notify_all`)が、待っている
    /// 受信側を速やかに起こすことを確認する。これが漏れると切断検出が
    /// 最悪タイムアウト分(実運用では 3600 秒)遅れる。
    #[test]
    fn a_blocked_receiver_is_woken_by_the_last_sender_drop_promptly() {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(tx);
        });

        let started = Instant::now();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(5)),
            Err(RecvTimeoutError::Disconnected)
        ));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "receiver must be woken by the sender drop, not by the 5s timeout"
        );
    }
}

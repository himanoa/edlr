use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use crate::runner::plugin::{PluginWork, PLUGIN_WORK_QUEUE_CAPACITY};

#[derive(Eq, PartialEq, Debug, Clone)]
enum Admit {
    Accept,
    DropNewest,
}

fn admit(queue_len: usize, _work: &PluginWork) -> Admit {
    if queue_len >= PLUGIN_WORK_QUEUE_CAPACITY {
        Admit::DropNewest
    } else {
        Admit::Accept
    }
}

pub(crate) fn channel() -> (PluginWorkSender, PluginWorkReceiver) {
    let shared_state = Arc::new(SharedState {
        state: Mutex::new(QueueState {
            queue: VecDeque::new(),
            senders: 1, // この channel() が返す Sender の分
            receiver_alive: true,
        }),
        cond: Condvar::new(),
    });
    (
        PluginWorkSender(shared_state.clone()),
        PluginWorkReceiver(shared_state),
    )
}

struct QueueState {
    queue: VecDeque<PluginWork>,
    senders: usize,
    receiver_alive: bool,
}

struct SharedState {
    state: Mutex<QueueState>,
    cond: Condvar,
}

pub(crate) struct PluginWorkSender(Arc<SharedState>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PushError {
    Dropped,
    Disconnected,
}

impl PluginWorkSender {
    pub(crate) fn push(&self, work: PluginWork) -> Result<(), PushError> {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());

        if !state.receiver_alive {
            return Err(PushError::Disconnected);
        }

        match admit(state.queue.len(), &work) {
            Admit::Accept => {
                state.queue.push_back(work);
                self.0.cond.notify_one();
                Ok(())
            }
            Admit::DropNewest => Err(PushError::Dropped),
        }
    }
}

impl Clone for PluginWorkSender {
    fn clone(&self) -> Self {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .senders += 1;
        Self(self.0.clone())
    }
}

impl Drop for PluginWorkSender {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        state.senders -= 1;
        if state.senders == 0 {
            self.0.cond.notify_all();
        }
    }
}

pub(crate) struct PluginWorkReceiver(Arc<SharedState>);

impl Drop for PluginWorkReceiver {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .receiver_alive = false
    }
}
impl PluginWorkReceiver {
    pub(crate) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<PluginWork, std::sync::mpsc::RecvTimeoutError> {
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

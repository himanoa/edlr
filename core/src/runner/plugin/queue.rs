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
    /// 新着を拒否する(`PushError::Dropped`)。ドライバキューの
    /// 「`publish` に `queue-full` を返す(捨てない)」契約用。
    DropNewest,
    /// 新着を積み、代わりにキュー内で最も古い evictable な仕事を捨てる
    /// (リングバッファ動作)。捨てた仕事は `push` の `Ok(Some(_))` で
    /// 呼び出し元へ返す(drop counter へ数えるため)。evictable が
    /// 1 つも無ければそのまま積む(`Stop`/`JobComplete` だけで満杯に
    /// なることは実質無く、なっても総量は別途有界)。
    DropOldest,
}

/// lossy(既定)プラグインの受け入れ判定(issue-hdly)。
///
/// 満杯時は新着を拒否するのではなく最古のイベント/配信を捨てる
/// (`DropOldest`)。coeiroink への読み上げ通知のような「最新が届く方が
/// 大事」な用途で、古い通知が居座って新しいものが捨てられるのを防ぐ。
fn admit_lossy(queue_len: usize, work: &PluginWork) -> Admit {
    match work {
        // 完了通知は捨てない: 捨てるとプラグインから見て「submit は成功
        // したのに結果が永遠に来ない」になる。総量は容量 64 +
        // `SUBMIT_IN_FLIGHT_LIMIT`(未完了 submit の上限)で有界のまま。
        PluginWork::JobComplete { .. } => Admit::Accept,
        PluginWork::Event(_) | PluginWork::Message(_) | PluginWork::Stop => {
            if queue_len >= PLUGIN_WORK_QUEUE_CAPACITY {
                Admit::DropOldest
            } else {
                Admit::Accept
            }
        }
    }
}

/// reliable プラグインの受け入れ判定: 何も捨てない(無制限)。
/// inara アップロードのような「取りこぼし厳禁」のプラグイン用(issue-hdly)。
fn admit_reliable(_queue_len: usize, _work: &PluginWork) -> Admit {
    Admit::Accept
}

/// `DropOldest` の追い出し対象。journal イベントとバス配信だけが捨てられる
/// (`Stop` は停止指示そのもの、`JobComplete` は捨てると submit の結果が
/// 永遠に来ない)。
fn plugin_work_evictable(work: &PluginWork) -> bool {
    matches!(work, PluginWork::Event(_) | PluginWork::Message(_))
}

/// プラグイン用の作業キュー(lossy: 満杯時は最古のイベントを捨てる)。
pub fn channel() -> (PluginWorkSender, PluginWorkReceiver) {
    channel_for(admit_lossy, plugin_work_evictable)
}

/// `delivery = "reliable"` なプラグイン用の無制限キュー(何も捨てない)。
pub fn reliable_channel() -> (PluginWorkSender, PluginWorkReceiver) {
    channel_for(admit_reliable, plugin_work_evictable)
}

/// 任意の作業型 `T` のキューを、種別ごとの受け入れ判定 `admit` と
/// `DropOldest` 時の追い出し対象判定 `evictable` 付きで作る。
pub(crate) fn channel_for<T>(
    admit: fn(usize, &T) -> Admit,
    evictable: fn(&T) -> bool,
) -> (WorkSender<T>, WorkReceiver<T>) {
    let shared_state = Arc::new(SharedState {
        state: Mutex::new(QueueState {
            queue: VecDeque::new(),
            senders: 1, // この channel_for() が返す Sender の分
            receiver_alive: true,
        }),
        cond: Condvar::new(),
        admit,
        evictable,
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
    evictable: fn(&T) -> bool,
}

pub struct WorkSender<T>(Arc<SharedState<T>>);

/// キュー長を読むだけのプローブ。プロファイラの gauge 走査(秒境界)が
/// `WorkSender<T>` の型パラメータ `T` を知らずに複数の異なる `T` の
/// キューを 1 つの `Vec<GaugeSource>` にまとめて持つために型を消す
/// (`.claude/rules/trait-di.md` の「dyn を標準にしない、必要が実証された
/// 場所を除く」に該当する箇所 — ここではジェネリクスをそのまま公開すると
/// `GaugeSource<T>` になってしまい、収集先の `Vec` を持てない)。
#[derive(Clone)]
pub struct QueueLenProbe(Arc<dyn Fn() -> usize + Send + Sync>);

impl QueueLenProbe {
    pub fn get(&self) -> usize {
        (self.0)()
    }
}

/// 既存の公開名(プラグイン用)。中身はジェネリック版の別名。
pub type PluginWorkSender = WorkSender<PluginWork>;
pub type PluginWorkReceiver = WorkReceiver<PluginWork>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushError {
    Dropped,
    Disconnected,
}

impl<T> WorkSender<T> {
    /// 仕事を積む。`Ok(Some(evicted))` は「新着は積んだが、代わりに最古の
    /// evictable な仕事を捨てた」(`Admit::DropOldest`)。呼び出し元は
    /// 捨てられた仕事を drop counter へ数えること(捨てられたのが
    /// 押し込んだ種別と違うことがある点に注意 -- イベントの push が
    /// バス配信を追い出すこともある)。
    pub fn push(&self, work: T) -> Result<Option<T>, PushError> {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());

        if !state.receiver_alive {
            return Err(PushError::Disconnected);
        }

        match (self.0.admit)(state.queue.len(), &work) {
            Admit::Accept => {
                state.queue.push_back(work);
                self.0.cond.notify_one();
                Ok(None)
            }
            Admit::DropNewest => Err(PushError::Dropped),
            Admit::DropOldest => {
                let evicted = state
                    .queue
                    .iter()
                    .position(self.0.evictable)
                    .and_then(|i| state.queue.remove(i));
                state.queue.push_back(work);
                self.0.cond.notify_one();
                Ok(evicted)
            }
        }
    }

    /// 現在のキュー長。state ロック 1 回。
    #[allow(clippy::len_without_is_empty)] // 空判定は今のところ使い道がない
    pub fn len(&self) -> usize {
        self.0.state.lock().unwrap_or_else(|p| p.into_inner()).queue.len()
    }
}

impl<T: Send + 'static> WorkSender<T> {
    /// このキューの長さを読むだけの `QueueLenProbe` を作る(プロファイラの
    /// gauge 走査用)。`Arc` 越しなので `WorkSender` の生存とは無関係に
    /// 呼べる。
    pub fn len_probe(&self) -> QueueLenProbe {
        let shared = self.0.clone();
        QueueLenProbe(Arc::new(move || {
            shared.state.lock().unwrap_or_else(|p| p.into_inner()).queue.len()
        }))
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
            let verdict = admit_lossy(PLUGIN_WORK_QUEUE_CAPACITY - 1, &PluginWork::Stop);
            assert_eq!(verdict, Admit::Accept);
        }

        /// lossy は満杯時に最古を捨てて新着を積む(issue-hdly)。
        #[test]
        fn at_capacity_lossy_drops_oldest() {
            let verdict = admit_lossy(PLUGIN_WORK_QUEUE_CAPACITY, &PluginWork::Stop);
            assert_eq!(verdict, Admit::DropOldest);
        }

        /// `JobComplete` だけは満杯でも受け入れる(issue-sizx 決定 2)。
        /// 捨てると「submit は成功したのに結果が永遠に来ない」になるため。
        #[test]
        fn job_complete_is_accepted_even_at_capacity() {
            let verdict = admit_lossy(
                PLUGIN_WORK_QUEUE_CAPACITY,
                &PluginWork::JobComplete {
                    generation: 0,
                    job_id: 1,
                    result_json: "{}".to_string(),
                },
            );
            assert_eq!(verdict, Admit::Accept);
        }

        /// reliable は容量に関わらず何も捨てない(issue-hdly)。
        #[test]
        fn reliable_accepts_everything_past_capacity() {
            let verdict = admit_reliable(PLUGIN_WORK_QUEUE_CAPACITY * 100, &journal_work("Evt"));
            assert_eq!(verdict, Admit::Accept);
        }

        /// 追い出せるのはイベントとバス配信だけ(`Stop`/`JobComplete` は不可)。
        #[test]
        fn only_events_and_messages_are_evictable() {
            assert!(plugin_work_evictable(&journal_work("Evt")));
            assert!(!plugin_work_evictable(&PluginWork::Stop));
            assert!(!plugin_work_evictable(&PluginWork::JobComplete {
                generation: 0,
                job_id: 1,
                result_json: "{}".to_string(),
            }));
        }
    }

    #[test]
    fn len_and_len_probe_report_the_queue_depth() {
        let (tx, _rx) = channel();
        tx.push(journal_work("a")).unwrap();
        tx.push(journal_work("b")).unwrap();
        assert_eq!(tx.len(), 2);
        let probe = tx.len_probe();
        assert_eq!(probe.get(), 2);
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

    /// lossy キューの満杯時はリングバッファ動作: 最古を捨てて新着を積む
    /// (issue-hdly。かつては新着を捨てていたため、coeiroink 通知のような
    /// 「最新が届く方が大事」な用途で古い仕事が居座っていた)。
    #[test]
    fn push_to_a_full_queue_evicts_the_oldest_and_keeps_the_newest() {
        let (tx, rx) = channel();
        for i in 0..PLUGIN_WORK_QUEUE_CAPACITY {
            tx.push(journal_work(&format!("Evt{i}"))).unwrap();
        }

        let evicted = tx
            .push(journal_work("overflow"))
            .expect("the {PLUGIN_WORK_QUEUE_CAPACITY}+1th push must be accepted, not block")
            .expect("a full queue must evict its oldest work");
        assert_eq!(event_name(&evicted), "Evt0");

        // 先頭は Evt1 になり、新着は末尾に積まれている。
        let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event_name(&first), "Evt1");
        let last = std::iter::from_fn(|| rx.recv_timeout(Duration::ZERO).ok())
            .last()
            .unwrap();
        assert_eq!(event_name(&last), "overflow");
    }

    /// 満杯のキューへの `Stop` は自分が捨てられるのではなく、最古の
    /// イベントを追い出して積まれる(待ちに入る直前のスレッドを確実に
    /// 起こせる)。逆に `Stop` 自身は追い出し対象にならない。
    #[test]
    fn stop_pushed_to_a_full_queue_evicts_an_event_and_is_never_evicted_itself() {
        let (tx, rx) = channel();
        for i in 0..PLUGIN_WORK_QUEUE_CAPACITY {
            tx.push(journal_work(&format!("Evt{i}"))).unwrap();
        }

        let evicted = tx.push(PluginWork::Stop).unwrap().unwrap();
        assert_eq!(event_name(&evicted), "Evt0");

        // さらにイベントを積み続けても Stop は追い出されず、イベント側が
        // 順に捨てられていく。
        for i in 0..PLUGIN_WORK_QUEUE_CAPACITY {
            tx.push(journal_work(&format!("After{i}"))).unwrap();
        }
        let stop_still_queued = std::iter::from_fn(|| rx.recv_timeout(Duration::ZERO).ok())
            .any(|work| matches!(work, PluginWork::Stop));
        assert!(stop_still_queued, "Stop must survive event pressure");
    }

    /// `reliable_channel` は容量を超えても一切捨てない(issue-hdly)。
    #[test]
    fn a_reliable_channel_never_drops_past_capacity() {
        let (tx, rx) = reliable_channel();
        let published = PLUGIN_WORK_QUEUE_CAPACITY * 3;
        for i in 0..published {
            let evicted = tx.push(journal_work(&format!("Evt{i}"))).unwrap();
            assert!(evicted.is_none(), "reliable queue must never evict");
        }
        let received = std::iter::from_fn(|| rx.recv_timeout(Duration::ZERO).ok()).count();
        assert_eq!(received, published);
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
        assert!(matches!(
            tx.push(PluginWork::Stop),
            Err(PushError::Disconnected)
        ));
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

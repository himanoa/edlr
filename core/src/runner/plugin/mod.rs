//! `plugins_dir` を走査し、各プラグインをロードして専用タスク/スレッドで駆動する。
//!
//! 各プラグインは以下の構成で動く:
//! - 専用の OS スレッドが `PluginHost::load` → `call_init` → イベントループ
//!   (`call_on_event`)を直列に実行する。`PluginInstance`(wasmtime の
//!   `Store`)はこのスレッドの外に出ることがなく、`Send` かどうかを気にする
//!   必要がない。wasm 呼び出しは同期・ブロッキングだが、これは tokio の
//!   ワーカースレッドとは独立した OS スレッドなので、非同期ランタイムを
//!   ブロックしない。
//! - 専用の tokio タスクが `router.subscribe()` した `broadcast::Receiver` から
//!   イベントを受け取り、`matches_event` でフィルタしたうえで `std::sync::mpsc`
//!   経由でプラグインスレッドへ転送する(こちらは非同期処理・待ち合わせ側)。
//!
//! プラグインスレッドが `call_on_event` の `Err`(trap を含む)を受け取ると、
//! レジストリを `Disabled` にしてループを抜け、スレッドを終了する。それに伴い
//! `std::sync::mpsc` の送信側(購読タスク)への送信も失敗するようになるため、
//! 購読タスクも次のイベントで終了する。他プラグインや監視コアには一切波及しない。
//!
//! journal イベントに加えて、バス経由でドライバから届く配信(`Delivery`)も
//! 同じプラグインスレッドで処理する。`PluginInstance` は 1 スレッドの外に出ない
//! という性質を保つため、2 本目のスレッドや 2 つ目の wasm 呼び出し口を増やす
//! のではなく、両方を 1 本の `PluginWork` キューに混ぜて直列化する
//! (`PluginWork` のドキュメントコメント参照)。バス側の配信は `Bus::subscribe`
//! が要求する `SyncSender<Delivery>` を別途受け取り、それを `PluginWork` へ
//! 詰め替えて転送する専用の tokio タスク(`spawn_bus_subscriber`)が
//! `spawn_event_subscriber` と対称の形で存在する。
//!
//! `manifest.schedules` に基づく `on-schedule` の発火も、この同じ
//! プラグインスレッドの中で行う(`PluginInstance` を 1 スレッドの外に
//! 出さない性質を保つため)。専用スレッドは `work_rx.recv_timeout` を
//! `ScheduleState::until_next` の残り時間でブロックし、タイムアウトかつ
//! 期限切れがあれば `call_on_schedule` を呼ぶ。仕事(イベント/バス配信)は
//! 発火より優先するが、`driver-http.send` などで wasm 呼び出しがブロック
//! している間に期限を過ぎたスケジュールは、その呼び出しの直後に
//! `ScheduleState::take_due` を引けるだけ引いて 1 回ずつまとめて発火する
//! (`fire_all_due` 参照)。「次に何をするか」の判定はテスト可能な純粋
//! 関数 `next_action`/`LoopAction` に切り出してある。

mod event_loop;
mod queue;
mod start;
mod subscriber;

pub use start::start_plugins;

use std::sync::Arc;

use edlr_driver_channel::Delivery;

use crate::event::Event;

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::{mpsc as std_mpsc, Mutex};
#[cfg(test)]
use std::thread;

#[cfg(test)]
use tokio::sync::broadcast;

#[cfg(test)]
use crate::manifest::Manifest;
#[cfg(test)]
use crate::runtime::dropped::DropCounters;

#[cfg(test)]
use event_loop::{
    deadline_verdict, next_action, DeadlineVerdict, LoopAction, CALL_DEADLINE_STRIKES,
};
#[cfg(test)]
use subscriber::{spawn_bus_subscriber, spawn_event_subscriber, subscribe_with_initial_value};

/// プラグイン専用の作業キュー(下記 `work_tx`/`work_rx`)の容量。journal
/// イベント(`PluginWork::Event`)とドライバからのバス配信
/// (`PluginWork::Message`)の両方をこの 1 本の有界チャネルに混ぜることで、
/// 2 種類の wasm 呼び出しをプラグイン専用の 1 スレッドに直列化する(モジュール
/// のドキュメントコメントと `PluginWork` を参照)。`spawn_bus_subscriber` が
/// 読む配信専用の `Delivery` チャネルの容量にも同じ値を使い回している。
///
/// `driver-http.send` は 1 呼び出しあたり `host::HTTP_TIMEOUT` までプラグイン
/// 専用スレッドをブロックしうる(プラグイン作者の管理下にないホストが単に
/// 応答しないことがある)。その間プラグイン自身のスレッドは `work_rx` を
/// 全く読まない。このチャネルはかつて無制限(`std_mpsc::channel`)で journal
/// イベントのみを運んでいたため、詰まったプラグインは router/monitor 側の
/// broadcast イベントをホストメモリ上に際限なく溜め込ませてしまっていた
/// -- `StoreLimits` から見えるプラグインの wasm 線形メモリの外側の話なので、
/// そこでは検知できない。ここで容量を切ることで、プラグイン 1 件あたりの
/// 積み残し(イベントであれバス配信であれ)を固定の小さい数に抑える。
///
/// 満杯時の方針: `spawn_event_subscriber` は新しいイベントを、
/// `spawn_bus_subscriber` は新しい配信を、それぞれ `tracing::warn!` を出して
/// 捨てる(購読タスク自身をブロックする(各ドキュメントコメント参照)のでも
/// プラグインを無効化するのでもない)。`driver-http.send` を待っているだけの
/// ような、単に遅いプラグインを、遅れを理由に殺すべきではないため。
///
/// **32 → 64 への変更(このタスクでの調整)**: この容量はもともと journal
/// イベントのみを運んでいた頃に決めた値。今は 2 つの独立したプロデューサ
/// (router のイベント購読タスクと、バスの配信購読タスク)が同じキューの
/// 枠を奪い合っており、両者の間には公平性も優先度も無いため、どちらか一方の
/// バーストがもう一方を飢えさせて捨てさせうる。容量を倍にすることで、
/// おおよそ元の数が想定していた「1 ストリームあたりの余裕」を両ストリームに
/// 復元する(ドライバ側 `DRIVER_MESSAGE_QUEUE_CAPACITY` の 64 とも揃う)。
///
/// **既知の限界**: これは緩和策であって解決策ではない。2 プロデューサ間の
/// 公平性は依然として無く、十分におしゃべりなドライバがあれば journal
/// イベント側が捨てられることは今も起こりうる。
///
/// ただし破棄は**観測可能**になった: `plugin::dropped::DropCounters` が
/// プラグインごとに数え、`plugins/list` の `dropped` として返す(UI にも
/// 非ゼロのときだけ表示される)。この数値をチューニングする際は、まず
/// そのカウンタを見ること -- 実際に何か捨てられているかも分からないまま
/// 容量だけをいじるのは当て推量でしかない。
const PLUGIN_WORK_QUEUE_CAPACITY: usize = 64;

/// プラグイン専用スレッドが処理する仕事。journal イベントとバスの配信を
/// 1 本のキューに混ぜることで、wasm 呼び出しが 1 スレッドに直列化される
/// 性質(`PluginInstance` が `Send` を気にしなくてよい根拠)を保つ。
///
/// `Stop`(Task 5)はデーモンの正常終了シーケンス
/// (`Registry::shutdown_plugins`)専用で、`Event`/`Message` と同じ 1 本の
/// キューに混ぜることで「仕事を全部処理し終えてから止める」のではなく
/// 「今の仕事のあとに来た Stop で速やかに止める」という程度の順序保証を
/// 得る(`work_tx` は有界なので、詰まっている間は他の仕事同様 Stop も
/// 送れないことがある -- `Registry::shutdown_plugins` が `try_send` の
/// 失敗を warn ログにして諦める理由)。
#[derive(Debug)]
pub(crate) enum PluginWork {
    Event(Arc<Event>),
    Message(Delivery),
    Stop,
}

#[cfg(test)]
mod tests {
    //! `spawn_event_subscriber` is what stands between the router's
    //! broadcast channel and a plugin thread that might be blocked for up
    //! to `host::HTTP_TIMEOUT` inside `driver-http.send`. These tests never
    //! drain `events_rx` (simulating a fully-stalled plugin thread) and
    //! assert that publishing far more events than
    //! `PLUGIN_WORK_QUEUE_CAPACITY` neither blocks the publishing task nor
    //! grows the queue past its bound.
    use super::*;
    use std::time::Duration;

    fn test_manifest() -> Manifest {
        Manifest {
            id: "queue-test-plugin".into(),
            name: "Queue Test".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec!["*".to_string()],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    fn journal_event(name: &str) -> Arc<Event> {
        Arc::new(Event::Journal {
            timestamp: "2026-07-25T00:00:00Z".into(),
            event: name.into(),
            raw: serde_json::json!({}),
            replay: false,
        })
    }

    #[tokio::test]
    async fn slow_plugin_channel_stays_bounded_and_publishing_does_not_block() {
        let (broadcast_tx, broadcast_rx) = broadcast::channel::<Arc<Event>>(4096);
        let (work_tx, work_rx) = std_mpsc::sync_channel::<PluginWork>(PLUGIN_WORK_QUEUE_CAPACITY);

        let drops = DropCounters::new();
        spawn_event_subscriber(test_manifest(), broadcast_rx, work_tx, drops.clone());

        // Simulate a plugin thread that's blocked in `driver-http.send`:
        // never drain `work_rx` while publishing far more events than the
        // queue's capacity. If `try_send` were replaced with a blocking
        // `send`, this loop (running on the current task, same as the
        // subscriber would on a shared runtime) would risk deadlocking or
        // at minimum this test would take a long time; with `try_send` it
        // must complete promptly regardless of channel fullness.
        let published = PLUGIN_WORK_QUEUE_CAPACITY * 50;
        for i in 0..published {
            broadcast_tx
                .send(journal_event(&format!("Evt{i}")))
                .expect("broadcast send should succeed (large capacity, no lag)");
        }

        // Give the subscriber tokio task a bounded window to drain the
        // broadcast channel into work_rx (or drop on overflow); this test
        // fails by hanging (via the outer test timeout) if the subscriber
        // ever blocks trying to push past the queue's capacity.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut queued = 0usize;
        while work_rx.try_recv().is_ok() {
            queued += 1;
        }

        assert!(
            queued <= PLUGIN_WORK_QUEUE_CAPACITY,
            "queued {queued} events, expected at most {PLUGIN_WORK_QUEUE_CAPACITY} \
             (publishing {published} events to a never-drained receiver must not \
             grow the queue past its bound)"
        );

        // 溢れた分は黙って消えるのではなく、`plugins/list` から見える形で
        // 数えられていること。
        let dropped = drops.snapshot().events;
        assert!(
            dropped > 0,
            "publishing {published} events into a {PLUGIN_WORK_QUEUE_CAPACITY}-slot \
             queue must record the overflow as dropped events, got {dropped}"
        );
        assert_eq!(
            dropped as usize + queued,
            published,
            "every published event must be either queued or counted as dropped"
        );
    }

    #[test]
    fn deliveries_reach_the_plugin_queue_and_full_queues_drop_the_message() {
        // Bus::subscribe に渡すのと同じ容量 1 の sync_channel を使い、
        // 2 通目が捨てられる(＝ emit 自体は成功する)ことを確認する。
        let bus = edlr_driver_channel::Bus::new();
        let (dtx, _drx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            }],
            dtx,
        );
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        bus.subscribe("translator", "ed-state", "current-system", tx);

        bus.emit("ed-state", "current-system", b"a".to_vec())
            .unwrap();
        bus.emit("ed-state", "current-system", b"b".to_vec())
            .unwrap();

        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
        assert_eq!(
            bus.retained_for("ed-state", "current-system"),
            Some(b"b".to_vec())
        );
    }

    #[test]
    fn subscribing_to_a_retained_topic_delivers_the_current_value_once() {
        let bus = edlr_driver_channel::Bus::new();
        let (dtx, _drx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            }],
            dtx,
        );
        bus.emit("ed-state", "current-system", b"Sol".to_vec())
            .unwrap();

        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        subscribe_with_initial_value(&bus, "translator", "ed-state", "current-system", tx);

        assert_eq!(rx.try_recv().unwrap().payload, b"Sol".to_vec());
    }

    /// `spawn_bus_subscriber` が承認取消を配信のたびに再確認することの検証。
    ///
    /// テストの信頼性を担保するため、承認あり/なしの 2 ケースを **同じ**
    /// 購読・同じ emit で作り、違いは `bus_json` の `granted` だけにする
    /// (「何も送っていないから届かない」で偽陽性になるのを避けるため)。
    #[tokio::test]
    async fn bus_subscriber_forwards_only_while_still_granted() {
        use crate::runtime::bus::{bus_json_string, BusRuntimeEntry};

        let bus = edlr_driver_channel::Bus::new();
        let (dtx, _drx) = std_mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: false,
                description: String::new(),
            }],
            dtx,
        );

        let granted_entry = |granted: bool| {
            bus_json_string(&[BusRuntimeEntry {
                driver: "ed-state".into(),
                granted,
                publish: vec![],
                subscribe: vec!["current-system".into()],
            }])
        };

        // 承認あり・承認なしを 2 つの独立したケースとして作ると、それぞれが
        // 「配信を 1 回しか送らない」ため、「サブスクライバ起動時に承認を
        // 一度だけ確認する」実装と「配信のたびに再確認する」実装のどちらでも
        // 両ケースが通ってしまう(前者は起動時の `bus_json` の値をそのまま
        // 使い続けるだけで、たまたま両ケースとも「起動時点の値」と「配信時点
        // の値」が一致しているに過ぎない)。ここでは **1 つのサブスクライバの
        // 生存期間の中で** 同じ `bus_json` バッファを書き換えることで、
        // 「配信のたびに再確認している」ことしか通らないようにする:
        // 1. 承認ありで購読・emit → 届く
        // 2. 同じバッファを(`refresh_bus_runtime` がやるのと同じように)
        //    承認なしへ書き換える
        // 3. 同じ購読のまま再度 emit → 届かない
        let bus_json = Arc::new(Mutex::new(granted_entry(true)));
        let (delivery_tx, delivery_rx) = std_mpsc::sync_channel(4);
        let (work_tx, work_rx) = std_mpsc::sync_channel::<PluginWork>(4);
        bus.subscribe("translator", "ed-state", "current-system", delivery_tx);
        spawn_bus_subscriber(
            test_manifest(),
            bus_json.clone(),
            delivery_rx,
            work_tx,
            Arc::new(AtomicBool::new(false)),
            DropCounters::new(),
        );

        bus.emit("ed-state", "current-system", b"Sol".to_vec())
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        match work_rx.try_recv() {
            Ok(PluginWork::Message(delivery)) => assert_eq!(delivery.payload, b"Sol".to_vec()),
            other => panic!("expected a granted delivery to reach the plugin queue, got {other:?}"),
        }

        // `Registry::refresh_bus_runtime` が実際に行うのと同じ操作: 同じ
        // 共有バッファの中身を書き換えるだけで、購読もサブスクライバタスクも
        // 再起動しない。
        *bus_json.lock().unwrap() = granted_entry(false);

        bus.emit("ed-state", "current-system", b"Jameson".to_vec())
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            work_rx.try_recv().is_err(),
            "revoking the grant on the same running subscriber must stop further \
             deliveries immediately, without re-subscribing"
        );
    }

    /// Regression test for a Critical bug found in review of this task: a
    /// live `spawn_bus_subscriber` task used to prevent its owning
    /// `tokio::Runtime` from ever finishing shutdown, because it blocked
    /// forever on `delivery_rx.recv()` and the `Sender` half (held by
    /// `edlr_driver_channel::Bus`'s subscription table, exactly as
    /// `Bus::subscribe` leaves it for the whole process's lifetime) never
    /// closes on its own. This reproduces the real shutdown shape end to
    /// end: build a `Runtime`, spawn a `spawn_bus_subscriber` task on it with
    /// its `delivery_tx` kept alive (the same shape `Bus::subscribe`
    /// produces), signal shutdown the same way
    /// `Registry::shutdown_bus_subscribers` does, then drop the `Runtime` and
    /// assert the drop completes within a bounded deadline.
    ///
    /// `Runtime::drop` is itself the call that can hang, so it's performed on
    /// a dedicated thread and observed through a bounded channel receive --
    /// this test fails via a timed-out assertion (not a real, unbounded
    /// process hang) if the fix regresses.
    #[test]
    fn spawn_bus_subscriber_lets_the_runtime_shut_down_once_signaled() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        let (delivery_tx, delivery_rx) = std_mpsc::sync_channel::<Delivery>(4);
        let (work_tx, _work_rx) = std_mpsc::sync_channel::<PluginWork>(4);
        let shutdown = Arc::new(AtomicBool::new(false));

        rt.block_on(async {
            spawn_bus_subscriber(
                test_manifest(),
                Arc::new(Mutex::new("{}".to_string())),
                delivery_rx,
                work_tx,
                shutdown.clone(),
                DropCounters::new(),
            );
        });

        // Keep the sender alive, exactly like `Bus`'s subscription table
        // does for as long as the plugin stays subscribed -- if this were
        // dropped instead, the subscriber would detect `Disconnected` and
        // exit on its own, and this test would no longer be exercising the
        // shutdown-signal path at all.
        let _keep_delivery_tx_alive = delivery_tx;

        // Signal shutdown *before* dropping the runtime, exactly as
        // `core/src/bin/edlr.rs` now calls
        // `registry.shutdown_bus_subscribers()` before letting `main` return
        // (and its implicit `Runtime` drop).
        shutdown.store(true, Ordering::Release);

        let (done_tx, done_rx) = std_mpsc::channel();
        thread::spawn(move || {
            drop(rt);
            let _ = done_tx.send(());
        });

        done_rx.recv_timeout(Duration::from_secs(5)).expect(
            "Runtime::drop must complete promptly once the shutdown flag was set \
             before the drop; a timeout here means the spawn_blocking task in \
             spawn_bus_subscriber is still blocking on delivery_rx.recv() instead \
             of observing the shutdown flag (this is the exact daemon shutdown \
             hang the fix addresses)",
        );
    }

    /// `next_action` はループの分岐を wasm 実体なしに検証するために切り
    /// 出した純粋関数。ここでは仕様の 4 分岐(タイムアウト+期限あり →
    /// `Fire`、タイムアウト+期限なし → `Idle`、`Ok` → `Handle`、
    /// `Disconnected` → `Exit`)と、「仕事が発火より優先される」こと
    /// (`Ok` は `due` の値に関わらず常に `Handle`)を確認する。
    mod next_action_tests {
        use super::*;

        fn some_work() -> PluginWork {
            PluginWork::Event(journal_event("Test"))
        }

        #[test]
        fn timeout_with_due_fires() {
            let action = next_action(
                Err(std_mpsc::RecvTimeoutError::Timeout),
                Some("flush".to_string()),
            );
            assert!(matches!(action, LoopAction::Fire(name) if name == "flush"));
        }

        #[test]
        fn timeout_without_due_is_idle() {
            let action = next_action(Err(std_mpsc::RecvTimeoutError::Timeout), None);
            assert!(matches!(action, LoopAction::Idle));
        }

        #[test]
        fn ok_work_takes_priority_over_a_pending_due_schedule() {
            // due が Some でも、recv が Ok なら仕事が優先されて Handle になる
            // (発火は Handle 後にループ側が改めて take_due するため、ここで
            // 落としても失われない)。
            let action = next_action(Ok(some_work()), Some("flush".to_string()));
            assert!(matches!(action, LoopAction::Handle(PluginWork::Event(_))));
        }

        #[test]
        fn disconnected_exits() {
            let action = next_action(Err(std_mpsc::RecvTimeoutError::Disconnected), None);
            assert!(matches!(action, LoopAction::Exit));
        }

        /// Task 5: `PluginWork::Stop` は `Handle` に混ぜず専用の `Stop` へ
        /// 振り分けられる(`due` が同時に立っていても優先度は変わらない --
        /// そもそも `Stop` はスレッド終了そのものなので優先度の概念が無い)。
        #[test]
        fn stop_work_is_routed_to_loop_action_stop() {
            let action = next_action(Ok(PluginWork::Stop), None);
            assert!(matches!(action, LoopAction::Stop));

            let action_with_due = next_action(Ok(PluginWork::Stop), Some("flush".to_string()));
            assert!(matches!(action_with_due, LoopAction::Stop));
        }
    }

    /// `deadline_verdict` は `handle_call_result!` から切り出した判定のみの
    /// 純粋関数。境界(`CALL_DEADLINE_STRIKES` 未満 / 到達 / 超過)を確認する。
    mod deadline_verdict_tests {
        use super::*;

        #[test]
        fn below_the_limit_restarts() {
            let verdict = deadline_verdict(CALL_DEADLINE_STRIKES - 1);
            assert_eq!(verdict, DeadlineVerdict::Restart);
        }

        #[test]
        fn at_the_limit_gives_up() {
            let verdict = deadline_verdict(CALL_DEADLINE_STRIKES);
            assert_eq!(verdict, DeadlineVerdict::GiveUp);
        }

        #[test]
        fn above_the_limit_gives_up() {
            let verdict = deadline_verdict(CALL_DEADLINE_STRIKES + 1);
            assert_eq!(verdict, DeadlineVerdict::GiveUp);
        }
    }
}

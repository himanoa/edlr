//! `crate::plugin::registry::Registry` が持っていたプラグインスレッドの監督
//! (登録・schedule view 公開・drop counter 公開・shutdown)を抽出したもの
//! (Phase 4 タスク3、move-only)。
//!
//! 具象のまま(trait 化しない): モックしたい consumer が未実証
//! (`.claude/rules/trait-di.md` 参照)。`Registry` は本体を `supervisor` field
//! として持ち、全メソッドを委譲する(公開シグネチャ・crate 内部シグネチャは
//! 不変)。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::plugin::dropped::{DropCounters, DroppedCounts};
use crate::plugin::runner::PluginWork;
use crate::schedule::ScheduleView;

/// `ThreadSupervisor::shutdown_all` が 1 プラグインの `JoinHandle` を待つ上限。
/// `edlr_config::PLUGIN_ON_STOP_GRACE_SECS` のドキュメントコメント参照
/// (= `PluginInstance::CALL_DEADLINE` + 余裕)。
const PLUGIN_STOP_JOIN_TIMEOUT: Duration =
    Duration::from_secs(edlr_config::PLUGIN_ON_STOP_GRACE_SECS);

/// `shutdown_all` が `JoinHandle::is_finished()` をポーリングする間隔。
/// std に `join_timeout` が無いための代替手段(タスクブリーフ参照)。値自体に
/// 強い意味は無く、`PLUGIN_STOP_JOIN_TIMEOUT` に対して十分細かく、かつ
/// 無駄なビジーポーリングにならない程度、という程度の選択。
const PLUGIN_STOP_JOIN_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// `ThreadSupervisor::plugin_threads` の 1 エントリ。`shutdown_all` 専用。
struct PluginThreadHandle {
    work_tx: std_mpsc::SyncSender<PluginWork>,
    handle: thread::JoinHandle<()>,
    /// `Stop` のアウトオブバンド経路。ランナーループが毎周期(ワークキューを
    /// 読む前に)確認するので、有界キューに積まれた先行ワークを追い越せる。
    /// `work_tx` への `PluginWork::Stop` 送信は、待ちに入っているスレッドを
    /// 起こすためのもの(`shutdown_all` 参照)。
    stop_flag: Arc<AtomicBool>,
}

/// プラグイン専用スレッドの登録・監督(停止・schedule view・drop counter の
/// 公開)を担う。`crate::plugin::registry::Registry` が 1 つ保持する。
pub(crate) struct ThreadSupervisor {
    /// `crate::plugin::runner::spawn_bus_subscriber` の各インスタンスが共有
    /// する shutdown フラグ。`shutdown_bus_subscribers` が立て、各購読タスクは
    /// `BUS_SUBSCRIBER_SHUTDOWN_POLL_INTERVAL` ごとにこれを見に行く。詳しい
    /// 経緯は `Registry::shutdown_bus_subscribers` のドキュメントコメント参照。
    bus_subscriber_shutdown: Arc<AtomicBool>,
    /// Running として起動した各プラグインの `work_tx`(の複製)と専用
    /// スレッドの `JoinHandle` を id で引けるようにしたマップ
    /// (`shutdown_all` 用)。`register_thread` が `runner::start_plugins` から
    /// 呼ばれて登録する。Disabled で終わったプラグイン(init 失敗など)は
    /// 登録しない -- そのスレッドは既に return 済みで `work_rx` を読んでい
    /// ないため、`Stop` を送っても意味が無い(`register_thread` のドキュメント
    /// コメント参照)。
    plugin_threads: Arc<Mutex<HashMap<String, PluginThreadHandle>>>,
    /// 各プラグインのランナーループが公開する「実際の次回発火時刻」を id で
    /// 引けるようにしたマップ(`plugins/list` 用)。`register_schedule_view` が
    /// プラグイン専用スレッド自身から呼ばれて登録する。
    ///
    /// これが無かった頃、`Registry::build_schedule_infos` は RPC のたびに
    /// `ScheduleState` を作り直していたため、interval の `next` は常に
    /// 「now + interval」になり、スレッドの実際の発火時点と無関係だった
    /// (UI のカウントダウンが意味を持たなかった)。
    schedule_views: Arc<Mutex<HashMap<String, ScheduleView>>>,
    /// 各プラグインの「作業キュー満杯で捨てた件数」を id で引けるようにした
    /// マップ(`plugins/list` 用)。`register_drop_counters` が
    /// `runner::start_plugins` から呼ばれて登録する。
    ///
    /// 取りこぼしを黙って捨てるのは設計判断(遅いだけのプラグインを殺さない)
    /// だが、何がどれだけ失われているか分からないままではキュー容量の
    /// チューニングが当て推量になる。`plugin::dropped` のモジュールドキュメント
    /// を参照。
    drop_counters: Arc<Mutex<HashMap<String, Arc<DropCounters>>>>,
}

impl ThreadSupervisor {
    pub(crate) fn new() -> Self {
        Self {
            bus_subscriber_shutdown: Arc::new(AtomicBool::new(false)),
            plugin_threads: Arc::new(Mutex::new(HashMap::new())),
            schedule_views: Arc::new(Mutex::new(HashMap::new())),
            drop_counters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// `runner::start_plugins`(実際には `load_and_run_plugin`)が Running と
    /// して起動したプラグインの `work_tx` の複製と専用スレッドの
    /// `JoinHandle` を登録する。`shutdown_all` がこれを引いて
    /// `PluginWork::Stop` を送り、スレッドの終了を待つ(crate 内部専用)。
    ///
    /// Disabled で終わったプラグイン(`load` や `init` の失敗)はこれを
    /// 呼ばない -- そのスレッドは既に `ready_tx` へ結果を送って return 済み
    /// で `work_rx` を二度と読まないため、登録しても `Stop` が届かないだけ
    /// でなく、`shutdown_all` 側の join 待ちを無駄に長引かせる理由も無い
    /// (スレッド自体は既に終了しているので `is_finished()` は直ちに `true`
    /// になり実害は無いが、そもそも意味のある登録ではないため呼び出し元
    /// [`runner::load_and_run_plugin`] は Running のときだけ呼ぶ)。
    pub(crate) fn register_thread(
        &self,
        id: &str,
        work_tx: std_mpsc::SyncSender<PluginWork>,
        handle: thread::JoinHandle<()>,
        stop_flag: Arc<AtomicBool>,
    ) {
        self.plugin_threads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                id.to_string(),
                PluginThreadHandle {
                    work_tx,
                    handle,
                    stop_flag,
                },
            );
    }

    /// プラグイン専用スレッドが、自分のスケジュール状態を公開する窓口を
    /// 登録する(`plugins/list` から読まれる)。スレッド自身がループへ入る
    /// 直前に呼ぶ(`runner::run_plugin_thread`)。
    ///
    /// スケジュールを 1 件も宣言していないプラグインは呼ばない。
    pub(crate) fn register_schedule_view(&self, id: &str, view: ScheduleView) {
        self.schedule_views
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_string(), view);
    }

    /// プラグインの取りこぼしカウンタを登録する(`plugins/list` から読まれる)。
    /// 購読タスクを起動する `runner::start_plugins` が呼ぶ。
    pub(crate) fn register_drop_counters(&self, id: &str, counters: Arc<DropCounters>) {
        self.drop_counters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_string(), counters);
    }

    /// `id` のプラグインの取りこぼし件数。カウンタが未登録(Disabled で
    /// 購読タスクが起動していない)なら 0 件。
    pub(crate) fn dropped_counts(&self, id: &str) -> DroppedCounts {
        self.drop_counters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .map(|counters| counters.snapshot())
            .unwrap_or_default()
    }

    /// `id` のプラグインが `register_schedule_view` で公開している、各
    /// スケジュール名 → 実際の次回発火時刻。未公開(起動途中/Disabled)なら
    /// 空(`Registry::build_schedule_infos` の推定値フォールバックを参照)。
    pub(crate) fn published_schedule(
        &self,
        id: &str,
    ) -> HashMap<String, chrono::DateTime<chrono::Local>> {
        self.schedule_views
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .map(|view| view.snapshot().into_iter().collect())
            .unwrap_or_default()
    }

    /// Running な全プラグインへ `PluginWork::Stop` を送り、それぞれの専用
    /// スレッドの終了を(1 件あたり `PLUGIN_STOP_JOIN_TIMEOUT` を上限に)
    /// 待つ。デーモンの正常終了シーケンス専用(`core/src/bin/edlr.rs` を
    /// 参照)。
    ///
    /// **`shutdown_bus_subscribers` より前に呼ぶこと**: on-stop の中で
    /// プラグインがまだバス経由の publish を行いたいかもしれない
    /// (`HostCtx::check_bus` はこの時点でまだ `shutdown_bus_subscribers` が
    /// 立てるフラグを見ないので、bus 自体はまだ生きている)。先に
    /// バス購読側を止めてしまう理由が無い以上、後始末の順序として自然な方
    /// (プラグインに最後の一仕事をさせてから、購読タスクを畳む)にしてある。
    ///
    /// **`Stop` はワークキューを追い越す**: 停止の合図は 2 経路ある。
    ///
    /// 1. `PluginThreadHandle::stop_flag`(主経路)-- ランナーループが毎周期、
    ///    ワークキューを読む**前**に確認する。これにより、有界 64 スロットの
    ///    キューに積まれた先行ワークを `Stop` が追い越せる
    /// 2. `work_tx` への `PluginWork::Stop`(補助)-- キューが空で
    ///    `recv_timeout` に入っているスレッドを直ちに起こすためだけのもの。
    ///    満杯なら `try_send` は失敗するが、その場合スレッドは待ちに入って
    ///    おらず、次の周回でフラグを見るので問題ない
    ///
    /// かつては 2 の経路しか無かったため、プラグインスレッドは先行する全ワーク
    /// を消化するまで `call_on_stop` へ到達できず(最悪 63 件 x
    /// `CALL_DEADLINE` 2 秒 ≒ 126 秒)、`PLUGIN_STOP_JOIN_TIMEOUT` しか待たない
    /// この関数から見ると on-stop の flush は事実上スキップされていた。
    ///
    /// 送信側が既に切断されている(スレッドが trap などで既に終了済み)場合も
    /// 何もしない -- いずれの場合も後続の join は行う(トラップ済みなら即座に
    /// `is_finished()` が `true` になるだけ)。
    ///
    /// **停止要求は全件へ先に送り、join は 1 つの共有デッドラインで待つ**:
    /// プラグインの on-stop は互いに独立なので、直列に待つ理由が無い。まず
    /// 全プラグインへ停止を伝えてから、全スレッドの `is_finished()` を
    /// `PLUGIN_STOP_JOIN_POLL_INTERVAL` 間隔でまとめてポーリングする。
    /// これにより最悪ケースが `N × PLUGIN_STOP_JOIN_TIMEOUT` から
    /// `1 × PLUGIN_STOP_JOIN_TIMEOUT` に縮む(Tauri 側の
    /// `daemon::STOP_GRACE` はこの最悪ケースを見積もって決まっているので、
    /// あちらの定数も一桁下げられる)。
    ///
    /// 標準ライブラリの `JoinHandle` には `join_timeout` が無いためポーリング
    /// で代替している。デッドラインを過ぎても終わっていないスレッドは諦める
    /// (warn ログを出して join しない -- プロセス自体はこの直後に終了するので
    /// 待ちきれなかったスレッドはプロセス終了と共に消える。ハングしたプラグイン
    /// のために shutdown シーケンス全体を止めるべきではない)。
    pub(crate) fn shutdown_all(&self) {
        let handles: Vec<(String, PluginThreadHandle)> = self
            .plugin_threads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .collect();

        // 第 1 段: 全プラグインへ停止を伝える。ここでは一切待たない。
        for (_, thread_handle) in &handles {
            // 主経路: フラグを先に立てる。ランナーループは次の周回で、
            // キューを読む前にこれを見て on-stop へ進む。
            thread_handle.stop_flag.store(true, Ordering::SeqCst);

            // 補助経路: `recv_timeout` で待ちに入っているスレッドを起こす。
            match thread_handle.work_tx.try_send(PluginWork::Stop) {
                Ok(()) => {}
                Err(std_mpsc::TrySendError::Full(_)) => {
                    // キューが満杯 = スレッドは待ちに入っていないので、
                    // 起こす必要が無い。次の周回でフラグを見る。
                }
                Err(std_mpsc::TrySendError::Disconnected(_)) => {
                    // プラグインスレッドは既に(trap などで)終了済み。
                }
            }
        }

        // 第 2 段: 1 つの共有デッドラインで、全スレッドの終了を並行に待つ。
        let deadline = Instant::now() + PLUGIN_STOP_JOIN_TIMEOUT;
        while handles.iter().any(|(_, h)| !h.handle.is_finished()) && Instant::now() < deadline {
            thread::sleep(PLUGIN_STOP_JOIN_POLL_INTERVAL);
        }

        for (id, thread_handle) in handles {
            if thread_handle.handle.is_finished() {
                let _ = thread_handle.handle.join();
            } else {
                tracing::warn!(
                    plugin_id = %id,
                    "plugin thread did not exit within {PLUGIN_STOP_JOIN_TIMEOUT:?} of the \
                     stop signal; abandoning join (the process is exiting regardless)"
                );
            }
        }
    }

    /// `crate::plugin::runner::spawn_bus_subscriber` が共有する shutdown
    /// フラグの `Arc` を返す。`runner.rs` がプラグインごとの購読タスクを
    /// 起動する際にこれを渡す(crate 内部専用)。
    pub(crate) fn shutdown_flag(&self) -> Arc<AtomicBool> {
        self.bus_subscriber_shutdown.clone()
    }

    /// 全プラグインの `spawn_bus_subscriber` タスクへ shutdown を通知する
    /// (デーモン shutdown 用)。
    ///
    /// **これを `main()` が戻る(= `Runtime::drop` される)前に呼ばないと
    /// デーモンは正常終了できない。** `[[bus]] subscribe` を宣言するプラグ
    /// インが 1 つでもあれば、その `spawn_bus_subscriber` タスクは
    /// `tokio::task::spawn_blocking` の中でブロッキング受信をしており、送信側
    /// (`Bus::subscribe` に渡した `Sender<Delivery>`)はそのプラグインの購読
    /// エントリとして `Bus` の購読表に居座り続けるため、明示的に知らせない
    /// 限り自然には終了しない。`Runtime::drop` は実行中の `spawn_blocking`
    /// タスクの完了を待つため、これを呼ばずに `main` を抜けようとすると
    /// **プロセスが `Runtime::drop` の中で無期限にハングする**(実際に踏んだ
    /// Critical バグ。詳細は `crate::plugin::runner::
    /// BUS_SUBSCRIBER_SHUTDOWN_POLL_INTERVAL` のドキュメントコメント参照)。
    ///
    /// `Registry::stop_all_sidecars` と同じくデーモンの shutdown シーケンス
    /// の一部として呼ぶことを想定している(`core/src/bin/edlr.rs` を参照)。
    /// フラグを立てるだけの軽い呼び出しなので、`stop_all_sidecars` のように
    /// `spawn_blocking` へ逃がす必要はない。
    pub(crate) fn shutdown_bus_subscribers(&self) {
        self.bus_subscriber_shutdown
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Clone for ThreadSupervisor {
    fn clone(&self) -> Self {
        Self {
            bus_subscriber_shutdown: self.bus_subscriber_shutdown.clone(),
            plugin_threads: self.plugin_threads.clone(),
            schedule_views: self.schedule_views.clone(),
            drop_counters: self.drop_counters.clone(),
        }
    }
}

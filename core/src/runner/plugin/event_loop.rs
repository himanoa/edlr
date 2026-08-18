//! プラグイン専用スレッドの本体(`run_plugin_thread`)と、そのループが
//! 「次に何をするか」を決める純関数群(`next_action`/`LoopAction`、
//! `deadline_verdict`)。すべての wasm 呼び出しはこのスレッド上でのみ
//! 発生する(親モジュールのドキュメントコメント参照)。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use edlr_driver_channel::Bus;

use crate::event::Event;
use crate::host::plugin::{HostCtx, PluginCallError, PluginHost, PluginInstance, PluginJobs};
use crate::manifest::Manifest;
use crate::profiler::{self, CallKind, GaugeSource, Profiler, Sample, Subject};
use crate::registry::plugin::{PluginState, Registry};
use crate::runtime::dropped::DropCounters;
use crate::schedule::store::ScheduleStore;
use crate::schedule::{Clock, ScheduleState, ScheduleView};

use super::queue::{PluginWorkReceiver, PluginWorkSender};
use super::PluginWork;

/// スケジュールが 1 件も無いプラグイン向けのフォールバックタイムアウト。
///
/// `ScheduleState::until_next` が `None`(スケジュール無し)を返す間は、
/// このタイムアウトで `work_rx.recv_timeout` をブロックする。値そのものに
/// 強い意味は無く、単に「スケジュール無しプラグインの挙動を今日と同一に
/// 保つ」ための実質無限大の代わり(`recv_timeout` は無限待ちを表現できない
/// ため)。タイムアウトのたびに `take_due` は必ず `None` を返す
/// (スケジュールが無いので)ので `LoopAction::Idle` になり、単に
/// もう一度待ち直すだけで観測できる差は無い。
const SCHEDULE_LESS_FALLBACK_TIMEOUT: Duration = Duration::from_secs(3600);

/// 連続して何回 `PluginInstance::CALL_DEADLINE` を使い切ったら、プラグインを
/// `Disabled` にするか。
///
/// 期限超過は trap(壊れたプラグイン)と違い、プラグイン作者の管理下にない
/// 事情でも起きる: `driver-http.send` の相手ホストが応答しない、サスペンド
/// からのレジューム直後に処理が詰まる、など。1 回の超過で恒久 `Disabled` に
/// すると、一時的なネットワーク停滞でプラグインがデーモン再起動まで
/// 二度と動かなくなる(実際にそうなっていた)。
///
/// 一方で無制限に許すと、毎回 2 秒使い切るプラグインがワークキューを
/// 詰まらせ続ける。3 回という値に強い根拠は無く、「単発の停滞は見逃し、
/// 恒常的な遅さは捕まえる」程度の線。**連続**回数なので、1 回でも成功すれば
/// 0 に戻る。
pub(super) const CALL_DEADLINE_STRIKES: u32 = 3;

/// 期限超過が strikes 回連続したときの扱い。判定だけを純関数にし、
/// 制御フロー(continue/break)と reason 文字列の組み立ては
/// `handle_call_result!` マクロに残す。
#[derive(Debug, PartialEq)]
pub(super) enum DeadlineVerdict {
    Restart,
    GiveUp,
}

pub(super) fn deadline_verdict(strikes: u32) -> DeadlineVerdict {
    if strikes >= CALL_DEADLINE_STRIKES {
        DeadlineVerdict::GiveUp
    } else {
        DeadlineVerdict::Restart
    }
}

/// `PluginWork::JobComplete` を届けるか捨てるかの判定(純関数)。
///
/// インスタンス再作成(deadline 復帰)で `PluginJobs::bump_generation` が
/// 世代を進めるため、submit 時点の世代と現在の世代が一致しない完了は
/// 「旧インスタンスのジョブ」であり、状態を失った新インスタンスへは
/// 届けない(issue-sizx 決定 6)。
#[derive(Debug, Clone, Copy, PartialEq)]
enum JobCompletionVerdict {
    Deliver,
    DropStale,
}

fn job_completion_verdict(job_generation: u64, current_generation: u64) -> JobCompletionVerdict {
    if job_generation == current_generation {
        JobCompletionVerdict::Deliver
    } else {
        JobCompletionVerdict::DropStale
    }
}

/// プラグイン専用スレッドの本体。`load` → `call_init` → イベントループを
/// 直列に実行する。すべての wasm 呼び出しはこのスレッド上でのみ発生する。
#[allow(clippy::too_many_arguments)]
pub(super) fn run_plugin_thread(
    host: Arc<PluginHost>,
    manifest: Manifest,
    entry_path: PathBuf,
    settings_json: Arc<Mutex<String>>,
    capabilities_json: Arc<Mutex<String>>,
    sidecars_json: Arc<Mutex<String>>,
    filesystem_json: Arc<Mutex<String>>,
    bus_json: Arc<Mutex<String>>,
    bus: Bus,
    registry: Registry,
    work_rx: PluginWorkReceiver,
    work_tx: PluginWorkSender,
    ready_tx: std_mpsc::Sender<PluginState>,
    stop_flag: Arc<AtomicBool>,
    schedule_store: Arc<ScheduleStore>,
    memory_gauge: Arc<AtomicU64>,
    profiler: Profiler,
    drops: Arc<DropCounters>,
) {
    // submit 系ジョブの共有状態。インスタンス再作成をまたいで同じ `Arc` を
    // 各 `HostCtx` へ配る(`PluginJobs` のドキュメントコメント参照)。
    let jobs = PluginJobs::new();

    // trap 時にこのプラグインの購読を `Bus` の購読表から取り除くために手元に
    // 残しておく(`ctx` へ渡す方は `HostCtx::new` に move する)。
    // `edlr_driver_channel::Bus::unsubscribe_plugin` のドキュメントコメント
    // 参照: 呼ばないと、このプラグインが二度と読まない購読エントリが
    // プロセスの生存期間中ずっと購読表に残り続ける。
    let bus_for_unsubscribe = bus.clone();

    // インスタンスの生成をクロージャに切り出しておく。期限超過からの復帰
    // (下記 `handle_call_result!`)で作り直す必要があるため。`HostCtx` が
    // 束ねているのは共有バッファ(`Arc<Mutex<String>>`)とドライバの `Arc`
    // だけなので、作り直しても承認・設定の状態は失われない。
    let load_instance = || -> Result<PluginInstance, String> {
        let ctx = HostCtx::new(
            manifest.id.clone(),
            settings_json.clone(),
            capabilities_json.clone(),
            sidecars_json.clone(),
            filesystem_json.clone(),
            bus_json.clone(),
            bus.clone(),
            host.http_driver(),
            host.process_driver(),
            host.fs_driver(),
            work_tx.clone(),
            jobs.clone(),
        );
        // `memory_gauge` は再作成(期限超過からの復帰)をまたいで同じ `Arc`
        // を使い回す。プロファイラの gauge 登録もこの `Arc` を指すので、
        // インスタンスが作り直されても登録し直す必要が無い。
        let mut instance = host
            .load(&entry_path, ctx, memory_gauge.clone())
            .map_err(|e| format!("failed to load plugin component: {e}"))?;
        instance
            .call_init()
            .map_err(|e| format!("init() failed: {e}"))?;
        Ok(instance)
    };

    let mut instance = match load_instance() {
        Ok(instance) => instance,
        Err(reason) => {
            let _ = ready_tx.send(PluginState::Disabled { reason });
            return;
        }
    };

    if ready_tx.send(PluginState::Running).is_err() {
        // start_plugins 側が既に受信を諦めている(通常起こらない)。
        return;
    }

    // gauge 登録は「Running を送った直後・ループに入る前」にこのスレッド
    // 自身が行う(呼び出し元の `start_plugins` 側の親スレッドではない)。
    // register/unregister を同じスレッド内で完結させることで、Running 送信
    // 直後にこのスレッドが死んでも「登録だけ残る」順序が起こらないようにする
    // (対応する unregister はこの関数の末尾)。
    // **万一この unregister が漏れると**、死んだプラグイン id の gauge が
    // 毎秒の走査で拾われ続け、queue_len/memory_bytes が更新されない stale な
    // サンプルを永遠に吐き続ける(次のデーモン再起動まで気付きにくい)。
    profiler.register_gauge(GaugeSource {
        subject: Subject::Plugin,
        id: manifest.id.clone(),
        queue: work_tx.len_probe(),
        drops: Some(drops.clone()),
        memory_bytes: memory_gauge.clone(),
    });

    // 時計はここ(ループ側)でのみ読む。`ScheduleState` 自身は時刻を
    // 引数でしか受け取らない(`schedule` モジュールのドキュメントコメント
    // 参照、テストで時刻を固定するため)。interval は `Clock` の単調時計、
    // cron は壁時計で評価される。
    // `catch-up = true` を宣言したスケジュールについては、デーモンが動いて
    // いなかった間に過ぎた定刻を 1 回だけ追い掛ける(`new_with_catch_up`)。
    let mut schedule_state = ScheduleState::new_with_catch_up(
        &manifest.schedules,
        Clock::now(),
        &schedule_store.last_fires(&manifest.id),
    );

    // このスレッドが実際に予定している発火時刻を `plugins/list` から読める
    // ようにする。`ScheduleState` 自体はここから出さない(`take_due` が状態を
    // 進める可変操作なので、スレッドをまたいで共有すると RPC と発火が競合
    // する)。宣言が無いプラグインでは窓口も作らない。
    let schedule_view = ScheduleView::default();
    if !manifest.schedules.is_empty() {
        registry.register_schedule_view(&manifest.id, schedule_view.clone());
    }

    // 1 回の `Err(reason)` を「warn ログ + disable + unsubscribe + ループ
    // 脱出」へ合流させるためのマクロ。`Handle` 後の wasm 呼び出し・`Fire`
    // 自身・`Fire` 後の追い発火のいずれで失敗しても同じ扱いにする。
    macro_rules! disable_and_break {
        ($reason:expr) => {{
            let reason = $reason;
            // 恒久 Disabled(trap = クラッシュ、または期限超過の諦め)は
            // error。以後このプラグインの仕事は全部止まるので、warn だと
            // ログ画面で埋もれて気付けない。一時的な遅さからの再起動
            // (handle_call_result! の restart 分岐)は従来どおり warn。
            tracing::error!(
                plugin_id = %manifest.id,
                "disabling plugin: {reason}"
            );
            registry.set_disabled(&manifest.id, reason);
            bus_for_unsubscribe.unsubscribe_plugin(&manifest.id);
            break;
        }};
    }

    // 連続して `CALL_DEADLINE` を使い切った回数。成功するたびに 0 に戻る。
    let mut deadline_strikes: u32 = 0;

    // wasm 呼び出しの結果を処理する。**期限超過と trap を区別する**のが要点:
    //
    // - trap(壊れたプラグイン)は次に呼んでも同じ結果なので、従来どおり
    //   1 回で恒久 `Disabled` にする
    // - 期限超過は「このプラグインは遅かった」でしかなく、原因はプラグイン
    //   作者の管理下にないこと(応答しない HTTP ホスト、レジューム直後の
    //   詰まり)でありうる。`CALL_DEADLINE_STRIKES` 回**連続**して超過した
    //   ときだけ諦める
    //
    // かつては両者を区別せず、一時的なネットワーク停滞でも 1 回で恒久
    // `Disabled` になり、ログには "on-event call failed" しか残らなかった。
    //
    // **期限超過からの復帰にはインスタンスの作り直しが要る**: epoch 割り込み
    // でトラップしたコンポーネントインスタンスは wasmtime に毒扱いされ、
    // 以後の呼び出しはすべて "cannot enter component instance" で失敗する。
    // そのため同じインスタンスを再試行しても意味が無く、`load_instance()` で
    // 新しく作り直して `init` からやり直す。副作用として、プラグインが
    // wasm 線形メモリ上に持っていた状態(未送信キューなど)は失われる --
    // ただし恒久 `Disabled` は同じものを失ったうえで以後の仕事も全部止める
    // ので、作り直す方が厳密に良い。
    macro_rules! handle_call_result {
        ($result:expr) => {{
            match $result {
                Ok(()) => {
                    deadline_strikes = 0;
                }
                Err(e) if e.is_deadline_exceeded() => {
                    deadline_strikes += 1;
                    if deadline_verdict(deadline_strikes) == DeadlineVerdict::GiveUp {
                        disable_and_break!(format!(
                            "{e} on {deadline_strikes} consecutive calls; the plugin is \
                             persistently too slow (a blocked host call, or work that does \
                             not fit the deadline)"
                        ));
                    }
                    // 再作成の前に世代を進める: 旧インスタンスが submit した
                    // ジョブの完了(`PluginWork::JobComplete`)は、wasm 線形
                    // メモリごと状態を失った新インスタンスには届けない
                    // (`PluginJobs::generation` のドキュメントコメント参照)。
                    jobs.bump_generation();
                    match load_instance() {
                        Ok(fresh) => {
                            tracing::warn!(
                                plugin_id = %manifest.id,
                                strikes = deadline_strikes,
                                limit = CALL_DEADLINE_STRIKES,
                                "{e}; restarted the plugin instead of disabling it \
                                 (transient slowness is not a fault)"
                            );
                            instance = fresh;
                        }
                        Err(reason) => disable_and_break!(format!(
                            "{e}, and the plugin could not be restarted: {reason}"
                        )),
                    }
                    continue;
                }
                Err(e) => disable_and_break!(e.to_string()),
            }
        }};
    }

    // `LoopAction::Stop` と、下のアウトオブバンド経路の両方から使う on-stop。
    // もう止まる以上、失敗しても disable する意味が無いので warn ログのみに
    // 留め、trap 用の `disable_and_break!` は使わない。
    macro_rules! stop_and_break {
        () => {{
            if let Err(e) = instance.call_on_stop() {
                tracing::warn!(
                    plugin_id = %manifest.id,
                    "on-stop call failed during shutdown: {e}"
                );
            }
            break;
        }};
    }

    loop {
        // **`Stop` はワークキューを追い越す**: `Registry::shutdown_plugins` は
        // このフラグを立ててから、待ちに入っているスレッドを起こすために
        // `PluginWork::Stop` も送る。フラグをここで(キューを読む前に)
        // 見ることで、`Stop` が有界 64 スロットのキューに並んだ先行ワークを
        // 追い越せる。
        //
        // かつて `Stop` はイベント/バス配信と同じキューに `try_send` される
        // だけだったため、プラグインスレッドは先行する全ワークを消化するまで
        // `call_on_stop` に到達できず(最悪 63 件 x `CALL_DEADLINE` 2 秒
        // ≒ 126 秒)、5 秒しか待たない `shutdown_plugins` から見ると on-stop の
        // flush は事実上スキップされていた。積み残したワークは捨てる -- どのみち
        // プロセスはこの直後に終了するので、最後の一仕事(flush)を優先する。
        if stop_flag.load(Ordering::SeqCst) {
            stop_and_break!();
        }

        // 待ちに入る直前に、いまの状態を公開する。発火(`take_due`/
        // `fire_all_due`)は必ずこのループを一周してここへ戻ってくるので、
        // 状態が進むたびに公開値も更新される。
        let clock = Clock::now();
        schedule_view.publish(&schedule_state, clock);
        let timeout = schedule_state
            .until_next(clock)
            .unwrap_or(SCHEDULE_LESS_FALLBACK_TIMEOUT);
        let recv_result = work_rx.recv_timeout(timeout);
        // due は `Timeout` のときだけ取り出す。`Ok(work)` のときにも
        // 呼んでしまうと(`next_action` がその値を無視するとしても)
        // `take_due` 自身が呼び出しのたびに状態を進めてしまうため、
        // 期限切れの発火を 1 回捨ててしまう(仕事優先の仕様に反する)。
        let due = match &recv_result {
            Err(std_mpsc::RecvTimeoutError::Timeout) => schedule_state.take_due(Clock::now()),
            _ => None,
        };

        match next_action(recv_result, due) {
            LoopAction::Handle(work) => {
                let started = Instant::now();
                // `JobComplete` の verdict はここで 1 回だけ判定する: match
                // アーム内でも使うし、下の「実際に wasm を呼んだか」の判定
                // (`DropStale` は `Ok(())` を返すだけで wasm を呼ばない)にも
                // 使う。呼んでいない仕事を計測サンプルとして記録すると、
                // duration≈0 の偽の `Ok` が Logs/Profiler に混じる。
                let job_verdict = match &work {
                    PluginWork::JobComplete { generation, .. } => {
                        Some(job_completion_verdict(*generation, jobs.current_generation()))
                    }
                    _ => None,
                };
                let result = match &work {
                    PluginWork::Event(event) => {
                        let (kind, timestamp, name, payload_json, replay) = event_params(event);
                        instance.call_on_event(
                            kind,
                            timestamp.as_deref(),
                            name.as_deref(),
                            &payload_json,
                            replay,
                        )
                    }
                    PluginWork::Message(delivery) => instance.call_on_message(
                        &delivery.driver_id,
                        &delivery.topic,
                        &delivery.payload,
                    ),
                    PluginWork::JobComplete { job_id, result_json, .. } => {
                        match job_verdict.expect("job_verdict is Some for PluginWork::JobComplete") {
                            JobCompletionVerdict::Deliver => {
                                instance.call_on_job_complete(*job_id, result_json)
                            }
                            JobCompletionVerdict::DropStale => {
                                // 旧世代のインスタンスが submit したジョブ。新
                                // インスタンスはそのジョブを知らない(wasm 線形
                                // メモリごと状態を失っている)ので、呼ばずに捨てる。
                                tracing::debug!(
                                    plugin_id = %manifest.id,
                                    job_id,
                                    "dropping a job completion from a previous instance generation"
                                );
                                Ok(())
                            }
                        }
                    }
                    // `next_action` は `PluginWork::Stop` を `LoopAction::Handle`
                    // ではなく専用の `LoopAction::Stop` に振り分けるので、ここに
                    // 来ることはない。
                    PluginWork::Stop => unreachable!(
                        "next_action routes PluginWork::Stop to LoopAction::Stop, not Handle"
                    ),
                };
                // 実際に wasm を呼んだ呼び出しだけを記録する(`DropStale` は
                // 呼んでいないので記録しない、driver 側の `continue` と対称)。
                if !matches!(job_verdict, Some(JobCompletionVerdict::DropStale)) {
                    profiler.record(Sample::Call(profiler::call_sample(
                        Subject::Plugin,
                        &manifest.id,
                        call_kind_of(&work),
                        &detail_of(&work),
                        started,
                        &result,
                        profiler::now_ts(),
                    )));
                }
                handle_call_result!(result);
                handle_call_result!(fire_all_due(
                    &mut schedule_state,
                    &mut instance,
                    &manifest.id,
                    &schedule_store,
                    &profiler,
                ));
            }
            LoopAction::Fire(name) => {
                let started = Instant::now();
                let result = instance.call_on_schedule(&name);
                profiler.record(Sample::Call(profiler::call_sample(
                    Subject::Plugin,
                    &manifest.id,
                    CallKind::OnSchedule,
                    &name,
                    started,
                    &result,
                    profiler::now_ts(),
                )));
                handle_call_result!(result);
                record_fire(&schedule_state, &manifest.id, &name, &schedule_store);
                handle_call_result!(fire_all_due(
                    &mut schedule_state,
                    &mut instance,
                    &manifest.id,
                    &schedule_store,
                    &profiler,
                ));
            }
            LoopAction::Idle => {}
            LoopAction::Exit => break,
            // デーモンの正常終了シーケンス(`Registry::shutdown_plugins`)から
            // 送られた `PluginWork::Stop`。キューが空で待ちに入っていた場合は
            // 上のフラグ検査より先にこちらへ届く(送信はスレッドを起こすため)。
            LoopAction::Stop => stop_and_break!(),
        }
    }

    // ループに入った後の全ての `break`(trap による disable、`Stop`、
    // 送信側全断)がここへ合流する。ループより前の 2 箇所の早期 `return`
    // (load/init 失敗)はここを通らないが、どちらも上の `register_gauge`
    // より前(`ready_tx.send(Running)` にすら届いていない)なので、登録され
    // ていないものを解除し損ねる心配はない。gauge は「登録している場合だけ」
    // 意味を持つ操作なので、未登録でも無条件に呼んで構わない
    // (`Profiler::unregister_gauge` は無ければ何もしない)。
    profiler.unregister_gauge(Subject::Plugin, &manifest.id);
}

/// 直前の wasm 呼び出し(ブロックしうる)の間に期限を過ぎたスケジュールを、
/// 1 件ずつ・各名前につき最大 1 回ずつ発火する。
///
/// `ScheduleState::take_due` は 1 回の呼び出しで最大 1 件しか返さない仕様
/// なので、`None` になるまでループして呼び切ることで「同時に複数期限切れ
/// でも、名前ごとにちょうど 1 回」という仕様(タスク仕様の「ブロック中に
/// 過ぎた発火は後で 1 回」)を満たす。
fn fire_all_due(
    state: &mut ScheduleState,
    instance: &mut PluginInstance,
    plugin_id: &str,
    schedule_store: &ScheduleStore,
    profiler: &Profiler,
) -> Result<(), PluginCallError> {
    loop {
        let Some(name) = state.take_due(Clock::now()) else {
            return Ok(());
        };
        let started = Instant::now();
        let result = instance.call_on_schedule(&name);
        profiler.record(Sample::Call(profiler::call_sample(
            Subject::Plugin,
            plugin_id,
            CallKind::OnSchedule,
            &name,
            started,
            &result,
            profiler::now_ts(),
        )));
        result?;
        record_fire(state, plugin_id, &name, schedule_store);
    }
}

/// 発火した時刻を永続化する。`catch-up` を宣言したスケジュールだけが対象
/// (他のスケジュールのために毎分ディスクへ書く理由は無い)。
///
/// **wasm 呼び出しが成功した後に呼ぶこと**: 失敗した発火を「実行済み」として
/// 記録すると、次回起動時にその打ち漏らしを追い掛けられなくなる。
fn record_fire(state: &ScheduleState, plugin_id: &str, name: &str, schedule_store: &ScheduleStore) {
    if state.is_catch_up(name) {
        schedule_store.record_fire(plugin_id, name, chrono::Local::now());
    }
}

/// `run_plugin_thread` のメインループが「次に何をするか」を決めるテスト
/// 可能な芯。wasm 実体や実際のチャネルを持ち込まずに分岐を検証できるよう、
/// `work_rx.recv_timeout(..)` の結果と `state.take_due(now)` の結果を
/// そのまま受け取って純粋に判定するだけの関数として切り出す。
///
/// **仕事が発火より優先**: `Ok(work)` は `due` の値に関わらず常に
/// `Handle`(`work` が `Stop` なら `Stop`)になる。期限超過分の発火は
/// ループ側が各 wasm 呼び出しの直後に改めて `take_due` を呼んで拾う
/// (このタスクの仕様「ブロック中に過ぎた発火は後で 1 回」)。
///
/// **`Timeout` + due なし**: 単に「何もせずループを継続する(次の待ちへ)」
/// ことを表す `Idle` を返す。
///
/// **`Ok(PluginWork::Stop)` → `LoopAction::Stop`**(Task 5): `Handle` に
/// 混ぜず専用の分岐にしてあるのは、ループ側が「on-stop を呼んでスレッドを
/// 終了する」という `Handle`/`Fire` とは異なる後始末をする必要があるため
/// (`disable_and_break!` の trap 分岐とも別 -- on-stop の失敗は disable
/// する意味が無い。もう止まるので)。
#[derive(Debug)]
pub(super) enum LoopAction {
    /// 通常の作業(journal イベント / バス配信)を処理する。
    Handle(PluginWork),
    /// このスケジュール名で `call_on_schedule` を呼ぶ。
    Fire(String),
    /// 何もせず次の `recv_timeout` へ戻る(タイムアウトしたが期限切れなし)。
    Idle,
    /// `work_rx` の送信側が全て閉じた。スレッドを終了する。
    Exit,
    /// `PluginWork::Stop` を受け取った。on-stop を呼んでスレッドを終了する。
    Stop,
}

pub(super) fn next_action(
    recv: Result<PluginWork, std_mpsc::RecvTimeoutError>,
    due: Option<String>,
) -> LoopAction {
    match recv {
        Ok(PluginWork::Stop) => LoopAction::Stop,
        Ok(work) => LoopAction::Handle(work),
        Err(std_mpsc::RecvTimeoutError::Timeout) => match due {
            Some(name) => LoopAction::Fire(name),
            None => LoopAction::Idle,
        },
        Err(std_mpsc::RecvTimeoutError::Disconnected) => LoopAction::Exit,
    }
}

/// `PluginWork` から計測点の `CallKind` を判定する純関数。
///
/// `PluginWork::Stop` は `next_action` が `LoopAction::Stop` へ振り分け、
/// `LoopAction::Handle` には never 来ないので `call_kind_of`/`detail_of` の
/// 対象にもならない。
fn call_kind_of(work: &PluginWork) -> CallKind {
    match work {
        PluginWork::Event(_) => CallKind::OnEvent,
        PluginWork::Message(_) => CallKind::OnMessage,
        PluginWork::JobComplete { .. } => CallKind::OnJobComplete,
        PluginWork::Stop => unreachable!(
            "next_action routes PluginWork::Stop to LoopAction::Stop, not Handle"
        ),
    }
}

/// `PluginWork` から計測点の `detail`(サンプルの人間可読な補足)を組み立てる
/// 純関数。journal イベントはイベント名(status イベントは名前が無いので
/// `"status"` で代用)、バス配信はプラグイン側から見た接続先の
/// `"driver_id/topic"` 形、ジョブ完了は `job_id`。
///
/// **`event_params` は使わない**: `event_params` は `raw.to_string()` で
/// ペイロード全体を毎回シリアライズしてから名前だけ使う(`call_on_event` の
/// 引数を組み立てるための関数)。`detail_of` は `Profiler::noop()` でも
/// 必ず実行される hot path なので、ここでは `Event` を直接 match して
/// 名前だけを取り出す(ペイロードの二重シリアライズを避ける)。
fn detail_of(work: &PluginWork) -> String {
    match work {
        PluginWork::Event(event) => match &**event {
            Event::Journal { event: name, .. } => name.clone(),
            Event::Status { .. } => "status".to_string(),
        },
        PluginWork::Message(delivery) => format!("{}/{}", delivery.driver_id, delivery.topic),
        PluginWork::JobComplete { job_id, .. } => job_id.to_string(),
        PluginWork::Stop => unreachable!(
            "next_action routes PluginWork::Stop to LoopAction::Stop, not Handle"
        ),
    }
}

fn event_params(event: &Event) -> (&'static str, Option<String>, Option<String>, String, bool) {
    match event {
        Event::Journal {
            timestamp,
            event: name,
            raw,
            replay,
        } => (
            "journal",
            Some(timestamp.clone()),
            Some(name.clone()),
            raw.to_string(),
            *replay,
        ),
        Event::Status { raw } => ("status", None, None, raw.to_string(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `job_completion_verdict` は「旧世代の完了を捨てる」判定だけの純関数
    /// (issue-sizx 決定 6)。trap/deadline でインスタンスが再作成された後、
    /// 旧インスタンスが submit したジョブの完了が新インスタンスに届かない
    /// ことの芯になる。
    mod job_completion_verdict_tests {
        use super::*;

        #[test]
        fn matching_generation_delivers() {
            assert_eq!(job_completion_verdict(0, 0), JobCompletionVerdict::Deliver);
            assert_eq!(job_completion_verdict(3, 3), JobCompletionVerdict::Deliver);
        }

        #[test]
        fn stale_generation_is_dropped() {
            // 再作成で bump された後、旧世代のジョブは捨てられる。
            assert_eq!(
                job_completion_verdict(0, 1),
                JobCompletionVerdict::DropStale
            );
        }

        #[test]
        fn future_generation_is_also_dropped() {
            // 起こらないはずの組み合わせだが、等値以外は全部捨てる
            // (「一致したときだけ届ける」が仕様)。
            assert_eq!(
                job_completion_verdict(2, 1),
                JobCompletionVerdict::DropStale
            );
        }
    }

    /// `call_kind_of`/`detail_of` は計測点(`profiler::call_sample` へ渡す
    /// 引数)を組み立てるだけの純関数。実際の記録がどこに着地するかは
    /// `profiler::collector` の統合テストで検証済みなので、ここでは
    /// `PluginWork` → `(CallKind, detail)` の対応だけを見る。
    mod call_kind_and_detail_tests {
        use super::*;
        use edlr_driver_channel::Delivery;

        fn journal_event(name: &str) -> PluginWork {
            PluginWork::Event(Arc::new(Event::Journal {
                timestamp: "2026-08-05T00:00:00Z".into(),
                event: name.into(),
                raw: serde_json::json!({}),
                replay: false,
            }))
        }

        #[test]
        fn journal_events_report_on_event_with_the_event_name() {
            let work = journal_event("FSDJump");
            assert!(matches!(call_kind_of(&work), CallKind::OnEvent));
            assert_eq!(detail_of(&work), "FSDJump");
        }

        #[test]
        fn status_events_fall_back_to_the_kind_as_detail() {
            let work = PluginWork::Event(Arc::new(Event::Status {
                raw: serde_json::json!({}),
            }));
            assert!(matches!(call_kind_of(&work), CallKind::OnEvent));
            assert_eq!(detail_of(&work), "status");
        }

        #[test]
        fn bus_deliveries_report_on_message_with_driver_slash_topic() {
            let work = PluginWork::Message(Delivery {
                plugin_id: "p1".into(),
                driver_id: "coeiroink".into(),
                topic: "speak".into(),
                payload: Vec::new(),
            });
            assert!(matches!(call_kind_of(&work), CallKind::OnMessage));
            assert_eq!(detail_of(&work), "coeiroink/speak");
        }

        #[test]
        fn job_completions_report_on_job_complete_with_the_job_id() {
            let work = PluginWork::JobComplete {
                generation: 0,
                job_id: 42,
                result_json: "{}".into(),
            };
            assert!(matches!(call_kind_of(&work), CallKind::OnJobComplete));
            assert_eq!(detail_of(&work), "42");
        }
    }
}

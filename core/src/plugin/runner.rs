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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

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

/// `spawn_bus_subscriber` のブロッキング受信を区切る間隔。
///
/// **これが無いとデーモンの正常終了がハングする(実際に踏んだ Critical
/// バグの修正)**: `spawn_bus_subscriber` は `tokio::task::spawn_blocking` の
/// 中で `delivery_rx` を読み続けるが、その送信側(`Bus::subscribe` に渡した
/// `Sender<Delivery>`)は `edlr_driver_channel::Bus` の購読表に居座り続け、
/// 明示的に `unsubscribe`(`run_plugin_thread` の trap 分岐、あるいはプロセス
/// 終了)されない限り閉じない。素朴な `for delivery in delivery_rx`(かつての
/// 実装)は「送信側が全部閉じるまで無期限に待つ」ブロッキング呼び出しであり、
/// `core/src/bin/edlr.rs` の `main` が(`Runtime::drop` を伴って)戻ろうとする
/// 際、`Runtime` はこの `spawn_blocking` タスクの完了を待ち続けるため、
/// **デーモンが SIGTERM/SIGINT を受けても `main` から永久に戻れない**
/// (Tauri アプリの「デーモン未起動なら自動 spawn し、終了時に道連れで止める」
/// が `STOP_GRACE` を使い切って最後は SIGKILL する羽目になる、という形で
/// 顕在化した)。`[[bus]] subscribe` を宣言するプラグインが 1 つでもあれば
/// 再現する。
///
/// 対策として、`delivery_rx.recv_timeout(..)` でブロッキング時間をこの間隔に
/// 区切り、タイムアウトのたびに `shutdown` フラグ(`bin/edlr.rs` の shutdown
/// シーケンスが `Registry::shutdown_bus_subscribers` 経由で `Runtime` を drop
/// する**前**に立てる)を確認する。200ms は「シグナル受信からタスク終了までの
/// 体感の遅延」と「アイドル時に無駄にウェイクアップする頻度」のバランスを
/// 取った値で、他に強い制約は無い。
const BUS_SUBSCRIBER_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(200);

use tokio::sync::broadcast;

use edlr_driver_channel::{Bus, Delivery};

use crate::driver::registry::DriverRegistry;
use crate::event::Event;
use crate::plugin::bus_runtime::{bus_json_string, parse_bus, BusRuntimeEntry};
use crate::plugin::dropped::DropCounters;
use crate::plugin::filesystem::FilesystemConfigStore;
use crate::plugin::fs_runtime::{filesystem_json_string, FsRuntimeEntry};
use crate::plugin::grants::GrantsStore;
use crate::plugin::host::{
    capabilities_json_string, HostCtx, PluginCallError, PluginHost, PluginInstance,
};
use crate::plugin::manifest::{load_manifest, matches_event};
use crate::plugin::registry::{PluginEntry, PluginState, Registry};
use crate::plugin::schedule::{Clock, ScheduleState, ScheduleView};
use crate::plugin::schedule_store::ScheduleStore;
use crate::plugin::settings::SettingsStore;
use crate::plugin::sidecar::{assign_ports, SidecarConfig, SidecarConfigStore};
use crate::plugin::sidecar_runtime::{
    implicit_http_hosts, sidecars_json_string, SidecarRuntimeEntry,
};
use crate::plugin::Manifest;
use crate::router::Router;

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
const CALL_DEADLINE_STRIKES: u32 = 3;

/// `plugins_dir` を走査し、各プラグインをロードして専用タスクで駆動する。
///
/// 戻り値の `Registry` は起動直後から `snapshot` 可能(= 各プラグインの
/// `load`/`init` 結果が確定した後に返る)。
///
/// **起動時にサイドカーを自動 spawn することはない**: ここで組み立てる
/// `sidecars_json`/`capabilities_json` はあくまで承認・設定状態のスナップ
/// ショットで、実プロセスの起動はプラグイン自身の `ensure-started` 呼び出し
/// か、ユーザー操作(`Registry::control_sidecar`)を経て初めて行われる。
///
/// **`bus` は呼び出し元が構築した 1 つのインスタンスを渡す**: ここで組み立て
/// る各プラグインの `HostCtx` はこの同じ `bus` を(`Clone` して)共有する
/// (`http_driver`/`process_driver`/`fs_driver` と同様、`HostCtx::bus` の
/// ドキュメントコメント参照)。呼び出し元は `crate::driver::start_drivers` を
/// この関数より先に呼び、そこで返した `DriverRegistry` の構築に使ったのと
/// 同じ `bus` をここへ渡すこと -- そうしないと、ドライバの登録
/// (`Bus::register_driver`)が完了する前にプラグインが起動し、`init` 中の
/// 最初の `bus.get` 呼び出しが `unknown-driver` を見てしまう(設計書
/// 「起動順序」参照)。
#[allow(clippy::too_many_arguments)]
pub fn start_plugins(
    plugins_dir: &Path,
    settings_store: SettingsStore,
    sidecar_config_store: SidecarConfigStore,
    filesystem_config_store: FilesystemConfigStore,
    grants_store: GrantsStore,
    schedule_store: ScheduleStore,
    router: &Router,
    bus: Bus,
    drivers: DriverRegistry,
    host: PluginHost,
) -> Registry {
    let host = Arc::new(host);
    let settings_store = Arc::new(settings_store);
    let schedule_store = Arc::new(schedule_store);
    let grants_store = Arc::new(grants_store);
    let sidecar_config_store = Arc::new(sidecar_config_store);
    let filesystem_config_store = Arc::new(filesystem_config_store);
    let process_driver = host.process_driver();
    let registry = Registry::new(
        host.clone(),
        settings_store.clone(),
        grants_store.clone(),
        sidecar_config_store.clone(),
        filesystem_config_store.clone(),
        process_driver,
        drivers.clone(),
        plugins_dir.to_path_buf(),
    );

    let dir_entries = match std::fs::read_dir(plugins_dir) {
        Ok(dir_entries) => dir_entries,
        Err(e) => {
            tracing::info!(
                plugins_dir = %plugins_dir.display(),
                "plugins directory not found or unreadable ({e}); starting with no plugins"
            );
            return registry;
        }
    };

    for dir_entry in dir_entries {
        let Ok(dir_entry) = dir_entry else { continue };
        let path = dir_entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest = match load_manifest(&path) {
            Ok(manifest) => manifest,
            Err(e) => {
                tracing::warn!(
                    plugin_dir = %path.display(),
                    "skipping invalid plugin: {e}"
                );
                continue;
            }
        };

        warn_unresolved_bus(&manifest, &drivers);

        load_and_run_plugin(
            &manifest,
            &path,
            &settings_store,
            &grants_store,
            &sidecar_config_store,
            &filesystem_config_store,
            &schedule_store,
            router,
            &bus,
            &host,
            &registry,
        );
    }

    registry
}

/// 1 プラグインをロードし、成功すれば専用スレッド・購読タスクを起動し、
/// 結果(Running/Disabled)を `registry` に登録する。
#[allow(clippy::too_many_arguments)]
fn load_and_run_plugin(
    manifest: &Manifest,
    dir: &Path,
    settings_store: &SettingsStore,
    grants_store: &GrantsStore,
    sidecar_config_store: &SidecarConfigStore,
    filesystem_config_store: &FilesystemConfigStore,
    schedule_store: &Arc<ScheduleStore>,
    router: &Router,
    bus: &Bus,
    host: &Arc<PluginHost>,
    registry: &Registry,
) {
    let entry_path = dir.join(&manifest.entry);
    let effective = settings_store.effective(manifest);
    let settings_json_string = serde_json::to_string(&serde_json::Value::Object(effective))
        .unwrap_or_else(|_| "{}".to_string());
    let settings_json = Arc::new(Mutex::new(settings_json_string));

    let grant_state = grants_store.state(manifest);

    // サイドカー 1 件ずつの設定(`SidecarConfigStore`)・承認
    // (`GrantsStore::sidecar_state`)を解決して `SidecarRuntimeEntry` に
    // まとめる。これは `Registry::refresh_sidecar_runtime` が承認・設定変更
    // のたびに作り直すのと同じ組み立て方(見た目の重複はあるが、あちらは
    // `Registry` 経由の更新用、こちらは起動直後の初期値用で、依存する
    // ライフサイクルの起点が異なるため 1 箇所に共通化はしていない)。
    let sidecar_configs = sidecar_config_store.effective(manifest);
    let sidecar_entries: Vec<SidecarRuntimeEntry> = manifest
        .sidecars
        .iter()
        .map(|request| {
            let config = sidecar_configs
                .get(&request.name)
                .cloned()
                .unwrap_or_else(|| SidecarConfig::from_request(request));
            let granted = grants_store.sidecar_state(manifest, &request.name).granted;
            SidecarRuntimeEntry {
                name: request.name.clone(),
                granted,
                command: config.command.clone(),
                args: config.args.clone(),
                ports: assign_ports(&config),
            }
        })
        .collect();
    let sidecars_json = Arc::new(Mutex::new(sidecars_json_string(&sidecar_entries)));

    // http capability の承認済み hosts と、承認済みサイドカーの暗黙許可を
    // 合流させる(`capabilities_json` に載るのは実効的に許可されたホストの
    // みで、サイドカーの暗黙許可は http capability の承認とは独立に効く)。
    let mut initial_hosts = if grant_state.granted {
        manifest.capability_hosts()
    } else {
        Vec::new()
    };
    initial_hosts.extend(implicit_http_hosts(&sidecar_entries));
    let initial_capabilities_json = capabilities_json_string(&initial_hosts);
    let capabilities_json = Arc::new(Mutex::new(initial_capabilities_json));

    // ファイルアクセスのルートごとの設定(`FilesystemConfigStore`)・承認
    // (`GrantsStore::filesystem_state`)を解決して `FsRuntimeEntry` にまと
    // める。サイドカーの初期値組み立てと同じ流儀(未承認のルートは
    // `filesystem_json_string` が `path` を落とす -- `fs_runtime` のドキュ
    // メント参照)。
    let filesystem_configs = filesystem_config_store.effective(manifest);
    let filesystem_entries: Vec<FsRuntimeEntry> = manifest
        .filesystem
        .iter()
        .map(|request| {
            let path = filesystem_configs
                .get(&request.name)
                .map(|config| config.path.clone())
                .unwrap_or_default();
            let granted = grants_store
                .filesystem_state(manifest, &request.name)
                .granted;
            FsRuntimeEntry {
                name: request.name.clone(),
                granted,
                mode: request.mode.as_str().to_string(),
                path,
            }
        })
        .collect();
    let filesystem_json = Arc::new(Mutex::new(filesystem_json_string(&filesystem_entries)));

    // バス接続先ごとの承認(`GrantsStore::bus_state`)を解決して
    // `BusRuntimeEntry` にまとめる。サイドカー/ファイルアクセスの初期値組み
    // 立てと同じ流儀(未承認の接続先は `bus_json_string` が `publish`/
    // `subscribe` を落とす -- `bus_runtime` のドキュメント参照)。トピック
    // 一覧自体は manifest の要求をそのまま載せる(`GrantsStore` はトピック
    // の中身を持たない)。
    let bus_entries: Vec<BusRuntimeEntry> = manifest
        .bus
        .iter()
        .map(|request| {
            let granted = grants_store.bus_state(manifest, &request.driver).granted;
            BusRuntimeEntry {
                driver: request.driver.clone(),
                granted,
                publish: request.publish.clone(),
                subscribe: request.subscribe.clone(),
            }
        })
        .collect();
    let bus_json = Arc::new(Mutex::new(bus_json_string(&bus_entries)));

    // journal イベントとバス配信を混ぜる 1 本の作業キュー(`PluginWork` の
    // ドキュメントコメント参照)。
    let (work_tx, work_rx) = std_mpsc::sync_channel::<PluginWork>(PLUGIN_WORK_QUEUE_CAPACITY);
    let (ready_tx, ready_rx) = std_mpsc::channel::<PluginState>();

    // `Stop` のアウトオブバンド経路(`Registry::shutdown_plugins` が立てる)。
    // 詳細は `run_plugin_thread` のループ先頭のコメント参照。
    let stop_flag = Arc::new(AtomicBool::new(false));

    let thread_handle = thread::spawn({
        let host = host.clone();
        let manifest = manifest.clone();
        let settings_json = settings_json.clone();
        let capabilities_json = capabilities_json.clone();
        let sidecars_json = sidecars_json.clone();
        let filesystem_json = filesystem_json.clone();
        let bus_json = bus_json.clone();
        let bus = bus.clone();
        let registry = registry.clone();
        let stop_flag = stop_flag.clone();
        let schedule_store = schedule_store.clone();
        move || {
            run_plugin_thread(
                host,
                manifest,
                entry_path,
                settings_json,
                capabilities_json,
                sidecars_json,
                filesystem_json,
                bus_json,
                bus,
                registry,
                work_rx,
                ready_tx,
                stop_flag,
                schedule_store,
            );
        }
    });

    let state = ready_rx.recv().unwrap_or_else(|_| PluginState::Disabled {
        reason: "plugin thread exited before reporting an init result".to_string(),
    });
    let running = matches!(state, PluginState::Running);

    registry.push(PluginEntry {
        manifest: manifest.clone(),
        state,
        settings_json,
        capabilities_json,
        sidecars_json,
        filesystem_json,
        bus_json: bus_json.clone(),
    });

    if running {
        // `shutdown_plugins`(デーモンの正常終了)がこのプラグインへ
        // `PluginWork::Stop` を送り、スレッドの終了を待てるように、送信側の
        // 複製とスレッドの `JoinHandle` を registry に登録する。Disabled
        // (=このブロックに入らない)プラグインのスレッドは、この時点で既に
        // `init` 失敗を `ready_tx` へ送って return 済みなので、登録せずに
        // `thread_handle` を(join せずに)そのまま drop してよい -- スレッド
        // 自体はもう終了しているか、終了する寸前でしかない。
        registry.register_plugin_thread(&manifest.id, work_tx.clone(), thread_handle, stop_flag);

        // 作業キュー満杯で捨てた件数を数える窓口。両方の購読タスク(書き手)と
        // `plugins/list`(読み手)が共有する。
        let drops = DropCounters::new();
        registry.register_drop_counters(&manifest.id, drops.clone());

        spawn_event_subscriber(
            manifest.clone(),
            router.subscribe(),
            work_tx.clone(),
            drops.clone(),
        );

        // `[[bus]]` の `subscribe` トピックがあれば、購読を登録して配信を
        // このプラグインの作業キューへ流し込む(`spawn_bus_subscriber` の
        // ドキュメントコメント参照)。登録は承認の有無に関わらず行う --
        // 承認は配信のたびに再確認するため(稼働中の取り消しを即座に効かせる
        // ため)、後から承認されても購読し直す必要がない。
        let subscribe_topics: Vec<(String, String)> = manifest
            .bus
            .iter()
            .flat_map(|request| {
                request
                    .subscribe
                    .iter()
                    .map(move |topic| (request.driver.clone(), topic.clone()))
            })
            .collect();
        if !subscribe_topics.is_empty() {
            let (delivery_tx, delivery_rx) =
                std_mpsc::sync_channel::<Delivery>(PLUGIN_WORK_QUEUE_CAPACITY);
            for (driver_id, topic) in &subscribe_topics {
                subscribe_with_initial_value(
                    bus,
                    &manifest.id,
                    driver_id,
                    topic,
                    delivery_tx.clone(),
                );
            }
            drop(delivery_tx);
            spawn_bus_subscriber(
                manifest.clone(),
                bus_json,
                delivery_rx,
                work_tx,
                registry.bus_subscriber_shutdown_flag(),
                drops,
            );
        }
    }
    // Disabled の場合、work_tx はここで drop される。プラグインスレッドは
    // 既に init 失敗で return 済みで work_rx を読まないので問題ない。
}

/// プラグイン専用スレッドの本体。`load` → `call_init` → イベントループを
/// 直列に実行する。すべての wasm 呼び出しはこのスレッド上でのみ発生する。
#[allow(clippy::too_many_arguments)]
fn run_plugin_thread(
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
    work_rx: std_mpsc::Receiver<PluginWork>,
    ready_tx: std_mpsc::Sender<PluginState>,
    stop_flag: Arc<AtomicBool>,
    schedule_store: Arc<ScheduleStore>,
) {
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
        );
        let mut instance = host
            .load(&entry_path, ctx)
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
            tracing::warn!(
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
                    if deadline_strikes >= CALL_DEADLINE_STRIKES {
                        disable_and_break!(format!(
                            "{e} on {deadline_strikes} consecutive calls; the plugin is \
                             persistently too slow (a blocked host call, or work that does \
                             not fit the deadline)"
                        ));
                    }
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
                    // `next_action` は `PluginWork::Stop` を `LoopAction::Handle`
                    // ではなく専用の `LoopAction::Stop` に振り分けるので、ここに
                    // 来ることはない。
                    PluginWork::Stop => unreachable!(
                        "next_action routes PluginWork::Stop to LoopAction::Stop, not Handle"
                    ),
                };
                handle_call_result!(result);
                handle_call_result!(fire_all_due(
                    &mut schedule_state,
                    &mut instance,
                    &manifest.id,
                    &schedule_store,
                ));
            }
            LoopAction::Fire(name) => {
                handle_call_result!(instance.call_on_schedule(&name));
                record_fire(&schedule_state, &manifest.id, &name, &schedule_store);
                handle_call_result!(fire_all_due(
                    &mut schedule_state,
                    &mut instance,
                    &manifest.id,
                    &schedule_store,
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
) -> Result<(), PluginCallError> {
    loop {
        let Some(name) = state.take_due(Clock::now()) else {
            return Ok(());
        };
        instance.call_on_schedule(&name)?;
        record_fire(state, plugin_id, &name, schedule_store);
    }
}

/// 発火した時刻を永続化する。`catch-up` を宣言したスケジュールだけが対象
/// (他のスケジュールのために毎分ディスクへ書く理由は無い)。
///
/// **wasm 呼び出しが成功した後に呼ぶこと**: 失敗した発火を「実行済み」として
/// 記録すると、次回起動時にその打ち漏らしを追い掛けられなくなる。
fn record_fire(
    state: &ScheduleState,
    plugin_id: &str,
    name: &str,
    schedule_store: &ScheduleStore,
) {
    if state.is_catch_up(name) {
        schedule_store.record_fire(plugin_id, name, chrono::Local::now());
    }
}

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
enum LoopAction {
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

fn next_action(
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

/// `router` を購読し、`manifest.events` にマッチしたイベントだけを
/// プラグインスレッドへ転送する tokio タスクを起動する。
///
/// `work_tx` は容量固定の `sync_channel`(`PLUGIN_WORK_QUEUE_CAPACITY`)
/// なので、送信には(ブロックする `send` ではなく)`try_send` を使う。この
/// tokio タスクは非同期ランタイムのワーカースレッド上で動くため、万一
/// プラグインスレッドが `driver-http.send` のブロッキング呼び出し中で
/// `work_rx` を全く読んでいなくても、ここで待たされてはいけない
/// (ワーカースレッドを塞ぐと router/monitor 全体に波及する)。キューが
/// 満杯の間に届いたイベントは `tracing::warn!` を出して破棄する
/// (`PLUGIN_WORK_QUEUE_CAPACITY` のドキュメント参照)。
fn spawn_event_subscriber(
    manifest: Manifest,
    mut rx: broadcast::Receiver<Arc<Event>>,
    work_tx: std_mpsc::SyncSender<PluginWork>,
    drops: Arc<DropCounters>,
) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if !matches_event(&manifest.events, &event) {
                        continue;
                    }
                    match work_tx.try_send(PluginWork::Event(event)) {
                        Ok(()) => {}
                        Err(std_mpsc::TrySendError::Full(_)) => {
                            // journal の読み取り位置は配送の成否と独立に進むので、
                            // ここで捨てたイベントは replay でも戻らない。
                            // `plugins/list` に出すために数えておく。
                            drops.record_event_drop();
                            tracing::warn!(
                                plugin_id = %manifest.id,
                                "event queue full ({PLUGIN_WORK_QUEUE_CAPACITY} pending), \
                                 dropping event for a slow/blocked plugin"
                            );
                        }
                        Err(std_mpsc::TrySendError::Disconnected(_)) => {
                            // プラグインスレッドが終了(disabled)済み。
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        plugin_id = %manifest.id,
                        "event subscriber lagged, skipped {skipped} events"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
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

/// 購読を登録し、retain 済みトピックなら現在値を 1 回だけ届ける。
///
/// 後から起動・後から承認されたプラグインにも最新値が渡るようにするため
/// (設計書「データフロー」参照)。ここで送るのは登録直後の 1 通だけで、
/// 以降は通常の `emit` 経路に乗る。
///
/// **登録が先、送信が後**: 先に `bus.subscribe` で購読表へ登録してから
/// 現在の retained 値を読んで送る。逆順にすると、値を読んだ直後・購読登録の
/// 前に割り込んだ `emit` を取りこぼす窓ができてしまう。
pub(crate) fn subscribe_with_initial_value(
    bus: &Bus,
    plugin_id: &str,
    driver_id: &str,
    topic: &str,
    sender: std_mpsc::SyncSender<Delivery>,
) {
    bus.subscribe(plugin_id, driver_id, topic, sender.clone());
    if let Some(payload) = bus.retained_for(driver_id, topic) {
        let _ = sender.try_send(Delivery {
            plugin_id: plugin_id.to_string(),
            driver_id: driver_id.to_string(),
            topic: topic.to_string(),
            payload,
        });
    }
}

/// `Bus::subscribe` に渡した `SyncSender<Delivery>` の受け口を読み、
/// 承認済み・宣言済みのままの配信だけを `PluginWork::Message` に詰め替えて
/// プラグインの作業キューへ転送する。`spawn_event_subscriber` と対称の形だが、
/// 転送元が(非同期の `broadcast::Receiver` ではなく)同期の
/// `std::sync::mpsc::Receiver` なので `tokio::task::spawn_blocking` を使う
/// (`bin/edlr.rs` の `spawn_blocking` 呼び出しと同じ流儀。非同期ランタイムの
/// ワーカースレッドを専有させない)。
///
/// **配信のたびに承認を再確認する**: 承認は稼働中も取り消せる
/// (`Registry::set_bus_grant`)ため、購読を登録した時点の承認状態を信じて
/// 転送し続けると、取り消し後も届いてしまう(fail-open)。ここでは毎回
/// `bus_json` を読み直し、`granted` かつ当該トピックが `subscribe` に
/// 含まれている場合だけ転送する(`HostCtx::check_bus` と同じ判定材料・
/// 同じ判定規則)。
///
/// **`shutdown` を定期的に確認しながらブロックする**(`for delivery in
/// delivery_rx` ではなく `delivery_rx.recv_timeout(..)` を使う理由は
/// `BUS_SUBSCRIBER_SHUTDOWN_POLL_INTERVAL` のドキュメントコメント参照)。
/// `shutdown` が立っていれば、キューに残りがあってもそこで打ち切って戻る --
/// デーモン終了シーケンスの一部として呼ばれるので、残りを律儀に配り切る
/// より `Runtime::drop` を進められる方を優先する。
fn spawn_bus_subscriber(
    manifest: Manifest,
    bus_json: Arc<Mutex<String>>,
    delivery_rx: std_mpsc::Receiver<Delivery>,
    work_tx: std_mpsc::SyncSender<PluginWork>,
    shutdown: Arc<AtomicBool>,
    drops: Arc<DropCounters>,
) {
    tokio::task::spawn_blocking(move || loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let delivery = match delivery_rx.recv_timeout(BUS_SUBSCRIBER_SHUTDOWN_POLL_INTERVAL) {
            Ok(delivery) => delivery,
            Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let raw = bus_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let entries = parse_bus(&raw);
        let still_granted = entries
            .get(&delivery.driver_id)
            .is_some_and(|entry| entry.granted && entry.subscribe.contains(&delivery.topic));
        if !still_granted {
            // 承認が取り消された(か、そもそも一度も承認されていない)。
            // 黙って捨てる -- `check_bus` が publish/get 側で同じ状況を
            // `permission-denied` として扱うのと違い、こちらはドライバ
            // 起点のプッシュ配信なので呼び出し元に返すエラーが無い。
            continue;
        }
        match work_tx.try_send(PluginWork::Message(delivery)) {
            Ok(()) => {}
            Err(std_mpsc::TrySendError::Full(_)) => {
                // バス配信は再送されないので、ここで捨てた分は失われる。
                drops.record_bus_delivery_drop();
                tracing::warn!(
                    plugin_id = %manifest.id,
                    "work queue full ({PLUGIN_WORK_QUEUE_CAPACITY} pending), \
                     dropping a bus delivery for a slow/blocked plugin"
                );
            }
            Err(std_mpsc::TrySendError::Disconnected(_)) => {
                // プラグインスレッドが終了(disabled)済み。
                break;
            }
        }
    });
}

/// `[[bus]]` の参照先が実在しないものを warn ログに出す。
///
/// **起動は止めない**(ドライバは後から入れられるべき)。ただし黙って
/// 動くと事故になるので、プラグイン ID・ドライバ ID・トピック名を全て
/// 含めて必ず 1 件ずつ出す。UI 側は `BusInfo::resolved` を見て「未解決」
/// バッジを出す。
pub(crate) fn warn_unresolved_bus(manifest: &Manifest, drivers: &DriverRegistry) {
    for request in &manifest.bus {
        let Some(driver) = drivers.manifest_of(&request.driver) else {
            tracing::warn!(
                plugin_id = %manifest.id,
                driver_id = %request.driver,
                "plugin declares a bus connection to a driver that is not installed"
            );
            continue;
        };
        for topic in request.publish.iter().chain(request.subscribe.iter()) {
            if driver.topic(topic).is_none() {
                tracing::warn!(
                    plugin_id = %manifest.id,
                    driver_id = %request.driver,
                    topic = %topic,
                    "plugin declares a bus topic the driver does not provide"
                );
            }
        }
    }
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
        let (work_tx, work_rx) =
            std_mpsc::sync_channel::<PluginWork>(PLUGIN_WORK_QUEUE_CAPACITY);

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

        bus.emit("ed-state", "current-system", b"a".to_vec()).unwrap();
        bus.emit("ed-state", "current-system", b"b".to_vec()).unwrap();

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
        use crate::plugin::bus_runtime::{bus_json_string, BusRuntimeEntry};

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
}

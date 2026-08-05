//! `drivers_dir` を走査し、各ドライバをロードして専用スレッドで駆動する。
//!
//! `crate::runner::plugin` と同じ構造(専用 OS スレッドが `DriverHost::load`
//! → `call_init` → メッセージループを直列に実行する)だが、以下が異なる:
//! - **イベント購読タスクを作らない**。プラグインは router の broadcast
//!   イベントを tokio タスクでフィルタして転送するが、ドライバはそもそも
//!   journal イベントを購読しない。バスのメッセージは
//!   `Bus::register_driver` に渡した sink(`DriverQueueSink`)が作業キュー
//!   (`DriverWork`)へ直結し、ドライバ専用スレッドが `recv` で回す。
//!   submit 完了通知(`DriverWork::JobComplete`)も同じキューに混ざる。
//! - ドライバは複数プラグインの結節点になりうるため、キュー容量は
//!   `DRIVER_MESSAGE_QUEUE_CAPACITY`(64)としてある。プラグイン側の
//!   `PLUGIN_WORK_QUEUE_CAPACITY` も同じ 64 だが(journal イベントとバス
//!   配信の 2 プロデューサが枠を奪い合うようになったため引き上げた -- 同
//!   定数のドキュメント参照)、そちらは 1 プラグインあたりの容量であるのに
//!   対しこちらは複数プラグインが同時に投げ込みうる結節点の容量なので、
//!   両者が同じ数字であることに深い意味は無い(たまたま揃っただけ)。

use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use edlr_driver_channel::{Bus, Message, MessageSink, SinkError, HOST_SENDER};

use crate::runner::plugin::queue::{
    channel_for, Admit, PushError, WorkReceiver, WorkSender,
};

use crate::capability::grants::GrantsStore;
use crate::host::driver::{DriverCtx, DriverHost};
use crate::manifest::driver::{load_driver_manifest, DriverManifest};
use crate::registry::driver::{DriverEntry, DriverRegistry, DriverState};
use crate::runner::bootstrap::{build_initial_buffers, InitialBuffers};
use crate::settings::filesystem::FilesystemConfigStore;
use crate::settings::sidecar::SidecarConfigStore;
use crate::settings::store::SettingsStore;

/// ドライバ 1 件あたりのメッセージキュー容量。
///
/// ドライバは複数プラグインの結節点で溢れやすく、1 メッセージの処理が
/// `DriverInstance::CALL_DEADLINE`(30 秒)まで伸びうるため、余裕を持って
/// 大きめに取ってある(プラグイン側の 1 プラグインあたりの容量である
/// `PLUGIN_WORK_QUEUE_CAPACITY` とは性質が異なる数字なので、値が近い/同じ
/// であること自体に意味は無い)。満杯時は `publish` が `queue-full` を
/// 返す(捨てない)ので、呼び出し側が状況を知れる。
const DRIVER_MESSAGE_QUEUE_CAPACITY: usize = 64;

/// ドライバ専用スレッドが処理する仕事。バスのメッセージと
/// `driver-http.submit-send` の完了通知を 1 本のキューに混ぜることで、
/// wasm 呼び出しが 1 スレッドに直列化される性質を保つ
/// (`crate::runner::plugin::PluginWork` と対称)。
#[derive(Debug)]
pub enum DriverWork {
    Message(Message),
    /// submit 系ジョブの完了通知(`crate::host::driver::DriverCtx::submit_send`
    /// が spawn したタスクが push する)。`generation` はプラグイン側との
    /// 対称性のために運ぶが、ドライバはインスタンス再作成が無い(呼び出し
    /// 失敗 = 即 Disabled)ため実質常に 0。
    JobComplete {
        generation: u64,
        job_id: u64,
        result_json: String,
    },
    /// `Bus` が sink を手放した(登録差し替え / Bus 破棄)。受信ループを
    /// 終了する -- かつて `for message in messages_rx` がチャネル切断で
    /// 終了していた挙動の置き換え(`DriverQueueSink` の `Drop` が押し込む)。
    Disconnected,
}

/// ドライバ作業キューの受け入れ判定(純関数)。
///
/// - `Message`(プラグイン発): 容量超過は `DropNewest`。`DriverQueueSink`
///   が `SinkError::Full` へ写像し、`Bus::publish` が `queue-full` として
///   呼び出し元へ返す(**捨てない**、という従来の契約の保存)
/// - `Message`(ホスト発 = `sidecar-ready` 等): 常に受け入れる。件数は
///   spawn 回数で有界で、取りこぼすと ready 通知の空白期間が再発するため
/// - `JobComplete` / `Disconnected`: 常に受け入れる(完了通知はプラグイン側
///   と同じ理由。`Disconnected` は終了指示そのもの)
pub(crate) fn admit_driver_work(queue_len: usize, work: &DriverWork) -> Admit {
    match work {
        DriverWork::JobComplete { .. } | DriverWork::Disconnected => Admit::Accept,
        DriverWork::Message(message) if message.from == HOST_SENDER => Admit::Accept,
        DriverWork::Message(_) => {
            if queue_len >= DRIVER_MESSAGE_QUEUE_CAPACITY {
                Admit::DropNewest
            } else {
                Admit::Accept
            }
        }
    }
}

/// `Bus::register_driver` へ渡す受け口。バスのメッセージをドライバの
/// 作業キューへ直結する。
///
/// `Drop` で `Disconnected` を push する: `Bus` がこの sink を手放すのは
/// 登録差し替えか `Bus` 破棄のときで、どちらも「もうこのキューへは何も
/// 来ない」を意味する。ドライバスレッド自身(の `DriverCtx`)も送信側を
/// 持つため、センダー数ゼロによる切断検出はもう起こらない -- 明示の
/// センチネルで受信ループを終了させる。
struct DriverQueueSink(WorkSender<DriverWork>);

impl MessageSink for DriverQueueSink {
    fn try_send(&self, message: Message) -> Result<(), SinkError> {
        match self.0.push(DriverWork::Message(message)) {
            Ok(()) => Ok(()),
            Err(PushError::Dropped) => Err(SinkError::Full),
            Err(PushError::Disconnected) => Err(SinkError::Closed),
        }
    }
}

impl Drop for DriverQueueSink {
    fn drop(&mut self) {
        let _ = self.0.push(DriverWork::Disconnected);
    }
}

/// `drivers_dir` を走査し、各ドライバをロードして専用スレッドで駆動する。
///
/// 戻り値の `DriverRegistry` は起動直後から `list` 可能(= 各ドライバの
/// `load`/`init` 結果が確定した後に返る)。
///
/// **呼び出し順序が重要**: この関数は `crate::runner::plugin::start_plugins`
/// より先に呼ぶこと。ドライバの登録(`Bus::register_driver`)が完了する前に
/// プラグインが起動すると、そのプラグインの `init` 中の最初の `bus.get` 呼び
/// 出しが `unknown-driver` を見てしまう(設計書「起動順序」参照)。
pub fn start_drivers(
    drivers_dir: &Path,
    settings_store: SettingsStore,
    sidecar_config_store: SidecarConfigStore,
    filesystem_config_store: FilesystemConfigStore,
    grants_store: GrantsStore,
    bus: Bus,
    host: DriverHost,
) -> DriverRegistry {
    let host = Arc::new(host);

    // spawn したサイドカーの port が初めて繋がった時点で、当該ドライバの
    // on-message へ sidecar-ready を届ける(設計書 sidecar-ready 参照)。
    // ドライバの init が最初の ensure-started を呼ぶより前に設定しておく
    // 必要があるため、走査ループより先に配線する。プラグイン側の
    // `PluginHost` は別の `ProcessDriver` を持つので影響しない。
    {
        let bus = bus.clone();
        host.process_driver()
            .set_ready_callback(Arc::new(move |event| forward_sidecar_ready(&bus, event)));
    }

    let settings_store = Arc::new(settings_store);
    let grants_store = Arc::new(grants_store);
    let sidecar_config_store = Arc::new(sidecar_config_store);
    let filesystem_config_store = Arc::new(filesystem_config_store);
    let registry = DriverRegistry::new(
        host.clone(),
        settings_store.clone(),
        grants_store.clone(),
        sidecar_config_store.clone(),
        filesystem_config_store.clone(),
        bus.clone(),
        drivers_dir.to_path_buf(),
    );

    let dir_entries = match std::fs::read_dir(drivers_dir) {
        Ok(dir_entries) => dir_entries,
        Err(e) => {
            tracing::info!(
                drivers_dir = %drivers_dir.display(),
                "drivers directory not found or unreadable ({e}); starting with no drivers"
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

        let manifest = match load_driver_manifest(&path) {
            Ok(manifest) => manifest,
            Err(e) => {
                tracing::warn!(
                    driver_dir = %path.display(),
                    "skipping invalid driver: {e}"
                );
                continue;
            }
        };

        load_and_run_driver(
            &manifest,
            &path,
            &settings_store,
            &grants_store,
            &sidecar_config_store,
            &filesystem_config_store,
            &bus,
            &host,
            &registry,
        );
    }

    registry
}

/// `ProcessDriver` の ready 監視(spawn したサイドカーの port へ初めて TCP
/// 接続できた)を、当該ドライバの `on-message` キューへ
/// `from = "host", topic = "sidecar-ready"` の合成メッセージとして届ける
/// (設計書 sidecar-ready 参照)。
///
/// `event.key` は `DriverCtx::sidecar_key` が組み立てる
/// `<driver-id>/<sidecar-name>`。配送失敗(ドライバが Disabled 等)は warn
/// ログに出して捨てる — ready 通知は起動直後の空白期間を埋める最適化であり、
/// 既存の「speak 受信時に取り直す」経路が保険として残っているため。
fn forward_sidecar_ready(bus: &Bus, event: edlr_driver_process::ReadyEvent) {
    let Some((driver_id, name)) = event.key.split_once('/') else {
        tracing::warn!(key = %event.key, "sidecar-ready with an unrecognized key; dropping");
        return;
    };
    let payload = serde_json::json!({
        "name": name,
        "index": event.index,
        "port": event.port,
    });
    if let Err(e) =
        bus.notify_from_host(driver_id, "sidecar-ready", payload.to_string().into_bytes())
    {
        tracing::warn!(
            driver_id,
            sidecar = name,
            "failed to deliver sidecar-ready: {e}"
        );
    }
}

/// 1 ドライバをロードし、成功すれば専用スレッドを起動し、結果
/// (Running/Disabled)を `registry` に登録する。
#[allow(clippy::too_many_arguments)]
fn load_and_run_driver(
    manifest: &DriverManifest,
    dir: &Path,
    settings_store: &SettingsStore,
    grants_store: &GrantsStore,
    sidecar_config_store: &SidecarConfigStore,
    filesystem_config_store: &FilesystemConfigStore,
    bus: &Bus,
    host: &Arc<DriverHost>,
    registry: &DriverRegistry,
) {
    let entry_path = dir.join(&manifest.entry);

    // layout.kdl / layout.json は不備があってもロードを一切妨げない
    // (`crate::layout` のモジュールドキュメント参照)。パース/解決の警告は
    // ここで warn ログへ落とし、解決済みの layout(または None)だけを
    // entry へ格納する(`crate::runner::plugin::load_and_run_plugin` と対称)。
    let (layout, layout_warnings) = crate::layout::load::load_layout(dir);
    let (layout, layout_warnings) = match layout {
        Some(parsed) => {
            let (resolved, mut resolve_warnings) =
                crate::layout::resolve::resolve(parsed, &manifest.settings);
            let mut all = layout_warnings;
            all.append(&mut resolve_warnings);
            (Some(resolved), all)
        }
        None => (None, layout_warnings),
    };
    for warning in &layout_warnings {
        tracing::warn!(driver = %manifest.id, "{warning}");
    }

    // settings/sidecars/capabilities/filesystem は plugin/driver 共通の
    // 組み立て方(`build_initial_buffers` のドキュメント参照)。この見た目の
    // 重複は `Registry::refresh_sidecar_runtime` 等(承認・設定変更のたびに
    // 作り直す更新用)とも意図的に共通化していない -- 依存するライフサイクル
    // の起点が異なるため。
    let InitialBuffers {
        settings_json,
        capabilities_json,
        sidecars_json,
        filesystem_json,
    } = build_initial_buffers(
        manifest,
        settings_store,
        grants_store,
        sidecar_config_store,
        filesystem_config_store,
    );

    // バスのメッセージと submit 完了通知を 1 本に混ぜる作業キュー
    // (`DriverWork` のドキュメントコメント参照)。
    let (work_tx, work_rx) = channel_for::<DriverWork>(admit_driver_work);
    let (ready_tx, ready_rx) = std_mpsc::channel::<DriverState>();

    // Bus への登録はスレッド起動前、`load`/`init` の成否が分かるより先に行う
    // (`start_drivers` のドキュメント参照: プラグイン起動より前に全ドライバの
    // 登録を終える必要がある)。
    bus.register_driver(
        &manifest.id,
        manifest.topics.clone(),
        DriverQueueSink(work_tx.clone()),
    );

    thread::spawn({
        let host = host.clone();
        let manifest = manifest.clone();
        let settings_json = settings_json.clone();
        let capabilities_json = capabilities_json.clone();
        let sidecars_json = sidecars_json.clone();
        let filesystem_json = filesystem_json.clone();
        let bus = bus.clone();
        let registry = registry.clone();
        move || {
            run_driver_thread(
                host,
                manifest,
                entry_path,
                settings_json,
                capabilities_json,
                sidecars_json,
                filesystem_json,
                bus,
                registry,
                work_rx,
                work_tx,
                ready_tx,
            );
        }
    });

    let state = ready_rx.recv().unwrap_or_else(|_| DriverState::Disabled {
        reason: "driver thread exited before reporting an init result".to_string(),
    });

    if matches!(state, DriverState::Disabled { .. }) {
        // load/init に失敗したドライバのスレッドはもう `messages_rx` を読ま
        // ない。登録済みの bus スロットを `available: true` のまま放置すると、
        // `get` はいつまでも古い/存在しない値を返し続け、プラグイン側が
        // 「まだ更新が来ていないだけ」なのか「もう誰も更新しない」のかを
        // 区別できない(`DriverRegistry::set_disabled` のドキュメント参照)。
        bus.disable_driver(&manifest.id);
    }

    registry.push(DriverEntry {
        manifest: manifest.clone(),
        state,
        settings_json,
        capabilities_json,
        sidecars_json,
        filesystem_json,
        layout,
    });
}

/// ドライバ専用スレッドの本体。`load` → `call_init` → メッセージループを
/// 直列に実行する。すべての wasm 呼び出しはこのスレッド上でのみ発生する。
#[allow(clippy::too_many_arguments)]
fn run_driver_thread(
    host: Arc<DriverHost>,
    manifest: DriverManifest,
    entry_path: PathBuf,
    settings_json: Arc<Mutex<String>>,
    capabilities_json: Arc<Mutex<String>>,
    sidecars_json: Arc<Mutex<String>>,
    filesystem_json: Arc<Mutex<String>>,
    bus: Bus,
    registry: DriverRegistry,
    work_rx: WorkReceiver<DriverWork>,
    work_tx: WorkSender<DriverWork>,
    ready_tx: std_mpsc::Sender<DriverState>,
) {
    // submit 系ジョブの共有状態。ドライバはインスタンス再作成が無いため
    // 世代は進まないが、プラグイン側と同じ型・同じ照合で対称に扱う。
    let jobs = crate::host::plugin::PluginJobs::new();

    let ctx = DriverCtx::new(
        manifest.id.clone(),
        settings_json,
        capabilities_json,
        sidecars_json,
        filesystem_json,
        bus,
        host.http_driver(),
        host.process_driver(),
        host.fs_driver(),
        work_tx,
        jobs.clone(),
    );
    let mut instance = match host.load(&entry_path, ctx) {
        Ok(instance) => instance,
        Err(e) => {
            let _ = ready_tx.send(DriverState::Disabled {
                reason: format!("failed to load driver component: {e}"),
            });
            return;
        }
    };

    if let Err(e) = instance.call_init() {
        let reason = format!("init() failed: {e}");
        // Minor: 最終レビューで見つかった取りこぼし。`init()` はドライバの
        // 唯一のセットアップフックであり、ここで trap する前に(ホスト関数
        // 経由で)サイドカーを起動していた場合、`ready_tx.send` するだけで
        // 素通りすると誰もそれを止めない -- このスレッドはここで終了し、
        // `messages_rx` を読むループにも到達しないので、`Disabled` になった
        // 後にメッセージが来て `set_disabled` が呼ばれる経路も無い。
        // `registry.set_disabled` はまだ `registry.push` されていない
        // (`load_and_run_driver` はこのスレッドが `ready_tx.send` で結果を
        // 返してから `push` する)エントリに対しても、`manifest` 経由で
        // バス切断・サイドカー停止を必ず行う設計になっている
        // (`DriverRegistry::set_disabled` のドキュメント参照)ので、ここで
        // 呼んでも安全かつ十分。
        registry.set_disabled(&manifest, reason.clone());
        let _ = ready_tx.send(DriverState::Disabled { reason });
        return;
    }

    if ready_tx.send(DriverState::Running).is_err() {
        // start_drivers 側が既に受信を諦めている(通常起こらない)。
        return;
    }

    // 全センダー切断(`Err`)は起こらない想定(自分の `DriverCtx` が送信側を
    // 持つため)だが、起きても素直に終了する。通常の終了経路は
    // `DriverWork::Disconnected`(`DriverQueueSink` の `Drop`)。
    while let Ok(work) = work_rx.recv() {
        let (call, result) = match &work {
            DriverWork::Message(message) => (
                "on-message",
                instance.call_on_message(&message.from, &message.topic, &message.payload),
            ),
            DriverWork::JobComplete {
                generation,
                job_id,
                result_json,
            } => {
                // 旧世代の完了を捨てる照合(プラグイン側と対称)。ドライバは
                // インスタンス再作成が無いため実際には常に一致する。
                if *generation != jobs.current_generation() {
                    tracing::debug!(
                        driver_id = %manifest.id,
                        job_id,
                        "dropping a job completion from a previous instance generation"
                    );
                    continue;
                }
                (
                    "on-job-complete",
                    instance.call_on_job_complete(*job_id, result_json),
                )
            }
            DriverWork::Disconnected => break,
        };
        if let Err(e) = result {
            tracing::warn!(
                driver_id = %manifest.id,
                "{call} call failed, disabling driver: {e}"
            );
            registry.set_disabled(&manifest, format!("{call} call failed: {e}"));
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin_message(from: &str) -> DriverWork {
        DriverWork::Message(Message {
            from: from.to_string(),
            topic: "t".to_string(),
            payload: Vec::new(),
        })
    }

    /// `admit_driver_work` の境界: プラグイン発メッセージは容量で弾く
    /// (`publish` の `queue-full` になる)が、ホスト発・完了通知・切断
    /// センチネルは満杯でも受け入れる。
    mod admit_driver_work_tests {
        use super::*;

        #[test]
        fn plugin_messages_hit_the_capacity_limit() {
            assert_eq!(
                admit_driver_work(DRIVER_MESSAGE_QUEUE_CAPACITY - 1, &plugin_message("p")),
                Admit::Accept
            );
            assert_eq!(
                admit_driver_work(DRIVER_MESSAGE_QUEUE_CAPACITY, &plugin_message("p")),
                Admit::DropNewest
            );
        }

        #[test]
        fn host_messages_and_completions_are_accepted_even_at_capacity() {
            assert_eq!(
                admit_driver_work(DRIVER_MESSAGE_QUEUE_CAPACITY, &plugin_message(HOST_SENDER)),
                Admit::Accept
            );
            assert_eq!(
                admit_driver_work(
                    DRIVER_MESSAGE_QUEUE_CAPACITY,
                    &DriverWork::JobComplete {
                        generation: 0,
                        job_id: 1,
                        result_json: "{}".to_string(),
                    }
                ),
                Admit::Accept
            );
            assert_eq!(
                admit_driver_work(DRIVER_MESSAGE_QUEUE_CAPACITY, &DriverWork::Disconnected),
                Admit::Accept
            );
        }
    }

    /// `Bus::publish` のバックプレッシャ契約の保存: sink 経由でも、キュー
    /// 満杯の publish は `queue-full` エラーとして呼び出し元へ返る(捨てない)。
    #[test]
    fn publish_through_the_queue_sink_returns_queue_full_when_full() {
        let bus = Bus::new();
        let (work_tx, work_rx) = channel_for::<DriverWork>(admit_driver_work);
        bus.register_driver(
            "d",
            vec![edlr_driver_channel::TopicSpec {
                name: "t".into(),
                retain: false,
                description: String::new(),
            }],
            DriverQueueSink(work_tx),
        );

        for _ in 0..DRIVER_MESSAGE_QUEUE_CAPACITY {
            bus.publish("p", "d", "t", Vec::new()).expect("fits");
        }
        assert!(matches!(
            bus.publish("p", "d", "t", Vec::new()),
            Err(edlr_driver_channel::BusError::QueueFull(_))
        ));

        // ホスト発の合成メッセージ(sidecar-ready)は満杯でも通る。
        bus.notify_from_host("d", "sidecar-ready", Vec::new())
            .expect("host notifications must not be rejected by a full queue");
        drop(work_rx);
    }

    /// `Bus` が sink を手放したら(登録差し替え)、`Disconnected` センチネルが
    /// キューへ届き受信ループが終了できる。かつての「チャネル切断で `for`
    /// ループ終了」の置き換えの検証。
    #[test]
    fn dropping_the_sink_delivers_a_disconnected_sentinel() {
        let bus = Bus::new();
        let (work_tx, work_rx) = channel_for::<DriverWork>(admit_driver_work);
        bus.register_driver("d", Vec::new(), DriverQueueSink(work_tx.clone()));

        // 同じ id で登録し直すと古い sink が drop される。
        let (new_tx, _new_rx) = channel_for::<DriverWork>(admit_driver_work);
        bus.register_driver("d", Vec::new(), DriverQueueSink(new_tx));

        match work_rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(DriverWork::Disconnected) => {}
            other => panic!("expected DriverWork::Disconnected, got {other:?}"),
        }
        drop(work_tx);
    }

    #[test]
    fn forward_sidecar_ready_delivers_to_the_driver_queue_as_host() {
        let bus = Bus::new();
        let (tx, rx) = std_mpsc::sync_channel::<Message>(4);
        bus.register_driver("coeiroink", vec![], tx);

        forward_sidecar_ready(
            &bus,
            edlr_driver_process::ReadyEvent {
                key: "coeiroink/worker".to_string(),
                index: 0,
                port: 50021,
            },
        );

        let msg = rx.try_recv().expect("sidecar-ready must be queued");
        assert_eq!(msg.from, edlr_driver_channel::HOST_SENDER);
        assert_eq!(msg.topic, "sidecar-ready");
        let payload: serde_json::Value = serde_json::from_slice(&msg.payload).unwrap();
        assert_eq!(payload["name"], "worker");
        assert_eq!(payload["index"], 0);
        assert_eq!(payload["port"], 50021);
    }

    /// key が `<driver-id>/<sidecar-name>` の形でない(想定外の呼び出し元)
    /// 場合は、panic せず黙って捨てる。
    #[test]
    fn forward_sidecar_ready_drops_an_unrecognized_key() {
        let bus = Bus::new();
        let (tx, rx) = std_mpsc::sync_channel::<Message>(4);
        bus.register_driver("coeiroink", vec![], tx);

        forward_sidecar_ready(
            &bus,
            edlr_driver_process::ReadyEvent {
                key: "no-slash-here".to_string(),
                index: 0,
                port: 50021,
            },
        );

        assert!(rx.try_recv().is_err(), "nothing must be delivered");
    }

    #[test]
    fn a_missing_drivers_dir_yields_an_empty_registry() {
        let registry = start_drivers_for_test(std::path::Path::new("/nonexistent/edlr-drivers"));
        assert!(registry.list().is_empty());
    }

    #[test]
    fn an_invalid_driver_dir_is_skipped_without_failing_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("broken")).unwrap();
        std::fs::write(dir.path().join("broken/driver.toml"), "not toml {{{").unwrap();

        // A second, *manifest-valid* driver dir alongside the broken one.
        // Its `entry` file is not real wasm, so manifest validation passes
        // (the file merely needs to exist) but `DriverHost::load` will fail,
        // landing it `Disabled`. That's still enough to prove the scan
        // continued past the broken directory -- without this fixture, the
        // test only proved "the broken driver is skipped", never "the rest
        // still load" (it would have passed even if one bad `driver.toml`
        // aborted the whole scan, since the fixture had nothing else to
        // load).
        let valid_dir = dir.path().join("ed-state");
        std::fs::create_dir(&valid_dir).unwrap();
        std::fs::write(valid_dir.join("driver.wasm"), b"not real wasm").unwrap();
        std::fs::write(
            valid_dir.join("driver.toml"),
            "id = \"ed-state\"\nname = \"ED State\"\nversion = \"0.1.0\"\nentry = \"driver.wasm\"\n",
        )
        .unwrap();

        let registry = start_drivers_for_test(dir.path());
        let infos = registry.list();
        assert_eq!(
            infos.len(),
            1,
            "the broken driver dir must be skipped, but the valid one must still load"
        );
        assert_eq!(infos[0].manifest.id, "ed-state");
        assert!(
            matches!(
                &infos[0].state,
                DriverState::Disabled { reason } if reason.contains("failed to load driver component")
            ),
            "the entry file isn't real wasm, so the driver must land Disabled \
             with a load-failure reason, not Running; got {:?}",
            infos[0].state
        );
    }

    fn start_drivers_for_test(dir: &std::path::Path) -> DriverRegistry {
        let tmp = tempfile::tempdir().unwrap();
        start_drivers(
            dir,
            SettingsStore::new(tmp.path().join("settings")),
            SidecarConfigStore::new(tmp.path().join("settings")),
            FilesystemConfigStore::new(tmp.path().join("settings"), vec![tmp.path().to_path_buf()]),
            GrantsStore::new_for_drivers(tmp.path().join("grants")),
            edlr_driver_channel::Bus::new(),
            DriverHost::new(crate::host::drivers::test_handle()).expect("wasmtime engine builds"),
        )
    }
}

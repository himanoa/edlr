//! journal イベントとバス配信をプラグインの作業キュー(`PluginWork`)へ
//! 転送する購読タスク群。`spawn_event_subscriber`(router の broadcast →
//! キュー)と `spawn_bus_subscriber`(`Bus` の配信 → キュー)が対称の形で
//! 並ぶ(親モジュールのドキュメントコメント参照)。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::time::Duration;

use tokio::sync::broadcast;

use edlr_driver_channel::{Bus, Delivery};

use crate::event::Event;
use crate::manifest::{matches_event, Manifest};
use crate::runner::plugin::queue::{PluginWorkSender, PushError};
use crate::runtime::bus::parse_bus;
use crate::runtime::dropped::DropCounters;

use super::{PluginWork, PLUGIN_WORK_QUEUE_CAPACITY};

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
pub(super) fn spawn_event_subscriber(
    manifest: Manifest,
    mut rx: broadcast::Receiver<Arc<Event>>,
    work_tx: PluginWorkSender,
    drops: Arc<DropCounters>,
) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if !matches_event(&manifest.events, &event) {
                        continue;
                    }
                    match work_tx.push(PluginWork::Event(event)) {
                        Ok(evicted) => record_eviction(&drops, &manifest.id, evicted),
                        Err(PushError::Dropped) => {
                            // lossy キューは DropOldest なのでここには来ない
                            // はずだが、数え漏らすよりは数えておく。
                            drops.record_event_drop();
                        }
                        Err(PushError::Disconnected) => {
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
pub(super) fn spawn_bus_subscriber(
    manifest: Manifest,
    bus_json: Arc<Mutex<String>>,
    delivery_rx: std_mpsc::Receiver<Delivery>,
    work_tx: PluginWorkSender,
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
        let still_granted = crate::host::resolve::check_bus_permission(
            &entries,
            &delivery.driver_id,
            &delivery.topic,
            crate::host::resolve::BusDirection::Subscribe,
        )
        .is_ok();
        if !still_granted {
            // 承認が取り消された(か、そもそも一度も承認されていない)。
            // 黙って捨てる -- `check_bus` が publish/get 側で同じ状況を
            // `permission-denied` として扱うのと違い、こちらはドライバ
            // 起点のプッシュ配信なので呼び出し元に返すエラーが無い。
            continue;
        }
        match work_tx.push(PluginWork::Message(delivery)) {
            Ok(evicted) => record_eviction(&drops, &manifest.id, evicted),
            Err(PushError::Dropped) => {
                // lossy キューは DropOldest なのでここには来ないはずだが、
                // 数え漏らすよりは数えておく。
                drops.record_bus_delivery_drop();
            }
            Err(PushError::Disconnected) => {
                // プラグインスレッドが終了(disabled)済み。
                break;
            }
        }
    });
}

/// lossy キューが満杯時に追い出した最古の仕事を drop counter へ数える
/// (issue-hdly)。押し込んだ種別ではなく**追い出された**種別で数える --
/// イベントの push がバス配信を追い出すこともあるため。
///
/// journal イベントの読み取り位置は配送の成否と独立に進むので、ここで
/// 捨てたイベントは replay でも戻らない。バス配信も再送されない。どちらも
/// `plugins/list` の `dropped` に出すために数えておく。
pub(crate) fn record_eviction(
    drops: &DropCounters,
    plugin_id: &str,
    evicted: Option<PluginWork>,
) {
    match evicted {
        None => {}
        Some(PluginWork::Event(_)) => {
            drops.record_event_drop();
            tracing::warn!(
                plugin_id = %plugin_id,
                "work queue full ({PLUGIN_WORK_QUEUE_CAPACITY} pending), \
                 evicted the oldest event for a slow/blocked plugin"
            );
        }
        Some(PluginWork::Message(_)) => {
            drops.record_bus_delivery_drop();
            tracing::warn!(
                plugin_id = %plugin_id,
                "work queue full ({PLUGIN_WORK_QUEUE_CAPACITY} pending), \
                 evicted the oldest bus delivery for a slow/blocked plugin"
            );
        }
        // Stop / JobComplete は evictable ではない(queue::channel 参照)。
        Some(other) => {
            tracing::error!(
                plugin_id = %plugin_id,
                "BUG: work queue evicted a non-droppable item: {other:?}"
            );
        }
    }
}

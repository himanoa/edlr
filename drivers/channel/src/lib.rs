//! プラグイン間バスの純ロジック。購読表・retained ストア・キュー方針を持つ。
//!
//! **wasmtime にも tokio にも依存しない**。承認(grants)の判定は `core` 側の
//! 責務で、このクレートは「誰が誰に送れるか」を知らない。`core` が承認済みの
//! 呼び出しだけをここに通す(`drivers/fs` が mode を知らないのと同じ分担)。

pub mod topic;

pub use topic::TopicSpec;

/// 1 メッセージのペイロード上限(256 KiB)。バスは制御メッセージの経路で
/// あり、大きなデータの受け渡しは `driver-fs` の担当という切り分け。
pub const BUS_MAX_PAYLOAD: usize = 256 * 1024;

/// ホストが合成するメッセージの予約送信元 id。`sidecar-ready` などの
/// ホスト発通知はこの `from` で届く。なりすましを塞ぐため、core の
/// manifest 検証はこの id を持つプラグイン/ドライバを拒否する。
pub const HOST_SENDER: &str = "host";

use std::collections::BTreeMap;
use std::fmt;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};

/// `MessageSink::try_send` の失敗。メッセージ本体は返さない(呼び出し側は
/// エラー種別しか使わないため)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkError {
    /// 受け口が満杯でメッセージを受け取れなかった。`publish` は
    /// `queue-full` として呼び出し元へ返す(捨てない)。
    Full,
    /// 受け口が閉じている(ドライバスレッド終了済みなど)。
    Closed,
}

/// `Bus` がドライバの受信キューへメッセージを渡すための口。
///
/// かつては `SyncSender<Message>` 固定だったが、core 側がドライバの作業
/// キュー(journal ならぬ bus メッセージと submit 完了通知を 1 本に混ぜる
/// 自作キュー)を直接受け口にできるよう trait にした。`SyncSender<Message>`
/// への実装は残してあるので、テストや単純な用途はそのまま使える。
///
/// **`Drop` に意味を持たせてよい**: `Bus` が sink を手放すのは
/// `register_driver` による差し替え時と `Bus` 自体の破棄時だけなので、
/// 実装側は `Drop` で「受け口が閉じた」ことを下流へ知らせられる
/// (core のドライバキュー sink はこれで受信ループを終了させる)。
pub trait MessageSink: Send + Sync {
    fn try_send(&self, message: Message) -> Result<(), SinkError>;
}

impl MessageSink for SyncSender<Message> {
    fn try_send(&self, message: Message) -> Result<(), SinkError> {
        SyncSender::try_send(self, message).map_err(|e| match e {
            TrySendError::Full(_) => SinkError::Full,
            TrySendError::Disconnected(_) => SinkError::Closed,
        })
    }
}

/// バスの呼び出しが返しうるエラー。WIT の `bus-error` variant と 1 対 1 で対応する。
#[derive(Debug, Clone, PartialEq)]
pub enum BusError {
    UnknownDriver(String),
    UnknownTopic(String),
    DriverUnavailable(String),
    QueueFull(String),
    TooLarge(String),
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BusError::UnknownDriver(m) => write!(f, "unknown driver: {m}"),
            BusError::UnknownTopic(m) => write!(f, "unknown topic: {m}"),
            BusError::DriverUnavailable(m) => write!(f, "driver unavailable: {m}"),
            BusError::QueueFull(m) => write!(f, "queue full: {m}"),
            BusError::TooLarge(m) => write!(f, "payload too large: {m}"),
        }
    }
}

impl std::error::Error for BusError {}

/// プラグイン → ドライバへ流れる 1 件。
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub from: String,
    pub topic: String,
    pub payload: Vec<u8>,
}

/// ドライバ → プラグインへ流れる 1 件。
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub plugin_id: String,
    pub driver_id: String,
    pub topic: String,
    pub payload: Vec<u8>,
}

struct DriverSlot {
    topics: Vec<TopicSpec>,
    sender: Arc<dyn MessageSink>,
    retained: BTreeMap<String, Vec<u8>>,
    available: bool,
}

impl DriverSlot {
    fn topic(&self, name: &str) -> Option<&TopicSpec> {
        self.topics.iter().find(|t| t.name == name)
    }
}

struct Subscription {
    plugin_id: String,
    driver_id: String,
    topic: String,
    sender: SyncSender<Delivery>,
}

#[derive(Default)]
struct BusState {
    drivers: BTreeMap<String, DriverSlot>,
    subscriptions: Vec<Subscription>,
}

/// プラグインとドライバの間のメッセージ経路。`Clone` は内部状態を共有する。
#[derive(Clone, Default)]
pub struct Bus {
    state: Arc<Mutex<BusState>>,
}

impl Bus {
    pub fn new() -> Bus {
        Bus::default()
    }

    /// バス状態のロックを取る。poison からは中身を取り出して回復する。
    ///
    /// `expect` で panic すると、ロック下で一度でも panic した以降は
    /// **全てのバス操作**が呼び出し元のスレッドを道連れに abort する。
    /// バスは縮退動作(取りこぼした購読があっても他は動く)であるべきなので、
    /// `core` 側(`plugin/registry.rs` の `plugin_threads` など)と同じく
    /// `into_inner()` で回復する。
    fn lock_state(&self) -> MutexGuard<'_, BusState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn register_driver(
        &self,
        driver_id: &str,
        topics: Vec<TopicSpec>,
        sender: impl MessageSink + 'static,
    ) {
        let mut state = self.lock_state();
        state.drivers.insert(
            driver_id.to_string(),
            DriverSlot {
                topics,
                sender: Arc::new(sender),
                retained: BTreeMap::new(),
                available: true,
            },
        );
    }

    pub fn subscribe(
        &self,
        plugin_id: &str,
        driver_id: &str,
        topic: &str,
        sender: SyncSender<Delivery>,
    ) {
        let mut state = self.lock_state();
        state.subscriptions.push(Subscription {
            plugin_id: plugin_id.to_string(),
            driver_id: driver_id.to_string(),
            topic: topic.to_string(),
            sender,
        });
    }

    pub fn publish(
        &self,
        from_plugin: &str,
        driver_id: &str,
        topic: &str,
        payload: Vec<u8>,
    ) -> Result<(), BusError> {
        if payload.len() > BUS_MAX_PAYLOAD {
            return Err(BusError::TooLarge(format!(
                "{} bytes exceeds the {BUS_MAX_PAYLOAD} byte limit",
                payload.len()
            )));
        }
        let state = self.lock_state();
        let slot = state
            .drivers
            .get(driver_id)
            .ok_or_else(|| BusError::UnknownDriver(driver_id.to_string()))?;
        if !slot.available {
            return Err(BusError::DriverUnavailable(driver_id.to_string()));
        }
        if slot.topic(topic).is_none() {
            return Err(BusError::UnknownTopic(format!("{driver_id}/{topic}")));
        }
        let message = Message {
            from: from_plugin.to_string(),
            topic: topic.to_string(),
            payload,
        };
        // 満杯なら捨てずにエラーを返す。publish は結果を返せる同期呼び出し
        // なので、呼び出し側が再送するか諦めるかを選べる(設計書参照)。
        match slot.sender.try_send(message) {
            Ok(()) => Ok(()),
            Err(SinkError::Full) => Err(BusError::QueueFull(format!("{driver_id}/{topic}"))),
            Err(SinkError::Closed) => {
                Err(BusError::DriverUnavailable(driver_id.to_string()))
            }
        }
    }

    /// ホスト発の合成メッセージ(`from = HOST_SENDER`)をドライバの
    /// キューへ届ける。ホスト予約経路なのでトピック宣言のチェックは行わない
    /// (`sidecar-ready` は manifest に宣言されない)。送信そのものはバスの
    /// ロックを手放してから行う。満杯時の扱いは下のコメント参照。
    pub fn notify_from_host(
        &self,
        driver_id: &str,
        topic: &str,
        payload: Vec<u8>,
    ) -> Result<(), BusError> {
        let sender = {
            let state = self.lock_state();
            let slot = state
                .drivers
                .get(driver_id)
                .ok_or_else(|| BusError::UnknownDriver(driver_id.to_string()))?;
            if !slot.available {
                return Err(BusError::DriverUnavailable(driver_id.to_string()));
            }
            slot.sender.clone()
        };
        let message = Message {
            from: HOST_SENDER.to_string(),
            topic: topic.to_string(),
            payload,
        };
        // かつては `SyncSender::send`(満杯ならブロック)だったが、sink 化に
        // 伴い try_send に変更した。core のドライバキュー sink はホスト発
        // (`from == HOST_SENDER`)のメッセージを満杯でも受け入れる
        // (admit 判定)ので、実運用で Full になるのは素の `SyncSender` を
        // 使うテスト経路だけ。届かなかった ready 通知は呼び出し側の保険経路
        // (`forward_sidecar_ready` のドキュメント参照)が拾う。
        sender.try_send(message).map_err(|e| match e {
            SinkError::Full => BusError::QueueFull(format!("{driver_id}/{topic}")),
            SinkError::Closed => BusError::DriverUnavailable(driver_id.to_string()),
        })
    }

    pub fn get(&self, driver_id: &str, topic: &str) -> Result<Option<Vec<u8>>, BusError> {
        let state = self.lock_state();
        let slot = state
            .drivers
            .get(driver_id)
            .ok_or_else(|| BusError::UnknownDriver(driver_id.to_string()))?;
        if !slot.available {
            return Err(BusError::DriverUnavailable(driver_id.to_string()));
        }
        if slot.topic(topic).is_none() {
            return Err(BusError::UnknownTopic(format!("{driver_id}/{topic}")));
        }
        Ok(slot.retained.get(topic).cloned())
    }

    pub fn emit(&self, driver_id: &str, topic: &str, payload: Vec<u8>) -> Result<(), BusError> {
        if payload.len() > BUS_MAX_PAYLOAD {
            return Err(BusError::TooLarge(format!(
                "{} bytes exceeds the {BUS_MAX_PAYLOAD} byte limit",
                payload.len()
            )));
        }
        let mut state = self.lock_state();
        let slot = state
            .drivers
            .get_mut(driver_id)
            .ok_or_else(|| BusError::UnknownDriver(driver_id.to_string()))?;
        if !slot.available {
            return Err(BusError::DriverUnavailable(driver_id.to_string()));
        }
        let spec = slot
            .topic(topic)
            .ok_or_else(|| BusError::UnknownTopic(format!("{driver_id}/{topic}")))?
            .clone();

        // retained の更新はキューとは独立に必ず行う。配信を取りこぼした
        // プラグインも `get` で最新値を拾えるようにするため(設計書参照)。
        if spec.retain {
            slot.retained.insert(topic.to_string(), payload.clone());
        }

        // 切断済み(受信側が既に消えた)購読を検出したらここで刈り取る。
        // `unsubscribe_plugin` の呼び忘れ・呼べない経路(プラグインが
        // `set_disabled` を経由せず単に応答しなくなった場合など)があっても、
        // 次にこの driver/topic へ `emit` されたタイミングで自己修復する
        // (`unsubscribe_plugin` のドキュメントコメント参照: 主経路はそちらで、
        // これは保険)。
        let mut stale_indices = Vec::new();
        for (index, subscription) in state.subscriptions.iter().enumerate() {
            if subscription.driver_id != driver_id || subscription.topic != topic {
                continue;
            }
            let delivery = Delivery {
                plugin_id: subscription.plugin_id.clone(),
                driver_id: driver_id.to_string(),
                topic: topic.to_string(),
                payload: payload.clone(),
            };
            match subscription.sender.try_send(delivery) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    // 遅いプラグイン 1 個がドライバ全体を止めないよう、満杯
                    // なら捨てる(publish 方向と非対称。設計書参照)。
                    // 呼び出し側(core)が warn ログを出せるよう、ここでは
                    // 黙って捨てる。
                    //
                    // **捨てるのは「今まさに送ろうとしている最新の 1 件」で
                    // あって、キューに既に並んでいる古い方ではない**
                    // (Important: 最終レビューで見つかった、README・設計書の
                    // 記述違い)。`std::sync::mpsc::SyncSender::try_send` は
                    // `Full` のとき送信しようとした値をそのまま呼び出し元に
                    // 返すだけで、キューの中身を覗いて古い要素を追い出す API
                    // ではない -- 実装できるドロップ方向は「送ろうとした値を
                    // 捨てる(＝最新を捨てる)」の一択で、これはそのまま最も
                    // 単純な実装になっている。`retain = false` のトピックは
                    // `get` に読み出しのフォールバックが無いため、詰まった
                    // 購読プラグインはこの間の更新を(古いものだけでなく)
                    // 一切受け取れない。詳細は docs/drivers.md
                    // 「キュー方針の非対称」節を参照。
                }
                Err(TrySendError::Disconnected(_)) => {
                    stale_indices.push(index);
                }
            }
        }
        if !stale_indices.is_empty() {
            let stale_indices: std::collections::HashSet<usize> =
                stale_indices.into_iter().collect();
            let mut index = 0usize;
            state.subscriptions.retain(|_| {
                let keep = !stale_indices.contains(&index);
                index += 1;
                keep
            });
        }
        Ok(())
    }

    /// `plugin_id` の全購読を購読表から取り除く。プラグインが無効化される
    /// (`crate::plugin::runner` の `run_plugin_thread` が trap を検出して
    /// `Registry::set_disabled` を呼ぶ)などして「消える」タイミングで
    /// 呼ぶことを想定した主経路。
    ///
    /// **なぜ必要か**: 購読の送信側(`Sender<Delivery>`、
    /// `crate::plugin::runner::spawn_bus_subscriber` が保持する受信側と対)は
    /// `subscribe` を呼んだ時点でこの `Bus` の購読表に永久にクローンされて
    /// 残る。プラグイン側の受信タスクが自発的に終了しても、送信側の
    /// `Sender` はこの購読表からは見えない別の場所(プラグイン専用スレッドの
    /// ローカル変数)にあり、この `Bus` からはそれが消えたことを知りようが
    /// ない。呼び出しを怠ると、無効化されたプラグインの購読エントリが
    /// プロセスの生存期間中ずっと購読表に残り続け、`emit` のたびに無駄な
    /// `try_send` を試みることになる(実害は限定的だが、蓄積すれば `emit`
    /// の 1 回あたりコストが線形に増える)。
    ///
    /// `emit` 自身も `TrySendError::Disconnected` を見つけた購読を刈り取る
    /// (上のコメント参照)が、それはその driver/topic へ次に `emit` される
    /// までの間は効かない保険であり、こちらが主経路。
    pub fn unsubscribe_plugin(&self, plugin_id: &str) {
        let mut state = self.lock_state();
        state
            .subscriptions
            .retain(|subscription| subscription.plugin_id != plugin_id);
    }

    pub fn disable_driver(&self, driver_id: &str) {
        let mut state = self.lock_state();
        if let Some(slot) = state.drivers.get_mut(driver_id) {
            slot.available = false;
            // 死んだドライバの古い状態を `get` が返し続けると、プラグイン側が
            // 「更新が止まっているだけ」と「もう誰も更新しない」を区別できない。
            slot.retained.clear();
        }
    }

    pub fn retained_for(&self, driver_id: &str, topic: &str) -> Option<Vec<u8>> {
        let state = self.lock_state();
        state.drivers.get(driver_id)?.retained.get(topic).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    fn topics() -> Vec<TopicSpec> {
        vec![
            TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            },
            TopicSpec {
                name: "ship-status".into(),
                retain: false,
                description: String::new(),
            },
        ]
    }

    fn bus_with_driver(capacity: usize) -> (Bus, std::sync::mpsc::Receiver<Message>) {
        let bus = Bus::new();
        let (tx, rx) = sync_channel::<Message>(capacity);
        bus.register_driver("ed-state", topics(), tx);
        (bus, rx)
    }

    /// ロック下で panic した後もバスが使えること。`expect` だと以後の
    /// 全操作が panic の連鎖になり、縮退動作にならない。
    #[test]
    fn bus_keeps_working_after_a_poisoned_lock() {
        let (bus, rx) = bus_with_driver(4);

        let poisoner = bus.clone();
        let result = std::thread::spawn(move || {
            let _guard = poisoner.lock_state();
            panic!("poison the bus lock");
        })
        .join();
        assert!(result.is_err(), "スレッドは panic しているはず");

        bus.publish(
            "plugin-a",
            "ed-state",
            "ship-status",
            b"still alive".to_vec(),
        )
        .expect("poison 後も publish できること");
        assert_eq!(rx.recv().unwrap().payload, b"still alive".to_vec());
    }

    #[test]
    fn publish_delivers_to_the_driver_queue() {
        let (bus, rx) = bus_with_driver(4);
        bus.publish("translator", "ed-state", "ship-status", b"hi".to_vec())
            .unwrap();
        let msg = rx.try_recv().expect("message queued");
        assert_eq!(msg.from, "translator");
        assert_eq!(msg.topic, "ship-status");
        assert_eq!(msg.payload, b"hi".to_vec());
    }

    #[test]
    fn notify_from_host_delivers_with_from_host_and_without_a_declared_topic() {
        let (bus, rx) = bus_with_driver(4);
        // "sidecar-ready" は topics() に宣言されていないが、ホスト予約経路は通る。
        bus.notify_from_host("ed-state", "sidecar-ready", b"{}".to_vec())
            .unwrap();
        let msg = rx.try_recv().expect("message queued");
        assert_eq!(msg.from, HOST_SENDER);
        assert_eq!(msg.topic, "sidecar-ready");
        assert_eq!(msg.payload, b"{}".to_vec());
    }

    #[test]
    fn notify_from_host_to_an_unknown_driver_is_rejected() {
        let bus = Bus::new();
        assert!(matches!(
            bus.notify_from_host("nope", "sidecar-ready", vec![]),
            Err(BusError::UnknownDriver(_))
        ));
    }

    #[test]
    fn notify_from_host_to_a_disabled_driver_is_rejected() {
        let (bus, _rx) = bus_with_driver(4);
        bus.disable_driver("ed-state");
        assert!(matches!(
            bus.notify_from_host("ed-state", "sidecar-ready", vec![]),
            Err(BusError::DriverUnavailable(_))
        ));
    }

    #[test]
    fn publish_to_unknown_driver_or_topic_is_rejected() {
        let (bus, _rx) = bus_with_driver(4);
        assert!(matches!(
            bus.publish("p", "nope", "ship-status", vec![]),
            Err(BusError::UnknownDriver(_))
        ));
        assert!(matches!(
            bus.publish("p", "ed-state", "nope", vec![]),
            Err(BusError::UnknownTopic(_))
        ));
    }

    #[test]
    fn publish_returns_queue_full_instead_of_dropping() {
        let (bus, _rx) = bus_with_driver(1);
        bus.publish("p", "ed-state", "ship-status", vec![1])
            .unwrap();
        assert!(matches!(
            bus.publish("p", "ed-state", "ship-status", vec![2]),
            Err(BusError::QueueFull(_))
        ));
    }

    #[test]
    fn oversized_payloads_are_rejected() {
        let (bus, _rx) = bus_with_driver(4);
        let big = vec![0u8; BUS_MAX_PAYLOAD + 1];
        assert!(matches!(
            bus.publish("p", "ed-state", "ship-status", big),
            Err(BusError::TooLarge(_))
        ));
    }

    #[test]
    fn emit_updates_retained_and_delivers_to_subscribers() {
        let (bus, _rx) = bus_with_driver(4);
        let (tx, drx) = sync_channel::<Delivery>(4);
        bus.subscribe("translator", "ed-state", "current-system", tx);

        bus.emit("ed-state", "current-system", b"Sol".to_vec())
            .unwrap();

        let delivery = drx.try_recv().expect("subscriber got the message");
        assert_eq!(delivery.plugin_id, "translator");
        assert_eq!(delivery.payload, b"Sol".to_vec());
        assert_eq!(
            bus.get("ed-state", "current-system").unwrap(),
            Some(b"Sol".to_vec())
        );
    }

    #[test]
    fn non_retained_topics_are_not_stored() {
        let (bus, _rx) = bus_with_driver(4);
        bus.emit("ed-state", "ship-status", b"x".to_vec()).unwrap();
        assert_eq!(bus.get("ed-state", "ship-status").unwrap(), None);
    }

    /// **旧名 `emit_drops_the_oldest_delivery_when_a_subscriber_is_full` は
    /// このテストが実際に確認していることと逆だった**(Important: 最終
    /// レビューで見つかった README・設計書との記述違い、テスト名も同じ誤りを
    /// 引きずっていた)。`emit` は `SyncSender::try_send` を使っており、
    /// `Full` のとき捨てられるのは常に「今まさに送ろうとしている値」
    /// (= 最新)である -- `try_send` にはキューの中身を覗いて既に並んでいる
    /// 古い要素を追い出す手段が無い。このテストはそれを実際に確認する:
    /// 容量 1 のキューに "a" を先に届け、キューが満杯のまま "b" を送ると、
    /// 受信側に残るのは古い方の "a" であって、新しい "b" ではない。
    #[test]
    fn emit_drops_the_newest_delivery_when_a_subscriber_is_full() {
        let (bus, _rx) = bus_with_driver(4);
        let (tx, drx) = sync_channel::<Delivery>(1);
        bus.subscribe("slow", "ed-state", "current-system", tx);

        bus.emit("ed-state", "current-system", b"a".to_vec())
            .unwrap();
        // 2 通目はキューが満杯。emit 自体は成功し、retained は必ず更新される。
        bus.emit("ed-state", "current-system", b"b".to_vec())
            .unwrap();

        assert_eq!(
            bus.get("ed-state", "current-system").unwrap(),
            Some(b"b".to_vec())
        );
        // 受信側のキューには、先に届いた古い方の "a" だけが残る -- 満杯時に
        // 捨てられるのは後から送ろうとした新しい方の "b" である。
        let delivery = drx
            .try_recv()
            .expect("the already-queued older delivery must survive");
        assert_eq!(
            delivery.payload,
            b"a".to_vec(),
            "emit must drop the newest delivery (the one being sent) on a full queue, \
             not the oldest (the one already queued)"
        );
        assert!(drx.try_recv().is_err());
    }

    #[test]
    fn emit_prunes_a_disconnected_subscription_from_the_table() {
        let (bus, _rx) = bus_with_driver(4);
        {
            // 受信側を drop してすぐ切断させる。
            let (tx, _drx) = sync_channel::<Delivery>(4);
            bus.subscribe("gone", "ed-state", "current-system", tx);
        }
        let (tx, drx) = sync_channel::<Delivery>(4);
        bus.subscribe("alive", "ed-state", "current-system", tx);

        // 切断済みの購読が混ざっていても emit 自体は失敗しない。
        bus.emit("ed-state", "current-system", b"Sol".to_vec())
            .unwrap();
        assert_eq!(
            drx.try_recv().map(|d| d.plugin_id),
            Ok("alive".to_string()),
            "the still-connected subscriber must still receive the delivery"
        );

        // 内部状態を直接検査する術は無いので、2 回目の emit がまだ落ちない
        // ことと、生きている購読への配信が壊れていないことで間接的に確認する
        // (`unsubscribe_plugin_removes_only_the_matching_plugins_subscriptions`
        // が購読表からの実際の削除を直接検証する)。
        bus.emit("ed-state", "current-system", b"Jameson".to_vec())
            .unwrap();
        assert_eq!(drx.try_recv().map(|d| d.payload), Ok(b"Jameson".to_vec()));
    }

    #[test]
    fn unsubscribe_plugin_removes_only_the_matching_plugins_subscriptions() {
        let (bus, _rx) = bus_with_driver(4);
        let (tx_a, drx_a) = sync_channel::<Delivery>(4);
        let (tx_b, drx_b) = sync_channel::<Delivery>(4);
        bus.subscribe("plugin-a", "ed-state", "current-system", tx_a);
        bus.subscribe("plugin-b", "ed-state", "current-system", tx_b);

        bus.unsubscribe_plugin("plugin-a");

        bus.emit("ed-state", "current-system", b"Sol".to_vec())
            .unwrap();
        assert!(
            drx_a.try_recv().is_err(),
            "plugin-a was unsubscribed and must not receive further deliveries"
        );
        assert_eq!(
            drx_b.try_recv().map(|d| d.plugin_id),
            Ok("plugin-b".to_string()),
            "plugin-b's subscription must be unaffected"
        );
    }

    #[test]
    fn unsubscribe_plugin_with_no_matching_subscriptions_is_a_no_op() {
        let (bus, _rx) = bus_with_driver(4);
        // 何も購読していない状態で呼んでもパニックしない。
        bus.unsubscribe_plugin("nobody");
    }

    #[test]
    fn emit_with_no_subscribers_succeeds() {
        let (bus, _rx) = bus_with_driver(4);
        assert!(bus
            .emit("ed-state", "current-system", b"Sol".to_vec())
            .is_ok());
    }

    #[test]
    fn emit_to_an_undeclared_topic_is_rejected() {
        let (bus, _rx) = bus_with_driver(4);
        assert!(matches!(
            bus.emit("ed-state", "nope", vec![]),
            Err(BusError::UnknownTopic(_))
        ));
    }

    #[test]
    fn disabling_a_driver_drops_retained_and_fails_calls() {
        let (bus, _rx) = bus_with_driver(4);
        bus.emit("ed-state", "current-system", b"Sol".to_vec())
            .unwrap();
        bus.disable_driver("ed-state");

        assert!(matches!(
            bus.get("ed-state", "current-system"),
            Err(BusError::DriverUnavailable(_))
        ));
        assert!(matches!(
            bus.publish("p", "ed-state", "ship-status", vec![]),
            Err(BusError::DriverUnavailable(_))
        ));
    }

    #[test]
    fn retained_for_returns_the_last_value() {
        let (bus, _rx) = bus_with_driver(4);
        assert_eq!(bus.retained_for("ed-state", "current-system"), None);
        bus.emit("ed-state", "current-system", b"Sol".to_vec())
            .unwrap();
        assert_eq!(
            bus.retained_for("ed-state", "current-system"),
            Some(b"Sol".to_vec())
        );
    }
}

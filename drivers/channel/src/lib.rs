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

use std::collections::BTreeMap;
use std::fmt;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

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
    sender: SyncSender<Message>,
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

    pub fn register_driver(
        &self,
        driver_id: &str,
        topics: Vec<TopicSpec>,
        sender: SyncSender<Message>,
    ) {
        let mut state = self.state.lock().expect("bus state poisoned");
        state.drivers.insert(
            driver_id.to_string(),
            DriverSlot { topics, sender, retained: BTreeMap::new(), available: true },
        );
    }

    pub fn subscribe(
        &self,
        plugin_id: &str,
        driver_id: &str,
        topic: &str,
        sender: SyncSender<Delivery>,
    ) {
        let mut state = self.state.lock().expect("bus state poisoned");
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
        let state = self.state.lock().expect("bus state poisoned");
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
            Err(TrySendError::Full(_)) => {
                Err(BusError::QueueFull(format!("{driver_id}/{topic}")))
            }
            Err(TrySendError::Disconnected(_)) => {
                Err(BusError::DriverUnavailable(driver_id.to_string()))
            }
        }
    }

    pub fn get(&self, driver_id: &str, topic: &str) -> Result<Option<Vec<u8>>, BusError> {
        let state = self.state.lock().expect("bus state poisoned");
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
        let mut state = self.state.lock().expect("bus state poisoned");
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

        for subscription in &state.subscriptions {
            if subscription.driver_id != driver_id || subscription.topic != topic {
                continue;
            }
            let delivery = Delivery {
                plugin_id: subscription.plugin_id.clone(),
                driver_id: driver_id.to_string(),
                topic: topic.to_string(),
                payload: payload.clone(),
            };
            // 遅いプラグイン 1 個がドライバ全体を止めないよう、満杯なら
            // 捨てる(publish 方向と非対称。設計書参照)。
            if let Err(TrySendError::Full(_)) = subscription.sender.try_send(delivery) {
                // 呼び出し側(core)が warn ログを出せるよう、ここでは黙って捨てる。
            }
        }
        Ok(())
    }

    pub fn disable_driver(&self, driver_id: &str) {
        let mut state = self.state.lock().expect("bus state poisoned");
        if let Some(slot) = state.drivers.get_mut(driver_id) {
            slot.available = false;
            // 死んだドライバの古い状態を `get` が返し続けると、プラグイン側が
            // 「更新が止まっているだけ」と「もう誰も更新しない」を区別できない。
            slot.retained.clear();
        }
    }

    pub fn retained_for(&self, driver_id: &str, topic: &str) -> Option<Vec<u8>> {
        let state = self.state.lock().expect("bus state poisoned");
        state.drivers.get(driver_id)?.retained.get(topic).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    fn topics() -> Vec<TopicSpec> {
        vec![
            TopicSpec { name: "current-system".into(), retain: true, description: String::new() },
            TopicSpec { name: "ship-status".into(), retain: false, description: String::new() },
        ]
    }

    fn bus_with_driver(capacity: usize) -> (Bus, std::sync::mpsc::Receiver<Message>) {
        let bus = Bus::new();
        let (tx, rx) = sync_channel::<Message>(capacity);
        bus.register_driver("ed-state", topics(), tx);
        (bus, rx)
    }

    #[test]
    fn publish_delivers_to_the_driver_queue() {
        let (bus, rx) = bus_with_driver(4);
        bus.publish("translator", "ed-state", "ship-status", b"hi".to_vec()).unwrap();
        let msg = rx.try_recv().expect("message queued");
        assert_eq!(msg.from, "translator");
        assert_eq!(msg.topic, "ship-status");
        assert_eq!(msg.payload, b"hi".to_vec());
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
        bus.publish("p", "ed-state", "ship-status", vec![1]).unwrap();
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

        bus.emit("ed-state", "current-system", b"Sol".to_vec()).unwrap();

        let delivery = drx.try_recv().expect("subscriber got the message");
        assert_eq!(delivery.plugin_id, "translator");
        assert_eq!(delivery.payload, b"Sol".to_vec());
        assert_eq!(bus.get("ed-state", "current-system").unwrap(), Some(b"Sol".to_vec()));
    }

    #[test]
    fn non_retained_topics_are_not_stored() {
        let (bus, _rx) = bus_with_driver(4);
        bus.emit("ed-state", "ship-status", b"x".to_vec()).unwrap();
        assert_eq!(bus.get("ed-state", "ship-status").unwrap(), None);
    }

    #[test]
    fn emit_drops_the_oldest_delivery_when_a_subscriber_is_full() {
        let (bus, _rx) = bus_with_driver(4);
        let (tx, drx) = sync_channel::<Delivery>(1);
        bus.subscribe("slow", "ed-state", "current-system", tx);

        bus.emit("ed-state", "current-system", b"a".to_vec()).unwrap();
        // 2 通目はキューが満杯。emit 自体は成功し、retained は必ず更新される。
        bus.emit("ed-state", "current-system", b"b".to_vec()).unwrap();

        assert_eq!(bus.get("ed-state", "current-system").unwrap(), Some(b"b".to_vec()));
        // 受信側には 1 通しか残っていない(古い方が残るか新しい方かは問わない)。
        assert!(drx.try_recv().is_ok());
        assert!(drx.try_recv().is_err());
    }

    #[test]
    fn emit_with_no_subscribers_succeeds() {
        let (bus, _rx) = bus_with_driver(4);
        assert!(bus.emit("ed-state", "current-system", b"Sol".to_vec()).is_ok());
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
        bus.emit("ed-state", "current-system", b"Sol".to_vec()).unwrap();
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
        bus.emit("ed-state", "current-system", b"Sol".to_vec()).unwrap();
        assert_eq!(bus.retained_for("ed-state", "current-system"), Some(b"Sol".to_vec()));
    }
}

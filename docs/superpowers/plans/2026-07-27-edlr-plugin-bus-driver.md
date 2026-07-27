# プラグイン間連携機構(bus とユーザー定義ドライバ)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** プラグイン同士が、ユーザーがインストールしたドライバ(常駐 wasm コンポーネント)を介して pub/sub と共有状態のやり取りをできるようにする。

**Architecture:** 空クレートだった `drivers/channel` にバスの純ロジック(購読表・retained ストア・キュー)を実装し、`core/src/driver/` に `core/src/plugin/` と対称なドライバのロード・駆動・承認を置く。プラグインは固定 ABI `bus`(publish / get)を、ドライバは `bus-host`(emit)を使い、ホストが仲介する。retained 値はドライバの中ではなくホストが持つため、`get` はドライバの wasm を呼ばない。

**Tech Stack:** Rust 2021 / wasmtime component model + WIT / axum WebSocket RPC / React + TypeScript + vitest / Tauri 2。

**設計書:** `docs/superpowers/specs/2026-07-27-edlr-plugin-bus-driver-design.md`

## Global Constraints

- Rust edition 2021。`drivers/channel` は既にワークスペース `members` に入っている(`Cargo.toml`)
- **`drivers/channel` に wasmtime / wasm / tokio を依存させない**。純ロジックのみ(`drivers/fs` と同じ方針)。依存は `serde` / `serde_json` まで
- WIT パッケージは `edlr:plugin@0.2.0` → **`edlr:plugin@0.3.0`**。`world plugin` に `import bus` と `export on-message` が増えるため既存プラグインは全て再ビルドが必要
- ペイロード上限は **256 KiB**(`BUS_MAX_PAYLOAD`)。超過は `too-large`
- ドライバのキュー容量は **64**(`DRIVER_MESSAGE_QUEUE_CAPACITY`)。プラグインの `PLUGIN_EVENT_QUEUE_CAPACITY`(32)とは別定数
- ドライバの呼び出し期限は **30 秒**(`DriverInstance::CALL_DEADLINE`)、ドライバ用 HTTP タイムアウトは **25 秒**(`DRIVER_HTTP_TIMEOUT`)。`DRIVER_HTTP_TIMEOUT < DriverInstance::CALL_DEADLINE` をコンパイル時アサーションで固定する
- `publish` はキュー満杯なら**捨てずに `queue-full` を返す**。`emit` の配信はキュー満杯なら**古いものから捨てる**(非対称。設計書「並行性と失敗時の振る舞い」)
- **retained 値の更新はキューとは独立に必ず行う**。ドライバ無効化時は retained を破棄する
- **ドライバは `bus` を import しない**(ドライバ間呼び出し無し)。**ドライバは `on-event` を持たない**(journal / status は届かない)
- ID 空間はプラグインとドライバで別。ドライバの設定は `<settings-dir>/drivers/<id>.json`、承認は `<grants-dir>/drivers/<id>.json`
- `get` は `subscribe` に宣言したトピックに対してのみ許す
- 未解決参照(未インストールのドライバ / 未宣言のトピック)は**起動を止めない**。warn ログ + UI バッジで可視化する
- ドキュメントコメントは既存コードにならい日本語。セキュリティ上の判断根拠は英語でも可
- テスト実行: Rust は `cargo test`、フロントエンドは `cd ui/frontend && mise exec -- pnpm test`、Tauri 側は `cd ui/src-tauri && cargo test`
- 各タスクの最後にコミットする(Conventional Commits)

## File Structure

**新規**

| ファイル | 責務 |
|---|---|
| `drivers/channel/src/lib.rs` | `Bus` — 購読表・retained ストア・キュー方針。純ロジック |
| `drivers/channel/src/topic.rs` | `TopicSpec` とトピック名の検証 |
| `core/src/driver/mod.rs` | `pub mod` / re-export |
| `core/src/driver/manifest.rs` | `driver.toml` のパース・検証・フィンガープリント |
| `core/src/driver/host.rs` | `DriverCtx` / `DriverHost` / `DriverInstance`、`bus-host.emit` 実装、期限定数 |
| `core/src/driver/registry.rs` | `DriverRegistry` — 一覧・設定・承認・無効化 |
| `core/src/driver/runner.rs` | `start_drivers` — 走査・ロード・専用スレッド駆動 |
| `core/src/plugin/bus_runtime.rs` | プラグイン側 `bus_json` 共有バッファの組み立て/解釈 |
| `core/tests/bus_integration.rs` | publish → on-message → emit → 購読着信の統合テスト |
| `examples/drivers/ed-state/` | retained state ドライバのサンプル(Rust) |
| `examples/plugins/state-reader/` | 上記を購読するサンプルプラグイン |
| `ui/frontend/src/pages/Drivers.tsx` | Drivers タブ |
| `ui/frontend/src/pages/Drivers.test.tsx` | 同テスト |
| `ui/frontend/src/components/BusSection.tsx` | プラグインの bus 接続承認 UI |
| `ui/frontend/src/components/BusSection.test.tsx` | 同テスト |

**変更**

| ファイル | 変更内容 |
|---|---|
| `core/wit/plugin.wit` | `bus` / `bus-host` interface、`world driver` / `world driver-guest`、`world plugin` に `import bus` と `export on-message`、パッケージを `@0.3.0` へ |
| `core/src/plugin/manifest.rs` | `[[bus]]` のパース・検証・フィンガープリント |
| `core/src/plugin/grants.rs` | `SavedGrant` に `bus` を追加(後方互換) |
| `core/src/plugin/host.rs` | `BusHost` 実装(`publish` / `get`)、`HostCtx` に `bus_json` と `bus` |
| `core/src/plugin/registry.rs` | `PluginInfo` に bus 情報、`set_bus_grant`、`refresh_bus_runtime` |
| `core/src/plugin/runner.rs` | 起動時の `bus_json` 構築、メッセージ配信タスク、`call_on_message` |
| `core/src/plugin/mod.rs` | 新モジュールの `pub mod` / re-export |
| `core/src/lib.rs` | `pub mod driver;` |
| `core/src/server.rs` | `drivers/*` RPC 4 メソッド + `plugins/list` への `bus` 追加 |
| `core/src/bin/edlr.rs` | `--drivers-dir` フラグ、`start_drivers` の配線 |
| `config/src/lib.rs` | `DRIVER_CALL_DEADLINE_SECS` 共有定数 |
| `ui/src-tauri/src/daemon.rs` | `STOP_GRACE` を 95 秒へ、アサーションにドライバ期限を加算 |
| `ui/frontend/src/types/plugin.ts` | `BusRequest` / `DriverInfo` 型 |
| `ui/frontend/src/App.tsx` | Drivers タブの追加 |
| `ui/frontend/src/pages/Plugins.tsx` | `BusSection` の配線 |
| `ui/frontend/src/rpc.ts` | `drivers/*` の呼び出し |
| `README.md` | ドライバとバスの節 |

---

### Task 1: `drivers/channel` — トピック仕様と検証

**Files:**
- Create: `drivers/channel/src/topic.rs`
- Modify: `drivers/channel/Cargo.toml`, `drivers/channel/src/lib.rs`

**Interfaces:**
- Consumes: なし(最初のタスク)
- Produces:
  - `edlr_driver_channel::TopicSpec { pub name: String, pub retain: bool, pub description: String }`
  - `edlr_driver_channel::topic::validate_name(name: &str) -> Result<(), String>` — `[a-z0-9-]+` かつ 1..=64 バイト
  - `edlr_driver_channel::BUS_MAX_PAYLOAD: usize`(256 * 1024)

- [ ] **Step 1: 失敗するテストを書く**

`drivers/channel/src/topic.rs`:

```rust
//! トピック名の検証と、ドライバが宣言するトピック仕様。

/// ドライバが `driver.toml` の `[[topics]]` で宣言する 1 件。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TopicSpec {
    pub name: String,
    #[serde(default)]
    pub retain: bool,
    #[serde(default)]
    pub description: String,
}

/// トピック名は `[a-z0-9-]+` の 1..=64 バイト。
/// プラグイン ID / ドライバ ID と同じ字種に揃えてある(UI とログで同じ
/// 扱いができ、パス片やクエリに埋めても曖昧さが出ないため)。
pub fn validate_name(name: &str) -> Result<(), String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_lowercase_kebab() {
        assert!(validate_name("current-system").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn rejects_uppercase_and_symbols() {
        assert!(validate_name("Current").is_err());
        assert!(validate_name("a_b").is_err());
        assert!(validate_name("a/b").is_err());
    }

    #[test]
    fn rejects_over_64_bytes() {
        assert!(validate_name(&"a".repeat(65)).is_err());
        assert!(validate_name(&"a".repeat(64)).is_ok());
    }
}
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-driver-channel`
Expected: `todo!()` で panic して FAIL

- [ ] **Step 3: 最小の実装を書く**

`drivers/channel/Cargo.toml`:

```toml
[package]
name = "edlr-driver-channel"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

`validate_name` の本体:

```rust
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("topic name must not be empty".to_string());
    }
    if name.len() > 64 {
        return Err(format!("topic name must be at most 64 bytes: {name}"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(format!("topic name must match [a-z0-9-]+: {name}"));
    }
    Ok(())
}
```

`drivers/channel/src/lib.rs`:

```rust
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
```

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-driver-channel`
Expected: PASS(4 tests)

- [ ] **Step 5: コミット**

```bash
git add drivers/channel
git commit -m "feat(drivers/channel): add topic specs and name validation"
```

---

### Task 2: `drivers/channel` — Bus 本体(登録・publish・get・emit・無効化)

**Files:**
- Modify: `drivers/channel/src/lib.rs`

**Interfaces:**
- Consumes: `TopicSpec`, `BUS_MAX_PAYLOAD`, `topic::validate_name`(Task 1)
- Produces:
  - `edlr_driver_channel::BusError::{UnknownDriver(String), UnknownTopic(String), DriverUnavailable(String), QueueFull(String), TooLarge(String)}`(`Display + std::error::Error`)
  - `edlr_driver_channel::Message { pub from: String, pub topic: String, pub payload: Vec<u8> }`
  - `edlr_driver_channel::Delivery { pub plugin_id: String, pub driver_id: String, pub topic: String, pub payload: Vec<u8> }`
  - `edlr_driver_channel::Bus::new() -> Bus`(`Clone`、内部 `Arc<Mutex<..>>`)
  - `Bus::register_driver(&self, driver_id: &str, topics: Vec<TopicSpec>, sender: std::sync::mpsc::SyncSender<Message>)`
  - `Bus::subscribe(&self, plugin_id: &str, driver_id: &str, topic: &str, sender: std::sync::mpsc::SyncSender<Delivery>)`
  - `Bus::publish(&self, from_plugin: &str, driver_id: &str, topic: &str, payload: Vec<u8>) -> Result<(), BusError>`
  - `Bus::get(&self, driver_id: &str, topic: &str) -> Result<Option<Vec<u8>>, BusError>`
  - `Bus::emit(&self, driver_id: &str, topic: &str, payload: Vec<u8>) -> Result<(), BusError>`
  - `Bus::disable_driver(&self, driver_id: &str)`
  - `Bus::retained_for(&self, driver_id: &str, topic: &str) -> Option<Vec<u8>>`(購読開始時の初回配信用)

- [ ] **Step 1: 失敗するテストを書く**

`drivers/channel/src/lib.rs` の末尾に:

```rust
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
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-driver-channel`
Expected: `Bus` / `BusError` / `Message` / `Delivery` が存在せずコンパイルエラー

- [ ] **Step 3: 最小の実装を書く**

`drivers/channel/src/lib.rs`(Task 1 で書いた冒頭に続けて):

```rust
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
```

`emit` の借用は `slot`(可変)と `state.subscriptions`(不変)が衝突するため、retained 更新を先に済ませてから `drop`/再取得するか、`let subscriptions = std::mem::take(&mut state.subscriptions);` ではなくトピック仕様と retained 更新だけをブロックで囲んで `slot` の借用を先に終わらせること。コンパイルが通る形に整えてよい(挙動は上記のまま)。

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-driver-channel`
Expected: PASS(15 tests)

- [ ] **Step 5: コミット**

```bash
git add drivers/channel
git commit -m "feat(drivers/channel): add the bus with retained topics and queues"
```

---

### Task 3: WIT を `@0.3.0` に上げ、`bus` / `bus-host` / `world driver` を足す

**Files:**
- Modify: `core/wit/plugin.wit`
- Modify: `examples/plugins/hello-logger/src/lib.rs`(`on-message` の空実装を追加)
- Modify: `examples/plugins/http-caller/src/lib.rs`, `examples/plugins/busy-loop/src/lib.rs`, `examples/plugins/init-trap/src/lib.rs`, `examples/plugins/memory-hog/src/lib.rs`(同上)

**Interfaces:**
- Consumes: なし
- Produces: WIT の型。ホスト側 `bindgen!` が生成する Rust 名は
  - `bindings::edlr::plugin::bus::{Host as BusHost, BusError as WitBusError}`
  - `bindings::edlr::plugin::bus_host::{Host as BusHostHost, BusError as WitBusHostError}`(`world driver` 用の別 `bindgen!` から)

- [ ] **Step 1: WIT を書き換える**

`core/wit/plugin.wit` の先頭を `package edlr:plugin@0.3.0;` に変更し、以下を追加する。

```wit
interface bus {
  variant bus-error {
    permission-denied(string),
    unknown-driver(string),
    unknown-topic(string),
    driver-unavailable(string),
    queue-full(string),
    too-large(string),
  }

  publish: func(driver: string, topic: string, payload: list<u8>) -> result<_, bus-error>;
  get:     func(driver: string, topic: string) -> result<option<list<u8>>, bus-error>;
}

interface bus-host {
  use bus.{bus-error};

  emit: func(topic: string, payload: list<u8>) -> result<_, bus-error>;
}
```

`world plugin` に 2 行足す:

```wit
  import bus;
  export on-message: func(driver: string, topic: string, payload: list<u8>);
```

新しい world を 2 つ足す:

```wit
/// ドライバ(ホスト側 `bindgen!` が使う)。`bus` は import しない
/// -- ドライバ間の呼び出しを構造的に不可能にするため。`on-event` も無い
/// -- ドライバは journal / status を受け取らない。
world driver {
  import host-log;
  import host-settings;
  import driver-http;
  import driver-process;
  import driver-fs;
  import bus-host;

  export init: func();
  export on-message: func(from: string, topic: string, payload: list<u8>);
}

/// ドライバ(ゲスト)がビルド時に対象とする world。`plugin-guest` と同じ理由で
/// WASI の import 一式を足してある。
world driver-guest {
  include driver;
  include wasi:cli/imports@0.2.0;
}
```

- [ ] **Step 2: 既存プラグインが壊れることを確認する**

Run: `cd examples/plugins/hello-logger && cargo build --release --target wasm32-wasip2`
Expected: `on-message` が未実装で FAIL(`not all trait items implemented`)

- [ ] **Step 3: 既存サンプルに `on-message` の空実装を足す**

各サンプルの `impl Guest for Component` に以下を足す(hello-logger はログを出す形にする)。

```rust
    fn on_message(driver: String, topic: String, _payload: Vec<u8>) {
        host_log::log(
            host_log::Level::Debug,
            &format!("ignoring bus message from {driver}/{topic}"),
        );
    }
```

`busy-loop` / `init-trap` / `memory-hog` / `http-caller` は本体が空でよい:

```rust
    fn on_message(_driver: String, _topic: String, _payload: Vec<u8>) {}
```

- [ ] **Step 4: 全サンプルがビルドできることを確認する**

Run:

```bash
for p in hello-logger http-caller busy-loop init-trap memory-hog; do
  (cd examples/plugins/$p && cargo build --release --target wasm32-wasip2) || exit 1
done
```

Expected: すべて成功

- [ ] **Step 5: コミット**

```bash
git add core/wit/plugin.wit examples/plugins
git commit -m "feat(wit): add the bus interfaces and the driver world at 0.3.0"
```

---

### Task 4: プラグイン manifest の `[[bus]]`

**Files:**
- Modify: `core/src/plugin/manifest.rs`

**Interfaces:**
- Consumes: なし
- Produces:
  - `crate::plugin::BusRequest { pub driver: String, pub publish: Vec<String>, pub subscribe: Vec<String>, pub reason: String }`
  - `Manifest.bus: Vec<BusRequest>`(`#[serde(default, rename = "bus")]`)
  - `Manifest::bus_request(&self, driver: &str) -> Option<&BusRequest>`
  - `Manifest::bus_fingerprint(&self, driver: &str) -> Option<String>`

- [ ] **Step 1: 失敗するテストを書く**

`core/src/plugin/manifest.rs` のテストモジュールに追加:

```rust
    #[test]
    fn parses_bus_requests() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manifest.toml"),
            r#"
id = "translator"
name = "Translator"
version = "0.1.0"
entry = "plugin.wasm"

[[bus]]
driver = "ed-state"
publish = ["ship-status"]
subscribe = ["current-system"]
reason = "現在システムを購読して翻訳先を切り替えるため"
"#,
        )
        .unwrap();
        let manifest = load_manifest(dir.path()).unwrap();
        let request = manifest.bus_request("ed-state").expect("bus request parsed");
        assert_eq!(request.publish, vec!["ship-status".to_string()]);
        assert_eq!(request.subscribe, vec!["current-system".to_string()]);
    }

    #[test]
    fn rejects_a_bus_block_with_neither_publish_nor_subscribe() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manifest.toml"),
            r#"
id = "translator"
name = "Translator"
version = "0.1.0"
entry = "plugin.wasm"

[[bus]]
driver = "ed-state"
reason = "何もしない"
"#,
        )
        .unwrap();
        assert!(load_manifest(dir.path()).is_err());
    }

    #[test]
    fn rejects_duplicate_bus_drivers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manifest.toml"),
            r#"
id = "translator"
name = "Translator"
version = "0.1.0"
entry = "plugin.wasm"

[[bus]]
driver = "ed-state"
publish = ["a"]
reason = "one"

[[bus]]
driver = "ed-state"
publish = ["b"]
reason = "two"
"#,
        )
        .unwrap();
        assert!(load_manifest(dir.path()).is_err());
    }

    #[test]
    fn bus_fingerprint_changes_with_the_requested_topics() {
        let base = BusRequest {
            driver: "ed-state".into(),
            publish: vec!["a".into()],
            subscribe: vec![],
            reason: "r".into(),
        };
        let mut widened = base.clone();
        widened.publish.push("b".into());

        let m1 = manifest_with_bus(vec![base]);
        let m2 = manifest_with_bus(vec![widened]);
        assert_ne!(
            m1.bus_fingerprint("ed-state"),
            m2.bus_fingerprint("ed-state")
        );
    }

    #[test]
    fn bus_fingerprint_ignores_topic_order() {
        let a = BusRequest {
            driver: "ed-state".into(),
            publish: vec!["a".into(), "b".into()],
            subscribe: vec![],
            reason: "r".into(),
        };
        let mut reordered = a.clone();
        reordered.publish.reverse();
        assert_eq!(
            manifest_with_bus(vec![a]).bus_fingerprint("ed-state"),
            manifest_with_bus(vec![reordered]).bus_fingerprint("ed-state")
        );
    }
```

同テストモジュールに補助関数を足す:

```rust
    fn manifest_with_bus(bus: Vec<BusRequest>) -> Manifest {
        Manifest {
            id: "translator".into(),
            name: "Translator".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: Vec::new(),
            settings: Vec::new(),
            capabilities: Vec::new(),
            sidecars: Vec::new(),
            filesystem: Vec::new(),
            bus,
        }
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core manifest::tests`
Expected: `BusRequest` が無くコンパイルエラー

- [ ] **Step 3: 実装する**

```rust
/// プラグインが要求するバス接続 1 件。
///
/// **`get` は `subscribe` に宣言したトピックにのみ許される**(「配信は要らないが
/// 最新値は読みたい」という区別は設けない -- 承認画面に出す情報を増やさないため)。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct BusRequest {
    pub driver: String,
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub subscribe: Vec<String>,
    pub reason: String,
}
```

`Manifest` に `#[serde(default)] pub bus: Vec<BusRequest>,` を足し、`load_manifest` の検証に以下を追加する(既存の `validate_*` と同じ場所・同じ流儀で):

- `driver` はブロック間で一意。重複は `ManifestError::Invalid`
- `driver` は `[a-z0-9-]+`
- `publish` と `subscribe` が両方空なら `Invalid`
- 各トピック名は `edlr_driver_channel::topic::validate_name` を通す
- `reason` は既存の `validate_reason`(trim / 制御文字 / ゼロ幅文字の拒否)と同じ処理を通す

フィンガープリント:

```rust
    /// バス接続 1 件の要求内容の安定フィンガープリント(grants の失効判定に使う)。
    /// `capabilities_fingerprint` と同じ長さ接頭辞エンコード + SHA-256。
    /// トピック順の違いは無視する(ソートしてから畳み込む)。
    pub fn bus_fingerprint(&self, driver: &str) -> Option<String> {
        let request = self.bus_request(driver)?;
        let mut publish = request.publish.clone();
        publish.sort();
        let mut subscribe = request.subscribe.clone();
        subscribe.sort();

        let mut canonical = encode_field("bus");
        canonical.push_str(&encode_field(&request.driver));
        canonical.push_str(&encode_field(&publish.len().to_string()));
        for topic in &publish {
            canonical.push_str(&encode_field(topic));
        }
        canonical.push_str(&encode_field(&subscribe.len().to_string()));
        for topic in &subscribe {
            canonical.push_str(&encode_field(topic));
        }
        canonical.push_str(&encode_field(&request.reason));
        Some(sha256_hex(&canonical))
    }

    pub fn bus_request(&self, driver: &str) -> Option<&BusRequest> {
        self.bus.iter().find(|r| r.driver == driver)
    }
```

`core/Cargo.toml` の `[dependencies]` に `edlr-driver-channel = { path = "../drivers/channel" }` を足す。`core/src/plugin/mod.rs` で `BusRequest` を re-export する。

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-core`
Expected: PASS(既存テストの `Manifest` リテラルに `bus: Vec::new()` を足す必要があればあわせて直す)

- [ ] **Step 5: コミット**

```bash
git add core
git commit -m "feat(plugin): parse and fingerprint bus requests in the manifest"
```

---

### Task 5: `driver.toml` のパース・検証

**Files:**
- Create: `core/src/driver/mod.rs`, `core/src/driver/manifest.rs`
- Modify: `core/src/lib.rs`

**Interfaces:**
- Consumes: `edlr_driver_channel::TopicSpec`, `topic::validate_name`(Task 1)
- Produces:
  - `crate::driver::DriverManifest { pub id, pub name, pub version, pub description, pub entry, pub topics: Vec<TopicSpec>, pub settings: Vec<SettingField>, pub capabilities: Vec<CapabilityRequest>, pub sidecars: Vec<SidecarRequest>, pub filesystem: Vec<FilesystemRequest> }`
  - `crate::driver::load_driver_manifest(dir: &Path) -> Result<DriverManifest, ManifestError>`
  - `DriverManifest::topic(&self, name: &str) -> Option<&TopicSpec>`

- [ ] **Step 1: 失敗するテストを書く**

`core/src/driver/manifest.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, body: &str) {
        std::fs::write(dir.join("driver.toml"), body).unwrap();
    }

    const VALID: &str = r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"

[[topics]]
name = "current-system"
retain = true
description = "現在のスターシステム"
"#;

    #[test]
    fn parses_a_valid_driver_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), VALID);
        let manifest = load_driver_manifest(dir.path()).unwrap();
        assert_eq!(manifest.id, "ed-state");
        assert!(manifest.topic("current-system").unwrap().retain);
    }

    #[test]
    fn rejects_an_id_that_differs_from_the_directory_name() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("other-name");
        std::fs::create_dir(&sub).unwrap();
        write(&sub, VALID);
        assert!(load_driver_manifest(&sub).is_err());
    }

    #[test]
    fn rejects_duplicate_topic_names() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"

[[topics]]
name = "a"

[[topics]]
name = "a"
"#,
        );
        assert!(load_driver_manifest(dir.path()).is_err());
    }

    #[test]
    fn rejects_an_invalid_topic_name() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"

[[topics]]
name = "Bad_Name"
"#,
        );
        assert!(load_driver_manifest(dir.path()).is_err());
    }

    #[test]
    fn a_driver_with_no_topics_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"
"#,
        );
        assert!(load_driver_manifest(dir.path()).is_ok());
    }
}
```

なお `rejects_an_id_that_differs_from_the_directory_name` はディレクトリ名との一致を見るため、テンポラリディレクトリ直下ではなく `other-name/` を作ってそこに置く。

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core driver::manifest`
Expected: モジュールが存在せずコンパイルエラー

- [ ] **Step 3: 実装する**

`core/src/driver/manifest.rs` は `core/src/plugin/manifest.rs` の `load_manifest` と同じ構造にする。読むファイル名は `driver.toml`、ID の字種検証・ディレクトリ名一致・`entry` の非空検証・`[[capabilities]]` / `[[sidecar]]` / `[[filesystem]]` / `[[settings]]` の検証は**プラグインと同じ関数を再利用**する(`core/src/plugin/manifest.rs` の該当ヘルパを `pub(crate)` に引き上げる)。トピックは以下を検証する。

- `name` はドライバ内で一意
- `name` は `edlr_driver_channel::topic::validate_name` を通る

`core/src/driver/mod.rs`:

```rust
//! ユーザー定義ドライバ(常駐 wasm コンポーネント)のロードと駆動。
//! `crate::plugin` と対称の構造だが、別レイヤーなので無理に共通化しない
//! (共有するのは grants / settings の下位ユーティリティ程度)。

pub mod manifest;

pub use manifest::{load_driver_manifest, DriverManifest};
```

`core/src/lib.rs` に `pub mod driver;` を足す。

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-core driver::manifest`
Expected: PASS(5 tests)

- [ ] **Step 5: コミット**

```bash
git add core
git commit -m "feat(driver): parse and validate driver.toml"
```

---

### Task 6: grants — bus 承認とドライバ用の保存先

**Files:**
- Modify: `core/src/plugin/grants.rs`

**Interfaces:**
- Consumes: `Manifest::bus_fingerprint`(Task 4)
- Produces:
  - `SavedGrant` に `#[serde(default)] bus: BTreeMap<String, String>`(ドライバ ID → 承認時フィンガープリント)
  - `GrantsStore::bus_state(&self, manifest: &Manifest, driver: &str) -> GrantState`
  - `GrantsStore::set_bus(&self, manifest: &Manifest, driver: &str, granted: bool) -> Result<GrantState, GrantsError>`
  - `GrantsStore::new_for_drivers(dir: PathBuf) -> GrantsStore` — `<grants-dir>/drivers/` を使う

- [ ] **Step 1: 失敗するテストを書く**

`core/src/plugin/grants.rs` のテストモジュールに追加:

```rust
    #[test]
    fn bus_grants_start_ungranted_and_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(dir.path().to_path_buf());
        let manifest = manifest_with_bus();

        assert!(!store.bus_state(&manifest, "ed-state").granted);
        store.set_bus(&manifest, "ed-state", true).unwrap();

        let reopened = GrantsStore::new(dir.path().to_path_buf());
        assert!(reopened.bus_state(&manifest, "ed-state").granted);
    }

    #[test]
    fn bus_grants_go_stale_when_the_request_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(dir.path().to_path_buf());
        let manifest = manifest_with_bus();
        store.set_bus(&manifest, "ed-state", true).unwrap();

        let mut widened = manifest.clone();
        widened.bus[0].publish.push("another-topic".to_string());

        let state = store.bus_state(&widened, "ed-state");
        assert!(!state.granted);
        assert!(state.stale);
    }

    #[test]
    fn existing_grant_files_without_a_bus_key_still_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("translator.json"),
            r#"{"capabilities":{"granted":false}}"#,
        )
        .unwrap();
        let store = GrantsStore::new(dir.path().to_path_buf());
        assert!(!store.bus_state(&manifest_with_bus(), "ed-state").granted);
    }
```

補助関数(既存の `manifest_with_*` に倣う):

```rust
    fn manifest_with_bus() -> Manifest {
        let mut manifest = base_manifest();  // 既存テストの補助関数
        manifest.bus = vec![BusRequest {
            driver: "ed-state".into(),
            publish: vec!["ship-status".into()],
            subscribe: vec!["current-system".into()],
            reason: "r".into(),
        }];
        manifest
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core grants`
Expected: `set_bus` / `bus_state` が無くコンパイルエラー

- [ ] **Step 3: 実装する**

`SavedGrant` への `bus` 追加、`bus_state` / `set_bus` は既存の `filesystem_state` / `set_filesystem` を**そのままの形で写して**フィンガープリント関数だけ `bus_fingerprint` に差し替える。`stale` の判定(保存値と現在のフィンガープリントの不一致)も同じ。

`new_for_drivers` は既存の `new` にディレクトリを `dir.join("drivers")` にして委譲するだけ:

```rust
    /// ドライバ用の grants ストア。ID 空間がプラグインと別なので、
    /// 保存先も `<grants-dir>/drivers/` に分ける。
    pub fn new_for_drivers(dir: PathBuf) -> GrantsStore {
        GrantsStore::new(dir.join("drivers"))
    }
```

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-core grants`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add core
git commit -m "feat(plugin): persist bus grants and split the drivers grants dir"
```

---

### Task 7: `bus_runtime` 共有バッファとプラグイン側の `bus` 実装

**Files:**
- Create: `core/src/plugin/bus_runtime.rs`
- Modify: `core/src/plugin/host.rs`, `core/src/plugin/mod.rs`

**Interfaces:**
- Consumes: `Bus`, `BusError`(Task 2)、WIT の `bus` interface(Task 3)、`BusRequest`(Task 4)
- Produces:
  - `crate::plugin::bus_runtime::BusRuntimeEntry { pub driver: String, pub granted: bool, pub publish: Vec<String>, pub subscribe: Vec<String> }`
  - `bus_runtime::bus_json_string(entries: &[BusRuntimeEntry]) -> String`
  - `bus_runtime::parse_bus(raw: &str) -> BTreeMap<String, BusRuntimeEntry>`
  - `HostCtx` に `pub bus_json: Arc<Mutex<String>>` と `bus: Bus` を追加。`HostCtx::new` の引数は **`filesystem_json` の直後・`http_driver` の直前に `bus_json`, `bus` の順で 2 つ増える**(以降のタスクのテストヘルパと `runner.rs` はこの順序に依存する)
  - `impl BusHost for HostCtx`(`publish` / `get`)

- [ ] **Step 1: 失敗するテストを書く**

`core/src/plugin/bus_runtime.rs`(`fs_runtime.rs` と同じ流儀):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(granted: bool) -> BusRuntimeEntry {
        BusRuntimeEntry {
            driver: "ed-state".into(),
            granted,
            publish: vec!["ship-status".into()],
            subscribe: vec!["current-system".into()],
        }
    }

    #[test]
    fn ungranted_entries_carry_no_topics() {
        let parsed = parse_bus(&bus_json_string(&[entry(false)]));
        let e = parsed.get("ed-state").expect("entry survives serialization");
        assert!(!e.granted);
        assert!(e.publish.is_empty());
        assert!(e.subscribe.is_empty());
    }

    #[test]
    fn granted_entries_round_trip() {
        let parsed = parse_bus(&bus_json_string(&[entry(true)]));
        let e = parsed.get("ed-state").unwrap();
        assert!(e.granted);
        assert_eq!(e.publish, vec!["ship-status".to_string()]);
    }

    #[test]
    fn broken_json_parses_as_no_entries() {
        assert!(parse_bus("not json {{{").is_empty());
    }
}
```

`core/src/plugin/host.rs` のテストモジュールに追加:

```rust
    #[test]
    fn publish_without_a_grant_is_denied() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "ship-status".into(),
                retain: false,
                description: String::new(),
            }],
            tx,
        );
        let mut ctx = test_ctx_with_bus(bus, &[entry_ungranted()]);
        let result = ctx.publish("ed-state".into(), "ship-status".into(), vec![1]);
        assert!(matches!(result, Err(WitBusError::PermissionDenied(_))));
    }

    #[test]
    fn publish_with_a_grant_reaches_the_driver_queue() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "ship-status".into(),
                retain: false,
                description: String::new(),
            }],
            tx,
        );
        let mut ctx = test_ctx_with_bus(bus, &[entry_granted()]);
        ctx.publish("ed-state".into(), "ship-status".into(), vec![1]).unwrap();
        assert_eq!(rx.try_recv().unwrap().from, "translator");
    }

    #[test]
    fn publishing_to_a_topic_outside_the_grant_is_denied() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "secret".into(),
                retain: false,
                description: String::new(),
            }],
            tx,
        );
        let mut ctx = test_ctx_with_bus(bus, &[entry_granted()]);
        let result = ctx.publish("ed-state".into(), "secret".into(), vec![1]);
        assert!(matches!(result, Err(WitBusError::PermissionDenied(_))));
    }

    #[test]
    fn get_is_limited_to_subscribed_topics() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![
                edlr_driver_channel::TopicSpec {
                    name: "current-system".into(),
                    retain: true,
                    description: String::new(),
                },
                edlr_driver_channel::TopicSpec {
                    name: "ship-status".into(),
                    retain: true,
                    description: String::new(),
                },
            ],
            tx,
        );
        bus.emit("ed-state", "current-system", b"Sol".to_vec()).unwrap();
        bus.emit("ed-state", "ship-status", b"x".to_vec()).unwrap();

        let mut ctx = test_ctx_with_bus(bus, &[entry_granted()]);
        assert_eq!(
            ctx.get("ed-state".into(), "current-system".into()).unwrap(),
            Some(b"Sol".to_vec())
        );
        // publish にしか宣言していないトピックは読めない。
        assert!(matches!(
            ctx.get("ed-state".into(), "ship-status".into()),
            Err(WitBusError::PermissionDenied(_))
        ));
    }
```

補助関数(既存の `HostCtx` 生成テストヘルパに倣って追加):

```rust
    fn entry_granted() -> crate::plugin::bus_runtime::BusRuntimeEntry {
        crate::plugin::bus_runtime::BusRuntimeEntry {
            driver: "ed-state".into(),
            granted: true,
            publish: vec!["ship-status".into()],
            subscribe: vec!["current-system".into()],
        }
    }

    fn entry_ungranted() -> crate::plugin::bus_runtime::BusRuntimeEntry {
        let mut e = entry_granted();
        e.granted = false;
        e
    }

    fn test_ctx_with_bus(
        bus: edlr_driver_channel::Bus,
        entries: &[crate::plugin::bus_runtime::BusRuntimeEntry],
    ) -> HostCtx {
        use crate::plugin::bus_runtime::bus_json_string;
        HostCtx::new(
            "translator".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(capabilities_json_string(&[]))),
            Arc::new(Mutex::new("[]".to_string())),
            Arc::new(Mutex::new("[]".to_string())),
            Arc::new(Mutex::new(bus_json_string(entries))),
            bus,
            Arc::new(
                edlr_driver_http::HttpDriver::new(HTTP_TIMEOUT, HTTP_MAX_BODY)
                    .expect("http driver builds"),
            ),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                SIDECAR_SHUTDOWN_GRACE,
                SIDECAR_SPAWN_MIN_INTERVAL,
            )),
            Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT)),
        )
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core plugin::host`
Expected: `BusHost` 未実装でコンパイルエラー

- [ ] **Step 3: 実装する**

`bus_json_string` は `fs_runtime::filesystem_json_string` と同じく**未承認のエントリからトピック一覧を落とす**。承認前は「どのトピックに触れるか」の情報自体がバッファに存在しない状態にするため。

`HostCtx` に `bus_json` と `bus` を足し、`HostCtx::new` の引数を 2 つ増やす(呼び出し元は Task 9 で直す)。

```rust
impl BusHost for HostCtx {
    fn publish(
        &mut self,
        driver: String,
        topic: String,
        payload: Vec<u8>,
    ) -> Result<(), WitBusError> {
        self.check_bus(&driver, &topic, BusDirection::Publish)?;
        self.bus
            .publish(&self.plugin_id, &driver, &topic, payload)
            .map_err(bus_error_to_wit)
    }

    fn get(&mut self, driver: String, topic: String) -> Result<Option<Vec<u8>>, WitBusError> {
        self.check_bus(&driver, &topic, BusDirection::Subscribe)?;
        self.bus.get(&driver, &topic).map_err(bus_error_to_wit)
    }
}

enum BusDirection {
    Publish,
    Subscribe,
}

impl HostCtx {
    /// 承認と宣言済みトピックの照合。プラグインは自分の ID も承認状態も
    /// 引数で渡さない -- `bus_json` は `Registry` だけが書き込む共有バッファで、
    /// 未承認のエントリはトピック一覧を持たないため、他プラグインの接続を
    /// 騙ることも、宣言していないトピックへ触ることもできない。
    fn check_bus(
        &self,
        driver: &str,
        topic: &str,
        direction: BusDirection,
    ) -> Result<(), WitBusError> {
        let raw = self.bus_json.lock().expect("bus_json poisoned").clone();
        let entries = crate::plugin::bus_runtime::parse_bus(&raw);
        let entry = entries
            .get(driver)
            .filter(|e| e.granted)
            .ok_or_else(|| {
                WitBusError::PermissionDenied(format!("bus access to {driver} is not granted"))
            })?;
        let topics = match direction {
            BusDirection::Publish => &entry.publish,
            BusDirection::Subscribe => &entry.subscribe,
        };
        if !topics.iter().any(|t| t == topic) {
            return Err(WitBusError::PermissionDenied(format!(
                "{driver}/{topic} is not in this plugin's granted bus topics"
            )));
        }
        Ok(())
    }
}

fn bus_error_to_wit(error: edlr_driver_channel::BusError) -> WitBusError {
    use edlr_driver_channel::BusError;
    match error {
        BusError::UnknownDriver(m) => WitBusError::UnknownDriver(m),
        BusError::UnknownTopic(m) => WitBusError::UnknownTopic(m),
        BusError::DriverUnavailable(m) => WitBusError::DriverUnavailable(m),
        BusError::QueueFull(m) => WitBusError::QueueFull(m),
        BusError::TooLarge(m) => WitBusError::TooLarge(m),
    }
}
```

`PluginInstance` に `call_on_message(&mut self, driver: &str, topic: &str, payload: &[u8]) -> anyhow::Result<()>` を足す(`call_on_event` と同じ形で、毎回 `set_epoch_deadline` を張り直す)。

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-core`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add core
git commit -m "feat(plugin): implement the bus imports with per-call grant checks"
```

---

### Task 8: ドライバのホスト(`DriverHost` / `DriverCtx` / 期限定数)

**Files:**
- Create: `core/src/driver/host.rs`
- Modify: `core/src/driver/mod.rs`, `config/src/lib.rs`

**Interfaces:**
- Consumes: WIT の `world driver`(Task 3)、`Bus`(Task 2)
- Produces:
  - `edlr_config::DRIVER_CALL_DEADLINE_SECS: u64`(30)
  - `crate::driver::host::DRIVER_HTTP_TIMEOUT: Duration`(25 秒)
  - `crate::driver::host::DriverInstance::CALL_DEADLINE: Duration`(`DRIVER_CALL_DEADLINE_SECS` 秒)
  - `DriverCtx::new(driver_id, settings_json, capabilities_json, sidecars_json, filesystem_json, bus, http_driver, process_driver, fs_driver) -> DriverCtx`
  - `DriverHost::new() -> anyhow::Result<DriverHost>` / `DriverHost::load(&self, wasm_path: &Path, ctx: DriverCtx) -> anyhow::Result<DriverInstance>`
  - `DriverInstance::call_init(&mut self) -> anyhow::Result<()>`
  - `DriverInstance::call_on_message(&mut self, from: &str, topic: &str, payload: &[u8]) -> anyhow::Result<()>`

- [ ] **Step 1: 失敗するテストを書く**

`core/src/driver/host.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_reaches_the_bus_and_updates_retained() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            }],
            tx,
        );
        let mut ctx = test_driver_ctx(bus.clone());

        ctx.emit("current-system".into(), b"Sol".to_vec()).unwrap();

        assert_eq!(bus.retained_for("ed-state", "current-system"), Some(b"Sol".to_vec()));
    }

    #[test]
    fn emit_to_an_undeclared_topic_is_rejected() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver("ed-state", vec![], tx);
        let mut ctx = test_driver_ctx(bus);

        assert!(matches!(
            ctx.emit("nope".into(), vec![]),
            Err(WitBusError::UnknownTopic(_))
        ));
    }

    #[test]
    fn oversized_emits_are_rejected() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            }],
            tx,
        );
        let mut ctx = test_driver_ctx(bus);
        let big = vec![0u8; edlr_driver_channel::BUS_MAX_PAYLOAD + 1];
        assert!(matches!(ctx.emit("current-system".into(), big), Err(WitBusError::TooLarge(_))));
    }

    fn test_driver_ctx(bus: edlr_driver_channel::Bus) -> DriverCtx {
        DriverCtx::new(
            "ed-state".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(r#"{"hosts":[]}"#.to_string())),
            Arc::new(Mutex::new("[]".to_string())),
            Arc::new(Mutex::new("[]".to_string())),
            bus,
            Arc::new(
                edlr_driver_http::HttpDriver::new(DRIVER_HTTP_TIMEOUT, HTTP_MAX_BODY)
                    .expect("http driver builds"),
            ),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                SIDECAR_SHUTDOWN_GRACE,
                SIDECAR_SPAWN_MIN_INTERVAL,
            )),
            Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT)),
        )
    }
}
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core driver::host`
Expected: モジュールが無くコンパイルエラー

- [ ] **Step 3: 実装する**

`core/src/driver/host.rs` は `core/src/plugin/host.rs` を土台にする。相違点だけ列挙する。

- `bindgen!` の `world` は `"driver"`
- `HostLogHost` / `HostSettingsHost` / `DriverHttpHost` / `DriverProcessHost` / `DriverFsHost` の実装は**プラグイン側と同じロジック**。`plugin_id` の代わりに `driver_id` をログに載せる
- `BusHostHost`(= `bus-host` interface)を実装する:

```rust
impl BusHostHost for DriverCtx {
    fn emit(&mut self, topic: String, payload: Vec<u8>) -> Result<(), WitBusError> {
        self.bus
            .emit(&self.driver_id, &topic, payload)
            .map_err(bus_error_to_wit)
    }
}
```

- 期限定数:

```rust
/// ドライバ向けの HTTP タイムアウト。プラグインの `HTTP_TIMEOUT`(1.5 秒)では
/// 音声合成のような数秒かかる呼び出しが完了しないため、ドライバ専用に長く取る。
///
/// epoch interruption は wasm の命令境界でしか作動せず、ブロッキングな HTTP
/// 呼び出し自体は打ち切れない。だから「HTTP タイムアウト < 呼び出し期限」の
/// 不変条件はドライバ側でも維持する必要がある(プラグイン側の `HTTP_TIMEOUT`
/// のドキュメント参照)。
pub const DRIVER_HTTP_TIMEOUT: Duration = Duration::from_secs(25);

const _: () = assert!(
    DRIVER_HTTP_TIMEOUT.as_millis() < DriverInstance::CALL_DEADLINE.as_millis(),
    "DRIVER_HTTP_TIMEOUT must stay strictly under DriverInstance::CALL_DEADLINE"
);

impl DriverInstance {
    /// ドライバ 1 呼び出しの期限。ドライバは専用スレッドで動きイベント配信の
    /// ループを塞がないため、プラグインの 2 秒より長く取れる。代償として
    /// この間そのドライバのキューは詰まる(設計書「並行性」参照)。
    pub const CALL_DEADLINE: Duration =
        Duration::from_secs(edlr_config::DRIVER_CALL_DEADLINE_SECS);
}
```

`config/src/lib.rs` に:

```rust
/// ドライバ 1 呼び出しの期限(秒)。`edlr-core`(`driver::host::DriverInstance::
/// CALL_DEADLINE`)と `edlr-ui`(`STOP_GRACE` のアサーション)の両方が参照する
/// ため、`SIDECAR_SHUTDOWN_GRACE_SECS` と同じくここで共有する。
pub const DRIVER_CALL_DEADLINE_SECS: u64 = 30;
```

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-core driver::host`
Expected: PASS(3 tests)

- [ ] **Step 5: コミット**

```bash
git add core config
git commit -m "feat(driver): add the driver host with its own call deadline"
```

---

### Task 9: ドライバの registry と runner(走査・駆動・無効化)

**Files:**
- Create: `core/src/driver/registry.rs`, `core/src/driver/runner.rs`
- Modify: `core/src/driver/mod.rs`, `core/src/plugin/runner.rs`, `core/src/plugin/registry.rs`

**Interfaces:**
- Consumes: `DriverManifest`(Task 5)、`DriverHost`(Task 8)、`Bus`(Task 2)、`GrantsStore::new_for_drivers`(Task 6)
- Produces:
  - `crate::driver::DriverState::{Running, Disabled { reason: String }}`
  - `crate::driver::DriverInfo { pub manifest: DriverManifest, pub state: DriverState, pub values: serde_json::Map<String, serde_json::Value>, pub grant_state: GrantState, pub sidecars: Vec<SidecarInfo>, pub filesystem: Vec<FilesystemInfo> }`
  - `crate::driver::DriverEntry { pub manifest: DriverManifest, pub state: DriverState, pub settings_json: Arc<Mutex<String>>, pub capabilities_json: Arc<Mutex<String>>, pub sidecars_json: Arc<Mutex<String>>, pub filesystem_json: Arc<Mutex<String>> }`(`PluginEntry` と対称)
  - `crate::driver::DriverRegistry`(`Clone`)、`new(host, settings_store, grants_store, sidecar_config_store, filesystem_config_store, bus, drivers_dir)` / `push(DriverEntry)` / `drivers_dir()` / `list()` / `values()` / `set_values()` / `set_capabilities()` / `set_disabled(id, reason)` / `manifest_of(id) -> Option<DriverManifest>`
  - `crate::driver::start_drivers(drivers_dir: &Path, settings_store, sidecar_config_store, filesystem_config_store, grants_store, bus: Bus, host: DriverHost) -> DriverRegistry`
  - `crate::plugin::runner::start_plugins` の引数に `bus: Bus` を追加

- [ ] **Step 1: 失敗するテストを書く**

`core/src/driver/runner.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

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

        let registry = start_drivers_for_test(dir.path());
        assert!(registry.list().is_empty());
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
            DriverHost::new().expect("wasmtime engine builds"),
        )
    }
}
```

`core/src/driver/registry.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabling_a_driver_marks_it_and_drops_its_retained_values() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            }],
            tx,
        );
        bus.emit("ed-state", "current-system", b"Sol".to_vec()).unwrap();

        let registry = test_registry(bus.clone());
        registry.set_disabled("ed-state", "on-message call failed".to_string());

        assert!(matches!(
            registry.list()[0].state,
            DriverState::Disabled { .. }
        ));
        assert_eq!(bus.retained_for("ed-state", "current-system"), None);
    }

    fn test_registry(bus: edlr_driver_channel::Bus) -> DriverRegistry {
        // `ed-state` を 1 件だけ載せた DriverRegistry を、wasm をロードせずに
        // 直接組み立てる(`DriverRegistry::push` に `DriverEntry` を渡す)。
        // `plugin::registry` のテストが `Registry::push` で同じことをしている
        // のと同じ流儀。
        let tmp = tempfile::tempdir().unwrap();
        let registry = DriverRegistry::new(
            Arc::new(DriverHost::new().expect("wasmtime engine builds")),
            Arc::new(SettingsStore::new(tmp.path().join("settings"))),
            Arc::new(GrantsStore::new_for_drivers(tmp.path().join("grants"))),
            Arc::new(SidecarConfigStore::new(tmp.path().join("settings"))),
            Arc::new(FilesystemConfigStore::new(
                tmp.path().join("settings"),
                vec![tmp.path().to_path_buf()],
            )),
            bus,
            tmp.path().join("drivers"),
        );
        registry.push(DriverEntry {
            manifest: DriverManifest {
                id: "ed-state".into(),
                name: "ED State".into(),
                version: "0.1.0".into(),
                description: String::new(),
                entry: "driver.wasm".into(),
                topics: vec![edlr_driver_channel::TopicSpec {
                    name: "current-system".into(),
                    retain: true,
                    description: String::new(),
                }],
                settings: Vec::new(),
                capabilities: Vec::new(),
                sidecars: Vec::new(),
                filesystem: Vec::new(),
            },
            state: DriverState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(r#"{"hosts":[]}"#.to_string())),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
        });
        registry
    }
}
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core driver::`
Expected: コンパイルエラー

- [ ] **Step 3: 実装する**

`registry.rs` は `core/src/plugin/registry.rs` の構造を写す。相違点:

- `set_disabled` は状態を `Disabled` にしたうえで **`bus.disable_driver(id)` を呼ぶ**(retained の破棄)。あわせて既存規則どおりそのドライバのサイドカーを停止する
- bus の承認 API は持たない(bus の承認はプラグイン側の `Registry` の責務)

`runner.rs` は `core/src/plugin/runner.rs` の構造を写す。相違点:

- 読むのは `driver.toml`、world は `driver`
- **イベント購読タスクを作らない**。代わりにメッセージ受信は `Bus::register_driver` で渡した `SyncSender<Message>` の受け口(`Receiver<Message>`)を、ドライバ専用スレッドがそのまま `for message in messages_rx` で回す
- キュー容量は `DRIVER_MESSAGE_QUEUE_CAPACITY = 64`
- `call_on_message` が `Err` を返したら warn ログ + `registry.set_disabled(...)` してループを抜ける

```rust
/// ドライバ 1 件あたりのメッセージキュー容量。
///
/// ドライバは複数プラグインの結節点で溢れやすく、1 メッセージの処理が
/// `DriverInstance::CALL_DEADLINE`(30 秒)まで伸びうるため、プラグインの
/// `PLUGIN_EVENT_QUEUE_CAPACITY`(32)より大きく取る。満杯時は `publish` が
/// `queue-full` を返す(捨てない)ので、呼び出し側が状況を知れる。
const DRIVER_MESSAGE_QUEUE_CAPACITY: usize = 64;
```

`start_plugins` は `bus: Bus` を受け取り、`HostCtx::new` へ渡す。あわせて各プラグインの `bus_json` を `GrantsStore::bus_state` と manifest から組み立てる(`filesystem_json` の組み立てと同じ流儀)。

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-core`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add core
git commit -m "feat(driver): load and drive drivers on dedicated threads"
```

---

### Task 10: ドライバ → プラグインの配信経路と未解決参照の警告

**Files:**
- Modify: `core/src/plugin/runner.rs`, `core/src/plugin/registry.rs`

**Interfaces:**
- Consumes: `Bus::subscribe` / `Bus::retained_for`(Task 2)、`call_on_message`(Task 7)、`DriverRegistry`(Task 9)
- Produces:
  - `crate::plugin::registry::Registry::set_bus_grant(&self, plugin: &str, driver: &str, granted: bool) -> Result<GrantState, RegistryError>`
  - `Registry::bus(&self, plugin: &str) -> Result<Vec<BusInfo>, RegistryError>`
  - `crate::plugin::registry::BusInfo { pub request: BusRequest, pub grant: GrantState, pub resolved: bool }`
  - `crate::plugin::runner::warn_unresolved_bus(manifest: &Manifest, drivers: &DriverRegistry)`

- [ ] **Step 1: 失敗するテストを書く**

`core/src/plugin/runner.rs` のテストモジュールに:

```rust
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
        assert_eq!(bus.retained_for("ed-state", "current-system"), Some(b"b".to_vec()));
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
        bus.emit("ed-state", "current-system", b"Sol".to_vec()).unwrap();

        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        subscribe_with_initial_value(&bus, "translator", "ed-state", "current-system", tx);

        assert_eq!(rx.try_recv().unwrap().payload, b"Sol".to_vec());
    }
```

`core/src/plugin/registry.rs` のテストモジュールに:

```rust
    #[test]
    fn a_bus_request_for_a_missing_driver_is_reported_as_unresolved() {
        // DriverRegistry が空のとき、BusInfo.resolved は false になる。
        // 既存の `Registry` テストヘルパ(`Registry::new` + `push`)で
        // `[[bus]]` を 1 件持つ manifest を載せ、DriverRegistry には何も
        // 登録しない状態を作る。
        let registry = test_registry_with_bus_request();
        let info = registry.bus("translator").unwrap();
        assert_eq!(info.len(), 1);
        assert!(!info[0].resolved);
    }

    /// `[[bus]]` を 1 件持つプラグインだけを載せた `Registry`。
    /// 既存の `Registry` テストヘルパ(このモジュール内の `test_registry` /
    /// `push_plugin` 相当)を使い、manifest の `bus` に
    /// `BusRequest { driver: "ed-state", publish: ["ship-status"],
    /// subscribe: ["current-system"], reason: "r" }` を入れる。
    /// `Registry::new` に渡す `DriverRegistry` は空のものにする。
    fn test_registry_with_bus_request() -> Registry {
        unimplemented!("既存の Registry テストヘルパに合わせて 3 行で書ける")
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core`
Expected: `subscribe_with_initial_value` / `BusInfo` が無くコンパイルエラー

- [ ] **Step 3: 実装する**

`core/src/plugin/runner.rs` に追加する。

```rust
/// 購読を登録し、retain 済みトピックなら現在値を 1 回だけ届ける。
///
/// 後から起動・後から承認されたプラグインにも最新値が渡るようにするため
/// (設計書「データフロー」参照)。ここで送るのは登録直後の 1 通だけで、
/// 以降は通常の `emit` 経路に乗る。
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
```

プラグインスレッドは `events_rx` と `messages_rx` の 2 本を待つ必要がある。**新しいスレッドを増やさず**、`std::sync::mpsc` の 1 本のチャネルに寄せる:

```rust
/// プラグイン専用スレッドが処理する仕事。journal イベントとバスの配信を
/// 1 本のキューに混ぜることで、wasm 呼び出しが 1 スレッドに直列化される
/// 性質(`PluginInstance` が `Send` を気にしなくてよい根拠)を保つ。
enum PluginWork {
    Event(Arc<Event>),
    Message(Delivery),
}
```

`events_tx` / bus の購読 sender はどちらもこの `PluginWork` を運ぶ `SyncSender` にする。ただし **`Bus::subscribe` は `SyncSender<Delivery>` を取る**ので、購読側に容量 `PLUGIN_EVENT_QUEUE_CAPACITY` の `SyncSender<Delivery>` を渡し、それを受けて `PluginWork::Message` に詰め替えて転送する小さな tokio タスクを 1 つ立てる(`spawn_event_subscriber` と同じ形)。

配信のたびに承認を再確認する:承認は稼働中も取り消せるので、転送タスクは `bus_json` を読み、`granted` かつ `subscribe` に含まれるトピックでなければ黙って捨てる。

`Registry::set_bus_grant` は `set_filesystem_grant` と同じ形で、承認変更後に `refresh_bus_runtime`(= `bus_json` の再構築)を呼ぶ。

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-core`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add core
git commit -m "feat(plugin): deliver bus messages and warn about unresolved refs"
```

---

### Task 11: CLI 配線・`STOP_GRACE`・統合テスト

**Files:**
- Create: `core/tests/bus_integration.rs`
- Modify: `core/src/bin/edlr.rs`, `ui/src-tauri/src/daemon.rs`

**Interfaces:**
- Consumes: `start_drivers`(Task 9)、`start_plugins`(Task 9 で引数が変わったもの)
- Produces: `--drivers-dir` フラグ

- [ ] **Step 1: 失敗するテストを書く**

`core/tests/bus_integration.rs`:

```rust
//! ドライバとプラグインを実際に wasm としてロードし、
//! publish → on-message → emit → 購読プラグイン着信 の 1 往復を通す。
//!
//! 使う wasm は `examples/drivers/ed-state` と `examples/plugins/state-reader`
//! (Task 13 で作る)。ビルド済みの成果物が無ければテストは skip する
//! (CI に wasm ターゲットが無い環境でも `cargo test` を壊さないため)。

#[test]
fn a_publish_round_trips_through_the_driver_to_a_subscriber() {
    let Some(driver_wasm) = built_example("examples/drivers/ed-state", "ed_state.wasm") else {
        eprintln!("skipping: build the example driver first");
        return;
    };
    let Some(plugin_wasm) = built_example("examples/plugins/state-reader", "state_reader.wasm")
    else {
        eprintln!("skipping: build the example plugin first");
        return;
    };

    // 1. drivers-dir / plugins-dir をテンポラリに組み立てる
    // 2. bus 接続を承認する(Registry::set_bus_grant)
    // 3. プラグイン側から publish 相当を Bus 経由で流す
    // 4. ドライバが emit した retained 値が Bus::retained_for で読める
    todo!("上記の手順を実装する");
}

fn built_example(dir: &str, file: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(dir)
        .join("target/wasm32-wasip2/release")
        .join(file);
    path.exists().then_some(path)
}

#[test]
fn an_unresolved_bus_reference_does_not_stop_the_plugin() {
    let Some(plugin_wasm) = built_example("examples/plugins/state-reader", "state_reader.wasm")
    else {
        eprintln!("skipping: build the example plugin first");
        return;
    };

    // ドライバを 1 つも置かない(drivers-dir 自体を作らない)状態で、
    // [[bus]] を持つプラグインが Running のままロードされることを確認する。
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("plugins/state-reader");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(&plugin_wasm, plugin_dir.join("plugin.wasm")).unwrap();
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../examples/plugins/state-reader/manifest.toml"),
        plugin_dir.join("manifest.toml"),
    )
    .unwrap();

    let registry = start_plugins_for_test(&tmp.path().join("plugins"), tmp.path());
    let info = registry.list();
    assert_eq!(info.len(), 1);
    assert!(matches!(info[0].state, edlr_core::plugin::PluginState::Running));
}

/// `start_plugins` をテンポラリのストア一式で呼ぶ。`bus` は空の
/// `Bus::new()`(ドライバ未登録)を渡す。
fn start_plugins_for_test(
    plugins_dir: &std::path::Path,
    tmp: &std::path::Path,
) -> edlr_core::plugin::Registry {
    use edlr_core::plugin::*;
    let router = edlr_core::router::Router::new();
    start_plugins(
        plugins_dir,
        SettingsStore::new(tmp.join("settings")),
        SidecarConfigStore::new(tmp.join("settings")),
        FilesystemConfigStore::new(tmp.join("settings"), vec![tmp.to_path_buf()]),
        GrantsStore::new(tmp.join("grants")),
        edlr_driver_channel::Bus::new(),
        &router,
        PluginHost::new().expect("wasmtime engine builds"),
    )
}
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core --test bus_integration`
Expected: `todo!()` で FAIL

- [ ] **Step 3: 実装する**

`core/src/bin/edlr.rs`:

```rust
    /// ドライバディレクトリ。未指定なら $XDG_CONFIG_HOME/edlr/drivers。
    #[arg(long)]
    drivers_dir: Option<PathBuf>,
```

`plugins_dir` と同じ流儀で既定値を解決し、`Bus::new()` を 1 つ作って `start_drivers` → `start_plugins` の順に渡す(ドライバを先に登録しておかないと、プラグインの初回 `get` が `unknown-driver` になるため)。ドライバ用のストアは:

```rust
    let driver_settings_store = SettingsStore::new(settings_dir.join("drivers"));
    let driver_grants_store = GrantsStore::new_for_drivers(grants_dir.clone());
```

`FilesystemConfigStore` の「掴ませない」リストに `drivers_dir` を追加する(既存の `vec![grants_dir, plugins_dir]` に並べる)。

`ui/src-tauri/src/daemon.rs`:

```rust
pub const STOP_GRACE: Duration = Duration::from_secs(95);

const _: () = assert!(
    STOP_GRACE.as_secs()
        > edlr_config::SIDECAR_SHUTDOWN_GRACE_SECS
            * edlr_config::SIDECAR_SHUTDOWN_WORST_CASE_INSTANCES
            + edlr_config::DRIVER_CALL_DEADLINE_SECS,
    "STOP_GRACE must strictly exceed the daemon's worst-case sequential sidecar \
     shutdown time plus one driver call deadline (a driver blocked in an HTTP call \
     cannot answer a stop request until its deadline elapses) -- see STOP_GRACE's \
     doc comment."
);
```

doc コメントにもドライバ期限が加算される理由を書き足す。

統合テストは上記の手順どおりに実装する。

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-core --test bus_integration && (cd ui/src-tauri && cargo test)`
Expected: PASS(サンプル未ビルドなら skip メッセージが出て PASS)

- [ ] **Step 5: コミット**

```bash
git add core ui/src-tauri
git commit -m "feat(core): wire drivers-dir and widen the daemon stop grace"
```

---

### Task 12: WebSocket RPC

**Files:**
- Modify: `core/src/server.rs`

**Interfaces:**
- Consumes: `DriverRegistry`(Task 9)、`Registry::bus` / `set_bus_grant`(Task 10)
- Produces: RPC メソッド `drivers/list` / `drivers/get-settings` / `drivers/set-settings` / `drivers/set-capabilities` / `plugins/set-bus-grant`、および `plugins/list` の各要素に `bus` フィールド

- [ ] **Step 1: 失敗するテストを書く**

`core/src/server.rs` のテストモジュールに:

```rust
    #[test]
    fn drivers_list_returns_the_dir_and_the_topics() {
        let (registry, drivers) = test_registries();
        let result = handle_rpc_with_drivers(Some(&registry), Some(&drivers), "drivers/list", &json!({}))
            .unwrap();
        assert!(result["driversDir"].is_string());
        assert_eq!(result["drivers"][0]["id"], "ed-state");
        assert_eq!(result["drivers"][0]["topics"][0]["name"], "current-system");
        assert_eq!(result["drivers"][0]["topics"][0]["retain"], true);
    }

    #[test]
    fn drivers_rpc_without_a_driver_registry_reports_unavailable() {
        let (registry, _drivers) = test_registries();
        let err = handle_rpc_with_drivers(Some(&registry), None, "drivers/list", &json!({}))
            .unwrap_err();
        assert_eq!(err, "drivers unavailable");
    }

    #[test]
    fn plugins_list_includes_bus_requests_with_their_resolution() {
        let (registry, drivers) = test_registries();
        let result =
            handle_rpc_with_drivers(Some(&registry), Some(&drivers), "plugins/list", &json!({}))
                .unwrap();
        let bus = &result["plugins"][0]["bus"][0];
        assert_eq!(bus["driver"], "ed-state");
        assert_eq!(bus["granted"], false);
        assert_eq!(bus["resolved"], true);
    }

    /// `translator`(`[[bus]]` を 1 件持つ)と `ed-state`(`current-system`
    /// を retain 付きで宣言)をそれぞれ 1 件だけ載せたレジストリの組。
    /// プラグイン側は Task 10 の `test_registry_with_bus_request` を、
    /// ドライバ側は Task 9 の `test_registry` を再利用する
    /// (どちらも wasm をロードせず `push` で組み立てる)。
    fn test_registries() -> (Registry, DriverRegistry) {
        (
            crate::plugin::registry::tests::test_registry_with_bus_request(),
            crate::driver::registry::tests::test_registry(edlr_driver_channel::Bus::new()),
        )
    }

    #[test]
    fn set_bus_grant_requires_a_plugin_and_a_driver() {
        let (registry, drivers) = test_registries();
        assert!(handle_rpc_with_drivers(
            Some(&registry),
            Some(&drivers),
            "plugins/set-bus-grant",
            &json!({"plugin": "translator"})
        )
        .is_err());
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core server`
Expected: `handle_rpc_with_drivers` が無くコンパイルエラー

- [ ] **Step 3: 実装する**

既存の `handle_rpc(registry, method, params)` を `handle_rpc_with_drivers(registry, drivers, method, params)` に拡張する(`handle_rpc` は `drivers = None` で委譲する薄いラッパとして残し、既存テストを壊さない)。`drivers/*` は `drivers` が `None` なら `Err("drivers unavailable")`。

`drivers/list` の応答:

```rust
Ok(serde_json::json!({
    "driversDir": drivers.drivers_dir().to_string_lossy(),
    "drivers": drivers.list().into_iter().map(|info| serde_json::json!({
        "id": info.manifest.id,
        "name": info.manifest.name,
        "version": info.manifest.version,
        "description": info.manifest.description,
        "topics": info.manifest.topics,
        "settings": info.manifest.settings,
        "values": info.values,
        "capabilities": capabilities_result_json(&info.manifest.capabilities, &info.grant_state),
        "sidecars": sidecars_result_json(&info.sidecars)["sidecars"],
        "filesystem": filesystem_result_json(&info.filesystem)["roots"],
        "state": match info.state { DriverState::Running => "running", _ => "disabled" },
    })).collect::<Vec<_>>(),
}))
```

`plugins/list` の各要素に `"bus"` を足す(`BusInfo` から `driver` / `publish` / `subscribe` / `reason` / `granted` / `stale` / `resolved`)。

`ServerState` に `Option<DriverRegistry>` を持たせ、WebSocket ハンドラから渡す。

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test -p edlr-core server`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add core
git commit -m "feat(server): add the drivers RPC and bus info in plugins/list"
```

---

### Task 13: UI(Drivers タブと bus 承認)

**Files:**
- Create: `ui/frontend/src/pages/Drivers.tsx`, `ui/frontend/src/pages/Drivers.test.tsx`, `ui/frontend/src/components/BusSection.tsx`, `ui/frontend/src/components/BusSection.test.tsx`
- Modify: `ui/frontend/src/types/plugin.ts`, `ui/frontend/src/rpc.ts`, `ui/frontend/src/App.tsx`, `ui/frontend/src/pages/Plugins.tsx`

**Interfaces:**
- Consumes: Task 12 の RPC
- Produces:
  - `types/plugin.ts` の `BusRequest { driver: string; publish: string[]; subscribe: string[]; reason: string; granted: boolean; stale: boolean; resolved: boolean }`
  - `types/plugin.ts` の `DriverInfo { id: string; name: string; version: string; description: string; topics: TopicSpec[]; state: "running" | "disabled"; reason?: string; ... }`
  - `rpc.ts` の `listDrivers()` / `setDriverSettings()` / `setDriverCapabilities()` / `setBusGrant()`

- [ ] **Step 1: 失敗するテストを書く**

`ui/frontend/src/components/BusSection.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BusSection } from "./BusSection";

const base = {
  driver: "ed-state",
  publish: ["ship-status"],
  subscribe: ["current-system"],
  reason: "現在システムを購読するため",
  granted: false,
  stale: false,
  resolved: true,
};

describe("BusSection", () => {
  it("shows the driver, the topics and the reason", () => {
    render(<BusSection pluginId="translator" bus={[base]} onSetGrant={vi.fn()} />);
    expect(screen.getByText("ed-state")).toBeInTheDocument();
    expect(screen.getByText(/ship-status/)).toBeInTheDocument();
    expect(screen.getByText(/current-system/)).toBeInTheDocument();
    expect(screen.getByText(/現在システムを購読するため/)).toBeInTheDocument();
  });

  it("marks unresolved connections", () => {
    render(
      <BusSection pluginId="translator" bus={[{ ...base, resolved: false }]} onSetGrant={vi.fn()} />,
    );
    expect(screen.getByText("未解決")).toBeInTheDocument();
  });

  it("marks stale grants as needing re-approval", () => {
    render(
      <BusSection pluginId="translator" bus={[{ ...base, stale: true }]} onSetGrant={vi.fn()} />,
    );
    expect(screen.getByText("要再承認")).toBeInTheDocument();
  });

  it("calls onSetGrant when approved", async () => {
    const onSetGrant = vi.fn();
    render(<BusSection pluginId="translator" bus={[base]} onSetGrant={onSetGrant} />);
    await userEvent.click(screen.getByRole("checkbox"));
    expect(onSetGrant).toHaveBeenCalledWith("translator", "ed-state", true);
  });
});
```

`ui/frontend/src/pages/Drivers.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Drivers } from "./Drivers";

const driver = {
  id: "ed-state",
  name: "ED State",
  version: "0.1.0",
  description: "状態を配るドライバ",
  topics: [{ name: "current-system", retain: true, description: "現在のシステム" }],
  settings: [],
  values: {},
  capabilities: { requests: [], granted: false, stale: false },
  sidecars: [],
  filesystem: [],
  state: "running" as const,
};

describe("Drivers page", () => {
  it("lists installed drivers with their topics", () => {
    render(<Drivers drivers={[driver]} driversDir="/home/u/.config/edlr/drivers" onReload={vi.fn()} />);
    expect(screen.getByText("ED State")).toBeInTheDocument();
    expect(screen.getByText("current-system")).toBeInTheDocument();
    expect(screen.getByText(/retain/i)).toBeInTheDocument();
  });

  it("shows an empty state with the drivers dir", () => {
    render(<Drivers drivers={[]} driversDir="/home/u/.config/edlr/drivers" onReload={vi.fn()} />);
    expect(screen.getByText(/\/home\/u\/\.config\/edlr\/drivers/)).toBeInTheDocument();
  });

  it("shows the disabled reason", () => {
    render(
      <Drivers
        drivers={[{ ...driver, state: "disabled" as const, reason: "on-message call failed" }]}
        driversDir="/d"
        onReload={vi.fn()}
      />,
    );
    expect(screen.getByText(/on-message call failed/)).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cd ui/frontend && mise exec -- pnpm test`
Expected: モジュールが見つからず FAIL

- [ ] **Step 3: 実装する**

`BusSection.tsx` は `FilesystemSection.tsx` と同じ構造(props で状態を受け、承認チェックボックスを出し、`onSetGrant` を呼ぶ)。`resolved === false` のとき「未解決」バッジ、`stale === true` のとき「要再承認」バッジを出す。

`Drivers.tsx` は `Plugins.tsx` の一覧部分を写し、トピック一覧(`retain` の有無つき)、設定フォーム(`PluginForm` を再利用)、capability / sidecar / filesystem の各セクションを並べる。

`App.tsx` のタブに `Drivers` を追加、`rpc.ts` に 4 メソッドを足す。`Plugins.tsx` は `plugins/list` の `bus` を `BusSection` に渡す。

- [ ] **Step 4: テストが通ることを確認する**

Run: `cd ui/frontend && mise exec -- pnpm test`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add ui/frontend
git commit -m "feat(ui): add the Drivers tab and the bus approval section"
```

---

### Task 14: サンプルドライバ・サンプルプラグイン・README

**Files:**
- Create: `examples/drivers/ed-state/Cargo.toml`, `examples/drivers/ed-state/src/lib.rs`, `examples/drivers/ed-state/driver.toml`, `examples/drivers/ed-state/README.md`
- Create: `examples/plugins/state-reader/Cargo.toml`, `examples/plugins/state-reader/src/lib.rs`, `examples/plugins/state-reader/manifest.toml`
- Modify: `README.md`, `core/tests/bus_integration.rs`(skip を外して実際に通す)

**Interfaces:**
- Consumes: `world driver-guest` / `world plugin-guest`(Task 3)
- Produces: ビルド済み wasm(統合テストが参照する)

- [ ] **Step 1: サンプルドライバを書く**

`examples/drivers/ed-state/src/lib.rs`:

```rust
//! 受け取った `set-system` メッセージを retained トピック `current-system`
//! として配り直すだけのサンプルドライバ。

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "driver-guest",
});

struct Component;

impl Guest for Component {
    fn init() {
        host_log::log(host_log::Level::Info, "ed-state driver started");
    }

    fn on_message(from: String, topic: String, payload: Vec<u8>) {
        if topic != "set-system" {
            return;
        }
        host_log::log(
            host_log::Level::Debug,
            &format!("system update from {from}"),
        );
        if let Err(e) = bus_host::emit("current-system", &payload) {
            host_log::log(host_log::Level::Warn, &format!("emit failed: {e:?}"));
        }
    }
}

export!(Component);
```

`examples/drivers/ed-state/driver.toml`:

```toml
id = "ed-state"
name = "ED State"
version = "0.1.0"
description = "受け取ったシステム名を retained トピックとして配る"
entry = "driver.wasm"

[[topics]]
name = "set-system"
retain = false
description = "プラグインからのシステム名の更新"

[[topics]]
name = "current-system"
retain = true
description = "現在のスターシステム"
```

- [ ] **Step 2: サンプルプラグインを書く**

`examples/plugins/state-reader/src/lib.rs`:

```rust
//! `FSDJump` を見たら `ed-state` ドライバへシステム名を publish し、
//! ドライバが配り直した `current-system` を `on-message` で受け取る。

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "plugin-guest",
});

struct Component;

impl Guest for Component {
    fn init() {}

    fn on_event(ev: Event) {
        if ev.name.as_deref() != Some("FSDJump") {
            return;
        }
        let system = serde_json_star_name(&ev.payload_json);
        if let Err(e) = bus::publish("ed-state", "set-system", system.as_bytes()) {
            host_log::log(host_log::Level::Warn, &format!("publish failed: {e:?}"));
        }
    }

    fn on_message(driver: String, topic: String, payload: Vec<u8>) {
        host_log::log(
            host_log::Level::Info,
            &format!(
                "{driver}/{topic} = {}",
                String::from_utf8_lossy(&payload)
            ),
        );
    }
}

/// `StarSystem` を素朴に取り出す(サンプルなので依存を増やさない)。
fn serde_json_star_name(raw: &str) -> String {
    let needle = "\"StarSystem\":\"";
    let Some(start) = raw.find(needle) else {
        return String::new();
    };
    let rest = &raw[start + needle.len()..];
    rest.split('"').next().unwrap_or("").to_string()
}

export!(Component);
```

`examples/plugins/state-reader/manifest.toml`:

```toml
id = "state-reader"
name = "State Reader"
version = "0.1.0"
entry = "plugin.wasm"
events = ["FSDJump"]

[[bus]]
driver = "ed-state"
publish = ["set-system"]
subscribe = ["current-system"]
reason = "ジャンプ先を ed-state ドライバへ渡し、配り直された現在システムを受け取るため"
```

- [ ] **Step 3: 両方をビルドする**

Run:

```bash
(cd examples/drivers/ed-state && cargo build --release --target wasm32-wasip2)
(cd examples/plugins/state-reader && cargo build --release --target wasm32-wasip2)
```

Expected: どちらも成功

- [ ] **Step 4: 統合テストの `todo!()` を実装して通す**

`core/tests/bus_integration.rs` の `a_publish_round_trips_through_the_driver_to_a_subscriber` に残っている `todo!()` を、上でビルドした wasm を使う実装に置き換える(手順はコメントの 1〜4 のとおり)。もう 1 つのテストは Task 11 で実装済み。

Run: `cargo test -p edlr-core --test bus_integration`
Expected: PASS(skip されない)

- [ ] **Step 5: README を書く**

`README.md` の「プラグイン」の節の後ろに「ドライバ(プラグイン間連携)」の節を足し、以下を書く。

- ドライバとは何か(別レイヤー・1 インスタンス・journal イベントは受け取らない)
- `--drivers-dir` を CLI フラグの表に追加
- drivers-dir のレイアウトと `driver.toml` の主なフィールド表
- プラグイン側 `[[bus]]` の書式と承認フロー
- `publish` は fire-and-forget、`get` は retained をホストから読む(ドライバを呼ばない)
- キュー方針の非対称(`publish` は `queue-full`、`emit` の配信は古いものを捨てる)
- ドライバ無効化時に retained が破棄されること
- WIT が `@0.3.0` に上がり、**既存プラグインの再ビルドが必要**なこと
- ドライバ間の通信は無いこと、`examples/drivers/ed-state` の使い方

- [ ] **Step 6: 全テストを通してコミット**

Run: `cargo test && (cd ui/frontend && mise exec -- pnpm test) && (cd ui/src-tauri && cargo test)`
Expected: すべて PASS

```bash
git add examples core README.md
git commit -m "docs: add the bus example driver, plugin and README section"
```

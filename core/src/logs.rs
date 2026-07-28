//! tracing ログを GUI(WS クライアント)へ流すためのブリッジ。
//!
//! `LogLayer` は INFO 以上のイベントを JSON フレームに整形して broadcast へ
//! 送るだけ。**この Layer の中では絶対にログしない**(自分のイベントを
//! 自分で拾う無限ループになる)。送信は `broadcast::Sender::send` のみで
//! 非ブロッキング。受信者がいなければ Err になるが黙って捨てる
//! (ログ表示はベストエフォートで、デーモン本体の動作に影響させない)。
//!
//! フレームは `ServerState::attach_log_stream` が journal/status イベントと
//! 同じ ReplayBuffer + broadcast に合流させ、WS クライアントには
//! `{"type":"event","kind":"log",...}` として届く。

use std::fmt::Write as _;
use std::sync::Arc;

use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// フレームのバッファ容量。WS 側の ReplayBuffer(1000 件)への合流前の
/// 一時経路なので小さめでよい。溢れた分は Lagged として捨てられる。
const CHANNEL_CAPACITY: usize = 256;

pub struct LogLayer {
    tx: broadcast::Sender<Arc<String>>,
}

/// GUI 転送用の Layer と、その出力を受け取る receiver の組を作る。
pub fn log_channel() -> (LogLayer, broadcast::Receiver<Arc<String>>) {
    let (tx, rx) = broadcast::channel(CHANNEL_CAPACITY);
    (LogLayer { tx }, rx)
}

/// WS へ流すフレームの整形(純関数。形は `ws.ts` の `parseWsMessage` と対)。
pub(crate) fn format_log_frame(
    level: &str,
    target: &str,
    timestamp: &str,
    message: &str,
) -> String {
    serde_json::json!({
        "type": "event",
        "kind": "log",
        "timestamp": timestamp,
        "level": level,
        "target": target,
        "message": message,
    })
    .to_string()
}

/// `message` フィールドを本文、それ以外を ` key=value` として畳み込む visitor。
/// stderr の fmt 出力(`plugin_id="x" message`)と概ね同じ情報量を 1 行に保つ。
struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > Level::INFO {
            return; // DEBUG/TRACE は転送しない
        }
        let mut visitor = MessageVisitor {
            message: String::new(),
            fields: String::new(),
        };
        event.record(&mut visitor);
        let message = format!("{}{}", visitor.message, visitor.fields);
        let level_str = match level {
            Level::ERROR => "error",
            Level::WARN => "warn",
            _ => "info",
        };
        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let frame = format_log_frame(level_str, event.metadata().target(), &timestamp, &message);
        // 受信者不在・詰まりは黙って捨てる(モジュールコメント参照)
        let _ = self.tx.send(Arc::new(frame));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_log_frame_produces_the_wire_shape() {
        let frame = format_log_frame(
            "info",
            "edlr_core::plugin",
            "2026-07-28T12:00:00.000Z",
            "hello",
        );
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["type"], "event");
        assert_eq!(v["kind"], "log");
        assert_eq!(v["level"], "info");
        assert_eq!(v["target"], "edlr_core::plugin");
        assert_eq!(v["timestamp"], "2026-07-28T12:00:00.000Z");
        assert_eq!(v["message"], "hello");
    }

    /// Layer を scoped subscriber として使い、INFO 以上だけがフレーム化される
    /// ことを確認する(グローバル subscriber は汚さない)。
    #[test]
    fn log_layer_forwards_info_and_above_with_fields() {
        use tracing_subscriber::layer::SubscriberExt;
        let (layer, mut rx) = log_channel();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!("dropped");
            tracing::info!(plugin_id = "widgety", "plugin started");
            tracing::warn!("watch out");
        });

        let first: serde_json::Value =
            serde_json::from_str(&rx.try_recv().expect("info frame")).unwrap();
        assert_eq!(first["level"], "info");
        let msg = first["message"].as_str().unwrap();
        assert!(msg.contains("plugin started"));
        assert!(
            msg.contains("plugin_id=\"widgety\""),
            "fields must be appended: {msg}"
        );
        assert!(first["timestamp"].as_str().unwrap().contains("T"));

        let second: serde_json::Value =
            serde_json::from_str(&rx.try_recv().expect("warn frame")).unwrap();
        assert_eq!(second["level"], "warn");
        // debug は転送されない
        assert!(rx.try_recv().is_err());
    }
}

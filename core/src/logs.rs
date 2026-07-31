//! tracing ログを GUI(WS クライアント)へ流すためのブリッジ。
//!
//! `LogLayer` は受け取ったイベントを JSON フレームに整形して broadcast へ
//! 送るだけ。**この Layer の中では絶対にログしない**(自分のイベントを
//! 自分で拾う無限ループになる)。送信は `broadcast::Sender::send` のみで
//! 非ブロッキング。受信者がいなければ Err になるが黙って捨てる
//! (ログ表示はベストエフォートで、デーモン本体の動作に影響させない)。
//!
//! **レベルの足切りはこの Layer では行わない**。閾値はデーモンが
//! レジストリ全体に掛ける `env_filter()`(既定 info / `RUST_LOG` で上書き)
//! ただ一箇所で決まり、stderr と GUI は同じものを見る。`RUST_LOG=debug` を
//! 付ければプラグインの `host-log` debug が GUI の Logs 画面にも出る。
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
use tracing_subscriber::{EnvFilter, Layer};

/// `RUST_LOG` が無い(または壊れている)ときのログレベル。
/// 従来の `fmt().init()` / `LevelFilter::INFO` と同じ既定を保つ。
const DEFAULT_LOG_FILTER: &str = "info";

/// デーモンのログフィルタ。既定は info で、`RUST_LOG` があればそれで上書きする。
///
/// レジストリ全体に掛ける前提なので、stderr の fmt 出力と GUI 転送用の
/// [`LogLayer`] は必ず同じ閾値になる。
pub fn env_filter() -> EnvFilter {
    filter_from_env_value(std::env::var("RUST_LOG").ok().as_deref())
}

/// [`env_filter`] の中身(環境変数の読み取りだけを外に出した形)。
///
/// 壊れた `RUST_LOG`(例: `"info,=="`)は既定にフォールバックする。ログの
/// 指定ミスでデーモンが起動しない、という挙動にはしない。
fn filter_from_env_value(value: Option<&str>) -> EnvFilter {
    value
        .filter(|v| !v.trim().is_empty())
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT_LOG_FILTER))
}

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
        let mut visitor = MessageVisitor {
            message: String::new(),
            fields: String::new(),
        };
        event.record(&mut visitor);
        let message = format!("{}{}", visitor.message, visitor.fields);
        let level_str = match level {
            Level::ERROR => "error",
            Level::WARN => "warn",
            Level::INFO => "info",
            Level::DEBUG => "debug",
            _ => "trace",
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
            "edlr_core::registry::plugin",
            "2026-07-28T12:00:00.000Z",
            "hello",
        );
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["type"], "event");
        assert_eq!(v["kind"], "log");
        assert_eq!(v["level"], "info");
        assert_eq!(v["target"], "edlr_core::registry::plugin");
        assert_eq!(v["timestamp"], "2026-07-28T12:00:00.000Z");
        assert_eq!(v["message"], "hello");
    }

    /// Layer を scoped subscriber として使い、既定フィルタ(info)の下では
    /// INFO 以上だけがフレーム化されることを確認する(グローバル subscriber は
    /// 汚さない)。足切りは Layer ではなくフィルタ側の責務なので、フィルタも
    /// 一緒に積んでデーモンと同じ構成にする。
    #[test]
    fn log_layer_forwards_info_and_above_with_fields() {
        use tracing_subscriber::layer::SubscriberExt;
        let (layer, mut rx) = log_channel();
        let subscriber = tracing_subscriber::registry()
            .with(filter_from_env_value(None))
            .with(layer);
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
        // 既定(info)では debug は転送されない
        assert!(rx.try_recv().is_err());
    }

    /// `RUST_LOG=debug` 相当のフィルタなら、プラグインの `host-log` debug
    /// (= `tracing::debug!`)が GUI 転送にも乗る(issue
    /// `info-host-log-debug-efeq`)。
    #[test]
    fn debug_reaches_the_gui_layer_when_the_filter_allows_it() {
        use tracing_subscriber::layer::SubscriberExt;
        let (layer, mut rx) = log_channel();
        let subscriber = tracing_subscriber::registry()
            .with(filter_from_env_value(Some("debug")))
            .with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(plugin_id = "widgety", "checked 3 systems");
        });

        let frame: serde_json::Value =
            serde_json::from_str(&rx.try_recv().expect("debug frame")).unwrap();
        assert_eq!(frame["level"], "debug");
        assert!(frame["message"]
            .as_str()
            .unwrap()
            .contains("checked 3 systems"));
    }

    /// ターゲット指定(`edlr_core::plugin::host=debug`)も素直に効く。
    #[test]
    fn filter_accepts_per_target_directives() {
        use tracing_subscriber::layer::SubscriberExt;
        let (layer, mut rx) = log_channel();
        let subscriber = tracing_subscriber::registry()
            .with(filter_from_env_value(Some(
                "info,edlr_core::logs::tests=debug",
            )))
            .with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!("kept");
        });
        assert!(rx.try_recv().is_ok(), "target directive must be honoured");
    }

    /// 壊れた `RUST_LOG` と空文字は既定(info)へフォールバックする
    /// — ログ指定のミスで起動できなくなる方が困る。
    #[test]
    fn broken_or_empty_rust_log_falls_back_to_info() {
        use tracing_subscriber::layer::SubscriberExt;
        for value in [Some("=="), Some(""), None] {
            let (layer, mut rx) = log_channel();
            let subscriber = tracing_subscriber::registry()
                .with(filter_from_env_value(value))
                .with(layer);
            tracing::subscriber::with_default(subscriber, || {
                tracing::debug!("dropped");
                tracing::info!("kept");
            });
            let frame: serde_json::Value =
                serde_json::from_str(&rx.try_recv().expect("info frame")).unwrap();
            assert_eq!(frame["level"], "info", "value={value:?}");
            assert!(rx.try_recv().is_err(), "debug must stay off for {value:?}");
        }
    }
}

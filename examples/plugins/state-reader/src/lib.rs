//! `FSDJump` を見たら `ed-state` ドライバへシステム名を publish し、
//! ドライバが配り直した `current-system` を `on-message` で受け取る。

use edlr_plugin_sdk as sdk;
use sdk::bus;
use sdk::host_log;

struct Component;

impl sdk::Plugin for Component {
    fn on_event(ev: sdk::Event) {
        if ev.name.as_deref() != Some("FSDJump") {
            return;
        }
        let system = star_system_name(&ev.payload_json);
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
fn star_system_name(raw: &str) -> String {
    let needle = "\"StarSystem\":\"";
    let Some(start) = raw.find(needle) else {
        return String::new();
    };
    let rest = &raw[start + needle.len()..];
    rest.split('"').next().unwrap_or("").to_string()
}

sdk::register!(Component);

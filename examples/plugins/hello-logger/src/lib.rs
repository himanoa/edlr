#![allow(clippy::too_many_arguments)]

use edlr_plugin_sdk as sdk;
use sdk::host_log::{log, Level};
use sdk::host_settings;

struct HelloLogger;

impl sdk::Plugin for HelloLogger {
    fn init() {
        log(Level::Info, "hello-logger initialized");
    }

    fn on_event(ev: sdk::Event) {
        let settings = host_settings::get_all();
        let enabled = serde_json::from_str::<serde_json::Value>(&settings)
            .ok()
            .and_then(|v| v.get("enabled").and_then(|b| b.as_bool()))
            .unwrap_or(true);

        if enabled {
            let name = ev.name.as_deref().unwrap_or("-");
            log(
                Level::Info,
                &format!(
                    "{}:{}{} {}",
                    ev.kind,
                    name,
                    if ev.replay { " (replay)" } else { "" },
                    ev.payload_json
                ),
            );
        }
    }

    fn on_message(driver: String, topic: String, _payload: Vec<u8>) {
        log(
            Level::Debug,
            &format!("ignoring bus message from {driver}/{topic}"),
        );
    }
}

sdk::register!(HelloLogger);

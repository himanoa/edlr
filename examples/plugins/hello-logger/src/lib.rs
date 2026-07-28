#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "plugin",
});

struct HelloLogger;

impl Guest for HelloLogger {
    fn init() {
        edlr::plugin::host_log::log(
            edlr::plugin::host_log::Level::Info,
            "hello-logger initialized",
        );
    }

    fn on_event(ev: Event) {
        let settings = edlr::plugin::host_settings::get_all();
        let enabled = serde_json::from_str::<serde_json::Value>(&settings)
            .ok()
            .and_then(|v| v.get("enabled").and_then(|b| b.as_bool()))
            .unwrap_or(true);

        if enabled {
            let name = ev.name.as_deref().unwrap_or("-");
            edlr::plugin::host_log::log(
                edlr::plugin::host_log::Level::Info,
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
        edlr::plugin::host_log::log(
            edlr::plugin::host_log::Level::Debug,
            &format!("ignoring bus message from {driver}/{topic}"),
        );
    }

    fn on_schedule(_name: String) {}

    fn on_stop() {}
}

export!(HelloLogger);

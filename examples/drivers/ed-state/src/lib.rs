//! 受け取った `set-system` メッセージを retained トピック `current-system`
//! として配り直すだけのサンプルドライバ。

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "driver-guest",
    generate_all,
});

use edlr::plugin::{bus_host, host_log};

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

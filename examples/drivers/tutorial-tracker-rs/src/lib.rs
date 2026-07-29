//! `docs/plugin-tutorial-rust.md` の 6 章で作るドライバ。
//!
//! プラグインから `visit` で受け取った星系名を数え、`last-system`
//! (`retain = true`)へ流し直す。ドライバはプロセス内に 1 インスタンスしか
//! 居ないので、複数のプラグインが publish しても数はこの 1 つに集まる。

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "driver-guest",
    generate_all,
});

use std::cell::RefCell;

use edlr::plugin::{bus_host, host_log};

thread_local! {
    static COUNT: RefCell<u32> = const { RefCell::new(0) };
}

struct Component;

impl Guest for Component {
    fn init() {
        host_log::log(host_log::Level::Info, "tutorial-tracker started");
    }

    fn on_message(from: String, topic: String, payload: Vec<u8>) {
        // ドライバは `driver.toml` の `[[topics]]` に書いた分しか受け取らないが、
        // トピックを増やしたときのために分岐しておく。
        if topic != "visit" {
            return;
        }

        let system = String::from_utf8_lossy(&payload).to_string();
        let count = COUNT.with(|c| {
            let mut c = c.borrow_mut();
            *c += 1;
            *c
        });
        // デーモンのログレベルは INFO 固定なので、debug では何も見えない。
        host_log::log(
            host_log::Level::Info,
            &format!("visit #{count} from {from}: {system}"),
        );

        // JSON の組み立てにライブラリは要らない程度なので、手で作る。
        let json = format!(
            "{{\"system\":\"{}\",\"count\":{count}}}",
            system.replace('\\', "\\\\").replace('"', "\\\"")
        );
        if let Err(e) = bus_host::emit("last-system", json.as_bytes()) {
            host_log::log(host_log::Level::Warn, &format!("emit failed: {e:?}"));
        }
    }
}

export!(Component);

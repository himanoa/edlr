#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "plugin",
});

struct BusyLoop;

impl Guest for BusyLoop {
    fn init() {
        edlr::plugin::host_log::log(edlr::plugin::host_log::Level::Info, "busy-loop initialized");
    }

    #[allow(clippy::empty_loop)]
    fn on_event(_ev: Event) {
        loop {
            // Intentionally never terminates. Used to exercise the host's
            // epoch-based call deadline.
        }
    }

    fn on_message(_driver: String, _topic: String, _payload: Vec<u8>) {}
}

export!(BusyLoop);

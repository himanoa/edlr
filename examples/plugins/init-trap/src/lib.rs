#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "plugin",
});

struct InitTrap;

impl Guest for InitTrap {
    #[allow(clippy::empty_loop)]
    fn init() {
        loop {
            // Intentionally never terminates, so the host's epoch-based call
            // deadline traps this call and `call_init` returns `Err`. This
            // exercises the runner's "init() failed -> Disabled, no event
            // task started" path.
        }
    }

    fn on_event(_ev: Event) {
        // Never reached: init() always traps, so the runner never starts an
        // event task for this plugin.
    }

    fn on_message(_driver: String, _topic: String, _payload: Vec<u8>) {}
}

export!(InitTrap);

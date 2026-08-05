#![allow(clippy::too_many_arguments)]

use edlr_plugin_sdk as sdk;
use sdk::host_log::{log, Level};

struct BusyLoop;

impl sdk::Plugin for BusyLoop {
    fn init() {
        log(Level::Info, "busy-loop initialized");
    }

    #[allow(clippy::empty_loop)]
    fn on_event(_ev: sdk::Event) {
        loop {
            // Intentionally never terminates. Used to exercise the host's
            // epoch-based call deadline.
        }
    }
}

sdk::register!(BusyLoop);

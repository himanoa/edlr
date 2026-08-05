#![allow(clippy::too_many_arguments)]

use edlr_plugin_sdk as sdk;

struct InitTrap;

impl sdk::Plugin for InitTrap {
    #[allow(clippy::empty_loop)]
    fn init() {
        loop {
            // Intentionally never terminates, so the host's epoch-based call
            // deadline traps this call and `call_init` returns `Err`. This
            // exercises the runner's "init() failed -> Disabled, no event
            // task started" path.
        }
    }
}

sdk::register!(InitTrap);

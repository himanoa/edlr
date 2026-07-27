#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "plugin",
});

/// Size of each chunk allocated per loop iteration in `on_event`. Large
/// enough that only a handful of iterations are needed to blow through the
/// host's per-plugin memory limit (see `PLUGIN_MEMORY_LIMIT` in
/// `core/src/plugin/host.rs`), so the memory trap fires well before the
/// host's 2s call-deadline trap could.
const CHUNK_BYTES: usize = 8 * 1024 * 1024;

struct MemoryHog;

impl Guest for MemoryHog {
    fn init() {
        edlr::plugin::host_log::log(edlr::plugin::host_log::Level::Info, "memory-hog initialized");
    }

    fn on_event(_ev: Event) {
        // Intentionally never terminates on its own: keeps allocating and
        // touching 8 MiB chunks until the host's `StoreLimits` memory cap
        // traps the guest's `memory.grow`. Chunks are retained (not
        // dropped) and filled with a non-zero byte so the allocator can't
        // elide the growth or reuse freed pages.
        let mut hog: Vec<Vec<u8>> = Vec::new();
        loop {
            hog.push(vec![0xABu8; CHUNK_BYTES]);
        }
    }

    fn on_message(_driver: String, _topic: String, _payload: Vec<u8>) {}
}

export!(MemoryHog);

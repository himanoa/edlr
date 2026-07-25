//! wasmtime component host: loads plugin components, wires host-log /
//! host-settings imports, and enforces a per-call deadline via epoch
//! interruption so a runaway guest traps instead of hanging the kernel.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "plugin",
    });
}

use bindings::edlr::plugin::host_log::{Host as HostLogHost, Level as WitLevel};
use bindings::edlr::plugin::host_settings::Host as HostSettingsHost;
use bindings::{Event as WitEvent, Plugin as PluginBindings};

/// Interval between epoch ticks driven by the background ticker thread.
const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Per-plugin-instance host state, exposed to the guest via the generated
/// `Host` traits.
pub struct HostCtx {
    pub plugin_id: String,
    /// Effective settings JSON string. Wrapped so the runner can swap the
    /// value in place between calls without reloading the plugin.
    pub settings_json: Arc<Mutex<String>>,
    /// WASI state. The `plugin` world itself does not import WASI, but
    /// components built for the `wasm32-wasip2` target still import a
    /// baseline set of WASI interfaces (io, random, clocks, ...) from the
    /// Rust standard library / adapter, so the host must satisfy them.
    wasi_ctx: WasiCtx,
    wasi_table: ResourceTable,
}

impl HostCtx {
    pub fn new(plugin_id: String, settings_json: Arc<Mutex<String>>) -> HostCtx {
        HostCtx {
            plugin_id,
            settings_json,
            wasi_ctx: WasiCtxBuilder::new().build(),
            wasi_table: ResourceTable::new(),
        }
    }
}

impl WasiView for HostCtx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.wasi_table,
        }
    }
}

impl HostLogHost for HostCtx {
    fn log(&mut self, level: WitLevel, message: String) {
        let plugin_id = self.plugin_id.as_str();
        match level {
            WitLevel::Debug => tracing::debug!(plugin_id, "{message}"),
            WitLevel::Info => tracing::info!(plugin_id, "{message}"),
            WitLevel::Warn => tracing::warn!(plugin_id, "{message}"),
            WitLevel::Error => tracing::error!(plugin_id, "{message}"),
        }
    }
}

impl HostSettingsHost for HostCtx {
    fn get_all(&mut self) -> String {
        self.settings_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Owns the wasmtime `Engine` and a background thread that periodically
/// increments the engine's epoch counter, driving epoch-interruption-based
/// call deadlines for every plugin instance loaded from this host.
pub struct PluginHost {
    engine: Engine,
    ticker_stop: Arc<AtomicBool>,
}

impl PluginHost {
    pub fn new() -> anyhow::Result<PluginHost> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)
            .map_err(|e| anyhow::anyhow!("failed to create wasmtime engine: {e}"))?;

        let ticker_stop = Arc::new(AtomicBool::new(false));
        let ticker_engine = engine.clone();
        let ticker_stop_flag = ticker_stop.clone();
        thread::spawn(move || {
            while !ticker_stop_flag.load(Ordering::Relaxed) {
                thread::sleep(EPOCH_TICK_INTERVAL);
                ticker_engine.increment_epoch();
            }
        });

        Ok(PluginHost {
            engine,
            ticker_stop,
        })
    }

    pub fn load(&self, wasm_path: &Path, ctx: HostCtx) -> anyhow::Result<PluginInstance> {
        let component = Component::from_file(&self.engine, wasm_path).map_err(|e| {
            anyhow::anyhow!("failed to load component at {}: {e}", wasm_path.display())
        })?;

        let mut linker = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| anyhow::anyhow!("failed to wire WASI imports into linker: {e}"))?;
        PluginBindings::add_to_linker::<_, HasSelf<_>>(&mut linker, |ctx| ctx)
            .map_err(|e| anyhow::anyhow!("failed to wire host imports into linker: {e}"))?;

        let mut store = Store::new(&self.engine, ctx);
        // Ticks-beyond-current is set fresh before every call in
        // `PluginInstance::call`; this initial deadline just prevents
        // instantiation itself (which may run guest start code) from
        // hanging forever.
        store.set_epoch_deadline(deadline_ticks(PluginInstance::CALL_DEADLINE));

        let bindings = PluginBindings::instantiate(&mut store, &component, &linker)
            .map_err(|e| anyhow::anyhow!("failed to instantiate plugin component: {e}"))?;

        Ok(PluginInstance { store, bindings })
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
    }
}

/// Number of epoch ticks corresponding to `duration`, rounded up, with a
/// minimum of one tick so a zero-length deadline still traps promptly.
fn deadline_ticks(duration: Duration) -> u64 {
    let ticks = duration.as_nanos().div_ceil(EPOCH_TICK_INTERVAL.as_nanos());
    u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
}

/// A loaded, instantiated plugin component together with its store.
pub struct PluginInstance {
    store: Store<HostCtx>,
    bindings: PluginBindings,
}

impl PluginInstance {
    /// Maximum wall-clock time a single guest call may take before the host
    /// forcibly traps it via epoch interruption.
    pub const CALL_DEADLINE: Duration = Duration::from_secs(2);

    pub fn call_init(&mut self) -> anyhow::Result<()> {
        self.store
            .set_epoch_deadline(deadline_ticks(Self::CALL_DEADLINE));
        self.bindings
            .call_init(&mut self.store)
            .map_err(|e| anyhow::anyhow!("plugin init() call failed or timed out: {e}"))
    }

    pub fn call_on_event(
        &mut self,
        kind: &str,
        timestamp: Option<&str>,
        name: Option<&str>,
        payload_json: &str,
    ) -> anyhow::Result<()> {
        let event = WitEvent {
            kind: kind.to_string(),
            timestamp: timestamp.map(|s| s.to_string()),
            name: name.map(|s| s.to_string()),
            payload_json: payload_json.to_string(),
        };
        self.store
            .set_epoch_deadline(deadline_ticks(Self::CALL_DEADLINE));
        self.bindings
            .call_on_event(&mut self.store, &event)
            .map_err(|e| anyhow::anyhow!("plugin on-event() call failed or timed out: {e}"))
    }
}

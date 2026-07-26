//! wasmtime component host: loads plugin components, wires host-log /
//! host-settings imports, and enforces a per-call deadline via epoch
//! interruption so a runaway guest traps instead of hanging the kernel.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "plugin",
    });
}

use bindings::edlr::plugin::driver_http::{
    DriverError as WitDriverError, Host as DriverHttpHost, Request as WitRequest,
    Response as WitResponse,
};
use bindings::edlr::plugin::host_log::{Host as HostLogHost, Level as WitLevel};
use bindings::edlr::plugin::host_settings::Host as HostSettingsHost;
use bindings::{Event as WitEvent, Plugin as PluginBindings};

use crate::plugin::allowlist::check_url;

/// Public re-exports of the generated `driver-http` WIT types, under names
/// that don't collide with `edlr_driver_http`'s own (structurally identical
/// but distinct) `Http*` types. These exist so that in-tree consumers --
/// currently `core/tests/driver_http_integration.rs` -- can call
/// `HostCtx::send` (the WIT-facing entry point) directly, without a full
/// wasm round-trip, the same way this module's own unit tests do. The
/// generated `bindings` module otherwise stays private.
pub use bindings::edlr::plugin::driver_http::{
    DriverError as WitHttpError, Host as WitDriverHttpHost, Request as WitHttpRequest,
    Response as WitHttpResponse,
};

/// Interval between epoch ticks driven by the background ticker thread.
const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Per-call timeout applied to every `driver-http.send` request (covers
/// connect through to the full response). Fixed for now; could become
/// configurable per-plugin later if a legitimate use case needs it.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum response body size, in bytes, a `driver-http.send` call will
/// return before failing with a `transport` error. See
/// `edlr_driver_http::HttpDriver::send` for how this is enforced (a
/// streaming read capped at this limit, not a trusted `Content-Length`
/// header).
pub const HTTP_MAX_BODY: usize = 8 * 1024 * 1024;

/// Maximum linear memory (bytes) a single plugin instance may allocate.
///
/// The epoch deadline (see `PluginInstance::CALL_DEADLINE`) bounds how long a
/// guest call may run, but says nothing about how much memory it may claim
/// while doing so; without a cap a plugin can grow its linear memory without
/// bound and OOM-kill the whole daemon, defeating the isolation the plugin
/// host is meant to provide. 64 MiB is a generous ceiling for the kind of
/// small, single-purpose plugins this host targets (log formatters, simple
/// notifiers, ...); it is a fixed constant for now but can be made
/// configurable per-plugin later if a legitimate use case needs more.
const PLUGIN_MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Maximum number of component instances / tables a single plugin `Store`
/// may create. Plugins in this host are single-component and single-table by
/// construction; these caps are a cheap extra guard against a
/// pathological/malicious guest rather than a limit anyone should expect to
/// approach in normal use.
const PLUGIN_INSTANCE_LIMIT: usize = 8;
const PLUGIN_TABLE_LIMIT: usize = 8;

/// Per-plugin-instance host state, exposed to the guest via the generated
/// `Host` traits.
pub struct HostCtx {
    pub plugin_id: String,
    /// Effective settings JSON string. Wrapped so the runner can swap the
    /// value in place between calls without reloading the plugin.
    pub settings_json: Arc<Mutex<String>>,
    /// Capability grant state JSON string, shape `{"granted": bool, "hosts":
    /// ["https://..."]}`. Built by the runner at startup from `GrantsStore` +
    /// the manifest's requested hosts, and readable/writable live via the
    /// same `Arc` the `Registry` holds -- so approving/revoking a capability
    /// takes effect on the very next `driver-http.send` call, no plugin
    /// restart required.
    ///
    /// This is the *only* source the `driver-http` host implementation
    /// consults to decide whether a call is permitted: the guest never
    /// passes its own id or grants as an argument, so a plugin cannot
    /// forge or observe another plugin's capability state, nor influence
    /// the decision through its inputs.
    pub capabilities_json: Arc<Mutex<String>>,
    /// The HTTP client used by `driver-http.send` once a call has passed
    /// the permission check above. Shared (via `Arc`) across every plugin
    /// instance the owning `PluginHost` loads -- see `PluginHost::new`,
    /// which builds exactly one `HttpDriver` for the whole host process, so
    /// construction cost (TLS setup, etc.) is paid once rather than per
    /// call or per plugin.
    http_driver: Arc<edlr_driver_http::HttpDriver>,
    /// WASI state. The `plugin` world itself does not import WASI, but
    /// components built for the `wasm32-wasip2` target still import a
    /// baseline set of WASI interfaces (io, random, clocks, ...) from the
    /// Rust standard library / adapter, so the host must satisfy them.
    wasi_ctx: WasiCtx,
    wasi_table: ResourceTable,
    /// Resource limits (memory/instances/tables) enforced on this plugin's
    /// `Store` via `Store::limiter`. See `PLUGIN_MEMORY_LIMIT`.
    limits: StoreLimits,
}

impl HostCtx {
    pub fn new(
        plugin_id: String,
        settings_json: Arc<Mutex<String>>,
        capabilities_json: Arc<Mutex<String>>,
        http_driver: Arc<edlr_driver_http::HttpDriver>,
    ) -> HostCtx {
        HostCtx {
            plugin_id,
            settings_json,
            capabilities_json,
            http_driver,
            // Deliberately empty sandbox default: no preopened directories,
            // no stdio, no network access, so a plugin can only interact
            // with the host through the `plugin` world's explicit imports.
            wasi_ctx: WasiCtxBuilder::new().build(),
            wasi_table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(PLUGIN_MEMORY_LIMIT)
                .instances(PLUGIN_INSTANCE_LIMIT)
                .tables(PLUGIN_TABLE_LIMIT)
                .trap_on_grow_failure(true)
                .build(),
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

/// Serializes the `capabilities_json` shape (`{"granted": bool, "hosts":
/// [...]}`) shared between `HostCtx` and `Registry`. `hosts` are only ever
/// consulted by `driver-http.send` when `granted` is true (see
/// `DriverHttpHost::send` below), and the ungranted case emits an empty
/// `hosts` list regardless of what's passed in, so the serialized buffer
/// itself carries no host information at all while ungranted -- there's
/// nothing to observe even if some future caller reads `hosts` without
/// checking `granted` first.
pub fn capabilities_json_string(granted: bool, hosts: &[String]) -> String {
    let hosts: &[String] = if granted { hosts } else { &[] };
    serde_json::to_string(&serde_json::json!({
        "granted": granted,
        "hosts": hosts,
    }))
    .unwrap_or_else(|_| r#"{"granted":false,"hosts":[]}"#.to_string())
}

/// Parsed view of `capabilities_json`, defaulting to "nothing granted" if the
/// shared buffer somehow holds unparseable or unexpected-shape JSON (this
/// host implementation never writes such a value, but the field is a shared
/// `Arc<Mutex<String>>` deliberately parsed defensively rather than trusted).
struct Capabilities {
    granted: bool,
    hosts: Vec<String>,
}

fn parse_capabilities(raw: &str) -> Capabilities {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => {
            return Capabilities {
                granted: false,
                hosts: Vec::new(),
            }
        }
    };

    let granted = value
        .get("granted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let hosts = value
        .get("hosts")
        .and_then(serde_json::Value::as_array)
        .map(|hosts| {
            hosts
                .iter()
                .filter_map(|h| h.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    Capabilities { granted, hosts }
}

impl DriverHttpHost for HostCtx {
    /// First decides *whether* the call is permitted, then -- only if so --
    /// performs it: not granted -> `permission-denied`; granted but the URL
    /// is outside the plugin's allowlisted hosts -> `permission-denied`
    /// with the allowlist rejection reason; otherwise the request is
    /// handed to `self.http_driver`, and its result is mapped onto the WIT
    /// types (`HttpError::InvalidRequest` -> `invalid-request`,
    /// `HttpError::Transport` -> `transport`).
    ///
    /// The permission decision is made *entirely* from
    /// `self.capabilities_json`, which is per-`HostCtx` (i.e. per plugin
    /// instance) and never derived from `req`. The guest supplies only the
    /// URL it wants to reach; it has no way to supply or influence its own
    /// grant state. Critically, the driver is never invoked until *after*
    /// this check succeeds -- a disallowed host is rejected without any
    /// network connection being attempted.
    fn send(&mut self, req: WitRequest) -> Result<WitResponse, WitDriverError> {
        let raw = self
            .capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let capabilities = parse_capabilities(&raw);

        if !capabilities.granted {
            return Err(WitDriverError::PermissionDenied(
                "capability not granted".to_string(),
            ));
        }

        check_url(&capabilities.hosts, &req.url).map_err(WitDriverError::PermissionDenied)?;

        let driver_request = edlr_driver_http::HttpRequest {
            method: req.method,
            url: req.url,
            headers: req.headers,
            body: req.body,
        };

        self.http_driver
            .send(driver_request)
            .map(|response| WitResponse {
                status: response.status,
                headers: response.headers,
                body: response.body,
            })
            .map_err(|e| match e {
                edlr_driver_http::HttpError::InvalidRequest(msg) => {
                    WitDriverError::InvalidRequest(msg)
                }
                edlr_driver_http::HttpError::Transport(msg) => WitDriverError::Transport(msg),
            })
    }
}

/// Owns the wasmtime `Engine` and a background thread that periodically
/// increments the engine's epoch counter, driving epoch-interruption-based
/// call deadlines for every plugin instance loaded from this host.
pub struct PluginHost {
    engine: Engine,
    ticker_stop: Arc<AtomicBool>,
    /// The single `HttpDriver` shared by every plugin instance this host
    /// loads. Built once here (not per plugin, not per call) since
    /// constructing a `reqwest::blocking::Client` sets up TLS/connection
    /// pool state that's meant to be reused.
    http_driver: Arc<edlr_driver_http::HttpDriver>,
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

        let http_driver = Arc::new(
            edlr_driver_http::HttpDriver::new(HTTP_TIMEOUT, HTTP_MAX_BODY)
                .map_err(|e| anyhow::anyhow!("failed to build http driver: {e}"))?,
        );

        Ok(PluginHost {
            engine,
            ticker_stop,
            http_driver,
        })
    }

    /// Returns a clone of the shared `HttpDriver` `Arc` for wiring into a
    /// new plugin's `HostCtx`. Cloning an `Arc` is cheap; this does not
    /// build a new HTTP client.
    pub fn http_driver(&self) -> Arc<edlr_driver_http::HttpDriver> {
        self.http_driver.clone()
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
        store.limiter(|ctx| &mut ctx.limits);
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

#[cfg(test)]
mod tests {
    //! Unit tests for `DriverHttpHost::send` called directly against
    //! `HostCtx`, without going through wasm at all. This is possible (and
    //! preferable to a full wasm round-trip for exercising the permission
    //! logic) because the whole point of the design is that the decision is
    //! made purely from `HostCtx`'s own `capabilities_json`, never from
    //! anything the guest passes in `req` -- so a host-side call with a
    //! hand-built `WitRequest` exercises exactly the same decision path a
    //! real guest call would.
    use super::*;

    fn test_http_driver() -> Arc<edlr_driver_http::HttpDriver> {
        Arc::new(
            edlr_driver_http::HttpDriver::new(HTTP_TIMEOUT, HTTP_MAX_BODY)
                .expect("build test http driver"),
        )
    }

    fn ctx(capabilities_json: &str) -> HostCtx {
        HostCtx::new(
            "test-plugin".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(capabilities_json.to_string())),
            test_http_driver(),
        )
    }

    fn request(url: &str) -> WitRequest {
        WitRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
        }
    }

    #[test]
    fn capabilities_json_string_omits_hosts_when_ungranted() {
        let json = capabilities_json_string(
            false,
            &[
                "https://api.example.com".to_string(),
                "https://x.com".to_string(),
            ],
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["granted"], serde_json::json!(false));
        assert_eq!(parsed["hosts"], serde_json::json!([]));
    }

    #[test]
    fn capabilities_json_string_includes_hosts_when_granted() {
        let json = capabilities_json_string(true, &["https://api.example.com".to_string()]);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["granted"], serde_json::json!(true));
        assert_eq!(
            parsed["hosts"],
            serde_json::json!(["https://api.example.com"])
        );
    }

    #[test]
    fn send_without_grant_is_permission_denied() {
        let mut ctx = ctx(&capabilities_json_string(false, &[]));

        let err = ctx
            .send(request("https://api.example.com/ping"))
            .expect_err("ungranted call should be rejected");

        assert!(
            matches!(err, WitDriverError::PermissionDenied(msg) if msg == "capability not granted")
        );
    }

    #[test]
    fn send_granted_but_disallowed_host_is_permission_denied() {
        let mut ctx = ctx(&capabilities_json_string(
            true,
            &["https://api.example.com".to_string()],
        ));

        let err = ctx
            .send(request("https://evil.example.com/ping"))
            .expect_err("call to a non-allowlisted host should be rejected");

        assert!(matches!(err, WitDriverError::PermissionDenied(_)));
    }

    /// Once permission checks pass, `send` must actually dispatch to the
    /// driver rather than short-circuiting -- this is what task 3's
    /// `not-implemented` placeholder used to do, and this task replaces it
    /// with real networking. This test doesn't hit a live server (see
    /// `core/tests/driver_http_integration.rs` for the real end-to-end
    /// cases); it targets a bound-then-dropped local port, which is
    /// guaranteed to refuse the connection, to prove the call reaches the
    /// driver and comes back as a typed `transport` error rather than a
    /// `permission-denied` or a panic.
    #[test]
    fn send_granted_and_allowed_host_reaches_the_driver() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);
        let url = format!("http://{addr}/ping");

        let mut ctx = ctx(&capabilities_json_string(true, &[format!("http://{addr}")]));

        let err = ctx
            .send(request(&url))
            .expect_err("connection to a closed local port should fail");

        assert!(matches!(err, WitDriverError::Transport(_)));
    }
}

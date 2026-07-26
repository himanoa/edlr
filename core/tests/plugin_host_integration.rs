use edlr_core::plugin::host::{HostCtx, PluginHost, PluginInstance, HTTP_MAX_BODY, HTTP_TIMEOUT};
use edlr_driver_http::HttpDriver;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Builds the fixture plugin crate at `crate_dir` for wasm32-wasip2 (release)
/// and returns the path to the resulting component. Cargo no-ops if the
/// artifact is already fresh.
fn build_fixture(crate_dir: &str, artifact_name: &str) -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crate_path = manifest_dir.join("..").join(crate_dir);

    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2", "--release"])
        .current_dir(&crate_path)
        .status()
        .expect("failed to spawn cargo build for fixture plugin");
    assert!(status.success(), "fixture build failed: {crate_dir}");

    crate_path
        .join("target/wasm32-wasip2/release")
        .join(artifact_name)
}

fn hello_logger_wasm() -> PathBuf {
    build_fixture("examples/plugins/hello-logger", "hello_logger.wasm")
}

fn busy_loop_wasm() -> PathBuf {
    build_fixture("examples/plugins/busy-loop", "busy_loop.wasm")
}

fn memory_hog_wasm() -> PathBuf {
    build_fixture("examples/plugins/memory-hog", "memory_hog.wasm")
}

fn http_caller_wasm() -> PathBuf {
    build_fixture("examples/plugins/http-caller", "http_caller.wasm")
}

/// Default `capabilities_json` for tests that don't care about capabilities:
/// nothing granted, no hosts.
const NO_CAPABILITIES: &str = r#"{"granted":false,"hosts":[]}"#;

fn ctx(settings_json: &str) -> (HostCtx, Arc<Mutex<String>>) {
    let (ctx, settings, _capabilities) = ctx_with_capabilities(settings_json, NO_CAPABILITIES);
    (ctx, settings)
}

/// These tests never exercise real networking (the http-caller fixture's
/// calls are always denied or would fail fast), so any driver instance
/// works; reuse the production constants purely to avoid a second
/// hardcoded timeout/cap drifting from `core/src/plugin/host.rs`.
fn test_http_driver() -> Arc<edlr_driver_http::HttpDriver> {
    Arc::new(HttpDriver::new(HTTP_TIMEOUT, HTTP_MAX_BODY).expect("build test http driver"))
}

fn ctx_with_capabilities(
    settings_json: &str,
    capabilities_json: &str,
) -> (HostCtx, Arc<Mutex<String>>, Arc<Mutex<String>>) {
    let settings = Arc::new(Mutex::new(settings_json.to_string()));
    let capabilities = Arc::new(Mutex::new(capabilities_json.to_string()));
    (
        HostCtx::new(
            "test-plugin".to_string(),
            settings.clone(),
            capabilities.clone(),
            test_http_driver(),
        ),
        settings,
        capabilities,
    )
}

fn load(
    host: &PluginHost,
    wasm_path: &Path,
    settings_json: &str,
) -> (PluginInstance, Arc<Mutex<String>>) {
    let (ctx, settings) = ctx(settings_json);
    let instance = host.load(wasm_path, ctx).expect("load should succeed");
    (instance, settings)
}

fn load_with_capabilities(
    host: &PluginHost,
    wasm_path: &Path,
    settings_json: &str,
    capabilities_json: &str,
) -> (PluginInstance, Arc<Mutex<String>>, Arc<Mutex<String>>) {
    let (ctx, settings, capabilities) = ctx_with_capabilities(settings_json, capabilities_json);
    let instance = host.load(wasm_path, ctx).expect("load should succeed");
    (instance, settings, capabilities)
}

#[test]
fn load_and_init_ok() {
    let wasm = hello_logger_wasm();
    let host = PluginHost::new().expect("host should start");
    let (mut instance, _settings) = load(&host, &wasm, r#"{"enabled": true}"#);

    instance.call_init().expect("call_init should succeed");
}

#[test]
fn on_event_ok() {
    let wasm = hello_logger_wasm();
    let host = PluginHost::new().expect("host should start");
    let (mut instance, _settings) = load(&host, &wasm, r#"{"enabled": true}"#);

    instance.call_init().expect("call_init should succeed");
    instance
        .call_on_event("journal", None, Some("FSDJump"), "{}")
        .expect("call_on_event should succeed");
}

#[test]
fn on_event_ok_after_settings_swap() {
    let wasm = hello_logger_wasm();
    let host = PluginHost::new().expect("host should start");
    let (mut instance, settings) = load(&host, &wasm, r#"{"enabled": true}"#);

    instance.call_init().expect("call_init should succeed");
    instance
        .call_on_event("journal", None, Some("FSDJump"), "{}")
        .expect("first call_on_event should succeed");

    *settings.lock().unwrap() = r#"{"enabled": false}"#.to_string();

    instance
        .call_on_event("journal", None, Some("FSDJump"), "{}")
        .expect("call_on_event after settings swap should succeed");
}

#[test]
fn load_nonexistent_path_returns_err() {
    let host = PluginHost::new().expect("host should start");
    let (ctx, _settings) = ctx(r#"{}"#);

    let result = host.load(Path::new("/nonexistent/path/does-not-exist.wasm"), ctx);

    assert!(result.is_err());
}

#[test]
fn busy_loop_on_event_hits_deadline() {
    let wasm = busy_loop_wasm();
    let host = PluginHost::new().expect("host should start");
    let (mut instance, _settings) = load(&host, &wasm, r#"{}"#);

    let start = Instant::now();
    let result = instance.call_on_event("journal", None, Some("FSDJump"), "{}");
    let elapsed = start.elapsed();

    assert!(result.is_err(), "busy loop call should trap/err");
    assert!(
        elapsed < PluginInstance::CALL_DEADLINE + std::time::Duration::from_secs(5),
        "call took too long: {elapsed:?}"
    );
}

/// Guards against unbounded linear-memory growth: a plugin that keeps
/// allocating past the host's `StoreLimits` memory cap (64 MiB, see
/// `PLUGIN_MEMORY_LIMIT` in `core/src/plugin/host.rs`) must be trapped by
/// the host rather than being allowed to OOM the daemon process.
///
/// The fixture allocates in 8 MiB chunks, so it blows through the 64 MiB
/// cap in well under a second -- far short of `PluginInstance::CALL_DEADLINE`
/// (2s). This test can't fully distinguish "trapped for memory" from
/// "trapped for time" purely from the `Err` return (wasmtime does not
/// expose a structured trap-reason enum through this host's error mapping),
/// but the tight elapsed-time bound below makes a deadline-trap implausible.
#[test]
fn memory_hog_on_event_hits_memory_limit() {
    let wasm = memory_hog_wasm();
    let host = PluginHost::new().expect("host should start");
    let (mut instance, _settings) = load(&host, &wasm, r#"{}"#);

    instance.call_init().expect("call_init should succeed");

    let start = Instant::now();
    let result = instance.call_on_event("journal", None, Some("FSDJump"), "{}");
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "memory hog call should trap/err once it exceeds the host's memory limit"
    );
    assert!(
        elapsed < PluginInstance::CALL_DEADLINE,
        "call took {elapsed:?}, which is suspiciously close to the call deadline; \
         this may be a deadline trap rather than a memory-limit trap"
    );
}

/// The `http-caller` fixture calls `driver-http.send` once in `on-event` and
/// catches whatever the host returns (logging it via `host-log`) instead of
/// propagating it. With capabilities ungranted, the host implementation
/// returns a typed `permission-denied` error, not a trap -- so the guest
/// call must complete normally (`Ok`), never trap.
#[test]
fn http_caller_on_event_does_not_trap_when_ungranted() {
    let wasm = http_caller_wasm();
    let host = PluginHost::new().expect("host should start");
    let (mut instance, _settings, _capabilities) =
        load_with_capabilities(&host, &wasm, r#"{}"#, NO_CAPABILITIES);

    instance.call_init().expect("call_init should succeed");
    instance
        .call_on_event("journal", None, Some("FSDJump"), "{}")
        .expect("on-event should not trap when the capability is ungranted");
}

/// Same as above, but with the capability granted for the url the fixture
/// calls, so the call reaches the real `HttpDriver` this task wires in.
/// Points at a bound-then-dropped local port (rather than a real internet
/// host) so the driver fails fast with a typed `transport` error -- still
/// not a trap, and without this test depending on outbound network access
/// being available in whatever environment runs it.
#[test]
fn http_caller_on_event_does_not_trap_when_granted_for_allowed_host() {
    let wasm = http_caller_wasm();
    let host = PluginHost::new().expect("host should start");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind throwaway listener");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);

    let settings_json = format!(r#"{{"url":"http://{addr}/ping"}}"#);
    let capabilities_json = format!(r#"{{"granted":true,"hosts":["http://{addr}"]}}"#);
    let (mut instance, _settings, _capabilities) =
        load_with_capabilities(&host, &wasm, &settings_json, &capabilities_json);

    instance.call_init().expect("call_init should succeed");
    instance
        .call_on_event("journal", None, Some("FSDJump"), "{}")
        .expect("on-event should not trap once permission checks pass");
}

/// A grant for a *different* host must not authorize the fixture's default
/// url; the host implementation should still return a typed
/// `permission-denied` (not a trap) because `check_url` rejects it.
#[test]
fn http_caller_on_event_does_not_trap_when_granted_for_other_host() {
    let wasm = http_caller_wasm();
    let host = PluginHost::new().expect("host should start");
    let capabilities_json = r#"{"granted":true,"hosts":["https://other.example.com"]}"#;
    let (mut instance, _settings, _capabilities) =
        load_with_capabilities(&host, &wasm, r#"{}"#, capabilities_json);

    instance.call_init().expect("call_init should succeed");
    instance
        .call_on_event("journal", None, Some("FSDJump"), "{}")
        .expect("on-event should not trap when granted for an unrelated host");
}

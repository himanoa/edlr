use edlr_core::plugin::host::{HostCtx, PluginHost, PluginInstance};
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

fn ctx(settings_json: &str) -> (HostCtx, Arc<Mutex<String>>) {
    let settings = Arc::new(Mutex::new(settings_json.to_string()));
    (
        HostCtx::new("test-plugin".to_string(), settings.clone()),
        settings,
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

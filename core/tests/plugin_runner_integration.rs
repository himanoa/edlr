use edlr_core::event::Event;
use edlr_core::plugin::host::PluginHost;
use edlr_core::plugin::registry::PluginState;
use edlr_core::plugin::runner::start_plugins;
use edlr_core::plugin::settings::SettingsStore;
use edlr_core::router::Router;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

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

/// Materializes a plugin directory under `plugins_dir` with a copy of
/// `wasm_src` as its entry and a manifest declaring `events`.
fn write_plugin(plugins_dir: &Path, id: &str, wasm_src: &Path, events: &[&str]) {
    let dir = plugins_dir.join(id);
    fs::create_dir_all(&dir).unwrap();
    let entry_name = wasm_src.file_name().unwrap().to_str().unwrap();
    fs::copy(wasm_src, dir.join(entry_name)).unwrap();

    let events_toml = events
        .iter()
        .map(|e| format!("\"{e}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        dir.join("manifest.toml"),
        format!(
            r#"
id = "{id}"
name = "{id}"
version = "0.1.0"
entry = "{entry_name}"
events = [{events_toml}]
"#
        ),
    )
    .unwrap();
}

fn state_of<'a>(
    snapshot: &'a [(edlr_core::plugin::Manifest, PluginState)],
    id: &str,
) -> Option<&'a PluginState> {
    snapshot.iter().find(|(m, _)| m.id == id).map(|(_, s)| s)
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_logger_stays_running_and_busy_loop_gets_disabled_after_publish() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    write_plugin(
        &plugins_dir,
        "hello-logger",
        &hello_logger_wasm(),
        &["FSDJump"],
    );
    write_plugin(&plugins_dir, "busy-loop", &busy_loop_wasm(), &["*"]);

    let settings_store = SettingsStore::new(tmp.path().join("settings"));
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(&plugins_dir, settings_store, &router, host);

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.len(), 2, "both plugins should load");
    assert_eq!(
        state_of(&snapshot, "hello-logger"),
        Some(&PluginState::Running)
    );
    assert_eq!(
        state_of(&snapshot, "busy-loop"),
        Some(&PluginState::Running)
    );

    router.publish(Event::Journal {
        timestamp: "2026-07-25T00:00:00Z".to_string(),
        event: "FSDJump".to_string(),
        raw: serde_json::json!({}),
    });

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot = registry.snapshot();
        if matches!(
            state_of(&snapshot, "busy-loop"),
            Some(PluginState::Disabled { .. })
        ) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "busy-loop was not disabled within the timeout"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let snapshot = registry.snapshot();
    assert_eq!(
        state_of(&snapshot, "hello-logger"),
        Some(&PluginState::Running),
        "hello-logger should remain running while busy-loop is disabled"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn broken_manifest_directory_is_skipped_but_others_still_load() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    write_plugin(
        &plugins_dir,
        "hello-logger",
        &hello_logger_wasm(),
        &["FSDJump"],
    );

    let broken_dir = plugins_dir.join("broken");
    fs::create_dir_all(&broken_dir).unwrap();
    fs::write(
        broken_dir.join("manifest.toml"),
        "this is not = = valid toml [[[",
    )
    .unwrap();

    let settings_store = SettingsStore::new(tmp.path().join("settings"));
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(&plugins_dir, settings_store, &router, host);

    let snapshot = registry.snapshot();
    assert_eq!(
        snapshot.len(),
        1,
        "only the valid plugin should be registered"
    );
    assert_eq!(snapshot[0].0.id, "hello-logger");
    assert_eq!(snapshot[0].1, PluginState::Running);
}

#[tokio::test(flavor = "multi_thread")]
async fn nonexistent_plugins_dir_yields_empty_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("does-not-exist");

    let settings_store = SettingsStore::new(tmp.path().join("settings"));
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(&plugins_dir, settings_store, &router, host);

    assert!(registry.snapshot().is_empty());
}

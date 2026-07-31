use edlr_core::capability::grants::GrantsStore;
use edlr_core::event::Event;
use edlr_core::host::driver::DriverHost;
use edlr_core::host::plugin::PluginHost;
use edlr_core::registry::driver::DriverRegistry;
use edlr_core::registry::plugin::PluginState;
use edlr_core::router::Router;
use edlr_core::runner::driver::start_drivers;
use edlr_core::runner::plugin::start_plugins;
use edlr_core::schedule::store::ScheduleStore;
use edlr_core::settings::filesystem::FilesystemConfigStore;
use edlr_core::settings::sidecar::SidecarConfigStore;
use edlr_core::settings::store::SettingsStore;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// ドライバを 1 件もロードしていない `DriverRegistry`(存在しないディレクトリ
/// を走査させることで空のまま作る)。このファイルのテストはバス機能を主眼に
/// しないので、これで十分。
fn empty_driver_registry(tmp_path: &Path) -> DriverRegistry {
    start_drivers(
        &tmp_path.join("drivers"),
        SettingsStore::new(tmp_path.join("driver-settings")),
        SidecarConfigStore::new(tmp_path.join("driver-settings")),
        FilesystemConfigStore::new(tmp_path.join("driver-settings"), Vec::new()),
        GrantsStore::new_for_drivers(tmp_path.join("driver-grants")),
        edlr_driver_channel::Bus::new(),
        DriverHost::new().expect("driver host should build"),
    )
}

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

fn init_trap_wasm() -> PathBuf {
    build_fixture("examples/plugins/init-trap", "init_trap.wasm")
}

/// Materializes a plugin directory under `plugins_dir` with a copy of
/// `wasm_src` as its entry and a manifest declaring `events`.
fn write_plugin(plugins_dir: &Path, id: &str, wasm_src: &Path, events: &[&str]) {
    write_plugin_with_settings(plugins_dir, id, wasm_src, events, "");
}

/// Like `write_plugin`, but appends `settings_toml` (one or more `[[settings]]`
/// tables, already formatted) to the generated manifest.
fn write_plugin_with_settings(
    plugins_dir: &Path,
    id: &str,
    wasm_src: &Path,
    events: &[&str],
    settings_toml: &str,
) {
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
{settings_toml}
"#
        ),
    )
    .unwrap();
}

const HELLO_LOGGER_ENABLED_SETTING: &str = r#"
[[settings]]
key = "enabled"
label = "Enabled"
type = "boolean"
default = true
"#;

fn state_of<'a>(
    snapshot: &'a [(edlr_core::manifest::Manifest, PluginState)],
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
    let grants_store = GrantsStore::new(tmp.path().join("grants"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(
        &plugins_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        host,
    );

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

    let publish_jump = || {
        router.publish(Event::Journal {
            timestamp: "2026-07-25T00:00:00Z".to_string(),
            event: "FSDJump".to_string(),
            raw: serde_json::json!({}),
            replay: false,
        });
    };

    // 1 回目の期限超過ではまだ無効化されない。期限超過は trap と違って
    // 一時的でありうる(応答しないホストなど)ため、`CALL_DEADLINE_STRIKES`
    // 回**連続**するまでは有効なまま(`runner::CALL_DEADLINE_STRIKES` 参照)。
    publish_jump();
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        state_of(&registry.snapshot(), "busy-loop"),
        Some(&PluginState::Running),
        "a single deadline overrun must not disable a plugin -- it may be transient"
    );

    // 連続して超過し続ければ、いずれ諦める。
    for _ in 0..4 {
        publish_jump();
    }

    let deadline = Instant::now() + Duration::from_secs(30);
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

    // Disabled の理由が「期限超過」だと分かること(UI にそのまま出る)。
    let snapshot = registry.snapshot();
    match state_of(&snapshot, "busy-loop") {
        Some(PluginState::Disabled { reason }) => assert!(
            reason.contains("deadline"),
            "the disable reason must say it was a deadline overrun, not just \
             'call failed'; got: {reason}"
        ),
        other => panic!("expected busy-loop to be disabled, got {other:?}"),
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
    let grants_store = GrantsStore::new(tmp.path().join("grants"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(
        &plugins_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        host,
    );

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
    let grants_store = GrantsStore::new(tmp.path().join("grants"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(
        &plugins_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        host,
    );

    assert!(registry.snapshot().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn init_failure_registers_disabled_and_starts_no_event_task() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    write_plugin(
        &plugins_dir,
        "hello-logger",
        &hello_logger_wasm(),
        &["FSDJump"],
    );
    write_plugin(&plugins_dir, "init-trap", &init_trap_wasm(), &["*"]);

    let settings_store = SettingsStore::new(tmp.path().join("settings"));
    let grants_store = GrantsStore::new(tmp.path().join("grants"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(
        &plugins_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        host,
    );

    // start_plugins only returns once every plugin's load/init outcome is
    // resolved, so init-trap must already be Disabled here (its init() loops
    // forever and gets trapped by the epoch deadline), while hello-logger
    // (unaffected) is Running.
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.len(), 2, "both plugins should be registered");
    assert!(
        matches!(
            state_of(&snapshot, "init-trap"),
            Some(PluginState::Disabled { .. })
        ),
        "init-trap should be Disabled immediately after start_plugins returns, got {:?}",
        state_of(&snapshot, "init-trap")
    );
    assert_eq!(
        state_of(&snapshot, "hello-logger"),
        Some(&PluginState::Running)
    );

    // Publish a matching event. Since init-trap's init() never completed, no
    // event task should have been started for it, so nothing should change:
    // it stays Disabled and hello-logger keeps running normally.
    router.publish(Event::Journal {
        timestamp: "2026-07-25T00:00:00Z".to_string(),
        event: "FSDJump".to_string(),
        raw: serde_json::json!({}),
        replay: false,
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    let snapshot = registry.snapshot();
    assert!(
        matches!(
            state_of(&snapshot, "init-trap"),
            Some(PluginState::Disabled { .. })
        ),
        "init-trap should remain Disabled after publish, got {:?}",
        state_of(&snapshot, "init-trap")
    );
    assert_eq!(
        state_of(&snapshot, "hello-logger"),
        Some(&PluginState::Running),
        "hello-logger should be unaffected by init-trap's failure"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_returns_plugin_info_with_effective_values_matching_manifest_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    write_plugin_with_settings(
        &plugins_dir,
        "hello-logger",
        &hello_logger_wasm(),
        &["FSDJump"],
        HELLO_LOGGER_ENABLED_SETTING,
    );

    let settings_store = SettingsStore::new(tmp.path().join("settings"));
    let grants_store = GrantsStore::new(tmp.path().join("grants"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(
        &plugins_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        host,
    );

    let list = registry.list();
    assert_eq!(list.len(), 1, "hello-logger should be registered");
    let info = &list[0];
    assert_eq!(info.manifest.id, "hello-logger");
    assert_eq!(info.state, PluginState::Running);

    for setting in &info.manifest.settings {
        assert_eq!(
            info.values.get(setting.key()),
            Some(&setting.default_value()),
            "values[{}] should match manifest default",
            setting.key()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn set_values_persists_validates_and_updates_shared_settings_json() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    write_plugin_with_settings(
        &plugins_dir,
        "hello-logger",
        &hello_logger_wasm(),
        &["FSDJump"],
        HELLO_LOGGER_ENABLED_SETTING,
    );

    let settings_store = SettingsStore::new(tmp.path().join("settings"));
    let grants_store = GrantsStore::new(tmp.path().join("grants"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(
        &plugins_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        host,
    );

    let mut new_values = serde_json::Map::new();
    new_values.insert("enabled".to_string(), serde_json::json!(false));

    let returned = registry
        .set_values("hello-logger", &new_values)
        .expect("set_values should succeed for a known key");
    assert_eq!(returned.get("enabled"), Some(&serde_json::json!(false)));

    let values = registry
        .values("hello-logger")
        .expect("values should succeed for a registered plugin");
    assert_eq!(values.get("enabled"), Some(&serde_json::json!(false)));

    let shared = registry
        .entry_settings("hello-logger")
        .expect("hello-logger should have a shared settings handle");
    let shared_json = shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let parsed: serde_json::Value =
        serde_json::from_str(&shared_json).expect("shared settings_json should be valid JSON");
    assert_eq!(
        parsed.get("enabled"),
        Some(&serde_json::json!(false)),
        "the running plugin's shared settings_json should reflect the update"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn set_values_with_unknown_key_returns_err_and_leaves_values_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    write_plugin_with_settings(
        &plugins_dir,
        "hello-logger",
        &hello_logger_wasm(),
        &["FSDJump"],
        HELLO_LOGGER_ENABLED_SETTING,
    );

    let settings_store = SettingsStore::new(tmp.path().join("settings"));
    let grants_store = GrantsStore::new(tmp.path().join("grants"));
    let sidecar_config_store = SidecarConfigStore::new(tmp.path().join("settings"));
    let filesystem_config_store =
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
    let router = Router::new(16);
    let host = PluginHost::new().expect("host should start");

    let registry = start_plugins(
        &plugins_dir,
        settings_store,
        sidecar_config_store,
        filesystem_config_store,
        grants_store,
        ScheduleStore::new(tmp.path().join("settings")),
        &router,
        edlr_driver_channel::Bus::new(),
        empty_driver_registry(tmp.path()),
        host,
    );

    let before = registry
        .values("hello-logger")
        .expect("values should succeed for a registered plugin");

    let mut bad_values = serde_json::Map::new();
    bad_values.insert("nope".to_string(), serde_json::json!(1));

    let err = registry
        .set_values("hello-logger", &bad_values)
        .expect_err("unknown settings key should be rejected");
    assert!(matches!(
        err,
        edlr_core::registry::plugin::RegistryError::Settings(_)
    ));

    let after = registry
        .values("hello-logger")
        .expect("values should succeed for a registered plugin");
    assert_eq!(
        before, after,
        "rejected update should not change existing values"
    );
}

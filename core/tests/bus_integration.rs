//! ドライバとプラグインを実際に wasm としてロードし、
//! publish → on-message → emit → 購読プラグイン着信 の 1 往復を通す。
//!
//! 使う wasm は `examples/drivers/ed-state` と `examples/plugins/state-reader`。
//! ビルド済みの成果物が無ければテストは skip する(CI に wasm ターゲットが
//! 無い環境でも `cargo test` を壊さないため)。
//!
//! `start_plugins` が内部で `tokio::spawn`(イベント購読タスク)を使うため、
//! tokio ランタイムの中で走らせる必要があり、通常の `#[tokio::test]` を使う。
//!
//! **`Registry::shutdown_bus_subscribers` を必ず呼んでからテスト関数を
//! 抜けること**: このファイルのプラグイン(`state-reader`)は `[[bus]]` の
//! `subscribe` を宣言しており、`start_plugins` はその購読を
//! `spawn_bus_subscriber`(`tokio::task::spawn_blocking` で動く)で転送する。
//! かつてはこのタスクを止める手段が無く、`#[tokio::test]` が生成する
//! `Runtime` がテスト関数から戻った直後に drop される際、`Runtime::drop` が
//! そのタスクの完了を無期限に待ってプロセスごとハングする Critical バグが
//! あった(`core/src/plugin/registry.rs` の `Registry::shutdown_bus_subscribers`
//! と `core/src/plugin/runner.rs` の `BUS_SUBSCRIBER_SHUTDOWN_POLL_INTERVAL`
//! のドキュメントコメント参照)。今は `core/src/bin/edlr.rs` の shutdown
//! シーケンスと同じように、`Registry` を使い終えたら明示的に
//! `shutdown_bus_subscribers()` を呼ぶ必要がある。ここでは `ShutdownGuard` の
//! `Drop` で行うことで、アサート失敗による早期リターン(パニック)でも確実に
//! 呼ばれるようにしている。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use edlr_core::event::Event;
use edlr_core::registry::plugin::{PluginState, Registry};

/// スコープを抜けるとき(正常終了・パニックのどちらでも)
/// `Registry::shutdown_bus_subscribers` を呼ぶ。`Registry` は `Clone`
/// (内部は `Arc` 共有)で安価に持ち回れるので、ここに渡す分は複製で構わない。
struct ShutdownGuard(Registry);

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        self.0.shutdown_bus_subscribers();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_publish_round_trips_through_the_driver_to_a_subscriber() {
    let Some(driver_wasm) = built_example("examples/drivers/ed-state", "ed_state.wasm") else {
        eprintln!("skipping: build the example driver first");
        return;
    };
    let Some(plugin_wasm) = built_example("examples/plugins/state-reader", "state_reader.wasm")
    else {
        eprintln!("skipping: build the example plugin first");
        return;
    };

    // 1. drivers-dir / plugins-dir をテンポラリに組み立てる。
    let tmp = tempfile::tempdir().unwrap();
    let drivers_dir = tmp.path().join("drivers");
    write_driver_dir(&drivers_dir, "ed-state", &driver_wasm, "driver.wasm");
    let plugins_dir = tmp.path().join("plugins");
    write_plugin_dir(&plugins_dir, "state-reader", &plugin_wasm, "plugin.wasm");

    let bus = edlr_driver_channel::Bus::new();
    let drivers = start_drivers_for_test(&drivers_dir, tmp.path(), bus.clone());
    let driver_infos = drivers.list();
    assert_eq!(driver_infos.len(), 1, "ed-state driver should be loaded");
    assert!(
        matches!(
            driver_infos[0].state,
            edlr_core::registry::driver::DriverState::Running
        ),
        "ed-state driver should init successfully, got {:?}",
        driver_infos[0].state
    );

    let router = edlr_core::router::Router::new(16);
    let registry = start_plugins_for_test(&plugins_dir, tmp.path(), &router, bus.clone(), drivers);
    let _shutdown_guard = ShutdownGuard(registry.clone());
    let plugin_infos = registry.list();
    assert_eq!(
        plugin_infos.len(),
        1,
        "state-reader plugin should be loaded"
    );
    assert_eq!(
        plugin_infos[0].state,
        PluginState::Running,
        "state-reader plugin should init successfully"
    );

    // 2. bus 接続を承認する(Registry::set_bus_grant)。
    registry
        .set_bus_grant("state-reader", "ed-state", true)
        .expect("granting the plugin's bus request should succeed");

    // 3. プラグイン側から publish 相当を Bus 経由で流す: 実際に FSDJump
    //    journal イベントを発行し、state-reader の `on-event` が実際に wasm
    //    の中から `bus::publish("ed-state", "set-system", ..)` を呼ぶように
    //    する(Bus を素通しで直接叩くのではなく、承認・wasm 実行を含めた
    //    経路全体を通す)。
    router.publish(Event::Journal {
        timestamp: "2026-07-27T00:00:00Z".to_string(),
        event: "FSDJump".to_string(),
        raw: serde_json::json!({"event": "FSDJump", "StarSystem": "Sol"}),
        replay: false,
    });

    // 4. ドライバが emit した retained 値が Bus::retained_for で読める。
    //    実際に旅した payload そのものを検証する(「エラーが出なかった」
    //    だけでは、ドライバがトピックを取り違えても検出できない)。
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(value) = bus.retained_for("ed-state", "current-system") {
            assert_eq!(
                value,
                b"Sol".to_vec(),
                "the retained current-system value must be exactly the payload the plugin \
                 published (StarSystem \"Sol\"), not merely present"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "ed-state/current-system was never retained within the timeout; the \
             publish -> on-message -> emit round trip did not complete"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unresolved_bus_reference_does_not_stop_the_plugin() {
    let Some(plugin_wasm) = built_example("examples/plugins/state-reader", "state_reader.wasm")
    else {
        eprintln!("skipping: build the example plugin first");
        return;
    };

    // ドライバを 1 つも置かない(drivers-dir 自体を作らない)状態で、
    // [[bus]] を持つプラグインが Running のままロードされることを確認する。
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    write_plugin_dir(&plugins_dir, "state-reader", &plugin_wasm, "plugin.wasm");

    let router = edlr_core::router::Router::new(16);
    let bus = edlr_driver_channel::Bus::new();
    let drivers = start_drivers_for_test(&tmp.path().join("drivers"), tmp.path(), bus.clone());
    let registry = start_plugins_for_test(&plugins_dir, tmp.path(), &router, bus, drivers);
    let _shutdown_guard = ShutdownGuard(registry.clone());
    let info = registry.list();
    assert_eq!(info.len(), 1);
    assert!(matches!(info[0].state, PluginState::Running));
}

fn built_example(dir: &str, file: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(dir)
        .join("target/wasm32-wasip2/release")
        .join(file);
    path.exists().then_some(path)
}

/// `examples/drivers/<id>` の `driver.toml` をコピーし、`wasm_src` を
/// `entry_name` として配置した `drivers_dir/<id>` を組み立てる。
fn write_driver_dir(drivers_dir: &Path, id: &str, wasm_src: &Path, entry_name: &str) {
    let dir = drivers_dir.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(wasm_src, dir.join(entry_name)).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../examples/drivers")
            .join(id)
            .join("driver.toml"),
        dir.join("driver.toml"),
    )
    .unwrap();
}

/// `examples/plugins/<id>` の `manifest.toml` をコピーし、`wasm_src` を
/// `entry_name` として配置した `plugins_dir/<id>` を組み立てる。
fn write_plugin_dir(plugins_dir: &Path, id: &str, wasm_src: &Path, entry_name: &str) {
    let dir = plugins_dir.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(wasm_src, dir.join(entry_name)).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../examples/plugins")
            .join(id)
            .join("manifest.toml"),
        dir.join("manifest.toml"),
    )
    .unwrap();
}

/// `start_drivers` をテンポラリのストア一式で呼ぶ。
fn start_drivers_for_test(
    drivers_dir: &Path,
    tmp: &Path,
    bus: edlr_driver_channel::Bus,
) -> edlr_core::registry::driver::DriverRegistry {
    use edlr_core::capability::grants::GrantsStore;
    use edlr_core::settings::filesystem::FilesystemConfigStore;
    use edlr_core::settings::sidecar::SidecarConfigStore;
    use edlr_core::settings::store::SettingsStore;
    edlr_core::runner::driver::start_drivers(
        drivers_dir,
        SettingsStore::new(tmp.join("driver-settings")),
        SidecarConfigStore::new(tmp.join("driver-settings")),
        FilesystemConfigStore::new(tmp.join("driver-settings"), Vec::new()),
        GrantsStore::new_for_drivers(tmp.join("driver-grants")),
        bus,
        edlr_core::host::driver::DriverHost::new(test_handle()).expect("driver host should build"),
    )
}

/// `start_plugins` をテンポラリのストア一式で呼ぶ。
fn start_plugins_for_test(
    plugins_dir: &Path,
    tmp: &Path,
    router: &edlr_core::router::Router,
    bus: edlr_driver_channel::Bus,
    drivers: edlr_core::registry::driver::DriverRegistry,
) -> edlr_core::registry::plugin::Registry {
    use edlr_core::capability::grants::GrantsStore;
    use edlr_core::host::plugin::PluginHost;
    use edlr_core::runner::plugin::start_plugins;
    use edlr_core::schedule::store::ScheduleStore;
    use edlr_core::settings::filesystem::FilesystemConfigStore;
    use edlr_core::settings::sidecar::SidecarConfigStore;
    use edlr_core::settings::store::SettingsStore;
    start_plugins(
        plugins_dir,
        SettingsStore::new(tmp.join("settings")),
        SidecarConfigStore::new(tmp.join("settings")),
        FilesystemConfigStore::new(tmp.join("settings"), vec![tmp.to_path_buf()]),
        GrantsStore::new(tmp.join("grants")),
        ScheduleStore::new(tmp.join("settings")),
        router,
        bus,
        drivers,
        PluginHost::new(test_handle()).expect("wasmtime engine builds"),
    )
}

/// テスト全体で共有する runtime の Handle(`HttpDriver` の同期 `send` の
/// `block_on` 先)。関数ローカルの Runtime だと drop 後の `block_on` で
/// panic するため static に生かす。
fn test_handle() -> tokio::runtime::Handle {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().expect("build test runtime"))
        .handle()
        .clone()
}

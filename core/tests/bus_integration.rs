//! ドライバとプラグインを実際に wasm としてロードし、
//! publish → on-message → emit → 購読プラグイン着信 の 1 往復を通す。
//!
//! 使う wasm は `examples/drivers/ed-state` と `examples/plugins/state-reader`
//! (Task 13 で作る)。ビルド済みの成果物が無ければテストは skip する
//! (CI に wasm ターゲットが無い環境でも `cargo test` を壊さないため)。

#[test]
fn a_publish_round_trips_through_the_driver_to_a_subscriber() {
    let Some(_driver_wasm) = built_example("examples/drivers/ed-state", "ed_state.wasm") else {
        eprintln!("skipping: build the example driver first");
        return;
    };
    let Some(_plugin_wasm) =
        built_example("examples/plugins/state-reader", "state_reader.wasm")
    else {
        eprintln!("skipping: build the example plugin first");
        return;
    };

    // 1. drivers-dir / plugins-dir をテンポラリに組み立てる
    // 2. bus 接続を承認する(Registry::set_bus_grant)
    // 3. プラグイン側から publish 相当を Bus 経由で流す
    // 4. ドライバが emit した retained 値が Bus::retained_for で読める
    todo!("上記の手順を実装する");
}

fn built_example(dir: &str, file: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(dir)
        .join("target/wasm32-wasip2/release")
        .join(file);
    path.exists().then_some(path)
}

#[test]
fn an_unresolved_bus_reference_does_not_stop_the_plugin() {
    let Some(plugin_wasm) = built_example("examples/plugins/state-reader", "state_reader.wasm")
    else {
        eprintln!("skipping: build the example plugin first");
        return;
    };

    // ドライバを 1 つも置かない(drivers-dir 自体を作らない)状態で、
    // [[bus]] を持つプラグインが Running のままロードされることを確認する。
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("plugins/state-reader");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(&plugin_wasm, plugin_dir.join("plugin.wasm")).unwrap();
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../examples/plugins/state-reader/manifest.toml"),
        plugin_dir.join("manifest.toml"),
    )
    .unwrap();

    let registry = start_plugins_for_test(&tmp.path().join("plugins"), tmp.path());
    let info = registry.list();
    assert_eq!(info.len(), 1);
    assert!(matches!(
        info[0].state,
        edlr_core::plugin::PluginState::Running
    ));
}

/// `start_plugins` をテンポラリのストア一式で呼ぶ。`bus` は空の
/// `Bus::new()`(ドライバ未登録)を渡す。ドライバ自体も
/// `drivers_dir` を作らないまま `start_drivers` に通し、ドライバ 0 件の
/// `DriverRegistry` を得る(`start_plugins` は `DriverRegistry` を要求する
/// ため、素の `Bus` を直接渡すことはできない)。
fn start_plugins_for_test(
    plugins_dir: &std::path::Path,
    tmp: &std::path::Path,
) -> edlr_core::plugin::Registry {
    use edlr_core::plugin::*;
    let router = edlr_core::router::Router::new(256);
    let bus = edlr_driver_channel::Bus::new();
    let drivers = edlr_core::driver::start_drivers(
        &tmp.join("drivers"),
        SettingsStore::new(tmp.join("driver-settings")),
        SidecarConfigStore::new(tmp.join("driver-settings")),
        FilesystemConfigStore::new(tmp.join("driver-settings"), Vec::new()),
        GrantsStore::new_for_drivers(tmp.join("driver-grants")),
        bus.clone(),
        edlr_core::driver::host::DriverHost::new().expect("driver host should build"),
    );
    start_plugins(
        plugins_dir,
        SettingsStore::new(tmp.join("settings")),
        SidecarConfigStore::new(tmp.join("settings")),
        FilesystemConfigStore::new(tmp.join("settings"), vec![tmp.to_path_buf()]),
        GrantsStore::new(tmp.join("grants")),
        &router,
        bus,
        drivers,
        PluginHost::new().expect("wasmtime engine builds"),
    )
}

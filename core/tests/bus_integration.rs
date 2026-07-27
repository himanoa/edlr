//! ドライバとプラグインを実際に wasm としてロードし、
//! publish → on-message → emit → 購読プラグイン着信 の 1 往復を通す。
//!
//! 使う wasm は `examples/drivers/ed-state` と `examples/plugins/state-reader`。
//! ビルド済みの成果物が無ければテストは skip する(CI に wasm ターゲットが
//! 無い環境でも `cargo test` を壊さないため)。
//!
//! `start_plugins` が内部で `tokio::spawn`(イベント購読タスク)を使うため、
//! tokio ランタイムの中で走らせる必要がある。**ただし `#[tokio::test]` は
//! 使わない**: このファイルのプラグイン(`state-reader`)は `[[bus]]` の
//! `subscribe` を宣言しており、`start_plugins` はその購読を
//! `tokio::task::spawn_blocking(move || for delivery in delivery_rx { .. })`
//! (`crate::plugin::runner::spawn_bus_subscriber`)で転送する。この
//! `delivery_rx` の送信側(`Sender`)は `Bus` の購読表(`state.subscriptions`)
//! に永久に保持されるため、テスト側で `Bus` を drop しない限りチャンネルは
//! 閉じず、`spawn_blocking` のループは終了しない。`#[tokio::test]` が生成
//! する `Runtime` はテスト関数から戻った直後に drop され、その drop は
//! (ドキュメント化されてはいないが実測で確認した挙動として)実行中の
//! blocking タスクの完了を待ち続けるため、**テスト自体は成功しているのに
//! プロセスが `Runtime::drop` の中で無期限にハングする**(cargo test の
//! `has been running for over 60 seconds` 警告だけが出続ける)。
//!
//! 対策として、ここでは手動で `Runtime` を組み立て、テスト本体を
//! `block_on` した後に **drop せず `mem::forget` で明示的にリークする**。
//! テストプロセスはこの関数を最後にすぐ終了するので、リークした
//! ワーカースレッド/blocking スレッドは(未回収のまま)プロセス終了時に
//! まとめて破棄される。パニック(アサート失敗)は `catch_unwind` で捕まえて
//! from `mem::forget` の後に再送出し、Runtime のリークはテスト結果の
//! pass/fail に関わらず必ず行う。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use edlr_core::event::Event;
use edlr_core::plugin::PluginState;

#[test]
fn a_publish_round_trips_through_the_driver_to_a_subscriber() {
    run_leaking_runtime(a_publish_round_trips_through_the_driver_to_a_subscriber_body);
}

async fn a_publish_round_trips_through_the_driver_to_a_subscriber_body() {
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
            edlr_core::driver::DriverState::Running
        ),
        "ed-state driver should init successfully, got {:?}",
        driver_infos[0].state
    );

    let router = edlr_core::router::Router::new(16);
    let registry = start_plugins_for_test(&plugins_dir, tmp.path(), &router, bus.clone(), drivers);
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

#[test]
fn an_unresolved_bus_reference_does_not_stop_the_plugin() {
    run_leaking_runtime(an_unresolved_bus_reference_does_not_stop_the_plugin_body);
}

async fn an_unresolved_bus_reference_does_not_stop_the_plugin_body() {
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
    let info = registry.list();
    assert_eq!(info.len(), 1);
    assert!(matches!(info[0].state, PluginState::Running));
}

/// `body` を専用の multi-thread tokio `Runtime` の上で走らせ、完了したら
/// その `Runtime` を(drop せず)リークする。理由はファイル先頭のドキュメント
/// コメント参照(`spawn_bus_subscriber` の `spawn_blocking` ループが
/// `Runtime::drop` を無期限にブロックするため)。`body` 内のパニック
/// (アサート失敗)は正しくこの関数の呼び出し元(テスト関数)まで伝播する。
fn run_leaking_runtime<F, Fut>(body: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime should build");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rt.block_on(body())));

    // `body` が正常終了・パニックのどちらであっても、この時点で `rt` を
    // drop すると `spawn_blocking` のバス購読タスクが待ち続けて永久に
    // ハングする。drop を回避してリークすることで、テストプロセス終了時に
    // 未回収のままスレッドごと破棄させる。
    std::mem::forget(rt);

    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
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
) -> edlr_core::driver::DriverRegistry {
    use edlr_core::plugin::*;
    edlr_core::driver::start_drivers(
        drivers_dir,
        SettingsStore::new(tmp.join("driver-settings")),
        SidecarConfigStore::new(tmp.join("driver-settings")),
        FilesystemConfigStore::new(tmp.join("driver-settings"), Vec::new()),
        GrantsStore::new_for_drivers(tmp.join("driver-grants")),
        bus,
        edlr_core::driver::host::DriverHost::new().expect("driver host should build"),
    )
}

/// `start_plugins` をテンポラリのストア一式で呼ぶ。
fn start_plugins_for_test(
    plugins_dir: &Path,
    tmp: &Path,
    router: &edlr_core::router::Router,
    bus: edlr_driver_channel::Bus,
    drivers: edlr_core::driver::DriverRegistry,
) -> edlr_core::plugin::Registry {
    use edlr_core::plugin::*;
    start_plugins(
        plugins_dir,
        SettingsStore::new(tmp.join("settings")),
        SidecarConfigStore::new(tmp.join("settings")),
        FilesystemConfigStore::new(tmp.join("settings"), vec![tmp.to_path_buf()]),
        GrantsStore::new(tmp.join("grants")),
        router,
        bus,
        drivers,
        PluginHost::new().expect("wasmtime engine builds"),
    )
}

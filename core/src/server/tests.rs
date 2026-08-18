use super::*;

/// bus フレームは kind="bus" で driver/topic/payload を運ぶ。payload は
/// UTF-8 文字列(非 UTF-8 は lossy)。
#[test]
fn bus_ws_frame_carries_driver_topic_and_lossy_payload() {
    let frame = bus_ws_frame("eddn", "upload-status", b"{\"ok\":true}");
    let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(v["type"], "event");
    assert_eq!(v["kind"], "bus");
    assert_eq!(v["driver"], "eddn");
    assert_eq!(v["topic"], "upload-status");
    assert_eq!(v["payload"], "{\"ok\":true}");

    let frame = bus_ws_frame("eddn", "upload-status", &[0xff, 0xfe]);
    let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(v["payload"], "\u{fffd}\u{fffd}");
}

/// drivers/bus-retained は retain 済みの最新値を返し、未保持なら null。
#[test]
fn drivers_bus_retained_returns_the_value_or_null() {
    let bus = edlr_driver_channel::Bus::new();
    let (tx, _rx) = std::sync::mpsc::sync_channel::<edlr_driver_channel::Message>(4);
    bus.register_driver(
        "eddn",
        vec![edlr_driver_channel::TopicSpec {
            name: "upload-status".into(),
            retain: true,
            description: String::new(),
        }],
        tx,
    );
    bus.emit("eddn", "upload-status", b"{\"ok\":true}".to_vec())
        .unwrap();
    let drivers = crate::registry::driver::tests::test_registry(bus);

    let result = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/bus-retained",
        &serde_json::json!({"driver": "eddn", "topic": "upload-status"}),
    )
    .unwrap();
    assert_eq!(result["payload"], "{\"ok\":true}");

    let result = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/bus-retained",
        &serde_json::json!({"driver": "eddn", "topic": "nope"}),
    )
    .unwrap();
    assert_eq!(result["payload"], serde_json::Value::Null);
}

#[test]
fn ws_json_carries_replay_for_journal_events_and_never_for_status() {
    let journal = Event::Journal {
        timestamp: "2026-07-27T12:00:00Z".into(),
        event: "FSDJump".into(),
        raw: serde_json::json!({"event": "FSDJump"}),
        replay: true,
    };
    let parsed: serde_json::Value = serde_json::from_str(&event_to_ws_json(&journal)).unwrap();
    assert_eq!(parsed["replay"], serde_json::json!(true));

    let status = Event::Status {
        raw: serde_json::json!({"Flags": 1}),
    };
    let parsed: serde_json::Value = serde_json::from_str(&event_to_ws_json(&status)).unwrap();
    assert_eq!(
        parsed.get("replay"),
        None,
        "status is a snapshot of the present; it has no replay notion"
    );
}

#[test]
fn drivers_list_returns_the_dir_and_the_topics() {
    let (registry, drivers) = test_registries();
    let result = handle_rpc_with_drivers(
        Some(&registry),
        Some(&drivers),
        "drivers/list",
        &serde_json::json!({}),
    )
    .unwrap();
    assert!(result["driversDir"].is_string());
    assert_eq!(result["drivers"][0]["id"], "ed-state");
    assert_eq!(result["drivers"][0]["topics"][0]["name"], "current-system");
    assert_eq!(result["drivers"][0]["topics"][0]["retain"], true);
}

/// Regression test for a Minor review finding: `drivers/list` used to
/// collapse `DriverState::Disabled { reason }` down to the bare string
/// `"disabled"`, dropping `reason` entirely -- unlike `plugins/list`,
/// which has always carried it. `ui/frontend/src/types/plugin.ts`
/// declares `reason?: string` and `Drivers.tsx` renders
/// `無効: {driver.reason}`, so a driver that failed to load showed a bare
/// "無効" with no diagnostic. `drivers/list` must now carry `reason` too,
/// mirroring `plugins/list`.
#[test]
fn drivers_list_carries_the_disabled_reason() {
    let bus = edlr_driver_channel::Bus::new();
    let drivers = crate::registry::driver::tests::test_registry_without_ed_state(bus);
    drivers.push(crate::registry::driver::DriverEntry {
        manifest: crate::manifest::driver::DriverManifest {
            id: "broken-driver".into(),
            name: "Broken Driver".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "driver.wasm".into(),
            topics: Vec::new(),
            settings: Vec::new(),
            capabilities: Vec::new(),
            sidecars: Vec::new(),
            filesystem: Vec::new(),
        },
        state: crate::registry::driver::DriverState::Disabled {
            reason: "init() failed: boom".to_string(),
        },
        settings_json: std::sync::Arc::new(std::sync::Mutex::new("{}".to_string())),
        capabilities_json: std::sync::Arc::new(std::sync::Mutex::new(
            r#"{"hosts":[]}"#.to_string(),
        )),
        sidecars_json: std::sync::Arc::new(std::sync::Mutex::new("[]".to_string())),
        filesystem_json: std::sync::Arc::new(std::sync::Mutex::new("[]".to_string())),
        layout: None,
    });

    let result = handle_drivers_rpc(&drivers, "list", &serde_json::json!({})).unwrap();
    let entry = &result["drivers"][0];
    assert_eq!(entry["state"], "disabled");
    assert_eq!(
        entry["reason"], "init() failed: boom",
        "drivers/list must carry the disabled reason like plugins/list does"
    );
}

#[test]
fn drivers_rpc_without_a_driver_registry_reports_unavailable() {
    let (registry, _drivers) = test_registries();
    let err = handle_rpc_with_drivers(
        Some(&registry),
        None,
        "drivers/list",
        &serde_json::json!({}),
    )
    .unwrap_err();
    assert_eq!(err, "drivers unavailable");
}

#[test]
fn plugins_rpc_without_a_registry_reports_unavailable_even_with_drivers_present() {
    // 逆方向のガード: `plugins/*` は `registry` の有無だけを見る必要が
    // あり、`drivers` が `Some` であることに引きずられて誤って通しては
    // ならない。
    let (_registry, drivers) = test_registries();
    let err = handle_rpc_with_drivers(None, Some(&drivers), "plugins/list", &serde_json::json!({}))
        .unwrap_err();
    assert_eq!(err, "plugins unavailable");
}

#[test]
fn plugins_list_includes_bus_requests_with_their_resolution() {
    let (registry, drivers) = test_registries();
    let result = handle_rpc_with_drivers(
        Some(&registry),
        Some(&drivers),
        "plugins/list",
        &serde_json::json!({}),
    )
    .unwrap();
    let bus = &result["plugins"][0]["bus"][0];
    assert_eq!(bus["driver"], "ed-state");
    assert_eq!(bus["publish"][0], "ship-status");
    assert_eq!(bus["subscribe"][0], "current-system");
    assert_eq!(bus["granted"], false);
    assert_eq!(bus["resolved"], true);
}

#[test]
fn plugins_list_includes_schedules_with_spec_strings_and_next() {
    let registry = crate::registry::plugin::tests::test_registry_with_schedule();
    let result = handle_rpc_with_drivers(
        Some(&registry),
        None,
        "plugins/list",
        &serde_json::json!({}),
    )
    .unwrap();
    let schedules = &result["plugins"][0]["schedules"];
    assert_eq!(schedules[0]["name"], "flush");
    assert_eq!(schedules[0]["spec"], "every 60s");
    assert_eq!(schedules[1]["name"], "daily");
    assert_eq!(schedules[1]["spec"], "cron: 0 9 * * *");
    // `next` は ISO8601 文字列としてパースできること(具体的な時刻値は
    // 呼び出しごとの `Local::now()` に依存するのでここでは検証しない --
    // `plugin::schedule::tests` / `plugin::registry::tests` が発火計算
    // 自体をテスト済み)。
    assert!(chrono::DateTime::parse_from_rfc3339(schedules[0]["next"].as_str().unwrap()).is_ok());
}

/// 取りこぼしは黙って失われるのではなく `plugins/list` から見えること。
/// 何も捨てていないプラグインでもフィールドは常に存在する(UI 側で
/// 「まだ数えていない」と「0 件」を区別する必要が無いように)。
#[test]
fn plugins_list_reports_dropped_counts() {
    let (registry, _drivers) = test_registries();
    let result = handle_rpc_with_drivers(
        Some(&registry),
        None,
        "plugins/list",
        &serde_json::json!({}),
    )
    .unwrap();
    let dropped = &result["plugins"][0]["dropped"];
    assert_eq!(dropped["events"], 0);
    assert_eq!(dropped["busDeliveries"], 0);
}

#[test]
fn plugins_list_reports_empty_schedules_array_when_none_declared() {
    let (registry, _drivers) = test_registries();
    let result = handle_rpc_with_drivers(
        Some(&registry),
        None,
        "plugins/list",
        &serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(result["plugins"][0]["schedules"], serde_json::json!([]));
}

/// `plugins/list` の各エントリは `layout` を必ず持つ(無ければ `null`)。
/// フィールド省略ではなく `null` を明示するのは、UI が「無い」を
/// `undefined` と区別しないで済むようにするため(Task 7)。
#[test]
fn plugins_list_includes_layout_or_null() {
    let (registry, drivers) = test_registries();
    let result = handle_rpc_with_drivers(
        Some(&registry),
        Some(&drivers),
        "plugins/list",
        &serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(result["plugins"][0]["layout"], serde_json::Value::Null);

    let layout = crate::layout::Layout {
        sections: vec![crate::layout::Section {
            title: "基本".into(),
            description: None,
            children: vec![crate::layout::Node::Field {
                field: "voice".into(),
            }],
        }],
    };
    registry.push(crate::registry::plugin::PluginEntry {
        manifest: crate::manifest::Manifest {
            id: "layout-plugin".into(),
            name: "Layout Plugin".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: Vec::new(),
            settings: Vec::new(),
            capabilities: Vec::new(),
            sidecars: Vec::new(),
            filesystem: Vec::new(),
            bus: Vec::new(),
            dashboard: Vec::new(),
            schedules: Vec::new(),
            delivery: Default::default(),
        },
        state: crate::registry::plugin::PluginState::Running,
        settings_json: std::sync::Arc::new(std::sync::Mutex::new("{}".to_string())),
        capabilities_json: std::sync::Arc::new(std::sync::Mutex::new(
            crate::host::plugin::capabilities_json_string(&[]),
        )),
        sidecars_json: std::sync::Arc::new(std::sync::Mutex::new("[]".to_string())),
        filesystem_json: std::sync::Arc::new(std::sync::Mutex::new("[]".to_string())),
        bus_json: std::sync::Arc::new(std::sync::Mutex::new("[]".to_string())),
        layout: Some(layout),
    });

    let result = handle_rpc_with_drivers(
        Some(&registry),
        Some(&drivers),
        "plugins/list",
        &serde_json::json!({}),
    )
    .unwrap();
    let entry = result["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "layout-plugin")
        .expect("layout-plugin entry must be present");
    assert_eq!(entry["layout"]["sections"][0]["title"], "基本");
}

/// `drivers/list` の各エントリも `plugins/list` と対称に `layout` を
/// 必ず持つ(無ければ `null`)。
#[test]
fn drivers_list_includes_layout_or_null() {
    let (_registry, drivers) = test_registries();
    let result = handle_drivers_rpc(&drivers, "list", &serde_json::json!({})).unwrap();
    assert_eq!(result["drivers"][0]["layout"], serde_json::Value::Null);

    let layout = crate::layout::Layout {
        sections: vec![crate::layout::Section {
            title: "基本".into(),
            description: None,
            children: vec![crate::layout::Node::Field {
                field: "port".into(),
            }],
        }],
    };
    drivers.push(crate::registry::driver::DriverEntry {
        manifest: crate::manifest::driver::DriverManifest {
            id: "layout-driver".into(),
            name: "Layout Driver".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "driver.wasm".into(),
            topics: Vec::new(),
            settings: Vec::new(),
            capabilities: Vec::new(),
            sidecars: Vec::new(),
            filesystem: Vec::new(),
        },
        state: crate::registry::driver::DriverState::Running,
        settings_json: std::sync::Arc::new(std::sync::Mutex::new("{}".to_string())),
        capabilities_json: std::sync::Arc::new(std::sync::Mutex::new(
            r#"{"hosts":[]}"#.to_string(),
        )),
        sidecars_json: std::sync::Arc::new(std::sync::Mutex::new("[]".to_string())),
        filesystem_json: std::sync::Arc::new(std::sync::Mutex::new("[]".to_string())),
        layout: Some(layout),
    });

    let result = handle_drivers_rpc(&drivers, "list", &serde_json::json!({})).unwrap();
    let entry = result["drivers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == "layout-driver")
        .expect("layout-driver entry must be present");
    assert_eq!(entry["layout"]["sections"][0]["title"], "基本");
}

/// 上のテストの裏付け: `registry` 自身が保持する `DriverRegistry`
/// (`test_registries()` が焼き込むものとは別に、ここでは `ed-state` を
/// 一切登録していないものを使う)に一致するドライバが無ければ、
/// `resolved` は `false` に落ちる。`resolved` の計算
/// (`crate::registry::bus`(`BusService::build_bus_infos`)、
/// `bus_result_json` はそれをそのまま JSON にするだけ)自体が効いている
/// ことを、true/false 両方を**別々の `Registry` インスタンス**(=
/// 別々の `DriverRegistry` を焼き込んだもの)で示すことで確認する
/// (常に `true` を返す実装でも通ってしまう単一ケースを避ける)。
#[test]
fn plugins_list_reports_unresolved_when_the_driver_is_not_installed() {
    let empty_drivers = crate::registry::driver::tests::test_registry_without_ed_state(
        edlr_driver_channel::Bus::new(),
    );
    let registry =
        crate::registry::plugin::tests::test_registry_with_bus_request_using(empty_drivers);
    let result = handle_rpc_with_drivers(
        Some(&registry),
        None,
        "plugins/list",
        &serde_json::json!({}),
    )
    .unwrap();
    let bus = &result["plugins"][0]["bus"][0];
    assert_eq!(bus["driver"], "ed-state");
    assert_eq!(bus["resolved"], false);
}

/// `translator`(`[[bus]]` を 1 件持つ)と `ed-state`(`current-system` を
/// retain 付きで宣言)をそれぞれ 1 件だけ載せたレジストリの組。**プラグイン
/// 側の `Registry` は返す `DriverRegistry` の `clone()` をそのままコンス
/// トラクタに焼き込む**(`edlr.rs` の本番配線 -- 同じ `DriverRegistry` を
/// `start_plugins` と `ServerState::new` の両方に配る -- を模したもの)。
/// これにより `registry.bus(id)` の `resolved`(`Registry` 自身の
/// `DriverRegistry` から計算)がこのテストファイルにも見える、単一の
/// 情報源になる。プラグイン側は Task 10 の
/// `test_registry_with_bus_request_using`、ドライバ側は Task 9 の
/// `test_registry` を再利用する(どちらも wasm をロードせず `push` で
/// 組み立てる)。
fn test_registries() -> (Registry, DriverRegistry) {
    let drivers = crate::registry::driver::tests::test_registry(edlr_driver_channel::Bus::new());
    let registry =
        crate::registry::plugin::tests::test_registry_with_bus_request_using(drivers.clone());
    (registry, drivers)
}

#[test]
fn set_bus_grant_requires_a_plugin_and_a_driver() {
    let (registry, drivers) = test_registries();
    assert!(handle_rpc_with_drivers(
        Some(&registry),
        Some(&drivers),
        "plugins/set-bus-grant",
        &serde_json::json!({"plugin": "translator"})
    )
    .is_err());
}

/// `plugins/set-bus-grant` は(`plugins/set-sidecar-grant`/
/// `plugins/set-filesystem-grant` と同じ流儀で)1 件だけの grant state
/// ではなく、その plugin の `bus[]` 一覧全体を返す。これにより UI は
/// 1 往復でリスト全体を更新できる -- 呼び出し側でこの応答だけを見て
/// 承認結果を判断できることを、`plugins/list` を経由せず確認する。
#[test]
fn set_bus_grant_returns_the_full_bus_array_for_that_plugin() {
    let (registry, drivers) = test_registries();
    let result = handle_rpc_with_drivers(
        Some(&registry),
        Some(&drivers),
        "plugins/set-bus-grant",
        &serde_json::json!({"plugin": "translator", "driver": "ed-state", "granted": true}),
    )
    .unwrap();
    assert_eq!(result["bus"][0]["driver"], "ed-state");
    assert_eq!(result["bus"][0]["granted"], true);
    assert_eq!(result["bus"][0]["resolved"], true);

    // 二重チェック: `plugins/list` を経由しても同じ承認状態が見える
    // (`Registry::set_bus_grant` が実際に永続化していることの裏付け)。
    let listed = handle_rpc_with_drivers(
        Some(&registry),
        Some(&drivers),
        "plugins/list",
        &serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(listed["plugins"][0]["bus"][0]["granted"], true);
}

/// attach_log_stream で流し込んだフレームが、journal/status イベントと
/// 同じ経路(ReplayBuffer + broadcast)で新規クライアントに届くことを
/// 確認する。
#[tokio::test]
async fn attached_log_frames_reach_the_replay_buffer_and_broadcast() {
    let router = crate::router::Router::new(8);
    let state = ServerState::new(&router, None, None, None);
    let (tx, rx) = tokio::sync::broadcast::channel::<std::sync::Arc<String>>(8);
    state.attach_log_stream(rx);

    tx.send(std::sync::Arc::new(crate::logs::format_log_frame(
        "info",
        "t",
        "2026-07-28T00:00:00.000Z",
        "hello",
    )))
    .unwrap();

    // feeder タスクが処理するまでポーリングで待つ
    let mut found = false;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let (snapshot, _rx) = state.snapshot_and_subscribe();
        if snapshot
            .iter()
            .any(|f| f.contains("\"kind\":\"log\"") && f.contains("hello"))
        {
            found = true;
            break;
        }
    }
    assert!(found, "log frame must appear in the replay snapshot");
}

#[tokio::test]
async fn plugin_ui_serves_granted_assets_with_cors_and_404s_everything_else() {
    use tower::ServiceExt;
    let (registry, tmp) = crate::registry::plugin::tests::test_registry_with_dashboard();
    let ui_dir = tmp.path().join("plugins").join("widgety").join("ui");
    std::fs::create_dir_all(&ui_dir).unwrap();
    std::fs::write(ui_dir.join("index.html"), "<html>w</html>").unwrap();

    let router = crate::router::Router::new(8);
    let state = ServerState::new(&router, Some(registry.clone()), None, None);
    let app = app(state, None);

    let get = |uri: &str| {
        axum::http::Request::builder()
            .uri(uri)
            .header("host", "127.0.0.1:8137")
            .body(axum::body::Body::empty())
            .unwrap()
    };

    // 未 grant → 404
    let res = app
        .clone()
        .oneshot(get("/plugin-ui/widgety/status/index.html"))
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);

    registry
        .set_dashboard_grant("widgety", "status", true)
        .unwrap();

    // grant 済み → 200 + CORS + Content-Type。ホストページ(tauri://localhost 等)
    // からの cross-origin dynamic import には CORS 許可が必須。
    let res = app
        .clone()
        .oneshot(get("/plugin-ui/widgety/status/index.html"))
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("access-control-allow-origin")
            .expect("cors header present"),
        "*"
    );
    assert!(res
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/html"));

    // トラバーサル(URL エンコード済み ..)→ 404
    let res = app
        .clone()
        .oneshot(get("/plugin-ui/widgety/status/..%2Fmanifest.toml"))
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
    // 不在ファイル → 404
    let res = app
        .clone()
        .oneshot(get("/plugin-ui/widgety/status/nope.js"))
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
    // 未知プラグイン → 404
    let res = app
        .clone()
        .oneshot(get("/plugin-ui/nope/status/index.html"))
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
}

#[test]
fn set_dashboard_grant_rpc_returns_full_dashboard_list() {
    let (registry, tmp) = crate::registry::plugin::tests::test_registry_with_dashboard();
    let ui_dir = tmp.path().join("plugins").join("widgety").join("ui");
    std::fs::create_dir_all(&ui_dir).unwrap();
    std::fs::write(ui_dir.join("index.html"), "<html></html>").unwrap();

    let result = handle_rpc_with_drivers(
        Some(&registry),
        None,
        "plugins/set-dashboard-grant",
        &serde_json::json!({"plugin": "widgety", "widget": "status", "granted": true}),
    )
    .unwrap();
    assert_eq!(result["dashboard"][0]["id"], "status");
    assert_eq!(result["dashboard"][0]["granted"], true);
    assert_eq!(result["dashboard"][0]["resolved"], true);
    assert_eq!(result["dashboard"][0]["size"], "small");

    // 二重チェック: `plugins/list` を経由しても同じ承認状態が見える
    let listed = handle_rpc_with_drivers(
        Some(&registry),
        None,
        "plugins/list",
        &serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(listed["plugins"][0]["dashboard"][0]["granted"], true);

    // dashboard/list は grant 済みウィジェットを URL 付きで返す
    let widgets = handle_rpc_with_drivers(
        Some(&registry),
        None,
        "dashboard/list",
        &serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(widgets["widgets"][0]["plugin"], "widgety");
    assert_eq!(widgets["widgets"][0]["widget"], "status");
    assert_eq!(widgets["widgets"][0]["title"], "Status");
    assert_eq!(
        widgets["widgets"][0]["url"],
        "/plugin-ui/widgety/status/index.html"
    );
    assert_eq!(widgets["widgets"][0]["size"], "small");
    assert_eq!(widgets["widgets"][0]["events"][0], "FSDJump");
    assert_eq!(widgets["widgets"][0]["resolved"], true);
    assert_eq!(widgets["widgets"][0]["state"], "running");
}

#[test]
fn dashboard_list_excludes_ungranted_widgets() {
    let (registry, _tmp) = crate::registry::plugin::tests::test_registry_with_dashboard();
    let widgets = handle_rpc_with_drivers(
        Some(&registry),
        None,
        "dashboard/list",
        &serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(widgets["widgets"].as_array().unwrap().len(), 0);
}

#[test]
fn set_dashboard_grant_requires_plugin_widget_and_granted() {
    let (registry, _tmp) = crate::registry::plugin::tests::test_registry_with_dashboard();
    assert!(handle_rpc_with_drivers(
        Some(&registry),
        None,
        "plugins/set-dashboard-grant",
        &serde_json::json!({"plugin": "widgety"})
    )
    .is_err());
    assert!(handle_rpc_with_drivers(
        Some(&registry),
        None,
        "plugins/set-dashboard-grant",
        &serde_json::json!({"plugin": "widgety", "widget": "nope", "granted": true})
    )
    .is_err());
}

#[test]
fn drivers_get_and_set_settings_round_trip() {
    let (_registry, drivers) = test_registries();
    let updated = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-settings",
        &serde_json::json!({"driver": "ed-state", "values": {}}),
    )
    .unwrap();
    assert!(updated.is_object());

    let fetched = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/get-settings",
        &serde_json::json!({"driver": "ed-state"}),
    )
    .unwrap();
    assert!(fetched.is_object());
}

/// `ed-state` の fixture(`crate::registry::driver::tests::test_registry`)
/// は http capability を 1 件宣言している(この review finding が入る
/// までは宣言しておらず、`GrantsStore::set` の
/// `capabilities_fingerprint` が常に `None` を返すため `granted` は
/// 要求してもいつも `false` になり、承認の可否ではなく応答の形しか
/// 確認できなかった)。ここでは承認が実際に切り替わり、`drivers/list`
/// (`DriverRegistry::list` 経由、`set-capabilities` とは別の読み出し
/// 経路)からも同じ状態が見える == ディスクに永続化されていることを
/// 確認する。
#[test]
fn drivers_set_capabilities_persists_the_grant() {
    let (_registry, drivers) = test_registries();

    let granted = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-capabilities",
        &serde_json::json!({"driver": "ed-state", "granted": true}),
    )
    .unwrap();
    assert_eq!(granted["granted"], true);
    assert_eq!(granted["requests"][0]["kind"], "http");
    assert_eq!(granted["staleGrant"], false);

    let listed =
        handle_rpc_with_drivers(None, Some(&drivers), "drivers/list", &serde_json::json!({}))
            .unwrap();
    assert_eq!(listed["drivers"][0]["capabilities"]["granted"], true);

    let revoked = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-capabilities",
        &serde_json::json!({"driver": "ed-state", "granted": false}),
    )
    .unwrap();
    assert_eq!(revoked["granted"], false);

    let listed_again =
        handle_rpc_with_drivers(None, Some(&drivers), "drivers/list", &serde_json::json!({}))
            .unwrap();
    assert_eq!(listed_again["drivers"][0]["capabilities"]["granted"], false);
}

/// `drivers/set-sidecar-config` requires `driver`, `name`, and `config`
/// (matching `plugins/set-sidecar-config`'s param names exactly, with
/// `driver` in place of `plugin`). Missing any one of them must fail
/// with the exact same wording `param_str`/the inline `config` check
/// produce for the plugin arm.
#[test]
fn drivers_set_sidecar_config_requires_driver_name_and_config() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-config",
        &serde_json::json!({"name": "engine", "config": {"command": "/bin/sh"}}),
    )
    .unwrap_err();
    assert_eq!(err, "params.driver must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-config",
        &serde_json::json!({"driver": "voice", "config": {"command": "/bin/sh"}}),
    )
    .unwrap_err();
    assert_eq!(err, "params.name must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-config",
        &serde_json::json!({"driver": "voice", "name": "engine"}),
    )
    .unwrap_err();
    assert_eq!(err, "params.config must be an object");
}

#[test]
fn drivers_set_sidecar_grant_requires_driver_name_and_granted() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-grant",
        &serde_json::json!({"name": "engine", "granted": true}),
    )
    .unwrap_err();
    assert_eq!(err, "params.driver must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-grant",
        &serde_json::json!({"driver": "voice", "granted": true}),
    )
    .unwrap_err();
    assert_eq!(err, "params.name must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-grant",
        &serde_json::json!({"driver": "voice", "name": "engine"}),
    )
    .unwrap_err();
    assert_eq!(err, "params.granted must be a bool");
}

#[test]
fn drivers_sidecar_control_requires_driver_name_and_action() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/sidecar-control",
        &serde_json::json!({"name": "engine", "action": "stop"}),
    )
    .unwrap_err();
    assert_eq!(err, "params.driver must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/sidecar-control",
        &serde_json::json!({"driver": "voice", "action": "stop"}),
    )
    .unwrap_err();
    assert_eq!(err, "params.name must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/sidecar-control",
        &serde_json::json!({"driver": "voice", "name": "engine"}),
    )
    .unwrap_err();
    assert_eq!(err, "params.action must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/sidecar-control",
        &serde_json::json!({"driver": "voice", "name": "engine", "action": "jump"}),
    )
    .unwrap_err();
    assert_eq!(err, "unknown action: jump");
}

#[test]
fn drivers_set_filesystem_config_requires_driver_name_and_config() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-filesystem-config",
        &serde_json::json!({"name": "cache", "config": {"path": "/tmp"}}),
    )
    .unwrap_err();
    assert_eq!(err, "params.driver must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-filesystem-config",
        &serde_json::json!({"driver": "voice", "config": {"path": "/tmp"}}),
    )
    .unwrap_err();
    assert_eq!(err, "params.name must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-filesystem-config",
        &serde_json::json!({"driver": "voice", "name": "cache"}),
    )
    .unwrap_err();
    assert_eq!(err, "params.config must be an object");
}

#[test]
fn drivers_set_filesystem_grant_requires_driver_name_and_granted() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-filesystem-grant",
        &serde_json::json!({"name": "cache", "granted": true}),
    )
    .unwrap_err();
    assert_eq!(err, "params.driver must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-filesystem-grant",
        &serde_json::json!({"driver": "voice", "granted": true}),
    )
    .unwrap_err();
    assert_eq!(err, "params.name must be a string");

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-filesystem-grant",
        &serde_json::json!({"driver": "voice", "name": "cache"}),
    )
    .unwrap_err();
    assert_eq!(err, "params.granted must be a bool");
}

/// `drivers/set-sidecar-grant` must actually flip the approval and
/// return it in the refreshed sidecar array (not just accept the call).
/// Configures a real executable first (`drivers/set-sidecar-config`, the
/// same round-trip the UI performs), then grants and checks the
/// response array directly -- this is the RPC-level counterpart to
/// `crate::registry::driver::tests::set_sidecar_config_and_grant_update_the_shared_sidecars_buffer`,
/// which checks the underlying shared buffer.
#[test]
fn drivers_set_sidecar_grant_persists_and_returns_the_full_sidecar_array() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();

    handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-config",
        &serde_json::json!({
            "driver": "voice",
            "name": "engine",
            "config": {"command": "/bin/sh", "args": ["-c", "sleep 30"], "port": 51500, "replicas": 1},
        }),
    )
    .unwrap();

    let result = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-grant",
        &serde_json::json!({"driver": "voice", "name": "engine", "granted": true}),
    )
    .unwrap();
    assert_eq!(result["sidecars"][0]["name"], "engine");
    assert_eq!(result["sidecars"][0]["granted"], true);
    assert_eq!(result["sidecars"][0]["config"]["command"], "/bin/sh");
}

/// `drivers/set-filesystem-grant` must refuse to approve a root that has
/// no directory configured, with the exact error the registry produces
/// (`RegistryError::Filesystem`'s message) -- this is the negative-test
/// the task brief singles out: pin the specific wording so the test
/// cannot pass merely because some unrelated validation (e.g. an
/// undeclared root) rejected the call first.
#[test]
fn drivers_set_filesystem_grant_rejects_granting_without_a_configured_path() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-filesystem-grant",
        &serde_json::json!({"driver": "voice", "name": "cache", "granted": true}),
    )
    .unwrap_err();
    assert_eq!(
        err,
        "filesystem root cache has no directory configured; cannot grant"
    );
}

/// `drivers/sidecar-control` `start` must refuse to launch a sidecar
/// that has never been granted, even once a `command` is configured.
#[test]
fn drivers_sidecar_control_rejects_starting_an_ungranted_sidecar() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();

    handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-config",
        &serde_json::json!({
            "driver": "voice",
            "name": "engine",
            "config": {"command": "/bin/sh", "args": ["-c", "sleep 30"], "port": 51500, "replicas": 1},
        }),
    )
    .unwrap();

    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/sidecar-control",
        &serde_json::json!({"driver": "voice", "name": "engine", "action": "start"}),
    )
    .unwrap_err();
    assert_eq!(err, "sidecar engine is not granted");
}

// Regression coverage for a review finding: the five new `drivers/*`
// sidecar/filesystem arms used to surface an unregistered driver id as
// `RegistryError::UnknownPlugin` ("unknown plugin: {id}"), while the
// pre-existing `drivers/set-capabilities` arm (via
// `DriverRegistryError::UnknownDriver`) already says "unknown driver:
// {id}" for the identical failure. Nothing exercised the unknown-driver
// path for the five new arms, so the inconsistency went uncaught. Each
// of the five gets its own test pinning the exact wording, one per arm
// (rather than one combined test) so a future regression on any single
// arm fails with an unambiguous test name.

#[test]
fn drivers_set_sidecar_config_reports_unknown_driver() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();
    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-config",
        &serde_json::json!({
            "driver": "not-a-driver",
            "name": "engine",
            "config": {"command": "/bin/sh", "args": [], "port": 51500, "replicas": 1},
        }),
    )
    .unwrap_err();
    assert_eq!(err, "unknown driver: not-a-driver");
}

#[test]
fn drivers_set_sidecar_grant_reports_unknown_driver() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();
    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-sidecar-grant",
        &serde_json::json!({"driver": "not-a-driver", "name": "engine", "granted": true}),
    )
    .unwrap_err();
    assert_eq!(err, "unknown driver: not-a-driver");
}

#[test]
fn drivers_sidecar_control_reports_unknown_driver() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();
    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/sidecar-control",
        &serde_json::json!({"driver": "not-a-driver", "name": "engine", "action": "start"}),
    )
    .unwrap_err();
    assert_eq!(err, "unknown driver: not-a-driver");
}

#[test]
fn drivers_set_filesystem_config_reports_unknown_driver() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();
    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-filesystem-config",
        &serde_json::json!({
            "driver": "not-a-driver",
            "name": "cache",
            "config": {"path": "/tmp"},
        }),
    )
    .unwrap_err();
    assert_eq!(err, "unknown driver: not-a-driver");
}

#[test]
fn drivers_set_filesystem_grant_reports_unknown_driver() {
    let (drivers, _tmp) =
        crate::registry::driver::tests::test_registry_with_sidecar_and_filesystem();
    let err = handle_rpc_with_drivers(
        None,
        Some(&drivers),
        "drivers/set-filesystem-grant",
        &serde_json::json!({"driver": "not-a-driver", "name": "cache", "granted": true}),
    )
    .unwrap_err();
    assert_eq!(err, "unknown driver: not-a-driver");
}

#[test]
fn profiler_rpc_reads_the_ring_and_errors_without_a_profiler() {
    assert!(handle_profiler_rpc(None, "profiler/summary", &serde_json::json!({})).is_err());

    let profiler = crate::profiler::Profiler::noop();
    profiler
        .ring()
        .lock()
        .unwrap()
        .insert(&crate::profiler::Sample::Call(
            crate::profiler::CallSample {
                ts: 100.0,
                subject: crate::profiler::Subject::Plugin,
                id: "p1".into(),
                call: crate::profiler::CallKind::OnEvent,
                detail: "E".into(),
                duration_us: 10,
                outcome: crate::profiler::Outcome::Ok,
            },
        ));
    let v =
        handle_profiler_rpc(Some(&profiler), "profiler/summary", &serde_json::json!({})).unwrap();
    assert_eq!(v["subjects"].as_array().unwrap().len(), 1);
    assert!(
        handle_profiler_rpc(Some(&profiler), "profiler/unknown", &serde_json::json!({})).is_err()
    );
}

/// `get-capabilities`/`set-capabilities` の result と `plugins/list` の各要素の
/// `capabilities` フィールドに使う共通の JSON 形: `{ requests, granted, staleGrant }`。
pub fn capabilities_result_json(
    requests: &[crate::plugin::CapabilityRequest],
    grant_state: &crate::plugin::GrantState,
) -> serde_json::Value {
    serde_json::json!({
        "requests": requests,
        "granted": grant_state.granted,
        "staleGrant": grant_state.stale,
    })
}

/// `plugins/set-bus-grant` の result と `plugins/list` の各要素の `bus`
/// フィールドに使う共通の JSON 形: `{ "bus": [...] }`(`sidecars_result_json`/
/// `filesystem_result_json` と同じ流儀 -- 1 件だけの grant state ではなく、
/// その plugin の bus 接続一覧全体を返す)。
///
/// `resolved` は渡された `bus: &[BusInfo]` の `BusInfo::resolved`
/// (`Registry::build_bus_infos` が `Registry` 自身の保持する
/// `DriverRegistry` から計算したもの)をそのまま使う -- 以前はここで
/// `ServerState` の `DriverRegistry` から独立に再計算していたが、それは
/// 同じ判定ロジックの二重管理になり、将来どちらか片方だけ直した変更が
/// サイレントに食い違ってしまう(コードレビュー指摘)。
/// `plugins/set-dashboard-grant`・`plugins/list` が共有する応答形。
pub fn dashboard_result_json(
    dashboard: &[crate::plugin::registry::DashboardInfo],
) -> serde_json::Value {
    let items: Vec<serde_json::Value> = dashboard
        .iter()
        .map(|info| {
            serde_json::json!({
                "id": info.request.id,
                "title": info.request.title,
                "entry": info.request.entry,
                "size": info.request.size.as_str(),
                "granted": info.grant.granted,
                "staleGrant": info.grant.stale,
                "resolved": info.resolved,
            })
        })
        .collect();
    serde_json::json!({ "dashboard": items })
}

pub fn bus_result_json(bus: &[crate::plugin::registry::BusInfo]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = bus
        .iter()
        .map(|info| {
            serde_json::json!({
                "driver": info.request.driver,
                "publish": info.request.publish,
                "subscribe": info.request.subscribe,
                "reason": info.request.reason,
                "granted": info.grant.granted,
                "staleGrant": info.grant.stale,
                "resolved": info.resolved,
            })
        })
        .collect();
    serde_json::json!({ "bus": items })
}

/// `plugins/list` の各要素の `schedules` フィールドに使う JSON:
/// `{ "schedules": [...] }`(他の `*_result_json` と同じ流儀)。
///
/// `spec` は `ScheduleSpec::display_string()`(`"every {n}s"` /
/// `"cron: {expr}"`)、`next` は ISO8601(ローカル時刻・オフセット付き)。
/// `next` は `Registry::ScheduleInfo` のドキュメントコメントが説明する
/// とおり、プラグインスレッドが `ScheduleView` へ公開した実際の発火予定時刻
/// (未公開・Disabled のときだけその場の推定値へフォールバックする)。
/// `plugins/list` の各要素の `dropped` フィールドに使う JSON:
/// `{ "events": n, "busDeliveries": n }`。
///
/// 作業キューが満杯だったために捨てた件数(デーモン起動時からの累計)。
/// journal イベントは読み取り位置が配送と独立に進むため replay でも戻らず、
/// バス配信も再送されない -- つまりこの数はそのまま**失われたイベント数**で
/// ある(`plugin::dropped` のモジュールドキュメント参照)。
pub fn dropped_result_json(dropped: &crate::plugin::dropped::DroppedCounts) -> serde_json::Value {
    serde_json::json!({
        "events": dropped.events,
        "busDeliveries": dropped.bus_deliveries,
    })
}

pub fn schedules_result_json(
    schedules: &[crate::plugin::registry::ScheduleInfo],
) -> serde_json::Value {
    let items: Vec<serde_json::Value> = schedules
        .iter()
        .map(|info| {
            serde_json::json!({
                "name": info.name,
                "spec": info.spec.display_string(),
                "next": info.next.to_rfc3339(),
            })
        })
        .collect();
    serde_json::json!({ "schedules": items })
}

/// `get-sidecars` / `set-sidecar-*` / `sidecar-control` の共通 result 形と、
/// `plugins/list` の各要素の `sidecars` フィールドに使う JSON。
pub fn sidecars_result_json(sidecars: &[crate::plugin::SidecarInfo]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = sidecars
        .iter()
        .map(|info| {
            serde_json::json!({
                "name": info.request.name,
                "reason": info.request.reason,
                "args": info.request.args,
                "port": info.request.port,
                "scalable": info.request.scalable,
                "granted": info.grant.granted,
                "staleGrant": info.grant.stale,
                "config": info.config,
                "instances": info.instances.iter().map(|instance| serde_json::json!({
                    "index": instance.index,
                    "port": instance.port,
                    "state": if instance.running { "running" } else { "exited" },
                    "exitCode": instance.exit_code,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({ "sidecars": items })
}

/// `get-filesystem` / `set-filesystem-*` の共通 result 形と、`plugins/list`
/// の各要素の `filesystem` フィールドに使う JSON: `{ "roots": [...] }`。
pub fn filesystem_result_json(roots: &[crate::plugin::FilesystemInfo]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = roots
        .iter()
        .map(|info| {
            serde_json::json!({
                "name": info.request.name,
                "reason": info.request.reason,
                "mode": info.request.mode.as_str(),
                "granted": info.grant.granted,
                "staleGrant": info.grant.stale,
                "config": info.config,
            })
        })
        .collect();
    serde_json::json!({ "roots": items })
}

/// `get-capabilities`/`set-capabilities` の result と `plugins/list` の各要素の
/// `capabilities` フィールドに使う共通の JSON 形: `{ requests, granted, staleGrant }`。
pub fn capabilities_result_json(
    requests: &[crate::capability::request::CapabilityRequest],
    grant_state: &crate::capability::grants::GrantState,
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
    dashboard: &[crate::registry::plugin::DashboardInfo],
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

pub fn bus_result_json(bus: &[crate::registry::plugin::BusInfo]) -> serde_json::Value {
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
pub fn dropped_result_json(dropped: &crate::runtime::dropped::DroppedCounts) -> serde_json::Value {
    serde_json::json!({
        "events": dropped.events,
        "busDeliveries": dropped.bus_deliveries,
    })
}

pub fn schedules_result_json(
    schedules: &[crate::registry::plugin::ScheduleInfo],
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
pub fn sidecars_result_json(
    sidecars: &[crate::registry::plugin::SidecarInfo],
) -> serde_json::Value {
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
pub fn filesystem_result_json(
    roots: &[crate::registry::plugin::FilesystemInfo],
) -> serde_json::Value {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::grants::GrantState;
    use crate::capability::request::{
        BusRequest, CapabilityRequest, DashboardWidget, FilesystemMode, FilesystemRequest,
        SidecarRequest, WidgetSize,
    };
    use crate::manifest::ScheduleSpec;
    use crate::registry::plugin::{
        BusInfo, DashboardInfo, FilesystemInfo, ScheduleInfo, SidecarInfo,
    };
    use crate::settings::filesystem::FilesystemConfig;
    use crate::settings::sidecar::SidecarConfig;
    use edlr_driver_process::InstanceStatus;

    #[test]
    fn capabilities_json_has_requests_granted_and_stale_grant() {
        let state = GrantState {
            granted: true,
            stale: false,
        };
        let json = capabilities_result_json(&[], &state);
        assert_eq!(
            json,
            serde_json::json!({ "requests": [], "granted": true, "staleGrant": false })
        );
    }

    #[test]
    fn capabilities_json_includes_populated_http_request() {
        let requests = [CapabilityRequest::Http {
            hosts: vec!["https://api.example.com".to_string()],
            reason: "call the api".to_string(),
        }];
        let state = GrantState {
            granted: false,
            stale: true,
        };
        let json = capabilities_result_json(&requests, &state);
        assert_eq!(
            json,
            serde_json::json!({
                "requests": [
                    { "kind": "http", "hosts": ["https://api.example.com"], "reason": "call the api" }
                ],
                "granted": false,
                "staleGrant": true,
            })
        );
    }

    #[test]
    fn dashboard_json_is_empty_for_empty_slice() {
        let json = dashboard_result_json(&[]);
        assert_eq!(json, serde_json::json!({ "dashboard": [] }));
    }

    #[test]
    fn dashboard_json_includes_populated_widget() {
        let dashboard = [DashboardInfo {
            request: DashboardWidget {
                id: "widget-1".to_string(),
                title: "Widget One".to_string(),
                entry: "widget.html".to_string(),
                size: WidgetSize::Medium,
            },
            grant: GrantState {
                granted: true,
                stale: false,
            },
            resolved: true,
        }];
        let json = dashboard_result_json(&dashboard);
        assert_eq!(
            json,
            serde_json::json!({
                "dashboard": [
                    {
                        "id": "widget-1",
                        "title": "Widget One",
                        "entry": "widget.html",
                        "size": "medium",
                        "granted": true,
                        "staleGrant": false,
                        "resolved": true,
                    }
                ]
            })
        );
    }

    #[test]
    fn bus_json_is_empty_for_empty_slice() {
        let json = bus_result_json(&[]);
        assert_eq!(json, serde_json::json!({ "bus": [] }));
    }

    #[test]
    fn bus_json_includes_populated_connection() {
        let bus = [BusInfo {
            request: BusRequest {
                driver: "mqtt".to_string(),
                publish: vec!["topic/out".to_string()],
                subscribe: vec!["topic/in".to_string()],
                reason: "telemetry".to_string(),
            },
            grant: GrantState {
                granted: true,
                stale: false,
            },
            resolved: false,
        }];
        let json = bus_result_json(&bus);
        assert_eq!(
            json,
            serde_json::json!({
                "bus": [
                    {
                        "driver": "mqtt",
                        "publish": ["topic/out"],
                        "subscribe": ["topic/in"],
                        "reason": "telemetry",
                        "granted": true,
                        "staleGrant": false,
                        "resolved": false,
                    }
                ]
            })
        );
    }

    #[test]
    fn dropped_json_reports_event_and_bus_delivery_counts() {
        let dropped = crate::runtime::dropped::DroppedCounts {
            events: 3,
            bus_deliveries: 7,
        };
        let json = dropped_result_json(&dropped);
        assert_eq!(json, serde_json::json!({ "events": 3, "busDeliveries": 7 }));
    }

    #[test]
    fn schedules_json_is_empty_for_empty_slice() {
        let json = schedules_result_json(&[]);
        assert_eq!(json, serde_json::json!({ "schedules": [] }));
    }

    #[test]
    fn schedules_json_includes_populated_schedule_with_fixed_timestamp() {
        let next = chrono::DateTime::parse_from_rfc3339("2026-07-30T12:00:00+09:00")
            .unwrap()
            .with_timezone(&chrono::Local);
        let schedules = [ScheduleInfo {
            name: "daily-report".to_string(),
            spec: ScheduleSpec::Cron("0 9 * * *".to_string()),
            next,
        }];
        let json = schedules_result_json(&schedules);
        assert_eq!(
            json,
            serde_json::json!({
                "schedules": [
                    {
                        "name": "daily-report",
                        "spec": "cron: 0 9 * * *",
                        "next": schedules[0].next.to_rfc3339(),
                    }
                ]
            })
        );
    }

    #[test]
    fn schedules_json_includes_populated_interval_schedule_with_fixed_timestamp() {
        let next = chrono::DateTime::parse_from_rfc3339("2026-07-30T12:00:00+09:00")
            .unwrap()
            .with_timezone(&chrono::Local);
        let schedules = [ScheduleInfo {
            name: "poll-status".to_string(),
            spec: ScheduleSpec::IntervalSeconds(30),
            next,
        }];
        let json = schedules_result_json(&schedules);
        assert_eq!(
            json,
            serde_json::json!({
                "schedules": [
                    {
                        "name": "poll-status",
                        "spec": "every 30s",
                        "next": schedules[0].next.to_rfc3339(),
                    }
                ]
            })
        );
    }

    #[test]
    fn sidecars_json_is_empty_for_empty_slice() {
        let json = sidecars_result_json(&[]);
        assert_eq!(json, serde_json::json!({ "sidecars": [] }));
    }

    #[test]
    fn sidecars_json_includes_populated_sidecar_with_instances() {
        let sidecars = [SidecarInfo {
            request: SidecarRequest {
                name: "worker".to_string(),
                reason: "background jobs".to_string(),
                args: vec!["--verbose".to_string()],
                port: 8080,
                scalable: true,
            },
            config: SidecarConfig {
                command: "/usr/bin/worker".to_string(),
                args: vec!["--verbose".to_string()],
                port: 8080,
                replicas: 2,
            },
            grant: GrantState {
                granted: true,
                stale: false,
            },
            instances: vec![
                InstanceStatus {
                    index: 0,
                    port: 8080,
                    running: true,
                    exit_code: None,
                },
                InstanceStatus {
                    index: 1,
                    port: 8081,
                    running: false,
                    exit_code: Some(1),
                },
            ],
        }];
        let json = sidecars_result_json(&sidecars);
        assert_eq!(
            json,
            serde_json::json!({
                "sidecars": [
                    {
                        "name": "worker",
                        "reason": "background jobs",
                        "args": ["--verbose"],
                        "port": 8080,
                        "scalable": true,
                        "granted": true,
                        "staleGrant": false,
                        "config": {
                            "command": "/usr/bin/worker",
                            "args": ["--verbose"],
                            "port": 8080,
                            "replicas": 2,
                        },
                        "instances": [
                            { "index": 0, "port": 8080, "state": "running", "exitCode": null },
                            { "index": 1, "port": 8081, "state": "exited", "exitCode": 1 },
                        ],
                    }
                ]
            })
        );
    }

    #[test]
    fn filesystem_json_is_empty_for_empty_slice() {
        let json = filesystem_result_json(&[]);
        assert_eq!(json, serde_json::json!({ "roots": [] }));
    }

    #[test]
    fn filesystem_json_includes_populated_root() {
        let roots = [FilesystemInfo {
            request: FilesystemRequest {
                name: "data".to_string(),
                reason: "read config files".to_string(),
                mode: FilesystemMode::ReadWrite,
            },
            config: FilesystemConfig {
                path: "/home/user/data".to_string(),
            },
            grant: GrantState {
                granted: false,
                stale: false,
            },
        }];
        let json = filesystem_result_json(&roots);
        assert_eq!(
            json,
            serde_json::json!({
                "roots": [
                    {
                        "name": "data",
                        "reason": "read config files",
                        "mode": "read-write",
                        "granted": false,
                        "staleGrant": false,
                        "config": { "path": "/home/user/data" },
                    }
                ]
            })
        );
    }
}

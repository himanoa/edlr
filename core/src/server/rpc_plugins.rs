//! `plugins/*` / `dashboard/*` RPC のメソッド別ハンドラ(params 解釈 →
//! Registry 呼び出し → JSON 整形)。dispatch は `super::handle_rpc_with_drivers`。

use crate::registry::plugin::Registry;
use crate::rpc::params::{param_bool, param_object, param_str};
use crate::rpc::render::*;

/// `plugins/list` の1要素分の JSON を組み立てる(40行制限のため
/// `list` 本体から分離)。
fn plugin_entry_json(
    registry: &Registry,
    info: crate::registry::plugin::PluginInfo,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "id": info.manifest.id,
        "name": info.manifest.name,
        "version": info.manifest.version,
        "description": info.manifest.description,
        "settings": info.manifest.settings,
        "values": info.values,
        "capabilities": capabilities_result_json(
            &info.capability_requests,
            &info.grant_state,
        ),
        "layout": info.layout,
    });
    value["sidecars"] = sidecars_result_json(&info.sidecars)["sidecars"].clone();
    value["filesystem"] = filesystem_result_json(&info.filesystem)["roots"].clone();
    let bus = registry.bus(&info.manifest.id).unwrap_or_default();
    value["bus"] = bus_result_json(&bus)["bus"].clone();
    value["dashboard"] = dashboard_result_json(&info.dashboard)["dashboard"].clone();
    value["schedules"] = schedules_result_json(&info.schedules)["schedules"].clone();
    value["dropped"] = dropped_result_json(&info.dropped);
    // `secret` 型設定の値は `values` に含まれない(write-only)。
    // 「設定済みかどうか」だけを UI に伝える。
    value["secretsSet"] = serde_json::json!(info.secrets_set);
    match info.state {
        crate::registry::plugin::PluginState::Running => {
            value["state"] = serde_json::json!("running");
        }
        crate::registry::plugin::PluginState::Disabled { reason } => {
            value["state"] = serde_json::json!("disabled");
            value["reason"] = serde_json::json!(reason);
        }
    }
    value
}

pub(super) fn list(
    registry: &Registry,
    _params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugins: Vec<serde_json::Value> = registry
        .list()
        .into_iter()
        .map(|info| plugin_entry_json(registry, info))
        .collect();
    Ok(serde_json::json!({
        "pluginsDir": registry.plugins_dir().to_string_lossy(),
        "plugins": plugins,
    }))
}

pub(super) fn set_bus_grant(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let driver = param_str(params, "driver")?;
    let granted = param_bool(params, "granted")?;
    registry
        .set_bus_grant(plugin, driver, granted)
        .map_err(|e| e.to_string())?;
    // `set_sidecar_grant`/`set_filesystem_grant` と同じ流儀: 1 件だけ
    // の grant state を返すのではなく、その plugin の bus 一覧全体を
    // 返す(UI が 1 往復でリスト全体を更新できるように)。
    let bus = registry.bus(plugin).map_err(|e| e.to_string())?;
    Ok(bus_result_json(&bus))
}

pub(super) fn set_dashboard_grant(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let widget = param_str(params, "widget")?;
    let granted = param_bool(params, "granted")?;
    // `set_bus_grant` と同じ流儀: その plugin のウィジェット一覧全体を
    // 返す(UI が 1 往復でリスト全体を更新できるように)。
    let dashboard = registry
        .set_dashboard_grant(plugin, widget, granted)
        .map_err(|e| e.to_string())?;
    Ok(dashboard_result_json(&dashboard))
}

pub(super) fn dashboard_list(
    registry: &Registry,
    _params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Dashboard 画面用: grant 済みウィジェットだけを、iframe が
    // そのまま使える URL 付きで返す。未 grant を混ぜないのは、
    // アセット配信側も未 grant を 404 にする(見えないものは
    // 取得もできない)のと対になる。
    let widgets: Vec<serde_json::Value> = registry
        .dashboard_widgets_for_ui()
        .into_iter()
        .filter(|(_, _, _, info)| info.grant.granted)
        .map(|(plugin_id, plugin_name, state, info)| {
            let entry_file = std::path::Path::new(&info.request.entry)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("index.html");
            let events = registry.events_of(&plugin_id).unwrap_or_default();
            let state_str = match state {
                crate::registry::plugin::PluginState::Running => "running",
                crate::registry::plugin::PluginState::Disabled { .. } => "disabled",
            };
            serde_json::json!({
                "plugin": plugin_id,
                "pluginName": plugin_name,
                "widget": info.request.id,
                "title": info.request.title,
                "url": format!("/plugin-ui/{plugin_id}/{}/{entry_file}", info.request.id),
                "size": info.request.size.as_str(),
                "events": events,
                "resolved": info.resolved,
                "state": state_str,
            })
        })
        .collect();
    Ok(serde_json::json!({ "widgets": widgets }))
}

pub(super) fn get_settings(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let values = registry.values(plugin).map_err(|e| e.to_string())?;
    Ok(serde_json::Value::Object(values))
}

pub(super) fn set_settings(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let values = param_object(params, "values")?;
    let updated = registry
        .set_values(plugin, values)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::Value::Object(updated))
}

pub(super) fn get_capabilities(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let (requests, grant_state) = registry.capabilities(plugin).map_err(|e| e.to_string())?;
    Ok(capabilities_result_json(&requests, &grant_state))
}

pub(super) fn set_capabilities(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let granted = param_bool(params, "granted")?;
    let grant_state = registry
        .set_capabilities(plugin, granted)
        .map_err(|e| e.to_string())?;
    let (requests, _) = registry.capabilities(plugin).map_err(|e| e.to_string())?;
    Ok(capabilities_result_json(&requests, &grant_state))
}

pub(super) fn get_sidecars(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let sidecars = registry.sidecars(plugin).map_err(|e| e.to_string())?;
    Ok(sidecars_result_json(&sidecars))
}

pub(super) fn set_sidecar_config(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let name = param_str(params, "name")?;
    let config: crate::settings::sidecar::SidecarConfig = serde_json::from_value(
        params
            .get("config")
            .cloned()
            .ok_or_else(|| "params.config must be an object".to_string())?,
    )
    .map_err(|e| format!("params.config is invalid: {e}"))?;
    let sidecars = registry
        .set_sidecar_config(plugin, name, &config)
        .map_err(|e| e.to_string())?;
    Ok(sidecars_result_json(&sidecars))
}

pub(super) fn set_sidecar_grant(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let name = param_str(params, "name")?;
    let granted = param_bool(params, "granted")?;
    let sidecars = registry
        .set_sidecar_grant(plugin, name, granted)
        .map_err(|e| e.to_string())?;
    Ok(sidecars_result_json(&sidecars))
}

pub(super) fn sidecar_control(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let name = param_str(params, "name")?;
    let action = match param_str(params, "action")? {
        "start" => crate::registry::plugin::SidecarAction::Start,
        "stop" => crate::registry::plugin::SidecarAction::Stop,
        "restart" => crate::registry::plugin::SidecarAction::Restart,
        other => return Err(format!("unknown action: {other}")),
    };
    let sidecars = registry
        .control_sidecar(plugin, name, action)
        .map_err(|e| e.to_string())?;
    Ok(sidecars_result_json(&sidecars))
}

pub(super) fn get_filesystem(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let roots = registry.filesystem(plugin).map_err(|e| e.to_string())?;
    Ok(filesystem_result_json(&roots))
}

pub(super) fn set_filesystem_config(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let name = param_str(params, "name")?;
    let config: crate::settings::filesystem::FilesystemConfig = serde_json::from_value(
        params
            .get("config")
            .cloned()
            .ok_or_else(|| "params.config must be an object".to_string())?,
    )
    .map_err(|e| format!("params.config is invalid: {e}"))?;
    let roots = registry
        .set_filesystem_config(plugin, name, &config)
        .map_err(|e| e.to_string())?;
    Ok(filesystem_result_json(&roots))
}

pub(super) fn set_filesystem_grant(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let name = param_str(params, "name")?;
    let granted = param_bool(params, "granted")?;
    let roots = registry
        .set_filesystem_grant(plugin, name, granted)
        .map_err(|e| e.to_string())?;
    Ok(filesystem_result_json(&roots))
}

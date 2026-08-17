//! `drivers/*` RPC のメソッド別ハンドラ(params 解釈 → DriverRegistry 呼び出し →
//! JSON 整形)。dispatch は `super::handle_drivers_rpc`。

use crate::registry::driver::{DriverInfo, DriverRegistry, DriverState};
use crate::rpc::params::{param_bool, param_object, param_str};
use crate::rpc::render::*;

/// `drivers/list` の1要素分の JSON を組み立てる(40行制限のため
/// `list` 本体から分離)。
fn driver_entry_json(info: DriverInfo) -> serde_json::Value {
    let mut value = serde_json::json!({
        "id": info.manifest.id,
        "name": info.manifest.name,
        "version": info.manifest.version,
        "description": info.manifest.description,
        "topics": info.manifest.topics,
        "settings": info.manifest.settings,
        "values": info.values,
        "capabilities": capabilities_result_json(&info.manifest.capabilities, &info.grant_state),
        "sidecars": sidecars_result_json(&info.sidecars)["sidecars"],
        "filesystem": filesystem_result_json(&info.filesystem)["roots"],
        "layout": info.layout,
    });
    // `plugins/list` と同じ流儀: `reason` は `Disabled` のときだけ
    // 載せる(`ui/frontend/src/types/plugin.ts` の `reason?: string`、
    // `Drivers.tsx` の「無効: {driver.reason}」表示が診断情報を
    // 拾えるように -- 最終レビューで見つかった Minor な取りこぼし。
    // 以前はここで `state` を文字列に潰すだけで `reason` を運んで
    // いなかった)。
    match info.state {
        DriverState::Running => {
            value["state"] = serde_json::json!("running");
        }
        DriverState::Disabled { reason } => {
            value["state"] = serde_json::json!("disabled");
            value["reason"] = serde_json::json!(reason);
        }
    }
    value
}

pub(super) fn list(
    drivers: &DriverRegistry,
    _params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "driversDir": drivers.drivers_dir().to_string_lossy(),
        "drivers": drivers.list().into_iter().map(driver_entry_json).collect::<Vec<_>>(),
    }))
}

/// `drivers/bus-retained`: driver/topic の retained 値を返す。未保持は
/// null(ウィジェットの初期表示用 -- 設計書参照)。payload は UTF-8 文字列
/// (lossy、`bus_ws_frame` と同じ妥協)。
pub(super) fn bus_retained(
    drivers: &DriverRegistry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let driver = param_str(params, "driver")?;
    let topic = param_str(params, "topic")?;
    let payload = drivers
        .bus_retained(driver, topic)
        .map(|p| String::from_utf8_lossy(&p).into_owned());
    Ok(serde_json::json!({ "payload": payload }))
}

pub(super) fn get_settings(
    drivers: &DriverRegistry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let driver = param_str(params, "driver")?;
    let values = drivers.values(driver).map_err(|e| e.to_string())?;
    Ok(serde_json::Value::Object(values))
}

pub(super) fn set_settings(
    drivers: &DriverRegistry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let driver = param_str(params, "driver")?;
    let values = param_object(params, "values")?;
    let updated = drivers
        .set_values(driver, values)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::Value::Object(updated))
}

pub(super) fn set_capabilities(
    drivers: &DriverRegistry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let driver = param_str(params, "driver")?;
    let granted = param_bool(params, "granted")?;
    let grant_state = drivers
        .set_capabilities(driver, granted)
        .map_err(|e| e.to_string())?;
    let manifest = drivers
        .manifest_of(driver)
        .ok_or_else(|| format!("unknown driver: {driver}"))?;
    Ok(capabilities_result_json(
        &manifest.capabilities,
        &grant_state,
    ))
}

pub(super) fn set_sidecar_config(
    drivers: &DriverRegistry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let driver = param_str(params, "driver")?;
    let name = param_str(params, "name")?;
    let config: crate::settings::sidecar::SidecarConfig = serde_json::from_value(
        params
            .get("config")
            .cloned()
            .ok_or_else(|| "params.config must be an object".to_string())?,
    )
    .map_err(|e| format!("params.config is invalid: {e}"))?;
    let sidecars = drivers
        .set_sidecar_config(driver, name, &config)
        .map_err(|e| e.to_string())?;
    Ok(sidecars_result_json(&sidecars))
}

pub(super) fn set_sidecar_grant(
    drivers: &DriverRegistry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let driver = param_str(params, "driver")?;
    let name = param_str(params, "name")?;
    let granted = param_bool(params, "granted")?;
    let sidecars = drivers
        .set_sidecar_grant(driver, name, granted)
        .map_err(|e| e.to_string())?;
    Ok(sidecars_result_json(&sidecars))
}

pub(super) fn sidecar_control(
    drivers: &DriverRegistry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let driver = param_str(params, "driver")?;
    let name = param_str(params, "name")?;
    // plugins 側(rpc_plugins::sidecar_control)と同一の action パース。
    // Phase 2 では共通化しない(Phase 4 のジェネリック化の対象)。
    let action = match param_str(params, "action")? {
        "start" => crate::registry::plugin::SidecarAction::Start,
        "stop" => crate::registry::plugin::SidecarAction::Stop,
        "restart" => crate::registry::plugin::SidecarAction::Restart,
        other => return Err(format!("unknown action: {other}")),
    };
    let sidecars = drivers
        .control_sidecar(driver, name, action)
        .map_err(|e| e.to_string())?;
    Ok(sidecars_result_json(&sidecars))
}

pub(super) fn set_filesystem_config(
    drivers: &DriverRegistry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let driver = param_str(params, "driver")?;
    let name = param_str(params, "name")?;
    let config: crate::settings::filesystem::FilesystemConfig = serde_json::from_value(
        params
            .get("config")
            .cloned()
            .ok_or_else(|| "params.config must be an object".to_string())?,
    )
    .map_err(|e| format!("params.config is invalid: {e}"))?;
    let roots = drivers
        .set_filesystem_config(driver, name, &config)
        .map_err(|e| e.to_string())?;
    Ok(filesystem_result_json(&roots))
}

pub(super) fn set_filesystem_grant(
    drivers: &DriverRegistry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let driver = param_str(params, "driver")?;
    let name = param_str(params, "name")?;
    let granted = param_bool(params, "granted")?;
    let roots = drivers
        .set_filesystem_grant(driver, name, granted)
        .map_err(|e| e.to_string())?;
    Ok(filesystem_result_json(&roots))
}

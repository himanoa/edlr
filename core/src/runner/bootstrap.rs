//! plugin/driver 共通の起動直後バッファ組み立て。
//!
//! `runner::plugin::load_and_run_plugin` と `runner::driver::load_and_run_driver`
//! はどちらも起動直後に settings/sidecars/capabilities/filesystem の 4 つの
//! `Arc<Mutex<String>>` を同じ手順(各ストアから effective 値を引き、
//! 承認状態と合わせて JSON にする)で組み立てる。この重複を
//! `registry::subject::RegistrySubject`(Phase 4)でジェネリックにまとめる。
//!
//! `bus` はプラグイン専用(ドライバは journal イベントを購読しない)なので、
//! 起動時初期値はここでは扱わない。呼び出し側(`runner::plugin::load_and_run_plugin`)
//! が別途組み立てる。
//!
//! **`registry::sidecar::SidecarService::refresh_sidecar_runtime` 等とは
//! 意図的に共通化しない**: あちらは承認・設定変更のたびに作り直す更新用、
//! こちらは起動直後の初期値用で、依存するライフサイクルの起点が異なる
//! (`runner::plugin` 288–293 の既存コメント参照)。

use std::sync::{Arc, Mutex};

use crate::capability::grants::GrantsStore;
use crate::host::plugin::capabilities_json_string;
use crate::registry::subject::RegistrySubject;
use crate::runtime::fs::{filesystem_json_string, FsRuntimeEntry};
use crate::runtime::sidecar::{implicit_http_hosts, sidecars_json_string, SidecarRuntimeEntry};
use crate::settings::filesystem::FilesystemConfigStore;
use crate::settings::sidecar::{assign_ports, SidecarConfig, SidecarConfigStore};
use crate::settings::store::SettingsStore;

/// 起動直後の共有 JSON バッファ初期値。plugin/driver 共通部
/// (settings / sidecars / capabilities / filesystem)。bus は plugin 専用
/// なので呼び出し側(runner/plugin.rs)が別途組み立てる。
pub(crate) struct InitialBuffers {
    pub(crate) settings_json: Arc<Mutex<String>>,
    pub(crate) capabilities_json: Arc<Mutex<String>>,
    pub(crate) sidecars_json: Arc<Mutex<String>>,
    pub(crate) filesystem_json: Arc<Mutex<String>>,
}

/// `subject`(plugin manifest / driver manifest)と各ストアから、起動直後の
/// 4 つの共有 JSON バッファを組み立てる。
///
/// 手順: settings の effective 値 → sidecar 1 件ずつの設定・承認の解決 →
/// capability hosts(承認済みなら manifest 由来、サイドカーの暗黙許可を
/// 常に追加)→ filesystem 1 件ずつの設定・承認の解決、の順。すべて
/// `subject.as_settings_manifest()` 経由で既存ストア(`Manifest` を引数に
/// 取る)を呼ぶ。
pub(crate) fn build_initial_buffers<S: RegistrySubject>(
    subject: &S,
    settings_store: &SettingsStore,
    grants_store: &GrantsStore,
    sidecar_config_store: &SidecarConfigStore,
    filesystem_config_store: &FilesystemConfigStore,
) -> InitialBuffers {
    let settings_manifest = subject.as_settings_manifest();

    let effective = settings_store.effective(&settings_manifest);
    let settings_json_string = serde_json::to_string(&serde_json::Value::Object(effective))
        .unwrap_or_else(|_| "{}".to_string());
    let settings_json = Arc::new(Mutex::new(settings_json_string));

    let grant_state = grants_store.state(&settings_manifest);

    let sidecar_configs = sidecar_config_store.effective(&settings_manifest);
    let sidecar_entries: Vec<SidecarRuntimeEntry> = subject
        .sidecars()
        .iter()
        .map(|request| {
            let config = sidecar_configs
                .get(&request.name)
                .cloned()
                .unwrap_or_else(|| SidecarConfig::from_request(request));
            let granted = grants_store
                .sidecar_state(&settings_manifest, &request.name)
                .granted;
            SidecarRuntimeEntry {
                name: request.name.clone(),
                granted,
                command: config.command.clone(),
                args: config.args.clone(),
                ports: assign_ports(&config),
            }
        })
        .collect();
    let sidecars_json = Arc::new(Mutex::new(sidecars_json_string(&sidecar_entries)));

    let granted_hosts = if grant_state.granted {
        settings_manifest.capability_hosts()
    } else {
        Vec::new()
    };
    let initial_hosts: Vec<String> = granted_hosts
        .into_iter()
        .chain(implicit_http_hosts(&sidecar_entries))
        .collect();
    let capabilities_json = Arc::new(Mutex::new(capabilities_json_string(&initial_hosts)));

    let filesystem_configs = filesystem_config_store.effective(&settings_manifest);
    let filesystem_entries: Vec<FsRuntimeEntry> = subject
        .filesystem()
        .iter()
        .map(|request| {
            let path = filesystem_configs
                .get(&request.name)
                .map(|config| config.path.clone())
                .unwrap_or_default();
            let granted = grants_store
                .filesystem_state(&settings_manifest, &request.name)
                .granted;
            FsRuntimeEntry {
                name: request.name.clone(),
                granted,
                mode: request.mode.as_str().to_string(),
                path,
                target: request.target.as_str().to_string(),
            }
        })
        .collect();
    let filesystem_json = Arc::new(Mutex::new(filesystem_json_string(&filesystem_entries)));

    InitialBuffers {
        settings_json,
        capabilities_json,
        sidecars_json,
        filesystem_json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::request::{
        CapabilityRequest, FilesystemMode, FilesystemRequest, SidecarRequest,
    };
    use crate::manifest::Manifest;

    fn plain_manifest(id: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![CapabilityRequest::Http {
                hosts: vec!["*".to_string()],
                reason: "test".into(),
            }],
            sidecars: vec![SidecarRequest {
                name: "worker".to_string(),
                reason: "reason".into(),
                args: vec![],
                port: 4000,
                scalable: false,
            }],
            filesystem: vec![FilesystemRequest {
                name: "workdir".to_string(),
                reason: "reason".into(),
                mode: FilesystemMode::Read,
                target: Default::default(),
            }],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    fn stores(
        dir: &std::path::Path,
    ) -> (
        SettingsStore,
        GrantsStore,
        SidecarConfigStore,
        FilesystemConfigStore,
    ) {
        (
            SettingsStore::new(dir.join("settings")),
            GrantsStore::new(dir.join("grants")),
            SidecarConfigStore::new(dir.join("settings")),
            FilesystemConfigStore::new(dir.join("settings"), vec![dir.to_path_buf()]),
        )
    }

    #[test]
    fn ungranted_plugin_has_empty_capabilities_but_still_lists_sidecars_and_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        let (settings_store, grants_store, sidecar_config_store, filesystem_config_store) =
            stores(tmp.path());
        let manifest = plain_manifest("p-ungranted");

        let buffers = build_initial_buffers(
            &manifest,
            &settings_store,
            &grants_store,
            &sidecar_config_store,
            &filesystem_config_store,
        );

        assert_eq!(*buffers.settings_json.lock().unwrap(), "{}");
        assert_eq!(*buffers.capabilities_json.lock().unwrap(), "{\"hosts\":[]}");
        let sidecars: serde_json::Value =
            serde_json::from_str(&buffers.sidecars_json.lock().unwrap()).unwrap();
        assert_eq!(sidecars[0]["name"], "worker");
        assert_eq!(sidecars[0]["granted"], false);
        let filesystem: serde_json::Value =
            serde_json::from_str(&buffers.filesystem_json.lock().unwrap()).unwrap();
        assert_eq!(filesystem[0]["name"], "workdir");
        assert_eq!(filesystem[0]["granted"], false);
        assert_eq!(filesystem[0]["path"], "");
    }

    #[test]
    fn granted_plugin_lists_capability_hosts_and_grants_reflect_in_sidecars_and_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        let (settings_store, grants_store, sidecar_config_store, filesystem_config_store) =
            stores(tmp.path());
        let manifest = plain_manifest("p-granted");

        grants_store.set(&manifest, true).unwrap();
        grants_store.set_sidecar(&manifest, "worker", true).unwrap();
        grants_store
            .set_filesystem(&manifest, "workdir", true)
            .unwrap();

        let buffers = build_initial_buffers(
            &manifest,
            &settings_store,
            &grants_store,
            &sidecar_config_store,
            &filesystem_config_store,
        );

        let capabilities: serde_json::Value =
            serde_json::from_str(&buffers.capabilities_json.lock().unwrap()).unwrap();
        assert_eq!(
            capabilities["hosts"],
            serde_json::json!(["*", "http://127.0.0.1:4000"]),
            "granted http capability hosts plus the granted sidecar's implicit host"
        );
        let sidecars: serde_json::Value =
            serde_json::from_str(&buffers.sidecars_json.lock().unwrap()).unwrap();
        assert_eq!(sidecars[0]["granted"], true);
        let filesystem: serde_json::Value =
            serde_json::from_str(&buffers.filesystem_json.lock().unwrap()).unwrap();
        assert_eq!(filesystem[0]["granted"], true);
    }
}

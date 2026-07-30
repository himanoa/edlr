//! plugin/driver どちらの manifest 型からもジェネリックなサービス
//! (`registry::filesystem::FilesystemService` が最初の consumer)を書けるように
//! する薄い trait。
//!
//! plugin 側は `crate::plugin::Manifest` そのもの、driver 側は
//! `crate::driver::manifest::DriverManifest` が実装する。両者の違いは
//! 「未登録 id のエラー variant」(`RegistryError::UnknownPlugin` vs
//! `UnknownDriver`)と「設定/承認ストア向けの `Manifest` 射影」
//! (plugin は自分自身を clone するだけ、driver は `id`/`settings`/
//! `capabilities`/`sidecars`/`filesystem` だけを詰めた `Manifest` を組み立てる
//! -- `DriverManifest::as_settings_manifest` のドキュメント参照)だけで、
//! それ以外のロジックは完全に同一(`docs/superpowers/specs/2026-07-31-phase4-registry-analysis.md`
//! §3 の同型コード対応表を参照)。
//!
//! `subject_noun` は `registry::sidecar::SidecarService::control_sidecar` の
//! "plugin {id} is disabled" / "driver {id} is disabled" 分岐で使う
//! (Phase 4 タスク6)。

use crate::plugin::registry::RegistryError;
use crate::plugin::{FilesystemRequest, Manifest, SidecarRequest};

/// ジェネリックな `registry` 系サービスが manifest 型に要求する最小限の面。
pub(crate) trait RegistrySubject: Clone {
    /// この manifest が属するプラグイン/ドライバの id。
    fn id(&self) -> &str;

    /// `[[filesystem]]` 宣言(宣言順)。
    fn filesystem(&self) -> &[FilesystemRequest];

    /// `[[sidecar]]` 宣言(宣言順)。
    fn sidecars(&self) -> &[SidecarRequest];

    /// `SettingsStore`/`GrantsStore`/`FilesystemConfigStore` など、既存の
    /// ストア類が引数に取る `crate::plugin::Manifest` への射影。plugin は
    /// 自分自身の clone、driver は `DriverManifest::as_settings_manifest`
    /// と同じ変換(タスクブリーフ参照)。
    fn as_settings_manifest(&self) -> Manifest;

    /// `id` が未登録だったときに返すエラー(`UnknownPlugin` vs
    /// `UnknownDriver`)。
    fn unknown_error(id: &str) -> RegistryError;

    /// エラーメッセージの主語("plugin"/"driver")。`control_sidecar` の
    /// disabled メッセージ用。
    fn subject_noun() -> &'static str;
}

impl RegistrySubject for Manifest {
    fn id(&self) -> &str {
        &self.id
    }

    fn filesystem(&self) -> &[FilesystemRequest] {
        &self.filesystem
    }

    fn sidecars(&self) -> &[SidecarRequest] {
        &self.sidecars
    }

    fn as_settings_manifest(&self) -> Manifest {
        self.clone()
    }

    fn unknown_error(id: &str) -> RegistryError {
        RegistryError::UnknownPlugin(id.to_string())
    }

    fn subject_noun() -> &'static str {
        "plugin"
    }
}

impl RegistrySubject for crate::driver::manifest::DriverManifest {
    fn id(&self) -> &str {
        &self.id
    }

    fn filesystem(&self) -> &[FilesystemRequest] {
        &self.filesystem
    }

    fn sidecars(&self) -> &[SidecarRequest] {
        &self.sidecars
    }

    fn as_settings_manifest(&self) -> Manifest {
        crate::driver::manifest::DriverManifest::as_settings_manifest(self)
    }

    fn unknown_error(id: &str) -> RegistryError {
        RegistryError::UnknownDriver(id.to_string())
    }

    fn subject_noun() -> &'static str {
        "driver"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::manifest::DriverManifest;

    fn plain_manifest(id: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    fn plain_driver_manifest(id: &str) -> DriverManifest {
        DriverManifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "driver.wasm".into(),
            topics: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
        }
    }

    #[test]
    fn manifest_unknown_error_is_unknown_plugin() {
        assert!(matches!(
            Manifest::unknown_error("nope"),
            RegistryError::UnknownPlugin(id) if id == "nope"
        ));
        assert_eq!(Manifest::subject_noun(), "plugin");
    }

    #[test]
    fn driver_manifest_unknown_error_is_unknown_driver() {
        assert!(matches!(
            DriverManifest::unknown_error("nope"),
            RegistryError::UnknownDriver(id) if id == "nope"
        ));
        assert_eq!(DriverManifest::subject_noun(), "driver");
    }

    #[test]
    fn manifest_as_settings_manifest_is_identity() {
        let manifest = plain_manifest("m");
        assert_eq!(manifest.as_settings_manifest(), manifest);
    }

    #[test]
    fn driver_manifest_as_settings_manifest_matches_the_existing_projection() {
        let driver = plain_driver_manifest("d");
        assert_eq!(
            RegistrySubject::as_settings_manifest(&driver),
            driver.as_settings_manifest()
        );
    }
}

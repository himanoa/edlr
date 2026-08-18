//! plugin/driver どちらの manifest 型からもジェネリックなサービス
//! (`registry::filesystem::FilesystemService` が最初の consumer)を書けるように
//! する薄い trait。
//!
//! plugin 側は `crate::manifest::Manifest` そのもの、driver 側は
//! `crate::manifest::driver::DriverManifest` が実装する。両者の違いは
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

use crate::capability::grants::GrantsError;
use crate::capability::request::{FilesystemRequest, SidecarRequest};
use crate::manifest::Manifest;
use crate::registry::driver::DriverRegistryError;
use crate::registry::plugin::RegistryError;
use crate::settings::store::SettingsError;

/// ジェネリックな `registry` 系サービスが manifest 型に要求する最小限の面。
pub(crate) trait RegistrySubject: Clone {
    /// settings/grants 共通経路(`registry::settings::SettingsService` /
    /// `registry::grants::GrantService` の非 dashboard 経路)が返すエラー型。
    /// plugin は `RegistryError`、driver は `DriverRegistryError`。この
    /// 関連型があることで、共通経路は subject ごとに正しいエラー型をそのまま
    /// 返せるようになり、呼び出し側(`DriverRegistry`)は変換なしで使える
    /// (`registry::driver::to_driver_error` を撤去した理由)。
    type Error: std::error::Error;

    /// この manifest が属するプラグイン/ドライバの id。
    fn id(&self) -> &str;

    /// `[[filesystem]]` 宣言(宣言順)。
    fn filesystem(&self) -> &[FilesystemRequest];

    /// `[[sidecar]]` 宣言(宣言順)。
    fn sidecars(&self) -> &[SidecarRequest];

    /// `SettingsStore`/`GrantsStore`/`FilesystemConfigStore` など、既存の
    /// ストア類が引数に取る `crate::manifest::Manifest` への射影。plugin は
    /// 自分自身の clone、driver は `DriverManifest::as_settings_manifest`
    /// と同じ変換(タスクブリーフ参照)。
    fn as_settings_manifest(&self) -> Manifest;

    /// `id` が未登録だったときに settings/grants 共通経路が返すエラー
    /// (`RegistryError::UnknownPlugin` vs `DriverRegistryError::UnknownDriver`)。
    fn unknown_error(id: &str) -> Self::Error;

    /// `SettingsStore::update`/`update_and_effective` の検証・永続化エラーを
    /// `Self::Error` へ写像する。
    fn settings_error(e: SettingsError) -> Self::Error;

    /// `GrantsStore::set` の永続化エラーを `Self::Error` へ写像する。
    fn grants_error(e: GrantsError) -> Self::Error;

    /// sidecar/filesystem 群専用の未登録エラー。この2群は driver 側でも
    /// `RegistryError` を使う設計上の非対称(`registry::driver` の
    /// `sidecars`/`filesystem` 等のドキュメントコメント参照)があるため、
    /// `Self::Error` ではなく常に共有の `RegistryError` を返す。
    /// required method なのは意図的: `subject_noun()` の文字列比較で導出する
    /// デフォルト実装だと、第3の Subject を足したとき無言で誤った variant に
    /// 落ちる(issue x0h7)。
    fn unknown_registry_error(id: &str) -> RegistryError;

    /// エラーメッセージの主語("plugin"/"driver")。`control_sidecar` の
    /// disabled メッセージ用。
    fn subject_noun() -> &'static str;
}

impl RegistrySubject for Manifest {
    type Error = RegistryError;

    fn id(&self) -> &str {
        &self.id
    }

    fn filesystem(&self) -> &[FilesystemRequest] {
        &self.filesystem
    }

    fn sidecars(&self) -> &[SidecarRequest] {
        &self.sidecars
    }

    /// plugin 側は恒等射影(`self.clone()`)。呼ばれるたびに `Manifest` 全体
    /// (`settings`/`capabilities`/`sidecars`/`filesystem`/`bus`/`dashboard`/
    /// `schedules` を含む)を deep clone する -- `fs`/`sidecar`/`settings`/
    /// `grants` の各 RPC 経路(`find_manifest`/`build_*_infos` などが
    /// 呼ばれるたびに)で毎回発生する。プロファイルでホットと実証されるまでは
    /// 容認する(issue kgc6 残件1): `Cow<'_, Manifest>` 化は、driver 側の
    /// projection(`DriverManifest::as_settings_manifest` -- こちらは元々
    /// 新規に組み立てる射影で恒等ではない)まで含めた generic 面の複雑化に
    /// 見合わない。
    fn as_settings_manifest(&self) -> Manifest {
        self.clone()
    }

    fn unknown_error(id: &str) -> RegistryError {
        RegistryError::UnknownPlugin(id.to_string())
    }

    fn unknown_registry_error(id: &str) -> RegistryError {
        RegistryError::UnknownPlugin(id.to_string())
    }

    fn settings_error(e: SettingsError) -> RegistryError {
        RegistryError::Settings(e)
    }

    fn grants_error(e: GrantsError) -> RegistryError {
        RegistryError::Grants(e)
    }

    fn subject_noun() -> &'static str {
        "plugin"
    }
}

impl RegistrySubject for crate::manifest::driver::DriverManifest {
    type Error = DriverRegistryError;

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
        crate::manifest::driver::DriverManifest::as_settings_manifest(self)
    }

    fn unknown_error(id: &str) -> DriverRegistryError {
        DriverRegistryError::UnknownDriver(id.to_string())
    }

    fn unknown_registry_error(id: &str) -> RegistryError {
        RegistryError::UnknownDriver(id.to_string())
    }

    fn settings_error(e: SettingsError) -> DriverRegistryError {
        DriverRegistryError::Settings(e)
    }

    fn grants_error(e: GrantsError) -> DriverRegistryError {
        DriverRegistryError::Grants(e)
    }

    fn subject_noun() -> &'static str {
        "driver"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::driver::DriverManifest;

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
            delivery: Default::default(),
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
            DriverRegistryError::UnknownDriver(id) if id == "nope"
        ));
        assert_eq!(DriverManifest::subject_noun(), "driver");
    }

    #[test]
    fn manifest_unknown_registry_error_is_unknown_plugin() {
        assert!(matches!(
            Manifest::unknown_registry_error("nope"),
            RegistryError::UnknownPlugin(id) if id == "nope"
        ));
    }

    #[test]
    fn driver_manifest_unknown_registry_error_is_unknown_driver() {
        assert!(matches!(
            DriverManifest::unknown_registry_error("nope"),
            RegistryError::UnknownDriver(id) if id == "nope"
        ));
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

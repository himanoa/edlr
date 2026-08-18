//! capability(プラグインが manifest で宣言する要求と、ユーザーによる承認)。
//!
//! 要求(request)と承認(grant)は capability という 1 概念の表と裏なので、
//! 両方をこのモジュール配下で扱う。詳細は
//! `docs/superpowers/specs/2026-07-30-core-refactoring-design.md` を参照。

pub mod fingerprint;
pub mod grants;
pub mod request;
pub mod validate;

use crate::manifest::Manifest;
use grants::{GrantState, GrantsError, GrantsStore};

/// capability 承認の永続化の口。ディスク実装は [`GrantsStore`]。
/// テストではインメモリ実装を注入して、tempdir なしの純粋テストを書く。
///
/// メソッドは `GrantsStore` の公開 API と同名同型(挙動不変で導入するため)。
pub trait GrantStorage {
    fn state(&self, manifest: &Manifest) -> GrantState;
    fn set(&self, manifest: &Manifest, granted: bool) -> Result<GrantState, GrantsError>;
    fn sidecar_state(&self, manifest: &Manifest, name: &str) -> GrantState;
    fn set_sidecar(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError>;
    fn filesystem_state(&self, manifest: &Manifest, name: &str) -> GrantState;
    fn set_filesystem(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError>;
    fn bus_state(&self, manifest: &Manifest, driver: &str) -> GrantState;
    fn set_bus(
        &self,
        manifest: &Manifest,
        driver: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError>;
    fn dashboard_state(&self, manifest: &Manifest, widget: &str) -> GrantState;
    fn set_dashboard(
        &self,
        manifest: &Manifest,
        widget: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError>;
}

impl GrantStorage for GrantsStore {
    fn state(&self, manifest: &Manifest) -> GrantState {
        GrantsStore::state(self, manifest)
    }
    fn set(&self, manifest: &Manifest, granted: bool) -> Result<GrantState, GrantsError> {
        GrantsStore::set(self, manifest, granted)
    }
    fn sidecar_state(&self, manifest: &Manifest, name: &str) -> GrantState {
        GrantsStore::sidecar_state(self, manifest, name)
    }
    fn set_sidecar(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        GrantsStore::set_sidecar(self, manifest, name, granted)
    }
    fn filesystem_state(&self, manifest: &Manifest, name: &str) -> GrantState {
        GrantsStore::filesystem_state(self, manifest, name)
    }
    fn set_filesystem(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        GrantsStore::set_filesystem(self, manifest, name, granted)
    }
    fn bus_state(&self, manifest: &Manifest, driver: &str) -> GrantState {
        GrantsStore::bus_state(self, manifest, driver)
    }
    fn set_bus(
        &self,
        manifest: &Manifest,
        driver: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        GrantsStore::set_bus(self, manifest, driver, granted)
    }
    fn dashboard_state(&self, manifest: &Manifest, widget: &str) -> GrantState {
        GrantsStore::dashboard_state(self, manifest, widget)
    }
    fn set_dashboard(
        &self,
        manifest: &Manifest,
        widget: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        GrantsStore::set_dashboard(self, manifest, widget, granted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// trait 経由でディスク実装を呼べること(= Registry 側をジェネリック化
    /// したとき既存実装がそのまま挿さること)の静的確認。
    fn state_via_trait<S: GrantStorage>(storage: &S, manifest: &Manifest) -> GrantState {
        storage.state(manifest)
    }

    #[test]
    fn grants_store_satisfies_grant_storage() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().to_path_buf());
        let manifest = Manifest {
            id: "cap-trait-check".into(),
            name: "cap-trait-check".into(),
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
        };
        let state = state_via_trait(&store, &manifest);
        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: false
            }
        );
    }
}

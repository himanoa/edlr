//! プラグイン設定の検証・マージと永続化の口。
//!
//! 検証・マージの純粋ロジックは [`store`] にある(Phase 3 で
//! `plugin/settings.rs` から移動)。

pub mod store;

use crate::plugin::Manifest;
use store::{SettingsError, SettingsStore};

/// 設定永続化の口。ディスク実装は [`SettingsStore`]。
pub trait Storage {
    /// manifest 由来の defaults に保存済みの値をマージして返す。
    fn effective(&self, manifest: &Manifest) -> serde_json::Map<String, serde_json::Value>;
    /// 検証してから部分適用で保存する(検証は書き込み前に全件)。
    fn update(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), SettingsError>;
    /// `update` と同じ検証・永続化を行い、書き込み後の effective settings を
    /// 同じロック区間内で返す。
    fn update_and_effective(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, SettingsError>;
}

impl Storage for SettingsStore {
    fn effective(&self, manifest: &Manifest) -> serde_json::Map<String, serde_json::Value> {
        SettingsStore::effective(self, manifest)
    }
    fn update(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), SettingsError> {
        SettingsStore::update(self, manifest, values)
    }
    fn update_and_effective(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, SettingsError> {
        SettingsStore::update_and_effective(self, manifest, values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effective_via_trait<S: Storage>(
        storage: &S,
        manifest: &Manifest,
    ) -> serde_json::Map<String, serde_json::Value> {
        storage.effective(manifest)
    }

    #[test]
    fn settings_store_satisfies_storage() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().to_path_buf());
        // Task 1 と同じ流儀で Manifest を構築(settings が空なら effective は空)
        let manifest = Manifest {
            id: "settings-trait-check".into(),
            name: "settings-trait-check".into(),
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
        };
        assert!(effective_via_trait(&store, &manifest).is_empty());
    }
}

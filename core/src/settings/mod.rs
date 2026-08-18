//! プラグイン設定の検証・マージと永続化の口。
//!
//! 検証・マージの純粋ロジックは [`validate`] にある。永続化(ディスク I/O)は
//! [`store`] が担う。

pub mod filesystem;
pub mod sidecar;
pub mod store;
pub mod validate;

use std::fmt;

use crate::manifest::Manifest;
use store::SettingsStore;

/// `SettingsStore::update` が返しうるエラー。
#[derive(Debug)]
pub enum SettingsError {
    /// マニフェストに存在しない key が指定された。
    UnknownKey(String),
    /// 値の JSON 型がフィールドの宣言型と一致しない(例: Boolean フィールドに文字列)。
    TypeMismatch { key: String, expected: &'static str },
    /// Select フィールドの値が `options` に含まれていない。
    NotAnOption { key: String, value: String },
    /// Slider フィールドの値が `min..=max` の範囲外。
    OutOfRange { key: String, min: f64, max: f64 },
    /// Map フィールドのエントリに空文字列のキーが含まれている。
    /// `TypeMismatch` と分けているのは、UI で「行を足したが名前を入力して
    /// いない」という具体的な状況を、型違いと同じ文言で報せると直しようが
    /// ないため(キー名にそれ以外の制約は課さない)。
    EmptyMapKey { key: String },
    /// ディレクトリ作成やファイル書き込みに失敗した。
    Io(std::io::Error),
    /// 保存直前の JSON シリアライズに失敗した。
    Serialize(serde_json::Error),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsError::UnknownKey(key) => write!(f, "unknown settings key: {key}"),
            SettingsError::TypeMismatch { key, expected } => {
                write!(f, "settings key {key} expected a {expected} value")
            }
            SettingsError::NotAnOption { key, value } => {
                write!(
                    f,
                    "settings key {key} value {value:?} is not one of the allowed options"
                )
            }
            SettingsError::OutOfRange { key, min, max } => {
                write!(f, "settings key {key} value must be between {min} and {max}")
            }
            SettingsError::EmptyMapKey { key } => {
                write!(f, "settings key {key} must not contain an empty entry key")
            }
            SettingsError::Io(e) => write!(f, "failed to write settings: {e}"),
            SettingsError::Serialize(e) => write!(f, "failed to serialize settings: {e}"),
        }
    }
}

impl std::error::Error for SettingsError {}

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
            delivery: Default::default(),
        };
        assert!(effective_via_trait(&store, &manifest).is_empty());
    }
}

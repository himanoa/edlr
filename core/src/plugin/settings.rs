use std::fmt;
use std::fs;
use std::path::PathBuf;

use crate::plugin::Manifest;

/// プラグインごとの設定値を `<settings-dir>/<id>.json` に保存するストア。
pub struct SettingsStore {
    dir: PathBuf,
}

/// `SettingsStore::update` が返しうるエラー。
#[derive(Debug)]
pub enum SettingsError {
    /// マニフェストに存在しない key が指定された。
    UnknownKey(String),
    /// ディレクトリ作成やファイル書き込みに失敗した。
    Io(std::io::Error),
    /// 保存直前の JSON シリアライズに失敗した。
    Serialize(serde_json::Error),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsError::UnknownKey(key) => write!(f, "unknown settings key: {key}"),
            SettingsError::Io(e) => write!(f, "failed to write settings: {e}"),
            SettingsError::Serialize(e) => write!(f, "failed to serialize settings: {e}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl SettingsStore {
    pub fn new(dir: PathBuf) -> Self {
        SettingsStore { dir }
    }

    fn path_for(&self, manifest: &Manifest) -> PathBuf {
        self.dir.join(format!("{}.json", manifest.id))
    }

    /// manifest 由来の defaults に保存済みの値をマージして返す。
    ///
    /// ファイルが存在しない・JSON が壊れている・トップレベルがオブジェクトでない
    /// 場合はいずれも defaults のみを返す(panic しない)。
    pub fn effective(&self, manifest: &Manifest) -> serde_json::Map<String, serde_json::Value> {
        let mut result = serde_json::Map::new();
        for setting in &manifest.settings {
            result.insert(setting.key().to_string(), setting.default_value());
        }

        let path = self.path_for(manifest);
        let Ok(content) = fs::read_to_string(&path) else {
            return result;
        };
        let Ok(serde_json::Value::Object(saved)) =
            serde_json::from_str::<serde_json::Value>(&content)
        else {
            return result;
        };

        for setting in &manifest.settings {
            if let Some(value) = saved.get(setting.key()) {
                result.insert(setting.key().to_string(), value.clone());
            }
        }

        result
    }

    /// 現在の保存値(なければ defaults)に `values` を部分適用して保存する。
    ///
    /// `dir` が存在しなければ作成する。`values` に manifest に存在しない key が
    /// あれば何も書き込まず `Err(SettingsError::UnknownKey)` を返す。
    pub fn update(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), SettingsError> {
        for key in values.keys() {
            if !manifest.settings.iter().any(|s| s.key() == key) {
                return Err(SettingsError::UnknownKey(key.clone()));
            }
        }

        let mut current = self.effective(manifest);
        for (key, value) in values {
            current.insert(key.clone(), value.clone());
        }

        fs::create_dir_all(&self.dir).map_err(SettingsError::Io)?;
        let serialized = serde_json::to_string_pretty(&serde_json::Value::Object(current))
            .map_err(SettingsError::Serialize)?;
        fs::write(self.path_for(manifest), serialized).map_err(SettingsError::Io)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::SettingField;
    use std::fs;

    fn sample_manifest() -> Manifest {
        Manifest {
            id: "sample-plugin".into(),
            name: "Sample Plugin".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![
                SettingField::Boolean {
                    key: "enabled".into(),
                    label: "Enabled".into(),
                    default: true,
                },
                SettingField::String {
                    key: "greeting".into(),
                    label: "Greeting".into(),
                    default: "hello".into(),
                },
            ],
        }
    }

    #[test]
    fn effective_returns_defaults_when_no_saved_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = sample_manifest();

        let effective = store.effective(&manifest);

        assert_eq!(effective.get("enabled"), Some(&serde_json::json!(true)));
        assert_eq!(effective.get("greeting"), Some(&serde_json::json!("hello")));
    }

    #[test]
    fn effective_merges_saved_values_over_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("sample-plugin.json"),
            serde_json::json!({"enabled": false}).to_string(),
        )
        .unwrap();

        let store = SettingsStore::new(dir);
        let manifest = sample_manifest();

        let effective = store.effective(&manifest);

        assert_eq!(effective.get("enabled"), Some(&serde_json::json!(false)));
        assert_eq!(effective.get("greeting"), Some(&serde_json::json!("hello")));
    }

    #[test]
    fn effective_falls_back_to_defaults_on_broken_or_non_object_json() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("sample-plugin.json"), "not valid json {{{").unwrap();

        let store = SettingsStore::new(dir.clone());
        let manifest = sample_manifest();
        let effective = store.effective(&manifest);
        assert_eq!(effective.get("enabled"), Some(&serde_json::json!(true)));
        assert_eq!(effective.get("greeting"), Some(&serde_json::json!("hello")));

        fs::write(dir.join("sample-plugin.json"), "[1, 2, 3]").unwrap();
        let effective = store.effective(&manifest);
        assert_eq!(effective.get("enabled"), Some(&serde_json::json!(true)));
        assert_eq!(effective.get("greeting"), Some(&serde_json::json!("hello")));
    }

    #[test]
    fn update_persists_partial_change_reflected_in_effective() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        let store = SettingsStore::new(dir.clone());
        let manifest = sample_manifest();

        let mut values = serde_json::Map::new();
        values.insert("greeting".to_string(), serde_json::json!("hi there"));
        store
            .update(&manifest, &values)
            .expect("update should succeed");

        assert!(dir.join("sample-plugin.json").is_file());

        let effective = store.effective(&manifest);
        assert_eq!(effective.get("enabled"), Some(&serde_json::json!(true)));
        assert_eq!(
            effective.get("greeting"),
            Some(&serde_json::json!("hi there"))
        );
    }

    #[test]
    fn update_with_unknown_key_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = sample_manifest();

        let mut values = serde_json::Map::new();
        values.insert("does-not-exist".to_string(), serde_json::json!(1));

        let err = store
            .update(&manifest, &values)
            .expect_err("unknown key should be rejected");
        assert!(matches!(err, SettingsError::UnknownKey(k) if k == "does-not-exist"));
    }

    #[test]
    fn update_creates_settings_dir_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("settings");
        assert!(!dir.exists());

        let store = SettingsStore::new(dir.clone());
        let manifest = sample_manifest();

        let mut values = serde_json::Map::new();
        values.insert("enabled".to_string(), serde_json::json!(false));

        store
            .update(&manifest, &values)
            .expect("update should create dir and succeed");

        assert!(dir.is_dir());
        assert!(dir.join("sample-plugin.json").is_file());
    }
}

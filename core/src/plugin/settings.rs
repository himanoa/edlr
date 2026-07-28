use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::plugin::{Manifest, SettingField};

/// プラグインごとの設定値を `<settings-dir>/<id>.json` に保存するストア。
///
/// 内部に `Mutex<()>` を持ち、`update`/`effective` は常にこのロックを保持した
/// 状態でファイルを読み書きする。これにより複数スレッド(RPC ごとに
/// `spawn_blocking` される)から同じマニフェストに対して同時に呼び出されても、
/// read-merge-write のロストアップデートや、書き込み途中のファイルを読んで
/// しまう競合が起きない。
pub struct SettingsStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

/// `SettingsStore::update` が返しうるエラー。
#[derive(Debug)]
pub enum SettingsError {
    /// マニフェストに存在しない key が指定された。
    UnknownKey(String),
    /// 値の JSON 型がフィールドの宣言型と一致しない(例: Boolean フィールドに文字列)。
    TypeMismatch { key: String, expected: &'static str },
    /// Select フィールドの値が `options` に含まれていない。
    NotAnOption { key: String, value: String },
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
            SettingsError::Io(e) => write!(f, "failed to write settings: {e}"),
            SettingsError::Serialize(e) => write!(f, "failed to serialize settings: {e}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl SettingsStore {
    pub fn new(dir: PathBuf) -> Self {
        SettingsStore {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn path_for(&self, manifest: &Manifest) -> PathBuf {
        self.dir.join(format!("{}.json", manifest.id))
    }

    /// `value` が `field` の宣言型(および Select の `options`)に適合するか検証する。
    fn validate_value(
        field: &SettingField,
        value: &serde_json::Value,
    ) -> Result<(), SettingsError> {
        match field {
            SettingField::Boolean { key, .. } => {
                if !value.is_boolean() {
                    return Err(SettingsError::TypeMismatch {
                        key: key.clone(),
                        expected: "boolean",
                    });
                }
            }
            SettingField::String { key, .. } => {
                if !value.is_string() {
                    return Err(SettingsError::TypeMismatch {
                        key: key.clone(),
                        expected: "string",
                    });
                }
            }
            SettingField::Number { key, .. } => {
                if !value.is_number() {
                    return Err(SettingsError::TypeMismatch {
                        key: key.clone(),
                        expected: "number",
                    });
                }
            }
            SettingField::Select { key, options, .. } => {
                let Some(s) = value.as_str() else {
                    return Err(SettingsError::TypeMismatch {
                        key: key.clone(),
                        expected: "string",
                    });
                };
                if !options.iter().any(|o| o == s) {
                    return Err(SettingsError::NotAnOption {
                        key: key.clone(),
                        value: s.to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    /// manifest 由来の defaults に保存済みの値をマージして返す。
    ///
    /// ファイルが存在しない・JSON が壊れている・トップレベルがオブジェクトでない
    /// 場合はいずれも defaults のみを返す(panic しない)。
    ///
    /// 保存ファイルに manifest の `settings` に存在しない key があっても無視する
    /// (意図的な挙動)。そのため `update()` を呼ぶと、次回保存時にそうした
    /// スキーマ外の古い key はマージ元(`effective()`)に含まれず、結果として
    /// ディスク上のファイルから間引かれる。
    pub fn effective(&self, manifest: &Manifest) -> serde_json::Map<String, serde_json::Value> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.effective_locked(manifest)
    }

    /// `effective()` の本体。呼び出し元が既に `self.lock` を保持していること
    /// を前提とする(二重ロックしない内部ヘルパー)。
    fn effective_locked(&self, manifest: &Manifest) -> serde_json::Map<String, serde_json::Value> {
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
    /// あれば何も書き込まず `Err(SettingsError::UnknownKey)` を返す。また各値の
    /// JSON 型がフィールドの宣言型(Select は `options` の値であること)と一致
    /// しない場合も何も書き込まず `Err` を返す。検証は書き込み前に全件行うため、
    /// 一部だけ書き込まれることはない。
    pub fn update(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), SettingsError> {
        self.update_and_effective(manifest, values)?;
        Ok(())
    }

    /// `update()` と同じ検証・永続化を行い、書き込み後の effective settings
    /// を同じロック区間内でまとめて返す。`update()` に続けて `effective()`
    /// を呼ぶと、その間に別スレッドの `update()` が割り込んで
    /// (呼び出し元が期待する)「自分が書いた値」と異なる effective 値を
    /// 読んでしまう可能性があるため、両者をアトミックに行いたい呼び出し元
    /// (`Registry::set_values` など)はこちらを使うこと。
    pub fn update_and_effective(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, SettingsError> {
        for (key, value) in values {
            let field = manifest
                .settings
                .iter()
                .find(|s| s.key() == key)
                .ok_or_else(|| SettingsError::UnknownKey(key.clone()))?;
            Self::validate_value(field, value)?;
        }

        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let mut current = self.effective_locked(manifest);
        for (key, value) in values {
            current.insert(key.clone(), value.clone());
        }

        fs::create_dir_all(&self.dir).map_err(SettingsError::Io)?;
        let serialized = serde_json::to_string_pretty(&serde_json::Value::Object(current))
            .map_err(SettingsError::Serialize)?;
        let target = self.path_for(manifest);
        let tmp_path = self
            .dir
            .join(format!("{}.json.tmp.{}", manifest.id, std::process::id()));
        fs::write(&tmp_path, serialized).map_err(SettingsError::Io)?;
        fs::rename(&tmp_path, &target).map_err(SettingsError::Io)?;

        Ok(self.effective_locked(manifest))
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
                SettingField::Select {
                    key: "mode".into(),
                    label: "Mode".into(),
                    default: "a".into(),
                    options: vec!["a".into(), "b".into()],
                },
            ],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
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

    #[test]
    fn update_accepts_bool_value_for_boolean_field() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = sample_manifest();

        let mut values = serde_json::Map::new();
        values.insert("enabled".to_string(), serde_json::json!(false));

        store
            .update(&manifest, &values)
            .expect("bool value should be accepted for boolean field");

        let effective = store.effective(&manifest);
        assert_eq!(effective.get("enabled"), Some(&serde_json::json!(false)));
    }

    #[test]
    fn update_rejects_string_value_for_boolean_field() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = sample_manifest();

        let mut values = serde_json::Map::new();
        values.insert("enabled".to_string(), serde_json::json!("yes"));

        let err = store
            .update(&manifest, &values)
            .expect_err("string value should be rejected for boolean field");
        assert!(matches!(
            err,
            SettingsError::TypeMismatch { key, expected }
                if key == "enabled" && expected == "boolean"
        ));
    }

    #[test]
    fn update_rejects_number_value_for_string_field() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = sample_manifest();

        let mut values = serde_json::Map::new();
        values.insert("greeting".to_string(), serde_json::json!(42));

        let err = store
            .update(&manifest, &values)
            .expect_err("number value should be rejected for string field");
        assert!(matches!(
            err,
            SettingsError::TypeMismatch { key, expected }
                if key == "greeting" && expected == "string"
        ));
    }

    #[test]
    fn update_accepts_valid_option_for_select_field() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = sample_manifest();

        let mut values = serde_json::Map::new();
        values.insert("mode".to_string(), serde_json::json!("b"));

        store
            .update(&manifest, &values)
            .expect("valid option should be accepted for select field");

        let effective = store.effective(&manifest);
        assert_eq!(effective.get("mode"), Some(&serde_json::json!("b")));
    }

    #[test]
    fn update_rejects_invalid_option_for_select_field() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = sample_manifest();

        let mut values = serde_json::Map::new();
        values.insert("mode".to_string(), serde_json::json!("not-an-option"));

        let err = store
            .update(&manifest, &values)
            .expect_err("invalid option should be rejected for select field");
        assert!(matches!(
            err,
            SettingsError::NotAnOption { key, value }
                if key == "mode" && value == "not-an-option"
        ));
    }

    #[test]
    fn update_rejected_by_type_mismatch_leaves_file_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        let store = SettingsStore::new(dir);
        let manifest = sample_manifest();

        let mut valid = serde_json::Map::new();
        valid.insert("greeting".to_string(), serde_json::json!("first value"));
        store
            .update(&manifest, &valid)
            .expect("valid update should succeed");

        let mut invalid = serde_json::Map::new();
        invalid.insert("greeting".to_string(), serde_json::json!(123));
        store
            .update(&manifest, &invalid)
            .expect_err("type mismatch update should be rejected");

        let effective = store.effective(&manifest);
        assert_eq!(
            effective.get("greeting"),
            Some(&serde_json::json!("first value"))
        );
    }

    /// N 個のフィールドを持つマニフェスト(並行更新テスト用)。
    fn concurrent_manifest(n: usize) -> Manifest {
        Manifest {
            id: "concurrent-plugin".into(),
            name: "Concurrent Plugin".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: (0..n)
                .map(|i| SettingField::String {
                    key: format!("key{i}"),
                    label: format!("Key {i}"),
                    default: "unset".into(),
                })
                .collect(),
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
        }
    }

    /// 複数スレッドが同一マニフェストの異なる key を同時に `update` しても、
    /// 全ての書き込みが失われずに `effective()` へ反映されることを確認する。
    /// `SettingsStore` 内部にロックがなく read-merge-write が非アトミックだった
    /// 旧実装では、スレッド同士が互いの書き込みを踏み潰すロストアップデートが
    /// 発生し、このテストは(すべてのキーが埋まらず)失敗していた。
    #[test]
    fn concurrent_updates_to_different_keys_are_not_lost() {
        use std::sync::Arc;
        use std::thread;

        const N: usize = 8;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        let store = Arc::new(SettingsStore::new(dir));
        let manifest = Arc::new(concurrent_manifest(N));

        let handles: Vec<_> = (0..N)
            .map(|i| {
                let store = Arc::clone(&store);
                let manifest = Arc::clone(&manifest);
                thread::spawn(move || {
                    let mut values = serde_json::Map::new();
                    values.insert(format!("key{i}"), serde_json::json!(format!("value{i}")));
                    store
                        .update(&manifest, &values)
                        .expect("concurrent update should succeed");
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("thread should not panic");
        }

        let effective = store.effective(&manifest);
        for i in 0..N {
            assert_eq!(
                effective.get(&format!("key{i}")),
                Some(&serde_json::json!(format!("value{i}"))),
                "key{i} should retain its concurrently written value"
            );
        }
    }
}

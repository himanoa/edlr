use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::manifest::Manifest;

pub use super::SettingsError;

/// RPC 応答用に、秘密情報を取り除いた設定値と「設定済みの秘密情報キー」の
/// 一覧に分ける。
///
/// `SettingField::Secret` は write-only(UI から書けるが読み出せない)なので、
/// 値そのものは応答に載せない。代わりに「空でない値が保存されているか」だけを
/// 返し、UI が「設定済み」と「未設定」を区別できるようにする。
///
/// この分離は**読み出し系 RPC 専用**であって、プラグインへ渡す
/// `settings_json`(`host-settings.get-all`)には適用しない -- 秘密情報を
/// 渡す相手はそのプラグイン自身であるため。
pub fn split_secrets(
    manifest: &Manifest,
    values: serde_json::Map<String, serde_json::Value>,
) -> (serde_json::Map<String, serde_json::Value>, Vec<String>) {
    let secret_keys: Vec<&str> = manifest
        .settings
        .iter()
        .filter(|field| field.is_secret())
        .map(|field| field.key())
        .collect();

    let mut visible = values;
    let mut configured = Vec::new();
    for key in secret_keys {
        let removed = visible.remove(key);
        let is_set = removed
            .as_ref()
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());
        if is_set {
            configured.push(key.to_string());
        }
    }
    (visible, configured)
}

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
        let saved = fs::read_to_string(self.path_for(manifest))
            .ok()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
            .and_then(|value| match value {
                serde_json::Value::Object(map) => Some(map),
                _ => None,
            });
        crate::settings::validate::effective_values(&manifest.settings, saved.as_ref())
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
            crate::settings::validate::validate_value(field, value)?;
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
    use crate::manifest::SettingField;
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
                    options: Some(vec!["a".into(), "b".into()]),
                    options_from: None,
                },
            ],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    /// `sample_manifest` に `secret` 型を 1 件足したもの。
    fn manifest_with_secret() -> Manifest {
        let mut manifest = sample_manifest();
        manifest.settings.push(SettingField::Secret {
            key: "api-key".into(),
            label: "API Key".into(),
        });
        manifest
    }

    /// `sample_manifest` に `map` 型を 1 件足したもの。
    fn manifest_with_map() -> Manifest {
        let mut manifest = sample_manifest();
        manifest.settings.push(SettingField::Map {
            key: "aliases".into(),
            label: "Aliases".into(),
        });
        manifest
    }

    #[test]
    fn map_defaults_to_an_empty_object_and_stores_string_pairs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = manifest_with_map();

        assert_eq!(
            store.effective(&manifest).get("aliases"),
            Some(&serde_json::json!({}))
        );

        let mut values = serde_json::Map::new();
        values.insert(
            "aliases".into(),
            serde_json::json!({"Sol": "太陽系", "Shinrarta Dezhra": "本拠"}),
        );
        store
            .update(&manifest, &values)
            .expect("a string -> string object should be accepted");

        assert_eq!(
            store.effective(&manifest).get("aliases"),
            Some(&serde_json::json!({"Sol": "太陽系", "Shinrarta Dezhra": "本拠"}))
        );
    }

    #[test]
    fn map_rejects_non_object_values() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = manifest_with_map();

        for bad in [
            serde_json::json!("Sol=太陽系"),
            serde_json::json!(["Sol", "太陽系"]),
            serde_json::json!(42),
        ] {
            let mut values = serde_json::Map::new();
            values.insert("aliases".into(), bad.clone());
            let err = store
                .update(&manifest, &values)
                .expect_err("a non-object value should be rejected for a map field");
            assert!(
                matches!(
                    err,
                    SettingsError::TypeMismatch { ref key, expected: "map" } if key == "aliases"
                ),
                "{bad} should be a map type mismatch, got: {err}"
            );
        }
    }

    /// 値に文字列以外(number/bool/入れ子)を許さない。
    #[test]
    fn map_rejects_non_string_values() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = manifest_with_map();

        for bad in [
            serde_json::json!({"a": 1}),
            serde_json::json!({"a": true}),
            serde_json::json!({"a": {"b": "c"}}),
            serde_json::json!({"a": null}),
        ] {
            let mut values = serde_json::Map::new();
            values.insert("aliases".into(), bad.clone());
            let err = store
                .update(&manifest, &values)
                .expect_err("a non-string map value should be rejected");
            assert!(
                matches!(
                    err,
                    SettingsError::TypeMismatch { ref key, expected: "map" } if key == "aliases"
                ),
                "{bad} should be a map type mismatch, got: {err}"
            );
        }
    }

    /// キー名に制約は課さないが、空文字列キーだけは弾く(UI 上で行を
    /// 消したのか未入力なのか区別がつかなくなるため)。
    #[test]
    fn map_rejects_an_empty_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = manifest_with_map();

        let mut values = serde_json::Map::new();
        values.insert("aliases".into(), serde_json::json!({"": "太陽系"}));

        let err = store
            .update(&manifest, &values)
            .expect_err("an empty map key should be rejected");
        assert!(
            matches!(err, SettingsError::EmptyMapKey { ref key } if key == "aliases"),
            "got: {err}"
        );
    }

    #[test]
    fn map_accepts_an_empty_object_to_clear_every_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = manifest_with_map();

        let mut values = serde_json::Map::new();
        values.insert("aliases".into(), serde_json::json!({"Sol": "太陽系"}));
        store.update(&manifest, &values).unwrap();

        let mut cleared = serde_json::Map::new();
        cleared.insert("aliases".into(), serde_json::json!({}));
        store
            .update(&manifest, &cleared)
            .expect("clearing every entry should be allowed");

        assert_eq!(
            store.effective(&manifest).get("aliases"),
            Some(&serde_json::json!({}))
        );
    }

    /// `map` は秘密情報ではない(読み出し応答から消えない)。
    #[test]
    fn map_values_are_visible_in_read_responses() {
        let manifest = manifest_with_map();
        let mut values = serde_json::Map::new();
        values.insert("aliases".into(), serde_json::json!({"Sol": "太陽系"}));

        let (visible, configured) = split_secrets(&manifest, values.clone());

        assert_eq!(visible, values);
        assert!(configured.is_empty());
    }

    #[test]
    fn secret_values_are_stored_and_handed_to_the_plugin_like_any_string() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = manifest_with_secret();

        let mut values = serde_json::Map::new();
        values.insert("api-key".into(), serde_json::json!("sk-live-123"));
        store
            .update(&manifest, &values)
            .expect("a secret is stored as a plain string");

        // `effective` は生の値を返す -- プラグインへ渡す経路はここを使う。
        assert_eq!(
            store.effective(&manifest).get("api-key"),
            Some(&serde_json::json!("sk-live-123"))
        );
    }

    #[test]
    fn secret_values_must_be_strings() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let manifest = manifest_with_secret();

        let mut values = serde_json::Map::new();
        values.insert("api-key".into(), serde_json::json!(42));

        let err = store
            .update(&manifest, &values)
            .expect_err("a non-string secret should be rejected");
        assert!(matches!(
            err,
            SettingsError::TypeMismatch { ref key, expected: "string" } if key == "api-key"
        ));
    }

    /// **これがこの機能の要点**: 読み出し系の応答から秘密情報が消えること。
    #[test]
    fn split_secrets_removes_the_value_and_reports_it_as_configured() {
        let manifest = manifest_with_secret();
        let mut values = serde_json::Map::new();
        values.insert("enabled".into(), serde_json::json!(true));
        values.insert("api-key".into(), serde_json::json!("sk-live-123"));

        let (visible, configured) = split_secrets(&manifest, values);

        assert!(
            !visible.contains_key("api-key"),
            "a secret must never appear in a read response"
        );
        assert_eq!(visible.get("enabled"), Some(&serde_json::json!(true)));
        assert_eq!(configured, vec!["api-key".to_string()]);
    }

    #[test]
    fn split_secrets_reports_an_empty_secret_as_not_configured() {
        let manifest = manifest_with_secret();
        let mut values = serde_json::Map::new();
        values.insert("api-key".into(), serde_json::json!(""));

        let (visible, configured) = split_secrets(&manifest, values);

        assert!(!visible.contains_key("api-key"));
        assert!(
            configured.is_empty(),
            "an empty secret means 'not set yet', not 'configured'"
        );
    }

    #[test]
    fn split_secrets_leaves_manifests_without_secrets_untouched() {
        let manifest = sample_manifest();
        let mut values = serde_json::Map::new();
        values.insert("greeting".into(), serde_json::json!("hi"));

        let (visible, configured) = split_secrets(&manifest, values.clone());

        assert_eq!(visible, values);
        assert!(configured.is_empty());
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

    /// `options-from` の select は候補と照合しない。候補は非同期に届くので、
    /// 照合すると「ドライバが起動するまで保存できない時間帯」ができてしまう
    /// (設計書「保存時の検証」参照)。
    #[test]
    fn update_accepts_any_string_for_a_select_backed_by_a_driver_topic() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let mut manifest = sample_manifest();
        manifest.settings.push(SettingField::Select {
            key: "speaker".into(),
            label: "話者".into(),
            default: String::new(),
            options: None,
            options_from: Some(crate::manifest::OptionsFrom {
                driver: "coeiroink".into(),
                topic: "speakers".into(),
            }),
        });

        let mut values = serde_json::Map::new();
        values.insert("speaker".to_string(), serde_json::json!("a1b2:3"));

        store
            .update(&manifest, &values)
            .expect("a value should be accepted even with no candidates on the bus");

        let effective = store.effective(&manifest);
        assert_eq!(effective.get("speaker"), Some(&serde_json::json!("a1b2:3")));
    }

    /// 照合しないのは候補との突き合わせだけで、型は変わらず string。
    #[test]
    fn update_still_rejects_a_non_string_for_a_select_backed_by_a_driver_topic() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().join("settings"));
        let mut manifest = sample_manifest();
        manifest.settings.push(SettingField::Select {
            key: "speaker".into(),
            label: "話者".into(),
            default: String::new(),
            options: None,
            options_from: Some(crate::manifest::OptionsFrom {
                driver: "coeiroink".into(),
                topic: "speakers".into(),
            }),
        });

        let mut values = serde_json::Map::new();
        values.insert("speaker".to_string(), serde_json::json!(3));

        let err = store
            .update(&manifest, &values)
            .expect_err("a non-string should still be rejected");
        assert!(matches!(
            err,
            SettingsError::TypeMismatch { key, expected }
                if key == "speaker" && expected == "string"
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
            dashboard: vec![],
            schedules: vec![],
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

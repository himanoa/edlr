//! 設定値の検証と、defaults への保存値マージの純粋ロジック。
//!
//! [`crate::settings::store::SettingsStore`] はここへ委譲する薄い手続きに
//! 留め、判断そのものはここに集める(値イン値アウト)。

use crate::manifest::SettingField;
use crate::settings::store::SettingsError;

/// `value` が `field` の宣言型(および Select の `options`)に適合するか検証する。
pub fn validate_value(
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
        // 秘密情報も保存形式は文字列。違うのは読み出し側の扱いだけ
        // (`SettingField::Secret` のドキュメントコメント参照)。
        SettingField::String { key, .. } | SettingField::Secret { key, .. } => {
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
        SettingField::Select {
            key,
            options,
            options_from,
            ..
        } => {
            let Some(s) = value.as_str() else {
                return Err(SettingsError::TypeMismatch {
                    key: key.clone(),
                    expected: "string",
                });
            };
            // **`options-from` の select は候補と照合しない。** 候補は
            // ドライバの retain トピック越しに非同期で届き、ドライバの
            // 無効化で消えもする。照合すると同じ操作がタイミングで成否を
            // 変え、「ドライバが起動するまで設定を保存できない時間帯」が
            // できてしまう。UI はドロップダウンなので、綴りを間違える経路は
            // そちらで塞がっている(設計書「保存時の検証」参照)。
            //
            // マニフェスト検証(`validate_settings`)が両方指定を弾いて
            // いるので、`options_from` が Some なら `options` は None。
            if options_from.is_none() {
                let in_options = options
                    .as_ref()
                    .is_some_and(|list| list.iter().any(|o| o.value == s));
                if !in_options {
                    return Err(SettingsError::NotAnOption {
                        key: key.clone(),
                        value: s.to_string(),
                    });
                }
            }
        }
        // `string -> string` に限る。値に number/bool/入れ子を許すと、
        // プラグイン側が受け取る形が行ごとに変わってしまう。
        SettingField::Map { key, .. } => {
            let Some(entries) = value.as_object() else {
                return Err(SettingsError::TypeMismatch {
                    key: key.clone(),
                    expected: "map",
                });
            };
            for (entry_key, entry_value) in entries {
                if entry_key.is_empty() {
                    return Err(SettingsError::EmptyMapKey { key: key.clone() });
                }
                if !entry_value.is_string() {
                    return Err(SettingsError::TypeMismatch {
                        key: key.clone(),
                        expected: "map",
                    });
                }
            }
        }
    }
    Ok(())
}

/// manifest の settings 宣言に defaults → 保存値の順で重ねた effective 値を作る。
///
/// `saved` が `None`(ファイルなし・壊れた JSON・非オブジェクト)なら defaults
/// のみを返す。宣言に無い保存 key は無視する。
pub fn effective_values(
    settings: &[SettingField],
    saved: Option<&serde_json::Map<String, serde_json::Value>>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut result = serde_json::Map::new();
    for setting in settings {
        result.insert(setting.key().to_string(), setting.default_value());
    }

    let Some(saved) = saved else {
        return result;
    };

    for setting in settings {
        if let Some(value) = saved.get(setting.key()) {
            result.insert(setting.key().to_string(), value.clone());
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boolean_field() -> SettingField {
        SettingField::Boolean {
            key: "enabled".into(),
            label: "Enabled".into(),
            default: true,
        }
    }

    fn string_field() -> SettingField {
        SettingField::String {
            key: "greeting".into(),
            label: "Greeting".into(),
            default: "hello".into(),
        }
    }

    fn number_field() -> SettingField {
        SettingField::Number {
            key: "count".into(),
            label: "Count".into(),
            default: 0.0,
        }
    }

    fn select_field() -> SettingField {
        SettingField::Select {
            key: "mode".into(),
            label: "Mode".into(),
            default: "a".into(),
            options: Some(vec![
                crate::manifest::SelectOption {
                    value: "a".into(),
                    label: "A".into(),
                },
                crate::manifest::SelectOption {
                    value: "b".into(),
                    label: "B".into(),
                },
            ]),
            options_from: None,
        }
    }

    fn select_field_from_driver() -> SettingField {
        SettingField::Select {
            key: "speaker".into(),
            label: "Speaker".into(),
            default: String::new(),
            options: None,
            options_from: Some(crate::manifest::OptionsFrom {
                driver: "coeiroink".into(),
                topic: "speakers".into(),
            }),
        }
    }

    fn map_field() -> SettingField {
        SettingField::Map {
            key: "aliases".into(),
            label: "Aliases".into(),
        }
    }

    fn secret_field() -> SettingField {
        SettingField::Secret {
            key: "api-key".into(),
            label: "API Key".into(),
        }
    }

    #[test]
    fn boolean_accepts_bool_and_rejects_others() {
        assert!(validate_value(&boolean_field(), &serde_json::json!(true)).is_ok());
        assert_eq!(
            validate_value(&boolean_field(), &serde_json::json!("yes"))
                .unwrap_err()
                .to_string(),
            "settings key enabled expected a boolean value"
        );
    }

    #[test]
    fn string_accepts_string_and_rejects_others() {
        assert!(validate_value(&string_field(), &serde_json::json!("hi")).is_ok());
        assert_eq!(
            validate_value(&string_field(), &serde_json::json!(42))
                .unwrap_err()
                .to_string(),
            "settings key greeting expected a string value"
        );
    }

    #[test]
    fn number_accepts_number_and_rejects_others() {
        assert!(validate_value(&number_field(), &serde_json::json!(3.0)).is_ok());
        assert_eq!(
            validate_value(&number_field(), &serde_json::json!("3"))
                .unwrap_err()
                .to_string(),
            "settings key count expected a number value"
        );
    }

    #[test]
    fn select_accepts_a_listed_option() {
        assert!(validate_value(&select_field(), &serde_json::json!("b")).is_ok());
    }

    #[test]
    fn select_rejects_an_unlisted_option() {
        assert_eq!(
            validate_value(&select_field(), &serde_json::json!("z"))
                .unwrap_err()
                .to_string(),
            "settings key mode value \"z\" is not one of the allowed options"
        );
    }

    #[test]
    fn select_rejects_a_non_string_value() {
        assert_eq!(
            validate_value(&select_field(), &serde_json::json!(1))
                .unwrap_err()
                .to_string(),
            "settings key mode expected a string value"
        );
    }

    #[test]
    fn select_backed_by_a_driver_topic_does_not_match_against_options() {
        assert!(validate_value(&select_field_from_driver(), &serde_json::json!("anything")).is_ok());
    }

    #[test]
    fn select_backed_by_a_driver_topic_still_requires_a_string() {
        assert_eq!(
            validate_value(&select_field_from_driver(), &serde_json::json!(1))
                .unwrap_err()
                .to_string(),
            "settings key speaker expected a string value"
        );
    }

    #[test]
    fn map_accepts_string_to_string_entries() {
        assert!(
            validate_value(&map_field(), &serde_json::json!({"a": "b"})).is_ok()
        );
    }

    #[test]
    fn map_rejects_a_non_object() {
        assert_eq!(
            validate_value(&map_field(), &serde_json::json!("not a map"))
                .unwrap_err()
                .to_string(),
            "settings key aliases expected a map value"
        );
    }

    #[test]
    fn map_rejects_an_empty_key() {
        assert_eq!(
            validate_value(&map_field(), &serde_json::json!({"": "b"}))
                .unwrap_err()
                .to_string(),
            "settings key aliases must not contain an empty entry key"
        );
    }

    #[test]
    fn map_rejects_a_non_string_value() {
        assert_eq!(
            validate_value(&map_field(), &serde_json::json!({"a": 1}))
                .unwrap_err()
                .to_string(),
            "settings key aliases expected a map value"
        );
    }

    #[test]
    fn secret_accepts_string_and_rejects_others() {
        assert!(validate_value(&secret_field(), &serde_json::json!("sk-live")).is_ok());
        assert_eq!(
            validate_value(&secret_field(), &serde_json::json!(1))
                .unwrap_err()
                .to_string(),
            "settings key api-key expected a string value"
        );
    }

    #[test]
    fn effective_values_falls_back_to_defaults_when_no_saved_map() {
        let settings = vec![boolean_field(), string_field()];
        let result = effective_values(&settings, None);
        assert_eq!(result.get("enabled"), Some(&serde_json::json!(true)));
        assert_eq!(result.get("greeting"), Some(&serde_json::json!("hello")));
    }

    #[test]
    fn effective_values_ignores_undeclared_saved_keys() {
        let settings = vec![boolean_field()];
        let mut saved = serde_json::Map::new();
        saved.insert("not-declared".into(), serde_json::json!("whatever"));
        let result = effective_values(&settings, Some(&saved));
        assert_eq!(result.get("enabled"), Some(&serde_json::json!(true)));
        assert_eq!(result.get("not-declared"), None);
    }

    #[test]
    fn effective_values_overlays_saved_over_defaults() {
        let settings = vec![boolean_field(), string_field()];
        let mut saved = serde_json::Map::new();
        saved.insert("enabled".into(), serde_json::json!(false));
        let result = effective_values(&settings, Some(&saved));
        assert_eq!(result.get("enabled"), Some(&serde_json::json!(false)));
        assert_eq!(result.get("greeting"), Some(&serde_json::json!("hello")));
    }
}

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::Path;

/// マニフェストの `[[settings]]` テーブル 1 件。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SettingField {
    Boolean {
        key: String,
        label: String,
        default: bool,
    },
    String {
        key: String,
        label: String,
        default: String,
    },
    Number {
        key: String,
        label: String,
        default: f64,
    },
    Select {
        key: String,
        label: String,
        default: String,
        options: Vec<String>,
    },
}

impl SettingField {
    pub fn key(&self) -> &str {
        match self {
            SettingField::Boolean { key, .. } => key,
            SettingField::String { key, .. } => key,
            SettingField::Number { key, .. } => key,
            SettingField::Select { key, .. } => key,
        }
    }

    pub fn default_value(&self) -> serde_json::Value {
        match self {
            SettingField::Boolean { default, .. } => serde_json::Value::Bool(*default),
            SettingField::String { default, .. } => serde_json::Value::String(default.clone()),
            SettingField::Number { default, .. } => {
                serde_json::json!(*default)
            }
            SettingField::Select { default, .. } => serde_json::Value::String(default.clone()),
        }
    }
}

/// `manifest.toml` のパース結果。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub entry: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub settings: Vec<SettingField>,
}

/// `load_manifest` が返しうるエラー。
#[derive(Debug)]
pub enum ManifestError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    /// `id` がディレクトリ名と不一致。
    IdMismatch,
    /// `id` が `[a-z0-9-]+` にマッチしない。
    BadId,
    /// `entry` が指すファイルが存在しない。
    MissingEntry,
    /// `settings` 内で `key` が重複している。
    DuplicateKey,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Io(e) => write!(f, "failed to read manifest.toml: {e}"),
            ManifestError::Parse(e) => write!(f, "failed to parse manifest.toml: {e}"),
            ManifestError::IdMismatch => {
                write!(f, "manifest id does not match plugin directory name")
            }
            ManifestError::BadId => write!(f, "manifest id must match [a-z0-9-]+"),
            ManifestError::MissingEntry => write!(f, "entry file does not exist"),
            ManifestError::DuplicateKey => write!(f, "duplicate settings key"),
        }
    }
}

impl std::error::Error for ManifestError {}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `dir/manifest.toml` を読み込み、検証して返す。
///
/// 検証エラーは `Err` として返す(panic しない)。呼び出し側は当該プラグインのみ
/// ロードスキップして warn するなど、エラーを握りつぶさずに扱うこと。
pub fn load_manifest(dir: &Path) -> Result<Manifest, ManifestError> {
    let manifest_path = dir.join("manifest.toml");
    let content = fs::read_to_string(&manifest_path).map_err(ManifestError::Io)?;
    let manifest: Manifest = toml::from_str(&content).map_err(ManifestError::Parse)?;

    if !is_valid_id(&manifest.id) {
        return Err(ManifestError::BadId);
    }

    let dir_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if manifest.id != dir_name {
        return Err(ManifestError::IdMismatch);
    }

    let entry_path = dir.join(&manifest.entry);
    if !entry_path.is_file() {
        return Err(ManifestError::MissingEntry);
    }

    let mut seen = HashSet::new();
    for setting in &manifest.settings {
        if !seen.insert(setting.key()) {
            return Err(ManifestError::DuplicateKey);
        }
    }

    Ok(manifest)
}

/// `events` フィルタが `event` にマッチするかどうか。
///
/// - `"*"` は全ての journal イベントにマッチ(status には false)
/// - `"status"` は Status イベントにのみマッチ
/// - それ以外は journal イベント名の完全一致
/// - 空リストは常に false
pub fn matches_event(events: &[String], event: &crate::event::Event) -> bool {
    match event {
        crate::event::Event::Journal { event: name, .. } => {
            events.iter().any(|e| e == "*" || e == name)
        }
        crate::event::Event::Status { .. } => events.iter().any(|e| e == "status"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use std::fs;

    fn write_manifest(dir: &Path, contents: &str) {
        fs::write(dir.join("manifest.toml"), contents).unwrap();
    }

    fn write_entry(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"\0asm").unwrap();
    }

    #[test]
    fn parses_full_manifest_with_all_setting_types() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("sample-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "sample-plugin"
name = "Sample Plugin"
version = "0.1.0"
description = "A sample plugin"
entry = "plugin.wasm"
events = ["FSDJump", "*"]

[[settings]]
key = "enabled"
label = "Enabled"
type = "boolean"
default = true

[[settings]]
key = "greeting"
label = "Greeting"
type = "string"
default = "hello"

[[settings]]
key = "count"
label = "Count"
type = "number"
default = 3.0

[[settings]]
key = "mode"
label = "Mode"
type = "select"
default = "a"
options = ["a", "b"]
"#,
        );

        let manifest = load_manifest(&plugin_dir).expect("manifest should parse");

        assert_eq!(manifest.id, "sample-plugin");
        assert_eq!(manifest.name, "Sample Plugin");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.description, "A sample plugin");
        assert_eq!(manifest.entry, "plugin.wasm");
        assert_eq!(
            manifest.events,
            vec!["FSDJump".to_string(), "*".to_string()]
        );
        assert_eq!(manifest.settings.len(), 4);

        assert_eq!(
            manifest.settings[0],
            SettingField::Boolean {
                key: "enabled".into(),
                label: "Enabled".into(),
                default: true,
            }
        );
        assert_eq!(
            manifest.settings[1],
            SettingField::String {
                key: "greeting".into(),
                label: "Greeting".into(),
                default: "hello".into(),
            }
        );
        assert_eq!(
            manifest.settings[2],
            SettingField::Number {
                key: "count".into(),
                label: "Count".into(),
                default: 3.0,
            }
        );
        assert_eq!(
            manifest.settings[3],
            SettingField::Select {
                key: "mode".into(),
                label: "Mode".into(),
                default: "a".into(),
                options: vec!["a".into(), "b".into()],
            }
        );
    }

    #[test]
    fn number_setting_accepts_bare_toml_integer_default() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("int-default-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "int-default-plugin"
name = "Int Default"
version = "0.1.0"
entry = "plugin.wasm"

[[settings]]
key = "volume"
label = "Volume"
type = "number"
default = 80
"#,
        );

        let manifest =
            load_manifest(&plugin_dir).expect("manifest with integer default should parse");

        assert_eq!(manifest.settings.len(), 1);
        assert_eq!(
            manifest.settings[0],
            SettingField::Number {
                key: "volume".into(),
                label: "Volume".into(),
                default: 80.0,
            }
        );
        assert_eq!(
            manifest.settings[0].default_value(),
            serde_json::json!(80.0)
        );
    }

    #[test]
    fn id_mismatch_with_directory_name_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("myplugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "other-plugin"
name = "Other"
version = "0.1.0"
entry = "plugin.wasm"
"#,
        );

        let err = load_manifest(&plugin_dir).expect_err("id mismatch should be rejected");
        assert!(matches!(err, ManifestError::IdMismatch));
    }

    #[test]
    fn bad_id_format_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("Bad_ID");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "Bad_ID"
name = "Bad"
version = "0.1.0"
entry = "plugin.wasm"
"#,
        );

        let err = load_manifest(&plugin_dir).expect_err("bad id format should be rejected");
        assert!(matches!(err, ManifestError::BadId));
    }

    #[test]
    fn missing_entry_file_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("no-entry-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_manifest(
            &plugin_dir,
            r#"
id = "no-entry-plugin"
name = "No Entry"
version = "0.1.0"
entry = "plugin.wasm"
"#,
        );
        // 意図的に entry ファイルは作らない

        let err = load_manifest(&plugin_dir).expect_err("missing entry should be rejected");
        assert!(matches!(err, ManifestError::MissingEntry));
    }

    #[test]
    fn duplicate_settings_key_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("dup-key-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "dup-key-plugin"
name = "Dup Key"
version = "0.1.0"
entry = "plugin.wasm"

[[settings]]
key = "foo"
label = "Foo"
type = "boolean"
default = true

[[settings]]
key = "foo"
label = "Foo Again"
type = "string"
default = "x"
"#,
        );

        let err = load_manifest(&plugin_dir).expect_err("duplicate key should be rejected");
        assert!(matches!(err, ManifestError::DuplicateKey));
    }

    #[test]
    fn toml_syntax_error_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("broken-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(&plugin_dir, "this is not valid = = toml [[[");

        let err = load_manifest(&plugin_dir).expect_err("toml syntax error should be rejected");
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    fn journal_event(name: &str) -> Event {
        Event::Journal {
            timestamp: "2026-07-25T00:00:00Z".into(),
            event: name.into(),
            raw: serde_json::json!({}),
        }
    }

    fn status_event() -> Event {
        Event::Status {
            raw: serde_json::json!({}),
        }
    }

    #[test]
    fn wildcard_matches_all_journal_events_but_not_status() {
        let events = vec!["*".to_string()];
        assert!(matches_event(&events, &journal_event("FSDJump")));
        assert!(matches_event(&events, &journal_event("Docked")));
        assert!(!matches_event(&events, &status_event()));
    }

    #[test]
    fn status_keyword_matches_only_status_events() {
        let events = vec!["status".to_string()];
        assert!(!matches_event(&events, &journal_event("FSDJump")));
        assert!(matches_event(&events, &status_event()));
    }

    #[test]
    fn exact_event_name_matches_only_that_journal_event() {
        let events = vec!["FSDJump".to_string()];
        assert!(matches_event(&events, &journal_event("FSDJump")));
        assert!(!matches_event(&events, &journal_event("Docked")));
        assert!(!matches_event(&events, &status_event()));
    }

    #[test]
    fn empty_event_list_matches_nothing() {
        let events: Vec<String> = vec![];
        assert!(!matches_event(&events, &journal_event("FSDJump")));
        assert!(!matches_event(&events, &status_event()));
    }
}

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

/// プラグインが要求する capability(実行時に許可が必要な外部リソースアクセス)。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CapabilityRequest {
    Http { hosts: Vec<String>, reason: String },
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
    #[serde(default)]
    pub capabilities: Vec<CapabilityRequest>,
}

impl Manifest {
    /// capability 要求一式の安定ハッシュ(grants の失効判定に使う)。
    ///
    /// - 同じ要求内容なら常に同じ値を返す(プロセスをまたいでも安定)。
    /// - `hosts` の順序は正規化(小文字化してソート)されるため、順序違いは無視される。
    /// - 要求内容が変われば異なる値になる。`reason` や `host` は検証済みとはいえ
    ///   自由記述のフィールドを含むため、区切り文字での結合ではなく長さ接頭辞で
    ///   エンコードして曖昧さ(衝突)を排除する(詳細は `encode_field` を参照)。
    /// - `capabilities` が空なら `None`。
    pub fn capabilities_fingerprint(&self) -> Option<String> {
        if self.capabilities.is_empty() {
            return None;
        }

        let mut canonical_requests: Vec<String> = self
            .capabilities
            .iter()
            .map(|req| match req {
                CapabilityRequest::Http { hosts, reason } => {
                    let mut normalized_hosts: Vec<String> =
                        hosts.iter().map(|h| h.to_lowercase()).collect();
                    normalized_hosts.sort();

                    let mut encoded = encode_field("http");
                    encoded.push_str(&encode_field(&normalized_hosts.len().to_string()));
                    for host in &normalized_hosts {
                        encoded.push_str(&encode_field(host));
                    }
                    encoded.push_str(&encode_field(reason));
                    encoded
                }
            })
            .collect();
        canonical_requests.sort();

        let mut canonical = encode_field(&canonical_requests.len().to_string());
        for request in &canonical_requests {
            canonical.push_str(&encode_field(request));
        }

        Some(fnv1a_hex(&canonical))
    }
}

/// 可変長文字列フィールドを長さ接頭辞方式でエンコードする: `"{byte_len}:{content}"`。
///
/// 複数の可変長フィールドを区切り文字(`;` や `|` など)で単純結合すると、
/// フィールドの中身に区切り文字そのものが含まれる場合(例えば `reason` は
/// 検証されない自由記述フィールド)に異なる入力が同じ結合結果を生みうる
/// (例: `"a;b"` と `"a" + ";" + "b"` の衝突)。長さを前置しておけば、
/// `encode_field(f1) + encode_field(f2) + ... + encode_field(fn)` は
/// `(f1, f2, ..., fn)` に対して単射になる — 先頭から「長さを読む→その
/// バイト数だけ読む」を繰り返せば一意に読み戻せるため、内容にどんな文字列が
/// 含まれていても後続フィールドとの衝突が起こらない。
fn encode_field(s: &str) -> String {
    format!("{}:{}", s.len(), s)
}

/// FNV-1a 64bit ハッシュ。`DefaultHasher` と異なり実行ごとに値が変わらないため、
/// マニフェスト間で安定比較が必要な fingerprint に使う(暗号強度は不要)。
fn fnv1a_hex(input: &str) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET_BASIS;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
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
    /// `capabilities` の内容が不正(host の形式・空リストなど)。
    BadCapability(String),
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
            ManifestError::BadCapability(msg) => write!(f, "invalid capability request: {msg}"),
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

/// capability の host エントリを検証する。
///
/// - `http://` または `https://` で始まること
/// - URL としてパース可能で、host を持つこと
/// - path・query・fragment を持たないこと(bare origin のみ)。ただし末尾の
///   `/` 一つだけの path (`https://example.com/`) は origin と等価なので許可する。
/// - userinfo(`user:pass@host` の形式)を含まないこと。人間がレビューする
///   capability 宣言に認証情報が紛れ込むのを防ぐ。
fn validate_host(host: &str) -> Result<(), String> {
    if !host.starts_with("http://") && !host.starts_with("https://") {
        return Err(format!("host must start with http:// or https://: {host}"));
    }

    let parsed =
        url::Url::parse(host).map_err(|e| format!("host is not a valid URL: {host} ({e})"))?;

    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(format!("host must have a non-empty hostname: {host}"));
    }

    if !matches!(parsed.path(), "" | "/") {
        return Err(format!("host must not contain a path: {host}"));
    }

    if parsed.query().is_some() {
        return Err(format!("host must not contain a query: {host}"));
    }

    if parsed.fragment().is_some() {
        return Err(format!("host must not contain a fragment: {host}"));
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!("host must not contain userinfo: {host}"));
    }

    Ok(())
}

fn validate_capabilities(capabilities: &[CapabilityRequest]) -> Result<(), ManifestError> {
    for capability in capabilities {
        match capability {
            CapabilityRequest::Http { hosts, reason } => {
                if hosts.is_empty() {
                    return Err(ManifestError::BadCapability(
                        "http capability requires at least one host".to_string(),
                    ));
                }
                if reason.trim().is_empty() {
                    return Err(ManifestError::BadCapability(
                        "http capability requires a non-empty reason".to_string(),
                    ));
                }
                for host in hosts {
                    validate_host(host).map_err(ManifestError::BadCapability)?;
                }
            }
        }
    }
    Ok(())
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

    validate_capabilities(&manifest.capabilities)?;

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

    #[test]
    fn capabilities_with_http_request_are_parsed() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("cap-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "cap-plugin"
name = "Cap Plugin"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://api.example.com", "https://api2.example.com"]
reason = "fetch fleet data"
"#,
        );

        let manifest = load_manifest(&plugin_dir).expect("manifest should parse");

        assert_eq!(manifest.capabilities.len(), 1);
        assert_eq!(
            manifest.capabilities[0],
            CapabilityRequest::Http {
                hosts: vec![
                    "https://api.example.com".to_string(),
                    "https://api2.example.com".to_string(),
                ],
                reason: "fetch fleet data".to_string(),
            }
        );
    }

    #[test]
    fn capabilities_default_to_empty_when_omitted() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("no-cap-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "no-cap-plugin"
name = "No Cap"
version = "0.1.0"
entry = "plugin.wasm"
"#,
        );

        let manifest = load_manifest(&plugin_dir).expect("manifest should parse");
        assert!(manifest.capabilities.is_empty());
    }

    #[test]
    fn unknown_capability_kind_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("unknown-kind-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "unknown-kind-plugin"
name = "Unknown Kind"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "filesystem"
hosts = ["https://api.example.com"]
reason = "n/a"
"#,
        );

        let err = load_manifest(&plugin_dir).expect_err("unknown capability kind should error");
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    #[test]
    fn host_without_scheme_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("no-scheme-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "no-scheme-plugin"
name = "No Scheme"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["api.example.com"]
reason = "fetch data"
"#,
        );

        let err = load_manifest(&plugin_dir).expect_err("host without scheme should be rejected");
        assert!(matches!(err, ManifestError::BadCapability(_)));
    }

    #[test]
    fn host_with_path_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("path-host-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "path-host-plugin"
name = "Path Host"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://api.example.com/v1"]
reason = "fetch data"
"#,
        );

        let err = load_manifest(&plugin_dir).expect_err("host with path should be rejected");
        assert!(matches!(err, ManifestError::BadCapability(_)));
    }

    #[test]
    fn empty_hosts_list_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("empty-hosts-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "empty-hosts-plugin"
name = "Empty Hosts"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = []
reason = "fetch data"
"#,
        );

        let err = load_manifest(&plugin_dir).expect_err("empty hosts should be rejected");
        assert!(matches!(err, ManifestError::BadCapability(_)));
    }

    #[test]
    fn empty_reason_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("empty-reason-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "empty-reason-plugin"
name = "Empty Reason"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://api.example.com"]
reason = ""
"#,
        );

        let err = load_manifest(&plugin_dir).expect_err("empty reason should be rejected");
        assert!(matches!(err, ManifestError::BadCapability(_)));
    }

    #[test]
    fn fingerprint_is_stable_order_independent_and_sensitive_to_content() {
        fn manifest_with_hosts(hosts: Vec<&str>) -> Manifest {
            Manifest {
                id: "fp-plugin".into(),
                name: "FP Plugin".into(),
                version: "0.1.0".into(),
                description: String::new(),
                entry: "plugin.wasm".into(),
                events: vec![],
                settings: vec![],
                capabilities: vec![CapabilityRequest::Http {
                    hosts: hosts.into_iter().map(String::from).collect(),
                    reason: "fetch data".into(),
                }],
            }
        }

        let a = manifest_with_hosts(vec!["https://api.example.com", "https://api2.example.com"]);
        let b = manifest_with_hosts(vec!["https://api.example.com", "https://api2.example.com"]);
        let reordered =
            manifest_with_hosts(vec!["https://api2.example.com", "https://api.example.com"]);
        let extra_host = manifest_with_hosts(vec![
            "https://api.example.com",
            "https://api2.example.com",
            "https://api3.example.com",
        ]);
        let mut no_capabilities = a.clone();
        no_capabilities.capabilities.clear();

        let fp_a = a.capabilities_fingerprint().expect("should have a value");
        let fp_b = b.capabilities_fingerprint().expect("should have a value");
        let fp_reordered = reordered
            .capabilities_fingerprint()
            .expect("should have a value");
        let fp_extra = extra_host
            .capabilities_fingerprint()
            .expect("should have a value");

        assert_eq!(
            fp_a, fp_b,
            "identical content must produce identical fingerprint"
        );
        assert_eq!(fp_a, fp_reordered, "host order must not affect fingerprint");
        assert_ne!(
            fp_a, fp_extra,
            "changing the request set must change the fingerprint"
        );
        assert_eq!(
            no_capabilities.capabilities_fingerprint(),
            None,
            "no capability requests must yield None"
        );
    }

    #[test]
    fn fingerprint_does_not_collide_when_reason_contains_delimiter_like_content() {
        // Set A: a single request whose `reason` contains text that looks like a
        // second serialized request (using the delimiters the old naive
        // implementation joined fields with: `;` between requests, `|` between
        // fields within a request).
        let set_a = Manifest {
            id: "fp-plugin".into(),
            name: "FP Plugin".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![CapabilityRequest::Http {
                hosts: vec!["https://h1.com".into()],
                reason: "foo;http|hosts=https://h2.com|reason=bar".into(),
            }],
        };

        // Set B: two separate requests that request an additional host
        // (`h2.com`) beyond what set A actually grants access to.
        let set_b = Manifest {
            id: "fp-plugin".into(),
            name: "FP Plugin".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![
                CapabilityRequest::Http {
                    hosts: vec!["https://h1.com".into()],
                    reason: "foo".into(),
                },
                CapabilityRequest::Http {
                    hosts: vec!["https://h2.com".into()],
                    reason: "bar".into(),
                },
            ],
        };

        let fp_a = set_a
            .capabilities_fingerprint()
            .expect("should have a value");
        let fp_b = set_b
            .capabilities_fingerprint()
            .expect("should have a value");

        assert_ne!(
            fp_a, fp_b,
            "a request set that grants an extra host must not share a fingerprint \
             with a single request whose free-text reason merely looks like it"
        );
    }

    #[test]
    fn host_with_userinfo_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("userinfo-host-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "userinfo-host-plugin"
name = "Userinfo Host"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://user:pw@api.example.com"]
reason = "fetch data"
"#,
        );

        let err = load_manifest(&plugin_dir).expect_err("host with userinfo should be rejected");
        assert!(matches!(err, ManifestError::BadCapability(_)));
    }

    #[test]
    fn host_with_bare_trailing_slash_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("trailing-slash-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        write_entry(&plugin_dir, "plugin.wasm");
        write_manifest(
            &plugin_dir,
            r#"
id = "trailing-slash-plugin"
name = "Trailing Slash"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://example.com/"]
reason = "fetch data"
"#,
        );

        let manifest =
            load_manifest(&plugin_dir).expect("bare trailing slash host should be accepted");
        assert_eq!(
            manifest.capabilities[0],
            CapabilityRequest::Http {
                hosts: vec!["https://example.com/".to_string()],
                reason: "fetch data".to_string(),
            }
        );
    }
}

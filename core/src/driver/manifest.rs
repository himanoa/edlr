use std::collections::HashSet;
use std::fs;
use std::path::Path;

use edlr_driver_channel::TopicSpec;

use crate::plugin::manifest::{
    is_valid_id, unknown_top_level_keys, validate_capabilities, validate_filesystem,
    validate_settings, validate_sidecars, warn_unknown_top_level_keys, CapabilityRequest,
    FilesystemRequest, ManifestError, SettingField, SidecarRequest,
};

/// `DriverManifest` が知っているトップレベルキー(serde の `rename` 後の名前)。
pub(crate) const DRIVER_MANIFEST_TOP_LEVEL_KEYS: &[&str] = &[
    "id",
    "name",
    "version",
    "description",
    "entry",
    "topics",
    "settings",
    "capabilities",
    "sidecar",
    "filesystem",
];

/// `driver.toml` のパース結果。
///
/// `crate::plugin::manifest::Manifest` と対称の形だが、ドライバは
/// プラグインと違い他のドライバとバス接続しない(`[[bus]]` は存在しない)
/// 代わりに、自身が公開する `[[topics]]` を持つ。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct DriverManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub entry: String,
    #[serde(default)]
    pub topics: Vec<TopicSpec>,
    #[serde(default)]
    pub settings: Vec<SettingField>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityRequest>,
    #[serde(default, rename = "sidecar")]
    pub sidecars: Vec<SidecarRequest>,
    #[serde(default)]
    pub filesystem: Vec<FilesystemRequest>,
}

impl DriverManifest {
    pub fn topic(&self, name: &str) -> Option<&TopicSpec> {
        self.topics.iter().find(|t| t.name == name)
    }

    /// `SettingsStore`/`GrantsStore`/`SidecarConfigStore`/`FilesystemConfigStore`
    /// はいずれも `crate::plugin::Manifest` を引数に取るよう作られている
    /// (`driver` モジュールのドキュメントコメントが言う「共有するのは
    /// grants / settings の下位ユーティリティ程度」の実体)。ドライバはこれら
    /// を書き換えずにそのまま再利用するため、それぞれのメソッドが実際に参照
    /// する値(`id`・`settings`・`capabilities`・`sidecars`・`filesystem`)だけを
    /// 詰めた `Manifest` をここで組み立てて渡す。`name`/`version`/
    /// `description`/`entry`/`events`/`bus` はこれらのストアのどのメソッドから
    /// も読まれないため、空/既定値のままでよい。
    pub(crate) fn as_settings_manifest(&self) -> crate::plugin::Manifest {
        crate::plugin::Manifest {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            description: String::new(),
            entry: self.entry.clone(),
            events: Vec::new(),
            settings: self.settings.clone(),
            capabilities: self.capabilities.clone(),
            sidecars: self.sidecars.clone(),
            filesystem: self.filesystem.clone(),
            bus: Vec::new(),
            dashboard: Vec::new(),
            schedules: Vec::new(),
        }
    }
}

/// `[[topics]]` を検証する。
///
/// - `name` はドライバ内で一意
/// - `name` は `edlr_driver_channel::topic::validate_name` を通る
fn validate_topics(topics: &[TopicSpec]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for topic in topics {
        edlr_driver_channel::topic::validate_name(&topic.name).map_err(ManifestError::BadTopic)?;
        if !seen.insert(topic.name.as_str()) {
            return Err(ManifestError::BadTopic(format!(
                "duplicate topic name: {}",
                topic.name
            )));
        }
    }
    Ok(())
}

/// `dir/driver.toml` を読み込み、検証して返す。
///
/// `crate::plugin::manifest::load_manifest` と同じ構造(id の字種検証・
/// ディレクトリ名との一致・`entry` の実在確認・`capabilities` /
/// `sidecar` / `filesystem` / `settings` の検証)に加えて `topics` を検証する。
/// 検証エラーは `Err` として返す(panic しない)。
pub fn load_driver_manifest(dir: &Path) -> Result<DriverManifest, ManifestError> {
    let manifest_path = dir.join("driver.toml");
    let content = fs::read_to_string(&manifest_path).map_err(ManifestError::Io)?;
    let mut manifest: DriverManifest = toml::from_str(&content).map_err(ManifestError::Parse)?;

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

    validate_settings(&manifest.settings)?;

    validate_capabilities(&mut manifest.capabilities)?;
    validate_sidecars(&mut manifest.sidecars)?;
    validate_filesystem(&mut manifest.filesystem)?;
    validate_topics(&manifest.topics)?;

    warn_unknown_top_level_keys(
        "driver.toml",
        &manifest.id,
        &unknown_top_level_keys(&content, DRIVER_MANIFEST_TOP_LEVEL_KEYS),
    );

    // 宣言と実際の読み取り結果の突き合わせ用(issue manifest-rjoa の提案 3)。
    tracing::info!(
        id = manifest.id.as_str(),
        topics = manifest.topics.len(),
        settings = manifest.settings.len(),
        capabilities = manifest.capabilities.len(),
        sidecars = manifest.sidecars.len(),
        filesystem = manifest.filesystem.len(),
        "driver manifest loaded"
    );

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, body: &str) {
        std::fs::write(dir.join("driver.toml"), body).unwrap();
    }

    /// `load_manifest` と同じく `entry` の実在を要求するため、テストは
    /// 対象ディレクトリに `driver.wasm` を用意する(Task 4 の教訓)。
    fn write_entry(dir: &std::path::Path) {
        std::fs::write(dir.join("driver.wasm"), b"\0asm").unwrap();
    }

    const VALID: &str = r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"

[[topics]]
name = "current-system"
retain = true
description = "現在のスターシステム"
"#;

    #[test]
    fn parses_a_valid_driver_manifest() {
        // `load_manifest` と同じくディレクトリ名と `id` の一致を要求するため、
        // (`tempfile::tempdir()` はランダムな名前を割り当てる)
        // `id` と同じ名前のサブディレクトリを用意する(brief の記載どおり
        // プラグイン側テストの `sample-plugin` パターンに合わせた)。
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("ed-state");
        std::fs::create_dir(&sub).unwrap();
        write_entry(&sub);
        write(&sub, VALID);
        let manifest = load_driver_manifest(&sub).unwrap();
        assert_eq!(manifest.id, "ed-state");
        assert!(manifest.topic("current-system").unwrap().retain);
    }

    #[test]
    fn rejects_an_id_that_differs_from_the_directory_name() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("other-name");
        std::fs::create_dir(&sub).unwrap();
        write(&sub, VALID);
        assert!(load_driver_manifest(&sub).is_err());
    }

    #[test]
    fn rejects_duplicate_topic_names() {
        // `id` をディレクトリ名と一致させ、`IdMismatch` ではなく実際に
        // `validate_topics` まで到達することを保証する(見せかけの is_err()
        // にしない -- レビュー指摘 Finding 1)。
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("ed-state");
        std::fs::create_dir(&sub).unwrap();
        write_entry(&sub);
        write(
            &sub,
            r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"

[[topics]]
name = "a"

[[topics]]
name = "a"
"#,
        );
        let err = load_driver_manifest(&sub).expect_err("duplicate topic name should be rejected");
        match err {
            ManifestError::BadTopic(msg) => {
                assert!(
                    msg.contains('a'),
                    "error message should name the offending topic: {msg}"
                );
            }
            other => panic!("expected ManifestError::BadTopic, got {other:?}"),
        }
    }

    #[test]
    fn rejects_an_invalid_topic_name() {
        // 同上(Finding 1): ディレクトリ名を `id` に一致させて topic 検証まで到達させる。
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("ed-state");
        std::fs::create_dir(&sub).unwrap();
        write_entry(&sub);
        write(
            &sub,
            r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"

[[topics]]
name = "Bad_Name"
"#,
        );
        let err = load_driver_manifest(&sub).expect_err("invalid topic name should be rejected");
        match err {
            ManifestError::BadTopic(msg) => {
                assert!(
                    msg.contains("Bad_Name"),
                    "error message should name the offending topic: {msg}"
                );
            }
            other => panic!("expected ManifestError::BadTopic, got {other:?}"),
        }
    }

    /// Issue manifest-rjoa の再現(ドライバ側)。`settings` を `[[sidecar]]` の
    /// 後ろに置くと TOML 的には `sidecar[0].settings` になる。以前はこれが
    /// 黙って捨てられ、`drivers/get-settings` が空を返していた。
    #[test]
    fn rejects_a_top_level_key_written_after_a_table_header() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("ed-state");
        std::fs::create_dir(&sub).unwrap();
        write_entry(&sub);
        write(
            &sub,
            r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"

[[sidecar]]
name = "worker"
reason = "音声合成を行う"
port = 51000

settings = [{ key = "voice", label = "Voice", type = "string", default = "a" }]
"#,
        );
        let err = load_driver_manifest(&sub)
            .expect_err("a stray key inside [[sidecar]] should be rejected");
        match err {
            ManifestError::Parse(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("settings"),
                    "error should name the offending key: {msg}"
                );
            }
            other => panic!("expected ManifestError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_keys_are_reported() {
        let unknown = crate::plugin::manifest::unknown_top_level_keys(
            r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"
topic = []
"#,
            DRIVER_MANIFEST_TOP_LEVEL_KEYS,
        );
        // `[[topics]]` の綴り違い(`topic`)は黙って消える典型。
        assert_eq!(unknown, vec!["topic".to_string()]);
    }

    #[test]
    fn a_driver_manifest_using_only_known_top_level_keys_reports_nothing() {
        let unknown = crate::plugin::manifest::unknown_top_level_keys(
            r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
description = "d"
entry = "driver.wasm"

[[topics]]
name = "current-system"

[[settings]]
key = "a"
label = "A"
type = "string"
default = ""

[[capabilities]]
kind = "http"
hosts = ["https://example.com"]
reason = "r"

[[sidecar]]
name = "worker"
reason = "r"
port = 51000

[[filesystem]]
name = "logs"
reason = "r"
mode = "read"
"#,
            DRIVER_MANIFEST_TOP_LEVEL_KEYS,
        );
        assert!(unknown.is_empty(), "unexpected unknown keys: {unknown:?}");
    }

    #[test]
    fn a_driver_with_no_topics_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("ed-state");
        std::fs::create_dir(&sub).unwrap();
        write_entry(&sub);
        write(
            &sub,
            r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"
"#,
        );
        assert!(load_driver_manifest(&sub).is_ok());
    }
}

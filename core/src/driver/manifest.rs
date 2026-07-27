use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::Path;

use edlr_driver_channel::TopicSpec;

use crate::plugin::manifest::{
    is_valid_id, validate_capabilities, validate_filesystem, validate_sidecars, CapabilityRequest,
    FilesystemRequest, ManifestError, SettingField, SidecarRequest,
};

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
}

/// `[[topics]]` を検証する。
///
/// - `name` はドライバ内で一意
/// - `name` は `edlr_driver_channel::topic::validate_name` を通る
fn validate_topics(topics: &[TopicSpec]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for topic in topics {
        edlr_driver_channel::topic::validate_name(&topic.name).map_err(ManifestError::BadBus)?;
        if !seen.insert(topic.name.as_str()) {
            return Err(ManifestError::BadBus(format!(
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

    let mut seen = HashSet::new();
    for setting in &manifest.settings {
        if !seen.insert(setting.key()) {
            return Err(ManifestError::DuplicateKey);
        }
    }

    validate_capabilities(&mut manifest.capabilities)?;
    validate_sidecars(&mut manifest.sidecars)?;
    validate_filesystem(&mut manifest.filesystem)?;
    validate_topics(&manifest.topics)?;

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
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
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
        assert!(load_driver_manifest(dir.path()).is_err());
    }

    #[test]
    fn rejects_an_invalid_topic_name() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            r#"
id = "ed-state"
name = "ED State"
version = "0.1.0"
entry = "driver.wasm"

[[topics]]
name = "Bad_Name"
"#,
        );
        assert!(load_driver_manifest(dir.path()).is_err());
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

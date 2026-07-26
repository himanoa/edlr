//! ファイルアクセス(`[[filesystem]]`)のユーザー設定(実ディレクトリのパス)の
//! 永続化と検証。
//!
//! 保存先は `<settings-dir>/<plugin-id>.filesystem.json`。通常の
//! `[[settings]]` とは別ファイルにしている: `SettingsStore::update` は
//! manifest の `[[settings]]` に無いキーをディスクから間引く実装なので、
//! 同じファイルに同居させると設定保存のたびにファイルアクセス設定が消えて
//! しまう(`[[sidecar]]` と同じ理由)。

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::plugin::Manifest;

/// ファイルアクセス 1 件のユーザー設定。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FilesystemConfig {
    /// 実ディレクトリの絶対パス。空文字は「未設定」(承認できない)。
    #[serde(default)]
    pub path: String,
}

#[derive(Debug)]
pub enum FilesystemConfigError {
    /// manifest にない `name` を指定した。
    UnknownRoot(String),
    /// 絶対パスでない。
    NotAbsolute(String),
    /// 実在しない、またはディレクトリでない。
    NotADirectory(String),
    /// システム上重要なディレクトリ「そのもの」を指定した。
    ProtectedDirectory(String),
    Io(std::io::Error),
    Serialize(serde_json::Error),
}

impl fmt::Display for FilesystemConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilesystemConfigError::UnknownRoot(name) => write!(f, "unknown filesystem root: {name}"),
            FilesystemConfigError::NotAbsolute(name) => {
                write!(f, "filesystem root {name} must be an absolute path")
            }
            FilesystemConfigError::NotADirectory(name) => {
                write!(f, "filesystem root {name} must be an existing directory")
            }
            FilesystemConfigError::ProtectedDirectory(name) => {
                write!(f, "filesystem root {name} may not point at a protected directory")
            }
            FilesystemConfigError::Io(e) => write!(f, "failed to write filesystem config: {e}"),
            FilesystemConfigError::Serialize(e) => {
                write!(f, "failed to serialize filesystem config: {e}")
            }
        }
    }
}

impl std::error::Error for FilesystemConfigError {}

/// `<settings-dir>/<plugin-id>.filesystem.json` を読み書きするストア。
/// `SidecarConfigStore` と同じく内部 `Mutex<()>` で read-merge-write を
/// 直列化する。
pub struct FilesystemConfigStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl FilesystemConfigStore {
    pub fn new(dir: PathBuf) -> Self {
        FilesystemConfigStore {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn path_for(&self, manifest: &Manifest) -> PathBuf {
        self.dir.join(format!("{}.filesystem.json", manifest.id))
    }

    /// manifest の既定値(空パス)に保存済みの値をマージした設定一覧を返す。
    /// ファイルが無い・壊れている場合は既定値のみ(panic しない)。
    pub fn effective(&self, manifest: &Manifest) -> BTreeMap<String, FilesystemConfig> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.effective_locked(manifest)
    }

    fn effective_locked(&self, manifest: &Manifest) -> BTreeMap<String, FilesystemConfig> {
        let saved: BTreeMap<String, FilesystemConfig> = fs::read_to_string(self.path_for(manifest))
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default();

        manifest
            .filesystem
            .iter()
            .map(|request| {
                let config = saved
                    .get(&request.name)
                    .cloned()
                    .unwrap_or_else(|| FilesystemConfig { path: String::new() });
                (request.name.clone(), config)
            })
            .collect()
    }

    /// 1 ファイルアクセス設定を検証して保存し、更新後の全設定を返す。
    /// 検証に失敗した場合は何も書き込まない。
    pub fn update_and_effective(
        &self,
        manifest: &Manifest,
        name: &str,
        config: &FilesystemConfig,
    ) -> Result<BTreeMap<String, FilesystemConfig>, FilesystemConfigError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        manifest
            .filesystem_root(name)
            .ok_or_else(|| FilesystemConfigError::UnknownRoot(name.to_string()))?;

        validate_path(name, &config.path)?;

        let mut merged = self.effective_locked(manifest);
        merged.insert(name.to_string(), config.clone());

        fs::create_dir_all(&self.dir).map_err(FilesystemConfigError::Io)?;
        let serialized =
            serde_json::to_string_pretty(&merged).map_err(FilesystemConfigError::Serialize)?;
        let target = self.path_for(manifest);
        let tmp_path = self.dir.join(format!(
            "{}.filesystem.json.tmp.{}",
            manifest.id,
            std::process::id()
        ));
        fs::write(&tmp_path, serialized).map_err(FilesystemConfigError::Io)?;
        fs::rename(&tmp_path, &target).map_err(FilesystemConfigError::Io)?;

        Ok(merged)
    }
}

/// ユーザーが選んだディレクトリを検証する。
///
/// システム上重要なディレクトリ「そのもの」は拒否する。承認画面での確認だけに
/// 頼らず、明らかな事故を 1 段止めるため(配下の任意のディレクトリは許可する
/// -- `/home/alice/Documents` は通り、`/home` は通らない)。
fn validate_path(name: &str, path: &str) -> Result<(), FilesystemConfigError> {
    if path.is_empty() {
        return Ok(()); // 未設定は許す(承認できないだけ)
    }

    let candidate = std::path::Path::new(path);
    if !candidate.is_absolute() {
        return Err(FilesystemConfigError::NotAbsolute(name.to_string()));
    }

    let canonical = candidate
        .canonicalize()
        .map_err(|_| FilesystemConfigError::NotADirectory(name.to_string()))?;
    if !canonical.is_dir() {
        return Err(FilesystemConfigError::NotADirectory(name.to_string()));
    }

    if is_protected(&canonical) {
        return Err(FilesystemConfigError::ProtectedDirectory(name.to_string()));
    }
    Ok(())
}

/// 「そのものは選ばせない」ディレクトリ。配下は許可する。
fn is_protected(canonical: &std::path::Path) -> bool {
    const PROTECTED: &[&str] = &[
        "/", "/home", "/etc", "/usr", "/var", "/boot", "/dev", "/proc", "/sys", "/root", "/bin",
        "/sbin", "/lib",
    ];
    if PROTECTED.iter().any(|p| canonical == std::path::Path::new(p)) {
        return true;
    }
    // ユーザーのホームディレクトリ「そのもの」も拒否する。`canonicalize()` 済み
    // の `candidate` と比較する前に `$HOME` 自体も canonicalize する
    // (シンボリックリンクや末尾スラッシュの有無で不一致にならないように)。
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            if let Ok(home_canonical) = std::path::Path::new(&home).canonicalize() {
                if canonical == home_canonical {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::{FilesystemMode, FilesystemRequest};

    fn manifest_with(requests: Vec<FilesystemRequest>) -> Manifest {
        Manifest {
            id: "fs-plugin".into(),
            name: "FS".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: requests,
        }
    }

    fn request(name: &str) -> FilesystemRequest {
        FilesystemRequest {
            name: name.into(),
            reason: "reason".into(),
            mode: FilesystemMode::ReadWrite,
        }
    }

    #[test]
    fn effective_defaults_to_an_empty_path() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("exports")]);

        assert_eq!(store.effective(&manifest)["exports"].path, "");
    }

    #[test]
    fn update_persists_and_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        let target = tmp.path().join("exports");
        std::fs::create_dir(&target).unwrap();
        let store = FilesystemConfigStore::new(dir.clone());
        let manifest = manifest_with(vec![request("exports")]);

        let updated = store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig {
                    path: target.to_string_lossy().to_string(),
                },
            )
            .expect("valid directory should be accepted");
        assert_eq!(updated["exports"].path, target.to_string_lossy());
        assert!(dir.join("fs-plugin.filesystem.json").is_file());

        let reread = FilesystemConfigStore::new(dir).effective(&manifest);
        assert_eq!(reread["exports"].path, target.to_string_lossy());
    }

    #[test]
    fn relative_paths_missing_paths_and_files_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("exports")]);
        let file = tmp.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();

        for (path, expect_not_a_dir) in [
            ("relative/dir".to_string(), false),
            (tmp.path().join("nope").to_string_lossy().to_string(), true),
            (file.to_string_lossy().to_string(), true),
        ] {
            let err = store
                .update_and_effective(&manifest, "exports", &FilesystemConfig { path })
                .expect_err("invalid directory must be rejected");
            if expect_not_a_dir {
                assert!(matches!(err, FilesystemConfigError::NotADirectory(_)));
            } else {
                assert!(matches!(err, FilesystemConfigError::NotAbsolute(_)));
            }
        }
    }

    #[test]
    fn protected_directories_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("exports")]);

        for path in ["/", "/etc", "/home", "/usr", "/var"] {
            let err = store
                .update_and_effective(
                    &manifest,
                    "exports",
                    &FilesystemConfig { path: path.to_string() },
                )
                .expect_err("a protected directory must be rejected");
            assert!(
                matches!(err, FilesystemConfigError::ProtectedDirectory(_)),
                "{path} should be protected"
            );
        }
    }

    #[test]
    fn a_rejected_update_leaves_the_stored_value_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        let target = tmp.path().join("exports");
        std::fs::create_dir(&target).unwrap();
        let store = FilesystemConfigStore::new(dir);
        let manifest = manifest_with(vec![request("exports")]);
        store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig { path: target.to_string_lossy().to_string() },
            )
            .unwrap();

        let _ = store.update_and_effective(
            &manifest,
            "exports",
            &FilesystemConfig { path: "/etc".to_string() },
        );

        assert_eq!(store.effective(&manifest)["exports"].path, target.to_string_lossy());
    }

    #[test]
    fn unknown_root_is_rejected_and_broken_json_falls_back_to_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fs-plugin.filesystem.json"), "not json {{{").unwrap();
        let store = FilesystemConfigStore::new(dir);
        let manifest = manifest_with(vec![request("exports")]);

        assert_eq!(store.effective(&manifest)["exports"].path, "");
        assert!(matches!(
            store
                .update_and_effective(&manifest, "nope", &FilesystemConfig { path: "/tmp".into() })
                .expect_err("unknown root"),
            FilesystemConfigError::UnknownRoot(_)
        ));
    }

    #[test]
    fn trailing_slash_on_a_protected_directory_is_still_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("exports")]);

        let err = store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig { path: "/etc/".to_string() },
            )
            .expect_err("/etc/ must be rejected just like /etc");
        assert!(matches!(err, FilesystemConfigError::ProtectedDirectory(_)));
    }

    #[test]
    fn dot_dot_traversal_into_a_protected_directory_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("exports")]);

        let err = store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig { path: "/etc/../etc".to_string() },
            )
            .expect_err("/etc/../etc must canonicalize to /etc and be rejected");
        assert!(matches!(err, FilesystemConfigError::ProtectedDirectory(_)));
    }

    #[test]
    fn a_symlink_that_resolves_to_a_protected_directory_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("exports")]);
        let link = tmp.path().join("etc-link");
        std::os::unix::fs::symlink("/etc", &link).unwrap();

        let err = store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig { path: link.to_string_lossy().to_string() },
            )
            .expect_err("a symlink resolving to /etc must be rejected");
        assert!(matches!(err, FilesystemConfigError::ProtectedDirectory(_)));
    }
}

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::plugin::Manifest;

/// プラグインの capability 承認状態。
#[derive(Debug, Clone, PartialEq)]
pub struct GrantState {
    /// 現在の manifest の capability 要求に対して承認済みかどうか。
    pub granted: bool,
    /// 過去に承認されたが、manifest の capability 要求(fingerprint)が
    /// 変わったため失効しているかどうか。`granted` が `true` の間は常に `false`。
    pub stale: bool,
}

/// プラグインごとの capability 承認を `<grants-dir>/<id>.json` に保存するストア。
///
/// 内部に `Mutex<()>` を持ち、`set` は常にこのロックを保持した状態でファイルを
/// 読み書きする(`SettingsStore` と同じ流儀)。これにより複数スレッドから同じ
/// マニフェストに対して同時に呼び出されても、書き込み途中のファイルを読んで
/// しまう競合が起きない。
pub struct GrantsStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

/// `GrantsStore::set` が返しうるエラー。
#[derive(Debug)]
pub enum GrantsError {
    /// ディレクトリ作成やファイル書き込みに失敗した。
    Io(std::io::Error),
    /// 保存直前の JSON シリアライズに失敗した。
    Serialize(serde_json::Error),
}

impl fmt::Display for GrantsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GrantsError::Io(e) => write!(f, "failed to write grants: {e}"),
            GrantsError::Serialize(e) => write!(f, "failed to serialize grants: {e}"),
        }
    }
}

impl std::error::Error for GrantsError {}

/// ディスクに保存された grant のパース結果。
#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct SavedGrant {
    granted: bool,
    fingerprint: String,
}

impl GrantsStore {
    pub fn new(dir: PathBuf) -> Self {
        GrantsStore {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn path_for(&self, manifest: &Manifest) -> PathBuf {
        self.dir.join(format!("{}.json", manifest.id))
    }

    /// ディスクから保存済みの grant を読む。ファイルが無い・壊れている・
    /// トップレベルの形が期待と違う場合はいずれも `None`(未保存扱い、panic しない)。
    fn read_saved(&self, manifest: &Manifest) -> Option<SavedGrant> {
        let path = self.path_for(manifest);
        let content = fs::read_to_string(&path).ok()?;
        serde_json::from_str::<SavedGrant>(&content).ok()
    }

    /// 保存された承認を読み、manifest の現在の fingerprint と照合する。
    ///
    /// - 要求のないマニフェスト(fingerprint が `None`)は常に
    ///   `{ granted: false, stale: false }`。
    /// - 未保存 → `{ granted: false, stale: false }`。
    /// - 保存済みだが fingerprint 不一致 → `{ granted: false, stale: true }`。
    /// - 保存済みで fingerprint 一致 → 保存された `granted` の値をそのまま返す
    ///   (取消保存は `granted: false, stale: false`)。
    pub fn state(&self, manifest: &Manifest) -> GrantState {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.state_locked(manifest)
    }

    /// `state()` の本体。呼び出し元が既に `self.lock` を保持していることを
    /// 前提とする(二重ロックしない内部ヘルパー)。
    fn state_locked(&self, manifest: &Manifest) -> GrantState {
        let Some(current_fingerprint) = manifest.capabilities_fingerprint() else {
            return GrantState {
                granted: false,
                stale: false,
            };
        };

        let Some(saved) = self.read_saved(manifest) else {
            return GrantState {
                granted: false,
                stale: false,
            };
        };

        if saved.fingerprint != current_fingerprint {
            return GrantState {
                granted: false,
                stale: true,
            };
        }

        GrantState {
            granted: saved.granted,
            stale: false,
        }
    }

    /// 承認/取消を保存する。`granted=true` のとき現在の fingerprint を一緒に
    /// 保存する。要求のないマニフェスト(fingerprint が `None`)に対しては何も
    /// 書き込まず、常に `{ granted: false, stale: false }` を返す。
    pub fn set(&self, manifest: &Manifest, granted: bool) -> Result<GrantState, GrantsError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let Some(current_fingerprint) = manifest.capabilities_fingerprint() else {
            return Ok(GrantState {
                granted: false,
                stale: false,
            });
        };

        let saved = SavedGrant {
            granted,
            fingerprint: current_fingerprint,
        };

        fs::create_dir_all(&self.dir).map_err(GrantsError::Io)?;
        let serialized = serde_json::to_string_pretty(&saved).map_err(GrantsError::Serialize)?;
        let target = self.path_for(manifest);
        let tmp_path = self
            .dir
            .join(format!("{}.json.tmp.{}", manifest.id, std::process::id()));
        fs::write(&tmp_path, serialized).map_err(GrantsError::Io)?;
        fs::rename(&tmp_path, &target).map_err(GrantsError::Io)?;

        Ok(self.state_locked(manifest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::CapabilityRequest;

    fn manifest_with_hosts(hosts: Vec<&str>) -> Manifest {
        Manifest {
            id: "cap-plugin".into(),
            name: "Cap Plugin".into(),
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

    fn manifest_without_capabilities() -> Manifest {
        Manifest {
            id: "no-cap-plugin".into(),
            name: "No Cap Plugin".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
        }
    }

    #[test]
    fn unsaved_state_is_not_granted_and_not_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_hosts(vec!["https://api.example.com"]);

        let state = store.state(&manifest);

        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: false
            }
        );
    }

    #[test]
    fn set_true_persists_grant_and_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("grants");
        let store = GrantsStore::new(dir.clone());
        let manifest = manifest_with_hosts(vec!["https://api.example.com"]);

        let state = store.set(&manifest, true).expect("set should succeed");
        assert_eq!(
            state,
            GrantState {
                granted: true,
                stale: false
            }
        );

        assert!(dir.join("cap-plugin.json").is_file());

        let state = store.state(&manifest);
        assert_eq!(
            state,
            GrantState {
                granted: true,
                stale: false
            }
        );
    }

    #[test]
    fn changed_fingerprint_makes_saved_grant_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let original = manifest_with_hosts(vec!["https://api.example.com"]);
        store.set(&original, true).expect("set should succeed");

        let mut changed = original.clone();
        changed.capabilities = vec![CapabilityRequest::Http {
            hosts: vec![
                "https://api.example.com".to_string(),
                "https://api2.example.com".to_string(),
            ],
            reason: "fetch data".into(),
        }];

        let state = store.state(&changed);
        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: true
            }
        );
    }

    #[test]
    fn set_false_revokes_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_hosts(vec!["https://api.example.com"]);

        store.set(&manifest, true).expect("set should succeed");
        let state = store.set(&manifest, false).expect("revoke should succeed");
        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: false
            }
        );

        let state = store.state(&manifest);
        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: false
            }
        );
    }

    #[test]
    fn old_format_fingerprint_on_disk_is_treated_as_stale_not_valid() {
        // A grant file written by the retired FNV-1a-64 fingerprint format
        // (16 hex chars) must not be mistaken for a valid grant under the
        // current SHA-256 format (64 hex chars): it must simply mismatch and
        // report stale, i.e. fail closed rather than silently re-validating.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("grants");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("cap-plugin.json"),
            r#"{"granted":true,"fingerprint":"0123456789abcdef"}"#,
        )
        .unwrap();

        let store = GrantsStore::new(dir);
        let manifest = manifest_with_hosts(vec!["https://api.example.com"]);

        let state = store.state(&manifest);
        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: true
            },
            "an old-format fingerprint must never coincide with the current one"
        );
    }

    #[test]
    fn revoke_then_manifest_change_does_not_resurrect_as_granted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let original = manifest_with_hosts(vec!["https://api.example.com"]);

        store.set(&original, true).expect("grant should succeed");
        store.set(&original, false).expect("revoke should succeed");

        let mut changed = original.clone();
        changed.capabilities = vec![CapabilityRequest::Http {
            hosts: vec![
                "https://api.example.com".to_string(),
                "https://api2.example.com".to_string(),
            ],
            reason: "fetch data".into(),
        }];

        let state = store.state(&changed);
        assert!(
            !state.granted,
            "a revoked grant followed by a manifest change must never resurrect as granted"
        );
    }

    #[test]
    fn broken_json_is_treated_as_unsaved() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("grants");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cap-plugin.json"), "not valid json {{{").unwrap();

        let store = GrantsStore::new(dir);
        let manifest = manifest_with_hosts(vec!["https://api.example.com"]);

        let state = store.state(&manifest);
        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: false
            }
        );
    }

    #[test]
    fn manifest_without_capability_requests_is_always_ungranted_and_set_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("grants");
        let store = GrantsStore::new(dir.clone());
        let manifest = manifest_without_capabilities();

        let state = store.state(&manifest);
        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: false
            }
        );

        let state = store
            .set(&manifest, true)
            .expect("set on no-capability manifest should succeed as a no-op");
        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: false
            }
        );

        assert!(!dir.join("no-cap-plugin.json").exists());
    }

    #[test]
    fn set_creates_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("grants");
        assert!(!dir.exists());

        let store = GrantsStore::new(dir.clone());
        let manifest = manifest_with_hosts(vec!["https://api.example.com"]);

        store
            .set(&manifest, true)
            .expect("set should create dir and succeed");

        assert!(dir.is_dir());
        assert!(dir.join("cap-plugin.json").is_file());
    }
}

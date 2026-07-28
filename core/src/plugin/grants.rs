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
#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct SavedGrant {
    #[serde(default)]
    granted: bool,
    #[serde(default)]
    fingerprint: String,
    /// サイドカー名 → その 1 件の承認状態。既存(サイドカー導入前)の
    /// grant ファイルにはこのキーが無いため `default` で空マップになる。
    #[serde(default)]
    sidecars: std::collections::BTreeMap<String, SavedSidecarGrant>,
    /// ファイルアクセスのルート名 → その 1 件の承認状態。既存(ファイル
    /// アクセス導入前)の grant ファイルにはこのキーが無いため `default` で
    /// 空マップになる。
    #[serde(default)]
    filesystem: std::collections::BTreeMap<String, SavedSidecarGrant>,
    /// バス接続先のドライバ ID → その 1 件の承認状態。既存(バス導入前)の
    /// grant ファイルにはこのキーが無いため `default` で空マップになる。
    #[serde(default)]
    bus: std::collections::BTreeMap<String, SavedSidecarGrant>,
    /// ダッシュボードウィジェット ID → その 1 件の承認状態。既存
    /// (ダッシュボード導入前)の grant ファイルにはこのキーが無いため
    /// `default` で空マップになる。
    #[serde(default)]
    dashboard: std::collections::BTreeMap<String, SavedSidecarGrant>,
}

/// サイドカー 1 件分の保存済み承認状態。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct SavedSidecarGrant {
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

        let mut saved = self.read_saved(manifest).unwrap_or_default();
        saved.granted = granted;
        saved.fingerprint = current_fingerprint;
        self.write_saved(manifest, &saved)?;

        Ok(self.state_locked(manifest))
    }

    /// `SavedGrant` 全体を原子的に書き込む(呼び出し元が `self.lock` 保持済み)。
    fn write_saved(&self, manifest: &Manifest, saved: &SavedGrant) -> Result<(), GrantsError> {
        fs::create_dir_all(&self.dir).map_err(GrantsError::Io)?;
        let serialized = serde_json::to_string_pretty(saved).map_err(GrantsError::Serialize)?;
        let target = self.path_for(manifest);
        let tmp_path = self
            .dir
            .join(format!("{}.json.tmp.{}", manifest.id, std::process::id()));
        fs::write(&tmp_path, serialized).map_err(GrantsError::Io)?;
        fs::rename(&tmp_path, &target).map_err(GrantsError::Io)
    }

    /// サイドカー 1 件の承認状態。判定規則は `state()` と同じ
    /// (未保存 → 未承認 / fingerprint 不一致 → stale / 一致 → 保存値)。
    pub fn sidecar_state(&self, manifest: &Manifest, name: &str) -> GrantState {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.sidecar_state_locked(manifest, name)
    }

    fn sidecar_state_locked(&self, manifest: &Manifest, name: &str) -> GrantState {
        let Some(current) = manifest.sidecar_fingerprint(name) else {
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
        let Some(entry) = saved.sidecars.get(name) else {
            return GrantState {
                granted: false,
                stale: false,
            };
        };
        if entry.fingerprint != current {
            return GrantState {
                granted: false,
                stale: true,
            };
        }
        GrantState {
            granted: entry.granted,
            stale: false,
        }
    }

    /// サイドカー 1 件の承認/取消を保存する。manifest にない `name` は no-op。
    pub fn set_sidecar(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let Some(current) = manifest.sidecar_fingerprint(name) else {
            return Ok(GrantState {
                granted: false,
                stale: false,
            });
        };

        let mut saved = self.read_saved(manifest).unwrap_or_default();
        saved.sidecars.insert(
            name.to_string(),
            SavedSidecarGrant {
                granted,
                fingerprint: current,
            },
        );
        self.write_saved(manifest, &saved)?;

        Ok(self.sidecar_state_locked(manifest, name))
    }

    /// ファイルアクセスのルート 1 件の承認状態。判定規則は `state()` と同じ
    /// (未保存 → 未承認 / fingerprint 不一致 → stale / 一致 → 保存値)。
    pub fn filesystem_state(&self, manifest: &Manifest, name: &str) -> GrantState {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.filesystem_state_locked(manifest, name)
    }

    fn filesystem_state_locked(&self, manifest: &Manifest, name: &str) -> GrantState {
        let Some(current) = manifest.filesystem_fingerprint(name) else {
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
        let Some(entry) = saved.filesystem.get(name) else {
            return GrantState {
                granted: false,
                stale: false,
            };
        };
        if entry.fingerprint != current {
            return GrantState {
                granted: false,
                stale: true,
            };
        }
        GrantState {
            granted: entry.granted,
            stale: false,
        }
    }

    /// ファイルアクセスのルート 1 件の承認/取消を保存する。manifest にない
    /// `name` は no-op。
    pub fn set_filesystem(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let Some(current) = manifest.filesystem_fingerprint(name) else {
            return Ok(GrantState {
                granted: false,
                stale: false,
            });
        };

        let mut saved = self.read_saved(manifest).unwrap_or_default();
        saved.filesystem.insert(
            name.to_string(),
            SavedSidecarGrant {
                granted,
                fingerprint: current,
            },
        );
        self.write_saved(manifest, &saved)?;

        Ok(self.filesystem_state_locked(manifest, name))
    }

    /// バス接続先のドライバ 1 件の承認状態。判定規則は `state()` と同じ
    /// (未保存 → 未承認 / fingerprint 不一致 → stale / 一致 → 保存値)。
    pub fn bus_state(&self, manifest: &Manifest, driver: &str) -> GrantState {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.bus_state_locked(manifest, driver)
    }

    fn bus_state_locked(&self, manifest: &Manifest, driver: &str) -> GrantState {
        let Some(current) = manifest.bus_fingerprint(driver) else {
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
        let Some(entry) = saved.bus.get(driver) else {
            return GrantState {
                granted: false,
                stale: false,
            };
        };
        if entry.fingerprint != current {
            return GrantState {
                granted: false,
                stale: true,
            };
        }
        GrantState {
            granted: entry.granted,
            stale: false,
        }
    }

    /// バス接続先のドライバ 1 件の承認/取消を保存する。manifest にない
    /// `driver` は no-op。
    pub fn set_bus(
        &self,
        manifest: &Manifest,
        driver: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let Some(current) = manifest.bus_fingerprint(driver) else {
            return Ok(GrantState {
                granted: false,
                stale: false,
            });
        };

        let mut saved = self.read_saved(manifest).unwrap_or_default();
        saved.bus.insert(
            driver.to_string(),
            SavedSidecarGrant {
                granted,
                fingerprint: current,
            },
        );
        self.write_saved(manifest, &saved)?;

        Ok(self.bus_state_locked(manifest, driver))
    }

    /// ダッシュボードウィジェット 1 件の承認状態。判定規則は `state()` と
    /// 同じ(未保存 → 未承認 / fingerprint 不一致 → stale / 一致 → 保存値)。
    pub fn dashboard_state(&self, manifest: &Manifest, widget: &str) -> GrantState {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.dashboard_state_locked(manifest, widget)
    }

    fn dashboard_state_locked(&self, manifest: &Manifest, widget: &str) -> GrantState {
        let Some(current) = manifest.dashboard_fingerprint(widget) else {
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
        let Some(entry) = saved.dashboard.get(widget) else {
            return GrantState {
                granted: false,
                stale: false,
            };
        };
        if entry.fingerprint != current {
            return GrantState {
                granted: false,
                stale: true,
            };
        }
        GrantState {
            granted: entry.granted,
            stale: false,
        }
    }

    /// ダッシュボードウィジェット 1 件の承認/取消を保存する。manifest に
    /// ない `widget` は no-op。
    pub fn set_dashboard(
        &self,
        manifest: &Manifest,
        widget: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let Some(current) = manifest.dashboard_fingerprint(widget) else {
            return Ok(GrantState {
                granted: false,
                stale: false,
            });
        };

        let mut saved = self.read_saved(manifest).unwrap_or_default();
        saved.dashboard.insert(
            widget.to_string(),
            SavedSidecarGrant {
                granted,
                fingerprint: current,
            },
        );
        self.write_saved(manifest, &saved)?;

        Ok(self.dashboard_state_locked(manifest, widget))
    }

    /// ドライバ用の grants ストア。ID 空間がプラグインと別なので、
    /// 保存先も `<grants-dir>/drivers/` に分ける。
    pub fn new_for_drivers(dir: PathBuf) -> GrantsStore {
        GrantsStore::new(dir.join("drivers"))
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
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
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
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
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

    fn manifest_with_sidecar(port: u16) -> Manifest {
        Manifest {
            id: "sc-plugin".into(),
            name: "SC".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![crate::plugin::manifest::SidecarRequest {
                name: "tts".into(),
                reason: "reason".into(),
                args: vec!["--port".into(), "{port}".into()],
                port,
                scalable: true,
            }],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
        }
    }

    #[test]
    fn sidecar_grant_defaults_to_ungranted_and_persists_per_name() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_sidecar(50021);

        assert_eq!(
            store.sidecar_state(&manifest, "tts"),
            GrantState {
                granted: false,
                stale: false
            }
        );

        let state = store
            .set_sidecar(&manifest, "tts", true)
            .expect("grant should succeed");
        assert_eq!(
            state,
            GrantState {
                granted: true,
                stale: false
            }
        );
        assert_eq!(
            store.sidecar_state(&manifest, "tts"),
            GrantState {
                granted: true,
                stale: false
            }
        );
    }

    #[test]
    fn changed_sidecar_request_makes_the_grant_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_sidecar(50021);
        store.set_sidecar(&manifest, "tts", true).unwrap();

        let changed = manifest_with_sidecar(50099);
        assert_eq!(
            store.sidecar_state(&changed, "tts"),
            GrantState {
                granted: false,
                stale: true
            }
        );
    }

    #[test]
    fn sidecar_grant_and_http_grant_are_independent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let mut manifest = manifest_with_sidecar(50021);
        manifest.capabilities = vec![CapabilityRequest::Http {
            hosts: vec!["https://api.example.com".into()],
            reason: "fetch".into(),
        }];

        store.set_sidecar(&manifest, "tts", true).unwrap();
        assert!(store.sidecar_state(&manifest, "tts").granted);
        assert!(
            !store.state(&manifest).granted,
            "granting a sidecar must not grant the http capability"
        );

        store.set(&manifest, true).unwrap();
        assert!(store.state(&manifest).granted);
        assert!(
            store.sidecar_state(&manifest, "tts").granted,
            "granting http must not clobber the sidecar grant"
        );
    }

    #[test]
    fn unknown_sidecar_name_is_never_granted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_sidecar(50021);

        assert_eq!(
            store.sidecar_state(&manifest, "nope"),
            GrantState {
                granted: false,
                stale: false
            }
        );
        let state = store
            .set_sidecar(&manifest, "nope", true)
            .expect("set on an unknown sidecar is a no-op, not an error");
        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: false
            }
        );
    }

    /// Minor(最終レビュー指摘): 壊れた grants JSON がディスクにある状態で
    /// `sidecar_state`/`set_sidecar` を呼んでも fail-closed(未承認扱い)の
    /// まま動くことを固定する回帰テスト。`state`/`set` 側には既に
    /// `broken_json_is_treated_as_unsaved` があるが、サイドカー単位の
    /// `sidecar_state`/`set_sidecar` には同等のテストが無かった。
    #[test]
    fn broken_grants_json_is_treated_as_unsaved_for_sidecar_state_and_set_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("grants");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("sc-plugin.json"), "not valid json {{{").unwrap();

        let store = GrantsStore::new(dir);
        let manifest = manifest_with_sidecar(50021);

        // 壊れたファイルは「未承認」扱い(stale でもない -- fail-closed だが
        // 「保存されているが不一致」とは違う無害な扱い)。
        assert_eq!(
            store.sidecar_state(&manifest, "tts"),
            GrantState {
                granted: false,
                stale: false
            },
            "broken grants JSON must fail closed as ungranted, not panic or resurrect a grant"
        );

        // `set_sidecar` は壊れたファイルの上に安全に新しい状態を書ける
        // (`read_saved` が `None` を返すので `unwrap_or_default` から出発する)。
        let state = store
            .set_sidecar(&manifest, "tts", true)
            .expect("set_sidecar must recover from a broken grants file, not fail or panic");
        assert_eq!(
            state,
            GrantState {
                granted: true,
                stale: false
            }
        );
        assert_eq!(
            store.sidecar_state(&manifest, "tts"),
            GrantState {
                granted: true,
                stale: false
            }
        );
    }

    fn manifest_with_fs(mode: crate::plugin::manifest::FilesystemMode) -> Manifest {
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
            filesystem: vec![crate::plugin::manifest::FilesystemRequest {
                name: "exports".into(),
                reason: "reason".into(),
                mode,
            }],
            bus: vec![],
            dashboard: vec![],
        }
    }

    #[test]
    fn filesystem_grant_defaults_to_ungranted_and_persists() {
        use crate::plugin::manifest::FilesystemMode;
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_fs(FilesystemMode::ReadWrite);

        assert_eq!(
            store.filesystem_state(&manifest, "exports"),
            GrantState {
                granted: false,
                stale: false
            }
        );
        store.set_filesystem(&manifest, "exports", true).unwrap();
        assert_eq!(
            store.filesystem_state(&manifest, "exports"),
            GrantState {
                granted: true,
                stale: false
            }
        );
    }

    #[test]
    fn changing_the_mode_makes_the_filesystem_grant_stale() {
        use crate::plugin::manifest::FilesystemMode;
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        store
            .set_filesystem(&manifest_with_fs(FilesystemMode::Read), "exports", true)
            .unwrap();

        assert_eq!(
            store.filesystem_state(&manifest_with_fs(FilesystemMode::ReadWrite), "exports"),
            GrantState {
                granted: false,
                stale: true
            }
        );
    }

    #[test]
    fn filesystem_http_and_sidecar_grants_are_independent() {
        use crate::plugin::manifest::FilesystemMode;
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let mut manifest = manifest_with_fs(FilesystemMode::ReadWrite);
        manifest.capabilities = vec![CapabilityRequest::Http {
            hosts: vec!["https://api.example.com".into()],
            reason: "fetch".into(),
        }];

        store.set_filesystem(&manifest, "exports", true).unwrap();
        store.set(&manifest, true).unwrap();

        assert!(store.state(&manifest).granted);
        assert!(
            store.filesystem_state(&manifest, "exports").granted,
            "granting http must not clobber the filesystem grant"
        );
    }

    #[test]
    fn unknown_filesystem_root_is_never_granted() {
        use crate::plugin::manifest::FilesystemMode;
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_fs(FilesystemMode::Read);

        assert_eq!(
            store.filesystem_state(&manifest, "nope"),
            GrantState {
                granted: false,
                stale: false
            }
        );
        assert_eq!(
            store.set_filesystem(&manifest, "nope", true).unwrap(),
            GrantState {
                granted: false,
                stale: false
            }
        );
    }

    fn manifest_with_bus() -> Manifest {
        Manifest {
            id: "bus-plugin".into(),
            name: "Bus Plugin".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![crate::plugin::manifest::BusRequest {
                driver: "ed-state".into(),
                publish: vec!["ship-status".into()],
                subscribe: vec!["current-system".into()],
                reason: "r".into(),
            }],
            dashboard: vec![],
        }
    }

    #[test]
    fn bus_grants_start_ungranted_and_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(dir.path().to_path_buf());
        let manifest = manifest_with_bus();

        assert!(!store.bus_state(&manifest, "ed-state").granted);
        store.set_bus(&manifest, "ed-state", true).unwrap();

        let reopened = GrantsStore::new(dir.path().to_path_buf());
        assert!(reopened.bus_state(&manifest, "ed-state").granted);
    }

    #[test]
    fn bus_grants_go_stale_when_the_request_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(dir.path().to_path_buf());
        let manifest = manifest_with_bus();
        store.set_bus(&manifest, "ed-state", true).unwrap();

        let mut widened = manifest.clone();
        widened.bus[0].publish.push("another-topic".to_string());

        let state = store.bus_state(&widened, "ed-state");
        assert!(!state.granted);
        assert!(state.stale);
    }

    /// `manifest_with_bus()` に加えて、既存(バス導入前)フォーマットの
    /// grant ファイルが持つはずのサイドカー要求も 1 件持つマニフェスト。
    fn manifest_with_bus_and_sidecar() -> Manifest {
        let mut manifest = manifest_with_bus();
        manifest.sidecars = vec![crate::plugin::manifest::SidecarRequest {
            name: "tts".into(),
            reason: "reason".into(),
            args: vec!["--port".into(), "{port}".into()],
            port: 50021,
            scalable: true,
        }];
        manifest
    }

    #[test]
    fn existing_grant_files_without_a_bus_key_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = manifest_with_bus_and_sidecar();
        let sidecar_fingerprint = manifest
            .sidecar_fingerprint("tts")
            .expect("manifest declares a tts sidecar");

        // A pre-bus-era grant file: no "bus" key at all, but a real, populated
        // "sidecars" entry that this version still has to honour. If parsing
        // this whole file failed (e.g. because `#[serde(default)]` were missing
        // from `SavedGrant.bus`), `read_saved` would swallow the error into
        // `None` and this sidecar grant would silently vanish too -- so
        // asserting the sidecar grant survives is what actually distinguishes
        // "parsed, bus defaulted to empty" from "parse failed, degraded to
        // no-saved-grant", which asserting only on `bus_state` cannot.
        std::fs::write(
            dir.path().join("bus-plugin.json"),
            format!(
                r#"{{"granted":false,"fingerprint":"","sidecars":{{"tts":{{"granted":true,"fingerprint":"{sidecar_fingerprint}"}}}},"filesystem":{{}}}}"#
            ),
        )
        .unwrap();

        let store = GrantsStore::new(dir.path().to_path_buf());
        assert!(
            store.sidecar_state(&manifest, "tts").granted,
            "the pre-existing sidecar grant must still parse and be honoured"
        );
        assert!(!store.bus_state(&manifest, "ed-state").granted);
    }

    #[test]
    fn unknown_bus_driver_is_never_granted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_bus();

        assert_eq!(
            store.bus_state(&manifest, "nope"),
            GrantState {
                granted: false,
                stale: false
            }
        );
        assert_eq!(
            store.set_bus(&manifest, "nope", true).unwrap(),
            GrantState {
                granted: false,
                stale: false
            }
        );
    }

    #[test]
    fn new_for_drivers_uses_a_drivers_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new_for_drivers(tmp.path().to_path_buf());
        let manifest = manifest_with_bus();

        // The driver's own manifest id does not matter for this test; what
        // matters is the file lands under drivers/, not directly under dir.
        store.set_bus(&manifest, "ed-state", true).unwrap();
        assert!(tmp.path().join("drivers").join("bus-plugin.json").is_file());
        assert!(!tmp.path().join("bus-plugin.json").exists());
    }

    fn manifest_with_dashboard() -> Manifest {
        use crate::plugin::manifest::{DashboardWidget, WidgetSize};
        Manifest {
            id: "widgety".into(),
            name: "Widgety".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![DashboardWidget {
                id: "status".into(),
                title: "S".into(),
                entry: "ui/index.html".into(),
                size: WidgetSize::Small,
            }],
        }
    }

    #[test]
    fn dashboard_grant_persists_and_goes_stale_on_fingerprint_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(dir.path().to_path_buf());
        let manifest = manifest_with_dashboard();

        // 初期状態: 未承認・stale でない
        let initial = store.dashboard_state(&manifest, "status");
        assert!(!initial.granted);
        assert!(!initial.stale);

        // 承認 → granted
        let granted = store.set_dashboard(&manifest, "status", true).unwrap();
        assert!(granted.granted);
        assert!(store.dashboard_state(&manifest, "status").granted);

        // manifest 側の宣言が変わる → staleGrant
        let mut changed = manifest.clone();
        changed.dashboard[0].entry = "ui/other.html".into();
        let state = store.dashboard_state(&changed, "status");
        assert!(!state.granted);
        assert!(state.stale);

        // 取消も永続化される
        let revoked = store.set_dashboard(&manifest, "status", false).unwrap();
        assert!(!revoked.granted);
        assert!(!store.dashboard_state(&manifest, "status").granted);

        // 未宣言 widget は常に false/false
        let unknown = store.dashboard_state(&manifest, "nope");
        assert!(!unknown.granted && !unknown.stale);
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

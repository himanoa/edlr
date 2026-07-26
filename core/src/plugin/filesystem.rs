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
    /// システム上重要なディレクトリ「そのもの」、または edlr 自身の状態
    /// ディレクトリ(そのもの・祖先・配下)を指定した。
    ProtectedDirectory(String),
    Io(std::io::Error),
    Serialize(serde_json::Error),
}

impl fmt::Display for FilesystemConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilesystemConfigError::UnknownRoot(name) => {
                write!(f, "unknown filesystem root: {name}")
            }
            FilesystemConfigError::NotAbsolute(name) => {
                write!(f, "filesystem root {name} must be an absolute path")
            }
            FilesystemConfigError::NotADirectory(name) => {
                write!(f, "filesystem root {name} must be an existing directory")
            }
            FilesystemConfigError::ProtectedDirectory(name) => {
                write!(
                    f,
                    "filesystem root {name} may not point at a protected directory"
                )
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
    /// edlr 自身の状態ディレクトリ(settings / grants / plugins)。
    /// 「そのもの、またはその祖先・配下」は承認先に選ばせない
    /// ([`FilesystemConfigStore::is_protected`])。
    state_dirs: Vec<PathBuf>,
    lock: Mutex<()>,
}

impl FilesystemConfigStore {
    /// `dir` は設定の保存先(= edlr の settings ディレクトリ)。
    /// `state_dirs` には edlr 自身の**それ以外の**状態ディレクトリ
    /// (grants / plugins)を渡す。`dir` 自身は常に保護対象に加わる。
    ///
    /// 呼び出し元は**既定値をハードコードせず、CLI で上書きされた実パス**を
    /// 渡すこと(`core/src/bin/edlr.rs` を参照)。ここを既定値で埋めると、
    /// `--grants-dir` で別の場所を指しているデーモンでその実際の場所が
    /// 無防備になる。
    pub fn new(dir: PathBuf, state_dirs: Vec<PathBuf>) -> Self {
        let mut protected = vec![dir.clone()];
        protected.extend(state_dirs);
        let protected = protected.iter().map(|p| best_effort_absolute(p)).collect();
        FilesystemConfigStore {
            dir,
            state_dirs: protected,
            lock: Mutex::new(()),
        }
    }

    /// ユーザーが選んだディレクトリを検証する。
    ///
    /// システム上重要なディレクトリ「そのもの」(`/`, `/etc`, `$HOME` など)と、
    /// edlr 自身の状態ディレクトリ(そのもの・祖先・配下)を拒否する。
    /// 承認画面での確認だけに頼らず、明らかな事故と昇格経路を 1 段止めるため
    /// (それ以外の配下は許可する -- `/home/alice/Documents` は通り、`/home`
    /// は通らない)。
    fn validate_path(&self, name: &str, path: &str) -> Result<(), FilesystemConfigError> {
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

        if is_system_protected(&canonical) || self.is_protected(&canonical) {
            return Err(FilesystemConfigError::ProtectedDirectory(name.to_string()));
        }
        Ok(())
    }

    /// edlr 自身の状態ディレクトリに触れる選択かどうか。
    ///
    /// 「そのもの」だけでなく**祖先**も拒否する。`~/.config` を選べてしまえば
    /// `~/.config/edlr/grants` 配下は丸ごと読み書きできてしまい、「そのもの」
    /// しか見ない判定は意味を成さない。配下(`<grants-dir>/sub` など)も同じ
    /// 理由で拒否する。
    fn is_protected(&self, canonical: &std::path::Path) -> bool {
        self.state_dirs.iter().any(|state| {
            canonical == state || state.starts_with(canonical) || canonical.starts_with(state)
        })
    }

    fn path_for(&self, manifest: &Manifest) -> PathBuf {
        self.dir.join(format!("{}.filesystem.json", manifest.id))
    }

    /// manifest の既定値(空パス)に保存済みの値をマージした設定一覧を返す。
    /// ファイルが無い・壊れている場合は既定値のみ(panic しない)。
    ///
    /// **保存済みの値も読み込み時に検証する。** ディスク上の
    /// `<id>.filesystem.json` は保存時の検証を経ていない可能性がある(手書き、
    /// 旧バージョンで保存されたもの、保護対象が増える前に保存されたもの)。
    /// 検証を通らない値は空パス(= 未設定)へ落とすので、承認もできず
    /// (`Registry::set_filesystem_grant`)、共有バッファにも `path` が載らず
    /// (`fs_runtime`)、ホスト側も `not-configured` で拒否する
    /// (`HostCtx::resolve_root`)。これで I1 の保護が保存時だけでなく恒久的に
    /// 閉じる。
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
                let config =
                    saved
                        .get(&request.name)
                        .cloned()
                        .unwrap_or_else(|| FilesystemConfig {
                            path: String::new(),
                        });
                let config = match self.validate_path(&request.name, &config.path) {
                    Ok(()) => config,
                    Err(e) => {
                        tracing::warn!(
                            plugin = %manifest.id,
                            root = %request.name,
                            path = %config.path,
                            "stored filesystem directory is no longer acceptable ({e}); \
                             treating it as unset"
                        );
                        FilesystemConfig {
                            path: String::new(),
                        }
                    }
                };
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

        self.validate_path(name, &config.path)?;

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

/// 存在しないディレクトリも比較の対象にするための正規化。
///
/// 状態ディレクトリは起動時にまだ存在しないことがある(初回起動の `grants`
/// など)。`canonicalize` は存在しないパスで失敗するので、存在する最長の
/// 祖先まで正規化し、残りをそのまま繋ぐ。
///
/// **相対パスは必ずカレントディレクトリを前置してから正規化する。** 比較相手
/// (ユーザーが選んだディレクトリ)は必ず `canonicalize()` 済みの絶対パスなので、
/// 相対パスのまま保護リストに載せると `starts_with` がどちら向きにも一致せず、
/// 保護が静かに外れる(`--grants-dir some/where` のような相対指定で、その祖先も
/// 1 つも存在しない場合に起きる)。スナップショットは起動時に 1 回きりなので、
/// 穴はプロセスの寿命の間ずっと開いたままになる。
///
/// カレントディレクトリすら取れず絶対パスにできなかった場合は、保護が効かない
/// ことを警告した上で元のパスを返す(黙って落とさない)。
fn best_effort_absolute(path: &std::path::Path) -> PathBuf {
    if path.is_relative() {
        match std::env::current_dir() {
            Ok(cwd) => return best_effort_absolute(&cwd.join(path)),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    "cannot resolve a relative state directory against the current directory \
                     ({e}); it will not be protected from being granted as a filesystem root"
                );
                return path.to_path_buf();
            }
        }
    }
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut remainder = Vec::new();
    let mut current = path;
    while let Some(parent) = current.parent() {
        let Some(name) = current.file_name() else {
            break;
        };
        remainder.push(name.to_os_string());
        if let Ok(canonical) = parent.canonicalize() {
            let mut resolved = canonical;
            for name in remainder.iter().rev() {
                resolved.push(name);
            }
            return resolved;
        }
        current = parent;
    }
    path.to_path_buf()
}

/// 「そのものは選ばせない」システム上重要なディレクトリ。配下は許可する。
fn is_system_protected(canonical: &std::path::Path) -> bool {
    const PROTECTED: &[&str] = &[
        "/", "/home", "/etc", "/usr", "/var", "/boot", "/dev", "/proc", "/sys", "/root", "/bin",
        "/sbin", "/lib",
    ];
    if PROTECTED
        .iter()
        .any(|p| canonical == std::path::Path::new(p))
    {
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
        let store = FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
        let manifest = manifest_with(vec![request("exports")]);

        assert_eq!(store.effective(&manifest)["exports"].path, "");
    }

    #[test]
    fn update_persists_and_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        let target = tmp.path().join("exports");
        std::fs::create_dir(&target).unwrap();
        let store = FilesystemConfigStore::new(dir.clone(), Vec::new());
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

        let reread = FilesystemConfigStore::new(dir, Vec::new()).effective(&manifest);
        assert_eq!(reread["exports"].path, target.to_string_lossy());
    }

    #[test]
    fn relative_paths_missing_paths_and_files_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
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
        let store = FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
        let manifest = manifest_with(vec![request("exports")]);

        for path in ["/", "/etc", "/home", "/usr", "/var"] {
            let err = store
                .update_and_effective(
                    &manifest,
                    "exports",
                    &FilesystemConfig {
                        path: path.to_string(),
                    },
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
        let store = FilesystemConfigStore::new(dir, Vec::new());
        let manifest = manifest_with(vec![request("exports")]);
        store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig {
                    path: target.to_string_lossy().to_string(),
                },
            )
            .unwrap();

        let _ = store.update_and_effective(
            &manifest,
            "exports",
            &FilesystemConfig {
                path: "/etc".to_string(),
            },
        );

        assert_eq!(
            store.effective(&manifest)["exports"].path,
            target.to_string_lossy()
        );
    }

    #[test]
    fn unknown_root_is_rejected_and_broken_json_falls_back_to_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fs-plugin.filesystem.json"), "not json {{{").unwrap();
        let store = FilesystemConfigStore::new(dir, Vec::new());
        let manifest = manifest_with(vec![request("exports")]);

        assert_eq!(store.effective(&manifest)["exports"].path, "");
        assert!(matches!(
            store
                .update_and_effective(
                    &manifest,
                    "nope",
                    &FilesystemConfig {
                        path: "/tmp".into()
                    }
                )
                .expect_err("unknown root"),
            FilesystemConfigError::UnknownRoot(_)
        ));
    }

    /// edlr 自身の状態ディレクトリ(settings / grants / plugins)と、その
    /// **祖先**は選ばせない。
    ///
    /// read-write でここを承認できてしまうと、ファイルアクセスの承認が他の
    /// capability への昇格経路になる: `<grants-dir>/<other-id>.json` を
    /// 書き換えて他プラグインの承認を捏造する、`<plugins-dir>/<id>/plugin.wasm`
    /// を差し替えて承認済みプラグインの中身をすり替える(fingerprint は
    /// manifest だけを覆っており wasm バイト列を含まない)、
    /// `<settings-dir>/<id>.sidecar.json` の `command` を書き換える、など。
    /// 承認ダイアログの文言は「選んだフォルダ内のファイルを読み書きできます」
    /// であって、他プラグインの承認を書き換えられる、ではない。
    #[test]
    fn edlrs_own_state_directories_and_their_ancestors_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("dot-config");
        let edlr = config.join("edlr");
        let settings = edlr.join("settings");
        let grants = edlr.join("grants");
        let plugins = edlr.join("plugins");
        for dir in [&settings, &grants, &plugins] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let store =
            FilesystemConfigStore::new(settings.clone(), vec![grants.clone(), plugins.clone()]);
        let manifest = manifest_with(vec![request("exports")]);

        for path in [
            settings.as_path(),
            grants.as_path(),
            plugins.as_path(),
            // 祖先。`~/.config` 相当を選ばれると素通り、では意味が無い。
            edlr.as_path(),
            config.as_path(),
            tmp.path(),
        ] {
            let err = store
                .update_and_effective(
                    &manifest,
                    "exports",
                    &FilesystemConfig {
                        path: path.to_string_lossy().to_string(),
                    },
                )
                .expect_err(&format!("{} must be rejected", path.display()));
            assert!(
                matches!(err, FilesystemConfigError::ProtectedDirectory(_)),
                "{} should be protected, got {err:?}",
                path.display()
            );
        }

        // 無関係なディレクトリは引き続き通ること(過剰に締めていないこと)。
        let unrelated = tmp.path().join("exports");
        std::fs::create_dir(&unrelated).unwrap();
        store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig {
                    path: unrelated.to_string_lossy().to_string(),
                },
            )
            .expect("an unrelated directory must still be accepted");
    }

    /// 起動時にはまだ存在しない状態ディレクトリ(初回起動の `grants` など)も
    /// 保護対象であること。`canonicalize` に失敗したら諦める実装だと、
    /// 「まだ無いディレクトリを先に承認しておく」で回避できてしまう。
    #[test]
    fn a_state_directory_that_does_not_exist_yet_is_still_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let grants = tmp.path().join("state").join("grants");
        let existing_parent = tmp.path().join("state");
        std::fs::create_dir(&existing_parent).unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"), vec![grants.clone()]);
        let manifest = manifest_with(vec![request("exports")]);

        // `grants` 自体はまだ無いので、その祖先(存在する)を選んで昇格を狙う。
        let err = store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig {
                    path: existing_parent.to_string_lossy().to_string(),
                },
            )
            .expect_err("an ancestor of a not-yet-created state directory must be rejected");
        assert!(matches!(err, FilesystemConfigError::ProtectedDirectory(_)));
    }

    /// 相対パスで渡された状態ディレクトリでも保護が効くこと。
    ///
    /// `--grants-dir some/where` のような相対指定で、その祖先も 1 つも存在
    /// しない場合、`best_effort_absolute` は相対パスをそのまま返す。候補側は
    /// 必ず `canonicalize()` 済みの絶対パスなので `starts_with` がどちら向きにも
    /// 一致せず、保護が静かに外れてしまう。スナップショットは起動時に 1 回きり
    /// なので、穴はプロセスの寿命の間ずっと開く。
    ///
    /// cwd を動かすと(`set_current_dir` はプロセス全体に効く)並列テストを
    /// 壊すので、cwd 配下にまだ存在しない名前を使って同じ状況を作る。
    #[test]
    fn a_relative_state_directory_path_is_still_protected() {
        let cwd = std::env::current_dir().unwrap();
        let relative_root =
            PathBuf::from(format!("edlr-fs-protection-test-{}", std::process::id()));
        let relative_grants = relative_root.join("grants");
        let absolute_grants = cwd.join(&relative_grants);
        // ストア構築時点では、祖先も含めて 1 つも存在しない。
        assert!(!cwd.join(&relative_root).exists());

        let tmp = tempfile::tempdir().unwrap();
        let store =
            FilesystemConfigStore::new(tmp.path().join("settings"), vec![relative_grants.clone()]);
        let manifest = manifest_with(vec![request("exports")]);

        // 起動後にディレクトリができてから、ユーザーがそれを選ぶ。
        std::fs::create_dir_all(&absolute_grants).unwrap();
        let result = store.update_and_effective(
            &manifest,
            "exports",
            &FilesystemConfig {
                path: absolute_grants.to_string_lossy().to_string(),
            },
        );
        let _ = std::fs::remove_dir_all(cwd.join(&relative_root));

        let err = result
            .expect_err("a state directory passed as a relative path must still be protected");
        assert!(matches!(err, FilesystemConfigError::ProtectedDirectory(_)));
    }

    /// ディスク上の設定は、保存時の検証を経ていない可能性がある(手書き、
    /// 旧バージョンで保存されたもの)。読み込み時にも検証し、通らないものは
    /// 空パス(= 未設定)へ落とすこと。これで I1 の保護が保存時だけでなく
    /// 恒久的に閉じる。
    #[test]
    fn a_hand_written_config_pointing_at_a_protected_directory_is_treated_as_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings");
        let grants = tmp.path().join("grants");
        std::fs::create_dir_all(&settings).unwrap();
        std::fs::create_dir_all(&grants).unwrap();
        let manifest = manifest_with(vec![request("exports"), request("cache")]);

        let good = tmp.path().join("exports");
        std::fs::create_dir(&good).unwrap();
        std::fs::write(
            settings.join("fs-plugin.filesystem.json"),
            serde_json::to_string(&serde_json::json!({
                "exports": { "path": grants.to_string_lossy() },
                "cache": { "path": good.to_string_lossy() },
            }))
            .unwrap(),
        )
        .unwrap();

        let store = FilesystemConfigStore::new(settings, vec![grants]);
        let effective = store.effective(&manifest);

        assert_eq!(
            effective["exports"].path, "",
            "a stored path that fails validation must be treated as unset"
        );
        assert_eq!(
            effective["cache"].path,
            good.to_string_lossy(),
            "a valid stored path must survive untouched"
        );
    }

    #[test]
    fn trailing_slash_on_a_protected_directory_is_still_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
        let manifest = manifest_with(vec![request("exports")]);

        let err = store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig {
                    path: "/etc/".to_string(),
                },
            )
            .expect_err("/etc/ must be rejected just like /etc");
        assert!(matches!(err, FilesystemConfigError::ProtectedDirectory(_)));
    }

    #[test]
    fn dot_dot_traversal_into_a_protected_directory_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
        let manifest = manifest_with(vec![request("exports")]);

        let err = store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig {
                    path: "/etc/../etc".to_string(),
                },
            )
            .expect_err("/etc/../etc must canonicalize to /etc and be rejected");
        assert!(matches!(err, FilesystemConfigError::ProtectedDirectory(_)));
    }

    #[test]
    fn a_symlink_that_resolves_to_a_protected_directory_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new());
        let manifest = manifest_with(vec![request("exports")]);
        let link = tmp.path().join("etc-link");
        std::os::unix::fs::symlink("/etc", &link).unwrap();

        let err = store
            .update_and_effective(
                &manifest,
                "exports",
                &FilesystemConfig {
                    path: link.to_string_lossy().to_string(),
                },
            )
            .expect_err("a symlink resolving to /etc must be rejected");
        assert!(matches!(err, FilesystemConfigError::ProtectedDirectory(_)));
    }
}

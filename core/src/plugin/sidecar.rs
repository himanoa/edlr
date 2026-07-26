//! サイドカーのユーザー設定(`command` / `args` / `port` / `replicas`)の
//! 永続化と検証、およびポート採番。
//!
//! 保存先は `<settings-dir>/<plugin-id>.sidecars.json`。通常の
//! `[[settings]]` とは別ファイルにしている: `SettingsStore::update` は
//! manifest の `[[settings]]` に無いキーをディスクから間引く実装なので、
//! 同じファイルに同居させると設定保存のたびにサイドカー設定が消えてしまう。

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::plugin::manifest::SidecarRequest;
use crate::plugin::Manifest;

/// サイドカー 1 件のユーザー設定。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SidecarConfig {
    /// 実行ファイルの絶対パス。空文字は「未設定」(承認も起動もできない)。
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub port: u16,
    #[serde(default = "one")]
    pub replicas: u16,
}

fn one() -> u16 {
    1
}

impl SidecarConfig {
    /// manifest の既定値から、`command` 未設定の初期設定を作る。
    pub fn from_request(request: &SidecarRequest) -> SidecarConfig {
        SidecarConfig {
            command: String::new(),
            args: request.args.clone(),
            port: request.port,
            replicas: 1,
        }
    }
}

/// `config` に対応する実ポート列(`port, port+1, …, port+replicas-1`)。
/// `replicas` が 0 のときは 1 台として扱う(UI/RPC 側の検証を通り抜けた
/// 値でも空の spec を作らないための下限)。
pub fn assign_ports(config: &SidecarConfig) -> Vec<u16> {
    let replicas = config.replicas.max(1);
    (0..replicas)
        .filter_map(|offset| config.port.checked_add(offset))
        .collect()
}

#[derive(Debug)]
pub enum SidecarConfigError {
    /// manifest にない `name` を指定した。
    UnknownSidecar(String),
    /// `scalable = false` のサイドカーに `replicas > 1` を指定した。
    NotScalable(String),
    /// `replicas = 0` を指定した(インスタンスが 1 つも存在しない構成は無意味)。
    ZeroReplicas(String),
    /// `args` に `{port}` が無いまま `replicas > 1` を指定した。
    MissingPortPlaceholder(String),
    /// ポート採番が 65535 を超える。
    PortOverflow(String),
    /// 同一プラグイン内で他のサイドカーとポート範囲が重なる。
    PortRangeOverlap(String),
    Io(std::io::Error),
    Serialize(serde_json::Error),
}

impl fmt::Display for SidecarConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SidecarConfigError::UnknownSidecar(name) => write!(f, "unknown sidecar: {name}"),
            SidecarConfigError::NotScalable(name) => {
                write!(f, "sidecar {name} does not allow replicas > 1")
            }
            SidecarConfigError::ZeroReplicas(name) => {
                write!(f, "sidecar {name} requires replicas >= 1")
            }
            SidecarConfigError::MissingPortPlaceholder(name) => write!(
                f,
                "sidecar {name} needs {{port}} in args to run more than one replica"
            ),
            SidecarConfigError::PortOverflow(name) => {
                write!(f, "sidecar {name} port range exceeds 65535")
            }
            SidecarConfigError::PortRangeOverlap(name) => {
                write!(f, "sidecar {name} port range overlaps another sidecar")
            }
            SidecarConfigError::Io(e) => write!(f, "failed to write sidecar config: {e}"),
            SidecarConfigError::Serialize(e) => {
                write!(f, "failed to serialize sidecar config: {e}")
            }
        }
    }
}

impl std::error::Error for SidecarConfigError {}

/// `<settings-dir>/<plugin-id>.sidecars.json` を読み書きするストア。
/// `SettingsStore` と同じく内部 `Mutex<()>` で read-merge-write を直列化する。
pub struct SidecarConfigStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl SidecarConfigStore {
    pub fn new(dir: PathBuf) -> Self {
        SidecarConfigStore {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn path_for(&self, manifest: &Manifest) -> PathBuf {
        self.dir.join(format!("{}.sidecars.json", manifest.id))
    }

    /// manifest の既定値に保存済みの値をマージした設定一覧を返す。
    /// ファイルが無い・壊れている場合は既定値のみ(panic しない)。
    pub fn effective(&self, manifest: &Manifest) -> BTreeMap<String, SidecarConfig> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.effective_locked(manifest)
    }

    fn effective_locked(&self, manifest: &Manifest) -> BTreeMap<String, SidecarConfig> {
        let saved: BTreeMap<String, SidecarConfig> = fs::read_to_string(self.path_for(manifest))
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default();

        manifest
            .sidecars
            .iter()
            .map(|request| {
                let config = saved
                    .get(&request.name)
                    .cloned()
                    .unwrap_or_else(|| SidecarConfig::from_request(request));
                (request.name.clone(), config)
            })
            .collect()
    }

    /// 1 サイドカーの設定を検証して保存し、更新後の全設定を返す。
    /// 検証に失敗した場合は何も書き込まない。
    pub fn update_and_effective(
        &self,
        manifest: &Manifest,
        name: &str,
        config: &SidecarConfig,
    ) -> Result<BTreeMap<String, SidecarConfig>, SidecarConfigError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let request = manifest
            .sidecar(name)
            .ok_or_else(|| SidecarConfigError::UnknownSidecar(name.to_string()))?;

        let mut merged = self.effective_locked(manifest);
        validate(name, request, config)?;
        merged.insert(name.to_string(), config.clone());
        validate_no_overlap(&merged)?;

        fs::create_dir_all(&self.dir).map_err(SidecarConfigError::Io)?;
        let serialized =
            serde_json::to_string_pretty(&merged).map_err(SidecarConfigError::Serialize)?;
        let target = self.path_for(manifest);
        let tmp_path = self.dir.join(format!(
            "{}.sidecars.json.tmp.{}",
            manifest.id,
            std::process::id()
        ));
        fs::write(&tmp_path, serialized).map_err(SidecarConfigError::Io)?;
        fs::rename(&tmp_path, &target).map_err(SidecarConfigError::Io)?;

        Ok(merged)
    }
}

fn validate(
    name: &str,
    request: &SidecarRequest,
    config: &SidecarConfig,
) -> Result<(), SidecarConfigError> {
    // `replicas = 0` は「インスタンスを 1 つも起動しない」設定であり、他の
    // すべての不正入力(NotScalable/MissingPortPlaceholder/PortOverflow/
    // PortRangeOverlap)は明示的に拒否しているのに、ここだけ `assign_ports`
    // の `.max(1)` に黙って 1 へ丸められてしまっていた(Minor: 最終レビュー
    // で見つかった不一致)。`assign_ports` 自身の `.max(1)` は、検証をすり
    // 抜けた古い保存済み設定を読んだときの防御的な下限として残す(こちらの
    // 検証は新規の保存だけをガードする)。
    if config.replicas == 0 {
        return Err(SidecarConfigError::ZeroReplicas(name.to_string()));
    }

    if config.replicas > 1 {
        if !request.scalable {
            return Err(SidecarConfigError::NotScalable(name.to_string()));
        }
        if !config.args.iter().any(|arg| arg.contains("{port}")) {
            return Err(SidecarConfigError::MissingPortPlaceholder(name.to_string()));
        }
    }

    let replicas = config.replicas.max(1);
    if config.port.checked_add(replicas - 1).is_none() {
        return Err(SidecarConfigError::PortOverflow(name.to_string()));
    }

    Ok(())
}

/// 同一プラグイン内でポート範囲が重ならないことを確認する。
fn validate_no_overlap(
    configs: &BTreeMap<String, SidecarConfig>,
) -> Result<(), SidecarConfigError> {
    let mut used: BTreeMap<u16, String> = BTreeMap::new();
    for (name, config) in configs {
        for port in assign_ports(config) {
            if let Some(other) = used.insert(port, name.clone()) {
                if &other != name {
                    return Err(SidecarConfigError::PortRangeOverlap(name.clone()));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::SidecarRequest;

    fn manifest_with(sidecars: Vec<SidecarRequest>) -> Manifest {
        Manifest {
            id: "sc-plugin".into(),
            name: "SC".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars,
            filesystem: vec![],
        }
    }

    fn request(name: &str, port: u16, scalable: bool) -> SidecarRequest {
        SidecarRequest {
            name: name.into(),
            reason: "reason".into(),
            args: vec!["--port".into(), "{port}".into()],
            port,
            scalable,
        }
    }

    #[test]
    fn effective_falls_back_to_manifest_defaults_with_empty_command() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SidecarConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("tts", 50021, true)]);

        let effective = store.effective(&manifest);
        let config = effective.get("tts").expect("tts config");
        assert_eq!(config.command, "");
        assert_eq!(config.port, 50021);
        assert_eq!(config.replicas, 1);
        assert_eq!(config.args, vec!["--port".to_string(), "{port}".to_string()]);
    }

    #[test]
    fn update_persists_and_assigns_sequential_ports() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        let store = SidecarConfigStore::new(dir.clone());
        let manifest = manifest_with(vec![request("tts", 50021, true)]);

        let updated = store
            .update_and_effective(
                &manifest,
                "tts",
                &SidecarConfig {
                    command: "/usr/bin/piper".into(),
                    args: vec!["--port".into(), "{port}".into()],
                    port: 50021,
                    replicas: 3,
                },
            )
            .expect("update should succeed");

        assert_eq!(updated["tts"].command, "/usr/bin/piper");
        assert_eq!(assign_ports(&updated["tts"]), vec![50021, 50022, 50023]);
        assert!(dir.join("sc-plugin.sidecars.json").is_file());

        // 再読込しても保持されている。
        let reread = SidecarConfigStore::new(dir).effective(&manifest);
        assert_eq!(reread["tts"].replicas, 3);
    }

    #[test]
    fn replicas_above_one_requires_scalable_and_a_port_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SidecarConfigStore::new(tmp.path().join("settings"));

        let not_scalable = manifest_with(vec![request("tts", 50021, false)]);
        let err = store
            .update_and_effective(
                &not_scalable,
                "tts",
                &SidecarConfig {
                    command: "/bin/true".into(),
                    args: vec!["--port".into(), "{port}".into()],
                    port: 50021,
                    replicas: 2,
                },
            )
            .expect_err("replicas > 1 on a non-scalable sidecar must be rejected");
        assert!(matches!(err, SidecarConfigError::NotScalable(_)));

        let scalable = manifest_with(vec![request("tts", 50021, true)]);
        let err = store
            .update_and_effective(
                &scalable,
                "tts",
                &SidecarConfig {
                    command: "/bin/true".into(),
                    args: vec!["--fixed-port".into(), "50021".into()],
                    port: 50021,
                    replicas: 2,
                },
            )
            .expect_err("replicas > 1 without {port} must be rejected");
        assert!(matches!(err, SidecarConfigError::MissingPortPlaceholder(_)));
    }

    #[test]
    fn replicas_zero_is_rejected_not_silently_rounded_up() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SidecarConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("tts", 50021, true)]);

        let err = store
            .update_and_effective(
                &manifest,
                "tts",
                &SidecarConfig {
                    command: "/bin/true".into(),
                    args: vec!["--port".into(), "{port}".into()],
                    port: 50021,
                    replicas: 0,
                },
            )
            .expect_err("replicas = 0 must be rejected, not silently rounded up to 1");
        assert!(matches!(err, SidecarConfigError::ZeroReplicas(_)));
    }

    #[test]
    fn overlapping_port_ranges_within_a_plugin_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SidecarConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("tts", 50021, true), request("tr", 50030, false)]);

        // tts が 50021..=50031 を占めると tr(50030)と重なる。
        let err = store
            .update_and_effective(
                &manifest,
                "tts",
                &SidecarConfig {
                    command: "/bin/true".into(),
                    args: vec!["--port".into(), "{port}".into()],
                    port: 50021,
                    replicas: 11,
                },
            )
            .expect_err("overlapping port ranges must be rejected");
        assert!(matches!(err, SidecarConfigError::PortRangeOverlap(_)));
    }

    #[test]
    fn port_range_overflowing_65535_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SidecarConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("tts", 65535, true)]);

        let err = store
            .update_and_effective(
                &manifest,
                "tts",
                &SidecarConfig {
                    command: "/bin/true".into(),
                    args: vec!["--port".into(), "{port}".into()],
                    port: 65535,
                    replicas: 2,
                },
            )
            .expect_err("a port range past 65535 must be rejected");
        assert!(matches!(err, SidecarConfigError::PortOverflow(_)));
    }

    #[test]
    fn broken_json_on_disk_falls_back_to_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sc-plugin.sidecars.json"), "not json {{{").unwrap();

        let store = SidecarConfigStore::new(dir);
        let manifest = manifest_with(vec![request("tts", 50021, true)]);
        assert_eq!(store.effective(&manifest)["tts"].port, 50021);
    }
}

use edlr_config::{config_file_path, AppConfig};
use std::path::PathBuf;

/// 読み込み結果。JSON が壊れていても起動は止めず、`error` に理由を持って
/// UI へ見せる(黙って既定値に倒すと原因が分からなくなるため)。
pub struct LoadedConfig {
    pub path: PathBuf,
    pub config: AppConfig,
    pub error: Option<String>,
}

/// 優先順位は env → 設定ファイル → None。
///
/// `None` は「`--journal-dir` を渡さない」を意味し、デーモンが従来どおり
/// Proton 既定パスの自動検出を行う。これにより「設定 > 自動検出」が成立し、
/// 自動検出が当たる環境では設定不要のままとなる。
pub fn resolve_journal_dir(env: Option<PathBuf>, config: Option<PathBuf>) -> Option<PathBuf> {
    env.or(config)
}

/// `$XDG_CONFIG_HOME` / `$HOME` からパスを解決して設定を読む。
pub fn load_from_env() -> LoadedConfig {
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = config_file_path(xdg.as_deref(), home.as_deref());

    match AppConfig::load(&path) {
        Ok(config) => LoadedConfig {
            path,
            config,
            error: None,
        },
        Err(e) => LoadedConfig {
            path,
            config: AppConfig::default(),
            error: Some(e.to_string()),
        },
    }
}

use serde::Serialize;

/// フロントエンドへ返す設定のスナップショット。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDto {
    pub journal_dir: Option<String>,
    /// Tauri が spawn したデーモンを保持しているか。`false` の場合は
    /// 外部起動のデーモンなので再起動できない(勝手に殺さない)。
    pub daemon_managed: bool,
    pub config_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_wins_over_config() {
        let resolved = resolve_journal_dir(
            Some(PathBuf::from("/from/env")),
            Some(PathBuf::from("/from/config")),
        );
        assert_eq!(resolved, Some(PathBuf::from("/from/env")));
    }

    #[test]
    fn config_used_when_env_absent() {
        let resolved = resolve_journal_dir(None, Some(PathBuf::from("/from/config")));
        assert_eq!(resolved, Some(PathBuf::from("/from/config")));
    }

    #[test]
    fn none_when_neither_set() {
        // None は「--journal-dir を渡さない」= デーモンの自動検出に委ねるを意味する
        assert_eq!(resolve_journal_dir(None, None), None);
    }

    #[test]
    fn dto_serializes_to_camel_case() {
        let dto = ConfigDto {
            journal_dir: Some("/mnt/game/ED".to_string()),
            daemon_managed: true,
            config_error: None,
        };

        let json = serde_json::to_value(&dto).unwrap();

        assert_eq!(json["journalDir"], "/mnt/game/ED");
        assert_eq!(json["daemonManaged"], true);
        assert!(json["configError"].is_null());
    }
}

use edlr_config::{config_file_path, AppConfig};
use std::path::PathBuf;

/// 読み込み結果。JSON が壊れていても起動は止めず、`error` に理由を持って
/// UI へ見せる(黙って既定値に倒すと原因が分からなくなるため)。
pub struct LoadedConfig {
    pub path: PathBuf,
    pub config: AppConfig,
    pub error: Option<String>,
}

/// 起動時にデーモンをどう扱ったか。
///
/// `Child` を持っているかどうかだけでは「外部で動いているので触らない」と
/// 「起動しようとして失敗した」を区別できない。前者だけが `daemonManaged:
/// false`(= このアプリは手出ししない)の正しい意味であり、後者を同じ扱いに
/// すると UI が「外部のデーモンが動作中」と嘘をつくうえ、再試行の経路も
/// 塞がってしまう。
#[derive(Debug, Clone, PartialEq)]
pub enum DaemonStartup {
    /// このアプリが spawn した。
    Spawned,
    /// 外部で既に動いていたので触っていない。
    External,
    /// このアプリが起動を試みて失敗した。何も動いていない。
    Failed(String),
}

impl DaemonStartup {
    /// このアプリがデーモンのライフサイクルに責任を持つか。
    ///
    /// 失敗した場合も責任は持ち続ける(再試行できる)。責任を手放すのは
    /// 外部起動のデーモンを見つけたときだけ。
    pub fn owns_daemon(&self) -> bool {
        !matches!(self, DaemonStartup::External)
    }

    /// UI に見せる起動失敗の理由。失敗していなければ `None`。
    pub fn error(&self) -> Option<String> {
        match self {
            DaemonStartup::Failed(reason) => Some(reason.clone()),
            _ => None,
        }
    }
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
    /// 実際にデーモンへ渡される(渡された)実効値。
    /// `resolve_journal_dir` で env と設定ファイルを解決した後の値であり、
    /// spawn・restart・この表示の 3 箇所は常にこの値で一致する。
    ///
    /// 表示用・`App.tsx` のルーティング判定用の値。編集・保存の起点には
    /// 使わない(`env_override` が true のとき、これは env 由来の値になり、
    /// 設定ファイルの値とは限らないため)。
    pub journal_dir: Option<String>,
    /// 設定ファイル(`config.json`)に実際に保存されている生の値。
    /// `env_override` の有無に関わらず、常に設定ファイルの内容そのもの。
    /// Settings 画面の編集フォームはこの値を起点にする(`journal_dir` を
    /// 起点にすると、env override 中の再保存で env の値を設定ファイルへ
    /// 書き戻してしまい、保存済みの値を消してしまう)。
    pub configured_journal_dir: Option<String>,
    /// Tauri が spawn したデーモンを保持しているか。`false` の場合は
    /// 外部起動のデーモンなので再起動できない(勝手に殺さない)。
    pub daemon_managed: bool,
    pub config_error: Option<String>,
    /// 起動時にデーモンを spawn しようとして失敗した理由。
    ///
    /// `daemon_managed: true` かつこれが `Some` のときは「このアプリの
    /// 責任範囲だが今は動いていない」を意味する。設定を保存すれば
    /// 再試行される。
    pub daemon_error: Option<String>,
    /// `EDLR_JOURNAL_DIR` が設定されており、`journal_dir` がそれに
    /// 由来しているか。true の間は設定ファイルを保存しても実効値は
    /// 変わらない(env が優先される)。
    pub env_override: bool,
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
            configured_journal_dir: Some("/mnt/game/ED".to_string()),
            daemon_managed: true,
            config_error: None,
            daemon_error: None,
            env_override: false,
        };

        let json = serde_json::to_value(&dto).unwrap();

        assert_eq!(json["journalDir"], "/mnt/game/ED");
        assert_eq!(json["configuredJournalDir"], "/mnt/game/ED");
        assert_eq!(json["daemonManaged"], true);
        assert!(json["configError"].is_null());
        assert_eq!(json["envOverride"], false);
        assert!(json["daemonError"].is_null());
    }

    #[test]
    fn dto_carries_daemon_startup_failure() {
        // 起動に失敗したときは owns_daemon を保ったまま(= 外部デーモン扱いに
        // しない)、理由を UI へ渡す。
        let startup = DaemonStartup::Failed("edlr binary not found".to_string());
        let dto = ConfigDto {
            journal_dir: None,
            configured_journal_dir: None,
            daemon_managed: startup.owns_daemon(),
            config_error: None,
            env_override: false,
            daemon_error: startup.error(),
        };

        let json = serde_json::to_value(&dto).unwrap();

        assert_eq!(json["daemonManaged"], true);
        assert_eq!(json["daemonError"], "edlr binary not found");
    }

    #[test]
    fn dto_distinguishes_effective_from_configured_when_env_overrides() {
        // env override 中は journal_dir(実効値)と configured_journal_dir
        // (設定ファイルの生の値)が食い違いうる。Settings 画面はこの 2 つを
        // 区別できないと、再保存のたびに env 由来の値で設定ファイルを
        // 上書きしてしまう(このテストが守る不変条件)。
        let dto = ConfigDto {
            journal_dir: Some("/from/env".to_string()),
            configured_journal_dir: Some("/from/config".to_string()),
            daemon_managed: true,
            config_error: None,
            daemon_error: None,
            env_override: true,
        };

        let json = serde_json::to_value(&dto).unwrap();

        assert_eq!(json["journalDir"], "/from/env");
        assert_eq!(json["configuredJournalDir"], "/from/config");
        assert_eq!(json["envOverride"], true);
    }

    #[test]
    fn resolve_journal_dir_is_used_for_env_override_detection() {
        // env が優先されるケースでは resolved == env であり、UI 側は
        // `env.is_some()` から env_override を導ける(main.rs 側の配線と同じ前提)。
        let env = Some(PathBuf::from("/from/env"));
        let config = Some(PathBuf::from("/from/config"));
        let resolved = resolve_journal_dir(env.clone(), config);
        assert_eq!(resolved, env);
    }

    #[test]
    fn spawned_daemon_is_owned_and_has_no_error() {
        let startup = DaemonStartup::Spawned;
        assert!(startup.owns_daemon());
        assert_eq!(startup.error(), None);
    }

    #[test]
    fn external_daemon_is_not_owned() {
        // 外部で起動しているデーモンには触らない。これが daemonManaged: false
        // の唯一の正しい意味。
        let startup = DaemonStartup::External;
        assert!(!startup.owns_daemon());
        assert_eq!(startup.error(), None);
    }

    #[test]
    fn failed_spawn_is_still_owned_and_reports_why() {
        // 起動に失敗しただけで「外部のデーモンが動いている」と表示するのは嘘。
        // 責任は依然このアプリにあり、再試行もできなければならない。
        let startup = DaemonStartup::Failed("edlr binary not found".to_string());
        assert!(startup.owns_daemon());
        assert_eq!(startup.error(), Some("edlr binary not found".to_string()));
    }
}

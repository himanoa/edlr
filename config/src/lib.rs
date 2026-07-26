use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const PROTON_JOURNAL_DIR: &str = ".steam/steam/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous";

/// SIGTERM 送信から SIGKILL へ昇格するまでの、サイドカー 1 インスタンスあたりの
/// 猶予(秒)。`edlr-core`(`plugin::host::SIDECAR_SHUTDOWN_GRACE`)と `edlr-ui`
/// (`daemon::STOP_GRACE` の下限を決める根拠)の両方から参照する共有定数。
///
/// この値をここに 1 箇所だけ置くのは、Tauri 側がデーモンを止める猶予
/// (`STOP_GRACE`)を、デーモン自身がサイドカーの後始末に使う猶予
/// (`SIDECAR_SHUTDOWN_GRACE`)より確実に長く保つため。2 つの crate で
/// 別々に定数を持っていると、片方だけ変更されたときに Tauri 側が
/// デーモンより先にタイムアウトして SIGKILL してしまい、デーモンが
/// サイドカーを killpg する前に道連れで消えてサイドカーが孤児として
/// 残る、という Critical な取りこぼしが再発しうる(実際に最終レビューで
/// 一度指摘された)。
pub const SIDECAR_SHUTDOWN_GRACE_SECS: u64 = 3;

/// デーモンの `stop_all`(`drivers/process::ProcessDriver::stop_all`)が
/// 現実的に処理しうる、全プラグイン・全サイドカーの合計インスタンス数の
/// 上限として運用上想定する値。
///
/// `stop_all` は SIGTERM 無視の子がいる場合、インスタンスごとに逐次
/// `SIDECAR_SHUTDOWN_GRACE_SECS` 秒ブロックしうる(`finish_stop` が
/// `taken` を順に処理するため)。したがってデーモン全体の後始末の最悪時間は
/// おおよそ `SIDECAR_SHUTDOWN_GRACE_SECS * SIDECAR_SHUTDOWN_WORST_CASE_INSTANCES`
/// に収まる、という前提を置く。実際の合計インスタンス数はユーザー設定
/// (`replicas` の合計)次第で理論上はこれを超えうるが、edlr は小規模な
/// ローカルプラグイン向けであり、それを大きく超える構成は非現実的とみなす。
/// `ui/src-tauri/src/daemon.rs` の `STOP_GRACE` はこの想定を超えて初めて
/// 「デーモンより先にタイムアウトしない」と言えるため、コンパイル時
/// アサーションでこの関係を固定してある(`daemon.rs` を参照)。
pub const SIDECAR_SHUTDOWN_WORST_CASE_INSTANCES: u64 = 20;

/// 既知の Journal ディレクトリを探す。現状は Proton 既定パスのみ。
pub fn default_journal_dir(home: &Path) -> Option<PathBuf> {
    let candidate = home.join(PROTON_JOURNAL_DIR);
    candidate.is_dir().then_some(candidate)
}

/// `edlr` の設定サブディレクトリ(例: `plugins`, `settings`)の既定パスを組み立てる。
///
/// `<home>/.config/edlr/<sub>` を返す。`$XDG_CONFIG_HOME` を考慮した解決は
/// [`config_subdir`] を使うこと。
pub fn default_config_subdir(home: &Path, sub: &str) -> PathBuf {
    home.join(".config").join("edlr").join(sub)
}

/// 設定ベースディレクトリ(`<base>/edlr`)を解決する内部ヘルパー。
/// `config_subdir` と `config_file_path` が共有する。
fn config_base(xdg_config_home: Option<&Path>, home: Option<&Path>) -> PathBuf {
    match xdg_config_home {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join("edlr"),
        _ => {
            let home = home
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".config").join("edlr")
        }
    }
}

/// `edlr` の設定サブディレクトリの実際の解決ロジック(`$XDG_CONFIG_HOME` 込み)。
///
/// `std::env::var_os` はプロセス全体の環境を読むため、環境変数の読み出しは
/// 呼び出し元(`edlr` バイナリの `main`)で行い、結果をここへ `Option<&Path>` として
/// 渡すことで本関数自体はユニットテスト可能な純粋関数のままにしている。
///
/// 解決順序:
/// 1. `xdg_config_home` が `Some` かつ空文字列でなければそれを設定ベースとして使う
///    (相対パスであっても特に検証はせずそのまま使う。絶対パスへの正規化はしない)
/// 2. そうでなければ `home` があれば `<home>/.config` を設定ベースとする
/// 3. `home` も `None` なら `.`(カレントディレクトリ)を設定ベースとする
///
/// いずれの場合も最終的に `<base>/edlr/<sub>` を返す。
pub fn config_subdir(xdg_config_home: Option<&Path>, home: Option<&Path>, sub: &str) -> PathBuf {
    config_base(xdg_config_home, home).join(sub)
}

/// 設定ファイル `<base>/edlr/config.json` の絶対パスを組み立てる。
pub fn config_file_path(xdg_config_home: Option<&Path>, home: Option<&Path>) -> PathBuf {
    config_base(xdg_config_home, home).join("config.json")
}

/// アプリ全体の設定(`<base>/edlr/config.json`)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// Journal ディレクトリ。`None` ならデーモンの自動検出に委ねる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_dir: Option<PathBuf>,
}

/// `AppConfig` の読み書きが返しうるエラー。
#[derive(Debug)]
pub enum ConfigError {
    /// 読み書き自体の失敗(ファイル不在を除く)。
    Io(io::Error),
    /// JSON として解釈できなかった。既定値へは倒さない。
    Parse(serde_json::Error),
    /// 保存直前のシリアライズに失敗した。
    Serialize(serde_json::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "failed to access config file: {e}"),
            ConfigError::Parse(e) => write!(f, "config file is not valid JSON: {e}"),
            ConfigError::Serialize(e) => write!(f, "failed to serialize config: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl AppConfig {
    /// 設定を読み込む。ファイルが存在しない場合のみ既定値を返す。
    ///
    /// JSON が壊れている場合は `Err(ConfigError::Parse)` を返し、既定値へは
    /// 倒さない。黙って倒すと「設定したのに反映されない」という、本機能が
    /// 解決しようとしている症状そのものになるため。
    pub fn load(path: &Path) -> Result<AppConfig, ConfigError> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(AppConfig::default()),
            Err(e) => return Err(ConfigError::Io(e)),
        };
        serde_json::from_str(&content).map_err(ConfigError::Parse)
    }

    /// 設定を保存する。親ディレクトリが無ければ作る。
    ///
    /// tmp ファイルへ書いてから `rename` することで、書き込み途中のファイルを
    /// 読まれることを防ぐ(`SettingsStore::update_and_effective` と同じ手口)。
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let dir = path.parent().ok_or_else(|| {
            ConfigError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "config path has no parent directory",
            ))
        })?;
        fs::create_dir_all(dir).map_err(ConfigError::Io)?;

        let serialized = serde_json::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        let tmp_path = dir.join(format!("config.json.tmp.{}", std::process::id()));
        fs::write(&tmp_path, serialized).map_err(ConfigError::Io)?;
        fs::rename(&tmp_path, path).map_err(ConfigError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_proton_dir_when_it_exists() {
        let home = tempfile::tempdir().unwrap();
        let proton = home.path().join(
            ".steam/steam/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous",
        );
        std::fs::create_dir_all(&proton).unwrap();
        assert_eq!(default_journal_dir(home.path()), Some(proton));
    }

    #[test]
    fn returns_none_when_absent() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(default_journal_dir(home.path()), None);
    }

    #[test]
    fn config_subdir_joins_home_dot_config_edlr_sub() {
        let home = Path::new("/home/pilot");
        assert_eq!(
            default_config_subdir(home, "plugins"),
            PathBuf::from("/home/pilot/.config/edlr/plugins")
        );
    }

    #[test]
    fn config_subdir_supports_different_sub_names() {
        let home = Path::new("/home/pilot");
        assert_eq!(
            default_config_subdir(home, "settings"),
            PathBuf::from("/home/pilot/.config/edlr/settings")
        );
    }

    #[test]
    fn config_subdir_uses_absolute_xdg_config_home_when_set() {
        let xdg = Path::new("/xdg/config");
        let home = Path::new("/home/pilot");
        assert_eq!(
            config_subdir(Some(xdg), Some(home), "plugins"),
            PathBuf::from("/xdg/config/edlr/plugins")
        );
    }

    #[test]
    fn config_subdir_uses_relative_xdg_config_home_as_is() {
        // 現行実装は XDG_CONFIG_HOME が相対パスかどうかを検証しない
        // (絶対パスへの正規化やフォールバックは行わない)。この挙動を
        // 意図的に維持しているため、その挙動をそのままテストする。
        let xdg = Path::new("relative/xdg");
        let home = Path::new("/home/pilot");
        assert_eq!(
            config_subdir(Some(xdg), Some(home), "plugins"),
            PathBuf::from("relative/xdg/edlr/plugins")
        );
    }

    #[test]
    fn config_subdir_falls_back_to_home_dot_config_when_xdg_unset() {
        let home = Path::new("/home/pilot");
        assert_eq!(
            config_subdir(None, Some(home), "settings"),
            PathBuf::from("/home/pilot/.config/edlr/settings")
        );
    }

    #[test]
    fn config_subdir_falls_back_to_dot_when_xdg_and_home_both_unset() {
        assert_eq!(
            config_subdir(None, None, "plugins"),
            PathBuf::from("./.config/edlr/plugins")
        );
    }

    #[test]
    fn config_subdir_treats_empty_xdg_config_home_as_unset() {
        let xdg = Path::new("");
        let home = Path::new("/home/pilot");
        assert_eq!(
            config_subdir(Some(xdg), Some(home), "plugins"),
            PathBuf::from("/home/pilot/.config/edlr/plugins")
        );
    }

    #[test]
    fn config_file_path_uses_xdg_when_set() {
        assert_eq!(
            config_file_path(Some(Path::new("/xdg/config")), Some(Path::new("/home/pilot"))),
            PathBuf::from("/xdg/config/edlr/config.json")
        );
    }

    #[test]
    fn config_file_path_falls_back_to_home_dot_config() {
        assert_eq!(
            config_file_path(None, Some(Path::new("/home/pilot"))),
            PathBuf::from("/home/pilot/.config/edlr/config.json")
        );
    }

    #[test]
    fn load_returns_default_when_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        assert_eq!(AppConfig::load(&path).unwrap(), AppConfig::default());
    }

    #[test]
    fn load_reads_journal_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, r#"{"journalDir":"/mnt/game/ED"}"#).unwrap();

        let loaded = AppConfig::load(&path).unwrap();

        assert_eq!(loaded.journal_dir, Some(PathBuf::from("/mnt/game/ED")));
    }

    #[test]
    fn load_returns_err_on_broken_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, "not valid json {{{").unwrap();

        let err = AppConfig::load(&path).expect_err("broken json must not fall back to default");

        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn save_creates_dir_and_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("edlr").join("config.json");
        let config = AppConfig {
            journal_dir: Some(PathBuf::from("/mnt/game/ED")),
        };

        config.save(&path).unwrap();

        assert!(path.is_file());
        assert_eq!(AppConfig::load(&path).unwrap(), config);
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("edlr");
        let path = dir.join("config.json");

        AppConfig::default().save(&path).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name != "config.json")
            .collect();
        assert!(leftovers.is_empty(), "unexpected leftovers: {leftovers:?}");
    }
}

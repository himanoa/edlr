use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const PROTON_JOURNAL_DIR: &str = ".steam/steam/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous";

/// SIGTERM 送信から SIGKILL へ昇格するまでの、サイドカー停止 1 回あたりの
/// 猶予(秒)。1 回の `stop_all`/`stop` に含まれる全インスタンスはこの猶予を
/// **共有**する(`drivers/process` の `kill_and_wait_all` が全員へ先に SIGTERM
/// を送り、1 本のデッドラインで待つ)ので、インスタンス数には比例しない。`edlr-core`(`plugin::host::SIDECAR_SHUTDOWN_GRACE`)と `edlr-ui`
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

/// ドライバ 1 呼び出しの期限(秒)。`edlr-core`(`driver::host::DriverInstance::
/// CALL_DEADLINE`)と `edlr-ui`(`STOP_GRACE` のアサーション)の両方が参照する
/// ため、`SIDECAR_SHUTDOWN_GRACE_SECS` と同じくここで共有する。
pub const DRIVER_CALL_DEADLINE_SECS: u64 = 30;

/// プラグインが停止要求を受け取ってから `PluginInstance::call_on_stop` を
/// 呼び終えるまでを `Registry::shutdown_plugins` が待つ上限(秒)。
///
/// **全プラグインでこの猶予を共有する**: `shutdown_plugins` は停止要求を
/// 全件へ先に送ってから、1 つの共有デッドラインで全スレッドを join するので、
/// プラグイン数には比例しない。
///
/// 値は `edlr-core` の `PluginInstance::CALL_DEADLINE`(2 秒)に、スレッドが
/// 停止要求に気づいてから実際に呼び出しに入るまでのスケジューリング遅延分の
/// 余裕(3 秒)を足したもの。停止要求は**ワークキューを追い越す**
/// (`PluginThreadHandle::stop_flag` -- ランナーループがキューを読む前に
/// 確認する)ため、キューに積み残しがあっても on-stop はこの猶予内に到達
/// できる。かつては `Stop` がキュー経由のみで、積み残し (64 - 1) 件 × 2 秒
/// ≈ 126 秒を消化しないと on-stop に辿り着けなかった。
///
/// それでも猶予は best-effort である: 終了時にちょうど実行中だった wasm
/// 呼び出し(応答しないホストへの `driver-http.send` など)がこの猶予内に
/// 返らなければ、warn ログを出して join を諦める。その直後にプロセス自体が
/// 終了するので影響は限定的で、Journal 由来の作業は読み取り位置が永続化
/// されているため次回起動時に replay として再送される。ただし
/// **バス配信(bus delivery)はこの限りではなく、再送されない**。
///
/// `CALL_DEADLINE` を変更した場合はこのコメントの数値も見直すこと
/// (値そのものを共有定数にしていないのは、`edlr_config` を `edlr-core` に
/// 依存させたくないため -- `SIDECAR_SHUTDOWN_GRACE_SECS` と同じ理由)。
///
/// `edlr-core`(`plugin::registry::Registry::shutdown_plugins` の join
/// タイムアウト)と `edlr-ui`(`daemon::STOP_GRACE` のアサーション)の両方が
/// 参照する。
pub const PLUGIN_ON_STOP_GRACE_SECS: u64 = 5;

/// 既知の Journal ディレクトリを探す。現状は Proton 既定パスのみ。
pub fn default_journal_dir(home: &Path) -> Option<PathBuf> {
    let candidate = home.join(PROTON_JOURNAL_DIR);
    candidate.is_dir().then_some(candidate)
}

/// journal ディレクトリの最終フォールバックパスを組み立てる。
///
/// CLI 引数・config.json・[`default_journal_dir`] の自動検出のどれでも
/// 解決できなかったときに、デーモンが「作成して使う」場所。パス計算のみで
/// ディレクトリの作成はしない(作成は `edlr` バイナリ側の仕事)。
///
/// `$XDG_DATA_HOME` が Some かつ非空ならそれを、そうでなければ
/// `<home>/.local/share` をデータベースディレクトリとして
/// `<base>/edlr/journal` を返す。home も無ければ `None`
/// (`config_base` と違いカレントディレクトリには落とさない —
/// 勝手に作る対象が CWD 相対になるのは危険なため)。
pub fn fallback_journal_dir(
    xdg_data_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    match (xdg_data_home, home) {
        (Some(data_home), _) if !data_home.as_os_str().is_empty() => {
            Some(data_home.join("edlr").join("journal"))
        }
        (_, Some(home)) => Some(
            home.join(".local")
                .join("share")
                .join("edlr")
                .join("journal"),
        ),
        _ => None,
    }
}

/// デーモン側の journal ディレクトリ解決。優先順: CLI 引数 →
/// 設定ファイル(`config.json` の `journalDir`)→ 既知パスの自動検出。
///
/// Tauri シェル側の [`resolve_journal_dir`](既定は env → config)とは役割が
/// 違う: あちらは「デーモンへ `--journal-dir` として渡す値」を決め、こちらは
/// デーモン自身が引数なしで起動されたときに同じ設定へフォールバックする
/// ためのもの。デーモン単体起動(`edlr`)でも Tauri 経由でも、設定した
/// journalDir が実効値になることをこの 2 段構えで保証する。
pub fn daemon_journal_dir(
    cli: Option<PathBuf>,
    configured: Option<PathBuf>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    cli.or(configured)
        .or_else(|| home.and_then(default_journal_dir))
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

/// 状態ファイルの置き場所 `<base>/edlr` を解決する。
///
/// XDG 的に「状態」(再作成できるが消えると不便なもの)は config ではなく
/// state に置く。`$XDG_STATE_HOME` が無ければ `~/.local/state`。
pub fn state_base(xdg_state_home: Option<&Path>, home: Option<&Path>) -> PathBuf {
    let base = match (xdg_state_home, home) {
        (Some(state_home), _) => state_home.to_path_buf(),
        (None, Some(home)) => home.join(".local").join("state"),
        (None, None) => PathBuf::from(".local").join("state"),
    };
    base.join("edlr")
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
            config_file_path(
                Some(Path::new("/xdg/config")),
                Some(Path::new("/home/pilot"))
            ),
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
    fn state_base_prefers_xdg_state_home() {
        let base = state_base(Some(Path::new("/x/state")), Some(Path::new("/home/u")));
        assert_eq!(base, Path::new("/x/state/edlr"));
    }

    #[test]
    fn state_base_falls_back_to_local_state_under_home() {
        let base = state_base(None, Some(Path::new("/home/u")));
        assert_eq!(base, Path::new("/home/u/.local/state/edlr"));
    }

    #[test]
    fn state_base_without_home_is_relative_to_the_current_directory() {
        // HOME も XDG_STATE_HOME も無い環境でも panic しない。
        let base = state_base(None, None);
        assert!(base.ends_with("edlr"));
    }

    #[test]
    fn fallback_journal_dir_prefers_xdg_data_home() {
        let dir = fallback_journal_dir(Some(Path::new("/x/data")), Some(Path::new("/home/u")));
        assert_eq!(dir, Some(PathBuf::from("/x/data/edlr/journal")));
    }

    #[test]
    fn fallback_journal_dir_falls_back_to_local_share_under_home() {
        let dir = fallback_journal_dir(None, Some(Path::new("/home/u")));
        assert_eq!(dir, Some(PathBuf::from("/home/u/.local/share/edlr/journal")));
    }

    #[test]
    fn fallback_journal_dir_treats_empty_xdg_data_home_as_unset() {
        let dir = fallback_journal_dir(Some(Path::new("")), Some(Path::new("/home/u")));
        assert_eq!(dir, Some(PathBuf::from("/home/u/.local/share/edlr/journal")));
    }

    #[test]
    fn fallback_journal_dir_none_when_nothing_available() {
        assert_eq!(fallback_journal_dir(None, None), None);
        // 空 XDG + home なしもフォールバック不能
        assert_eq!(fallback_journal_dir(Some(Path::new("")), None), None);
    }

    #[test]
    fn daemon_journal_dir_prefers_cli_arg() {
        let cli = PathBuf::from("/from/cli");
        let configured = PathBuf::from("/from/config");
        assert_eq!(
            daemon_journal_dir(Some(cli.clone()), Some(configured), None),
            Some(cli)
        );
    }

    #[test]
    fn daemon_journal_dir_uses_config_when_no_cli_arg() {
        let configured = PathBuf::from("/from/config");
        assert_eq!(
            daemon_journal_dir(None, Some(configured.clone()), None),
            Some(configured)
        );
    }

    #[test]
    fn daemon_journal_dir_falls_back_to_auto_detection() {
        let home = tempfile::tempdir().unwrap();
        let proton = home.path().join(PROTON_JOURNAL_DIR);
        std::fs::create_dir_all(&proton).unwrap();
        assert_eq!(
            daemon_journal_dir(None, None, Some(home.path())),
            Some(proton)
        );
    }

    #[test]
    fn daemon_journal_dir_none_when_nothing_resolves() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(daemon_journal_dir(None, None, Some(home.path())), None);
        assert_eq!(daemon_journal_dir(None, None, None), None);
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

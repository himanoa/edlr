use std::path::{Path, PathBuf};

const PROTON_JOURNAL_DIR: &str = ".steam/steam/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous";

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
    match xdg_config_home {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join("edlr").join(sub),
        _ => {
            let home = home
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            default_config_subdir(&home, sub)
        }
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
}

use std::path::{Path, PathBuf};

const PROTON_JOURNAL_DIR: &str = ".steam/steam/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous";

/// 既知の Journal ディレクトリを探す。現状は Proton 既定パスのみ。
pub fn default_journal_dir(home: &Path) -> Option<PathBuf> {
    let candidate = home.join(PROTON_JOURNAL_DIR);
    candidate.is_dir().then_some(candidate)
}

/// `edlr` の設定サブディレクトリ(例: `plugins`, `settings`)の既定パスを組み立てる。
///
/// `<home>/.config/edlr/<sub>` を返す(`$XDG_CONFIG_HOME` が設定されている場合の
/// 上書きは呼び出し側の責務。`std::env::var_os` はプロセス全体の環境を読むため
/// 純粋関数であるここには持ち込まず、呼び出し元で `XDG_CONFIG_HOME` を読んで
/// 絶対パスがあればそちらを優先し、なければこの関数の戻り値を使う)。
pub fn default_config_subdir(home: &Path, sub: &str) -> PathBuf {
    home.join(".config").join("edlr").join(sub)
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
}

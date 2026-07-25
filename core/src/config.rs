use std::path::{Path, PathBuf};

const PROTON_JOURNAL_DIR: &str = ".steam/steam/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous";

/// 既知の Journal ディレクトリを探す。現状は Proton 既定パスのみ。
pub fn default_journal_dir(home: &Path) -> Option<PathBuf> {
    let candidate = home.join(PROTON_JOURNAL_DIR);
    candidate.is_dir().then_some(candidate)
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
}

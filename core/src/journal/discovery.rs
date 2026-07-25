use std::io;
use std::path::{Path, PathBuf};

/// dir 内の最新 Journal ファイルを返す。ファイル名はタイムスタンプを含むため辞書順最大が最新。
pub fn latest_journal(dir: &Path) -> io::Result<Option<PathBuf>> {
    let mut latest: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(name.starts_with("Journal.") && name.ends_with(".log")) {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if latest.as_ref().is_none_or(|l| path > *l) {
            latest = Some(path);
        }
    }
    Ok(latest)
}

/// current より辞書順で次の Journal ファイルを返す。
/// 複数回転を順番に追跡する場合に使用。
pub fn next_journal_after(dir: &Path, current: &Path) -> io::Result<Option<PathBuf>> {
    let mut next: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(name.starts_with("Journal.") && name.ends_with(".log")) {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if path <= *current {
            continue;
        }
        if next.as_ref().is_none_or(|n| path < *n) {
            next = Some(path);
        }
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_lexicographically_latest_journal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Journal.2026-07-24T090000.01.log"), "").unwrap();
        std::fs::write(dir.path().join("Journal.2026-07-25T120000.01.log"), "").unwrap();
        std::fs::write(dir.path().join("Status.json"), "").unwrap();
        let latest = latest_journal(dir.path()).unwrap().unwrap();
        assert_eq!(
            latest.file_name().unwrap().to_str().unwrap(),
            "Journal.2026-07-25T120000.01.log"
        );
    }

    #[test]
    fn returns_none_when_no_journal() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(latest_journal(dir.path()).unwrap(), None);
    }

    #[test]
    fn finds_next_journal_after_current() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("Journal.2026-07-25T100000.01.log");
        let mid = dir.path().join("Journal.2026-07-25T110000.01.log");
        let new = dir.path().join("Journal.2026-07-25T120000.01.log");
        std::fs::write(&old, "").unwrap();
        std::fs::write(&mid, "").unwrap();
        std::fs::write(&new, "").unwrap();

        let next = next_journal_after(dir.path(), &old).unwrap();
        assert_eq!(next, Some(mid.clone()));

        let next = next_journal_after(dir.path(), &mid).unwrap();
        assert_eq!(next, Some(new.clone()));

        let next = next_journal_after(dir.path(), &new).unwrap();
        assert_eq!(next, None);
    }

    #[test]
    fn ignores_directories_matching_journal_naming_pattern() {
        let dir = tempfile::tempdir().unwrap();
        // ディレクトリなのに Journal.*.log という名前を持つ紛らわしいエントリ
        std::fs::create_dir(dir.path().join("Journal.2026-07-25T130000.01.log")).unwrap();
        let real = dir.path().join("Journal.2026-07-25T120000.01.log");
        std::fs::write(&real, "").unwrap();

        let latest = latest_journal(dir.path()).unwrap().unwrap();
        assert_eq!(latest, real);

        let old = dir.path().join("Journal.2026-07-25T100000.01.log");
        std::fs::write(&old, "").unwrap();
        let next = next_journal_after(dir.path(), &old).unwrap();
        assert_eq!(next, Some(real));
    }
}

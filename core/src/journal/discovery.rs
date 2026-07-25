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
        if latest.as_ref().is_none_or(|l| path > *l) {
            latest = Some(path);
        }
    }
    Ok(latest)
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
}

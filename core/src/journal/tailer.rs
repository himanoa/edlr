use super::discovery::latest_journal;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

/// Journal ディレクトリを tail する。position 追跡により読み取りは冪等。
pub struct JournalTailer {
    dir: PathBuf,
    current: Option<PathBuf>,
    pos: u64,
    partial: String,
}

impl JournalTailer {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, current: None, pos: 0, partial: String::new() }
    }

    /// 追記された完全な行を返す。新しい Journal が現れたら旧ファイルを読み切って切り替える。
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        let mut lines = Vec::new();
        let latest = latest_journal(&self.dir)?;
        if let Some(cur) = self.current.clone() {
            self.read_new(&cur, &mut lines)?;
        }
        if latest != self.current {
            // 切り替え: 旧ファイルは読み切り済みなので新ファイルを先頭から
            self.current = latest;
            self.pos = 0;
            self.partial.clear();
            if let Some(cur) = self.current.clone() {
                self.read_new(&cur, &mut lines)?;
            }
        }
        Ok(lines)
    }

    fn read_new(&mut self, path: &std::path::Path, lines: &mut Vec<String>) -> io::Result<()> {
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return Ok(()), // 消えた/一時的に開けない → 次回リトライ
        };
        let len = file.metadata()?.len();
        if len < self.pos {
            // truncate された → 先頭から読み直す
            self.pos = 0;
            self.partial.clear();
        }
        file.seek(SeekFrom::Start(self.pos))?;
        let mut chunk = String::new();
        file.read_to_string(&mut chunk)?;
        self.pos = len;
        self.partial.push_str(&chunk);
        while let Some(nl) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=nl).collect();
            let line = line.trim_end();
            if !line.is_empty() {
                lines.push(line.to_string());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;

    fn append(path: &std::path::Path, s: &str) {
        let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn reads_only_appended_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let j = dir.path().join("Journal.2026-07-25T120000.01.log");
        append(&j, "line1\nline2\n");
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        assert_eq!(t.poll().unwrap(), vec!["line1", "line2"]);
        assert_eq!(t.poll().unwrap(), Vec::<String>::new()); // 追記なし → 空
        append(&j, "line3\npart"); // 書きかけ行は返さない
        assert_eq!(t.poll().unwrap(), vec!["line3"]);
        append(&j, "ial\n"); // 書きかけの続き
        assert_eq!(t.poll().unwrap(), vec!["partial"]);
    }

    #[test]
    fn follows_rotation_to_newer_file() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("Journal.2026-07-25T120000.01.log");
        append(&old, "old1\n");
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        assert_eq!(t.poll().unwrap(), vec!["old1"]);
        append(&old, "old2\n"); // 新ファイル出現と同時に旧ファイルにも追記済みのケース
        let new = dir.path().join("Journal.2026-07-25T130000.01.log");
        append(&new, "new1\n");
        assert_eq!(t.poll().unwrap(), vec!["old2", "new1"]);
    }

    #[test]
    fn restarts_from_top_on_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let j = dir.path().join("Journal.2026-07-25T120000.01.log");
        append(&j, "aaaa\nbbbb\n");
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        t.poll().unwrap();
        std::fs::write(&j, "cc\n").unwrap(); // 短縮
        assert_eq!(t.poll().unwrap(), vec!["cc"]);
    }

    #[test]
    fn empty_dir_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        assert_eq!(t.poll().unwrap(), Vec::<String>::new());
    }
}

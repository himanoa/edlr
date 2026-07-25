use super::discovery::{latest_journal, next_journal_after};
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
    /// 複数回転がある場合は順番に全ファイルを読む。
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        let mut lines = Vec::new();

        // 現在のファイルから読む
        if let Some(cur) = self.current.clone() {
            if let Err(e) = self.read_new(&cur, &mut lines) {
                // 現在のファイルの読み取り失敗かつ行が未収集 → エラー返す
                if lines.is_empty() {
                    return Err(e);
                }
                // 行が収集済みならば警告して返す
                eprintln!("warning: failed to read current journal: {}", e);
                return Ok(lines);
            }
        }

        // 次のファイルを探して順番に読む
        loop {
            let latest = latest_journal(&self.dir)?;
            let next = if let Some(cur) = &self.current {
                next_journal_after(&self.dir, cur)?
            } else {
                latest.clone()
            };

            if let Some(next_path) = next {
                // 新ファイルへ切り替え
                self.current = Some(next_path.clone());
                self.pos = 0;
                self.partial.clear();

                if let Err(e) = self.read_new(&next_path, &mut lines) {
                    // 新ファイルの読み取り失敗
                    if lines.is_empty() {
                        // 行がまだ収集されていなければエラー返す
                        return Err(e);
                    }
                    // 行が収集済みならば警告して返す
                    eprintln!("warning: failed to read rotated journal: {}", e);
                    return Ok(lines);
                }
            } else {
                // これ以上新しいファイルなし → 完了
                break;
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

    #[test]
    fn follows_multiple_rotations_in_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("Journal.2026-07-25T100000.01.log");
        append(&old, "old_line1\n");
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        assert_eq!(t.poll().unwrap(), vec!["old_line1"]);

        // 次回のpoll前に2つの新ファイルを作成
        append(&old, "old_line2\n"); // 旧ファイルにもさらに追記
        let mid = dir.path().join("Journal.2026-07-25T110000.01.log");
        append(&mid, "mid_line1\n");
        let new = dir.path().join("Journal.2026-07-25T120000.01.log");
        append(&new, "new_line1\n");

        // 次のpollで全ファイルを順番に読む
        assert_eq!(
            t.poll().unwrap(),
            vec!["old_line2", "mid_line1", "new_line1"]
        );
    }
}

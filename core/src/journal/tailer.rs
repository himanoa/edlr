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
        Self {
            dir,
            current: None,
            pos: 0,
            partial: String::new(),
        }
    }

    /// 追記された完全な行を返す。新しい Journal が現れたら旧ファイルを読み切って切り替える。
    /// 複数回転がある場合は順番に全ファイルを読む。
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        let mut lines = Vec::new();

        // 現在のファイルから読む
        if let Some(cur) = self.current.clone() {
            if let Err(e) = self.read_new(&cur, &mut lines) {
                // 現在のファイルの読み取りに失敗しても、ここで即座にエラーを
                // 返すとローテーション検出処理に到達できず、新しい Journal
                // ファイルが永遠に発見されなくなってしまう。警告を出しつつ
                // ローテーション探索へフォールスルーする。
                tracing::warn!("failed to read current journal: {}", e);
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
                    tracing::warn!("failed to read rotated journal: {}", e);
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
        // 実際に読んだバイト数だけ position を進める。metadata() 取得後に
        // ファイルへ追記があった場合でも、read_to_string は EOF まで読むため
        // len を使うと次回ポーリングで既読分を取りこぼす/重複する。
        self.pos += chunk.len() as u64;
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
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
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

    /// pos は metadata() で取得した len ではなく、実際に read_to_string で
    /// 読んだバイト数だけ進めなければならない。もし len を使うと、
    /// metadata() 取得後・read_to_string 実行前にファイルへ追記された分は
    /// 今回の poll で読み取られてしまうにもかかわらず pos には反映されず、
    /// 次回 poll で同じ行が再送出されてしまう(重複配信)。
    #[test]
    fn pos_advances_by_bytes_actually_read_not_stale_metadata_len() {
        let dir = tempfile::tempdir().unwrap();
        let j = dir.path().join("Journal.2026-07-25T120000.01.log");
        append(&j, "hello\n"); // 6 bytes
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        let mut lines = Vec::new();
        t.read_new(&j, &mut lines).unwrap();
        assert_eq!(t.pos, 6);
        assert_eq!(lines, vec!["hello"]);

        append(&j, "world\n"); // さらに6バイト追記(合計12バイト)
        let mut lines2 = Vec::new();
        t.read_new(&j, &mut lines2).unwrap();
        // pos はこれまでの pos + 今回読んだバイト数(6) = 12 になるべき。
        // len() をそのまま使う実装でも今回はたまたま一致してしまうが、
        // 「pos は読んだバイト数の積み上げ」という不変条件をここで固定する。
        assert_eq!(t.pos, 12);
        assert_eq!(lines2, vec!["world"]);
    }

    /// ファイルが複数回のポーリングをまたいで少しずつ追記される場合でも、
    /// 一度返した行が再び返される(重複配信される)ことがあってはならない。
    #[test]
    fn no_duplicate_lines_across_successive_polls_as_file_grows() {
        let dir = tempfile::tempdir().unwrap();
        let j = dir.path().join("Journal.2026-07-25T120000.01.log");
        append(&j, "a\n");
        let mut t = JournalTailer::new(dir.path().to_path_buf());

        let mut all = Vec::new();
        all.extend(t.poll().unwrap());
        append(&j, "b\n");
        all.extend(t.poll().unwrap());
        append(&j, "c\n");
        all.extend(t.poll().unwrap());

        assert_eq!(all, vec!["a", "b", "c"]);
        // 追記がない状態でさらに poll しても重複しない
        assert_eq!(t.poll().unwrap(), Vec::<String>::new());
    }

    /// 現在ファイルの読み取りが失敗し続けても、ローテーション探索処理へ
    /// フォールスルーして新しい Journal ファイルを発見できなければならない。
    /// (現在ファイルを同名のディレクトリに置き換えることで読み取り失敗を再現する)
    #[test]
    fn continues_rotation_discovery_after_current_file_read_error() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("Journal.2026-07-25T100000.01.log");
        append(&old, "old1\n");
        let mut t = JournalTailer::new(dir.path().to_path_buf());
        assert_eq!(t.poll().unwrap(), vec!["old1"]);

        // 現在ファイルを削除して同名のディレクトリに置き換える → 以後の読み取りは
        // 「ディレクトリを read しようとしてエラー」になる。
        std::fs::remove_file(&old).unwrap();
        std::fs::create_dir(&old).unwrap();

        let new = dir.path().join("Journal.2026-07-25T110000.01.log");
        append(&new, "new1\n");

        // 現在ファイル(ディレクトリ)の読み取りに失敗しても、poll は
        // エラーを返さずローテーション探索を継続し、新ファイルの行を返す。
        assert_eq!(t.poll().unwrap(), vec!["new1"]);
    }
}

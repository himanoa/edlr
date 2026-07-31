use super::discovery::{latest_journal, next_journal_after};
use super::position::Position;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

/// tail で読み取った 1 行と、その行がデーモン起動前に既に書かれていたか。
#[derive(Debug, Clone, PartialEq)]
pub struct JournalLine {
    pub text: String,
    /// デーモンが動き出す前に既にファイルへ書かれていた行。
    pub replay: bool,
}

/// Journal ディレクトリを tail する。position 追跡により読み取りは冪等。
pub struct JournalTailer {
    dir: PathBuf,
    current: Option<PathBuf>,
    pos: u64,
    partial: String,
    /// 最初の poll を終えたか。起動直後の 1 回目で読み切った分までを
    /// `replay` とし、それ以降の追記を live とするための境界。
    caught_up: bool,
}

impl JournalTailer {
    pub fn new(dir: PathBuf) -> Self {
        Self::resume_from(dir, None)
    }

    /// 保存された `Position` から読み始める。`position` が `None` なら
    /// 最新の Journal を先頭から読む(従来の `new` と同じ挙動)。
    ///
    /// ここではファイルの存在確認をしない。指すファイルが既に消えている
    /// 場合の復旧(次のファイルへ進む、無ければ最新へフォールバック)は
    /// `poll` のローテーション処理に任せる。
    pub fn resume_from(dir: PathBuf, position: Option<Position>) -> Self {
        let (current, pos) = match position {
            Some(p) => (Some(dir.join(&p.file)), p.offset),
            None => (None, 0),
        };
        Self {
            dir,
            current,
            pos,
            partial: String::new(),
            caught_up: false,
        }
    }

    /// 保存すべき位置(最後の完全な行の直後)。まだ何も読んでいなければ `None`。
    ///
    /// `pos` は読み込んだバイト数ぶん進んでおり、行の途中で切れた分は
    /// `partial` にメモリ上で保持している。`pos` をそのまま保存すると、
    /// 再起動時に `partial` が失われ、その行が頭を欠いた状態で読まれて
    /// しまう(パーサが警告して捨てる = イベントが 1 つ消える)。
    pub fn position(&self) -> Option<Position> {
        let current = self.current.as_ref()?;
        let file = current.file_name()?.to_str()?.to_string();
        Some(Position {
            file,
            offset: self.pos - self.partial.len() as u64,
        })
    }

    /// 追記された完全な行を返す。新しい Journal が現れたら旧ファイルを読み切って切り替える。
    /// 複数回転がある場合は順番に全ファイルを読む。
    pub fn poll(&mut self) -> io::Result<Vec<JournalLine>> {
        let mut lines = Vec::new();

        // 現在のファイルが既に無くなっている場合、次のファイルへ進む。
        //
        // 次が無いときのフォールバック(最新ファイル)は、それが**消えたファイル
        // より厳密に新しいときだけ**採る。より古いファイルへ巻き戻すと、そこから
        // ローテーションのループが前へ歩いてディレクトリ全体を先頭から読み直し、
        // しかも `caught_up` が既に true なので全てが `replay = false`(= 今
        // 起きたイベント)として再配信されてしまう。新しいファイルが現れるまでは
        // 現在位置を据え置き、次の poll で再試行する。
        if let Some(cur) = &self.current {
            if !cur.is_file() {
                let next = match next_journal_after(&self.dir, cur)? {
                    Some(next) => Some(next),
                    None => latest_journal(&self.dir)?.filter(|latest| latest > cur),
                };
                if let Some(next) = next {
                    self.current = Some(next);
                    self.pos = 0;
                    self.partial.clear();
                }
            }
        }

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
            let latest = match latest_journal(&self.dir) {
                Ok(latest) => latest,
                Err(e) => {
                    return Self::interrupted(lines, e, "failed to scan the journal directory")
                }
            };
            let next = if let Some(cur) = &self.current {
                match next_journal_after(&self.dir, cur) {
                    Ok(next) => next,
                    Err(e) => {
                        return Self::interrupted(lines, e, "failed to scan the journal directory")
                    }
                }
            } else {
                latest.clone()
            };

            if let Some(next_path) = next {
                // 新ファイルへ切り替え
                self.current = Some(next_path.clone());
                self.pos = 0;
                self.partial.clear();

                if let Err(e) = self.read_new(&next_path, &mut lines) {
                    // 新ファイルの読み取り失敗。ここで先へ進むと、このファイルは
                    // 二度と読まれない(current は既に進んでいる)ので、必ず
                    // ここで打ち切って次の poll で同じファイルを読み直す。
                    return Self::interrupted(lines, e, "failed to read rotated journal");
                }
            } else {
                // これ以上新しいファイルなし → 完了
                break;
            }
        }

        self.caught_up = true;
        Ok(lines)
    }

    /// poll の途中で走査・読み取りが失敗したときの打ち切り。
    ///
    /// 既に読んだ行がある場合は、それを捨てずに返す。捨ててしまうと `self.pos`
    /// だけが進んでいるため、その行は(動き続けるデーモンでは)二度と配信されない。
    /// `caught_up` はここでは立てない — まだ追いつけていないので、残りは次の
    /// poll で `replay` として読む。
    fn interrupted(
        lines: Vec<JournalLine>,
        e: io::Error,
        what: &str,
    ) -> io::Result<Vec<JournalLine>> {
        if lines.is_empty() {
            return Err(e);
        }
        tracing::warn!("{what}: {e}; returning the lines read so far");
        Ok(lines)
    }

    fn read_new(&mut self, path: &std::path::Path, lines: &mut Vec<JournalLine>) -> io::Result<()> {
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            // 消えたファイルは飛ばしてよい(ローテーション処理が次へ進める)。
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            // 一時的に開けないだけ(EACCES / EMFILE など)の場合は握り潰さない。
            // ローテーションのループは既に current をこのファイルへ進めているため、
            // Ok を返すとそのままさらに次のファイルへ進み、このファイルの内容が
            // 恒久的にスキップされてしまう。
            Err(e) => return Err(e),
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
                lines.push(JournalLine {
                    text: line.to_string(),
                    replay: !self.caught_up,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

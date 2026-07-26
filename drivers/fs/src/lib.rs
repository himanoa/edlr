//! 承認済みルートディレクトリ配下に限ってファイルを操作するドライバ。
//!
//! 呼び出し元(edlr のプラグインホスト)は「どのルートか」と「その配下の
//! 相対パス」だけを渡す。ルートの外へ出る経路が無いことをこのクレートが
//! 保証する。承認そのもの(誰がどのルートを使ってよいか)は呼び出し元の
//! 責務で、このクレートは grants を知らない。

pub mod openat;
pub mod path;

use std::fmt;

#[derive(Debug)]
pub enum FsError {
    /// 構文が不正、またはルート配下から出ている。
    InvalidPath(String),
    NotFound(String),
    TooLarge(String),
    Io(String),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsError::InvalidPath(m) => write!(f, "invalid path: {m}"),
            FsError::NotFound(m) => write!(f, "not found: {m}"),
            FsError::TooLarge(m) => write!(f, "too large: {m}"),
            FsError::Io(m) => write!(f, "io error: {m}"),
        }
    }
}

impl std::error::Error for FsError {}

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::UNIX_EPOCH;

/// [`FsDriver::list`] が降りるディレクトリ階層の上限。
///
/// 再帰の停止条件。ルート配下にはいくらでも深いツリーを作れてしまうので、
/// 深さで止める(スタックの深さと、同時に保持する fd 数の上限でもある)。
pub const MAX_LIST_DEPTH: usize = 64;

/// [`FsDriver::list`] が 1 回の呼び出しで見てよいディレクトリエントリの総数。
///
/// `list_limit` は「返す通常ファイルの数」しか数えないので、サブディレクトリ・
/// シンボリックリンク・FIFO だけでできた巨大ツリーは上限に一度も触れないまま
/// 全走査されてしまう(しかも 1 エントリごとに `statat` を発行する)。
/// プラグイン自身が `write` を繰り返すだけでこの状態を作れるので、返却数とは
/// **別に**走査量の予算を持つ。超えたら `too-large`。
pub const MAX_LIST_SCAN: usize = 100_000;

/// 1 ファイル分のメタデータ。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// ルートからの相対パス(区切りは `/`)。
    pub path: String,
    pub size: u64,
    /// Unix epoch 秒。取得できなければ `None`。
    pub modified: Option<u64>,
}

/// 承認済みルート配下でのファイル操作。
///
/// `root` は呼び出しごとに渡される(プラグインごと・エントリごとに違うため)。
/// このドライバは承認状態を知らない — 呼び出してよいか、read か read-write か
/// の判断は呼び出し元(`core` 側のホスト実装)が行う。
///
/// どの操作も 3 段の検証を通る: 構文検証 → 正規化後の配下チェック
/// (どちらも [`path`])→ [`openat`] による `openat2` 拘束。
pub struct FsDriver {
    read_limit: usize,
    list_limit: usize,
    scan_limit: usize,
}

impl FsDriver {
    pub fn new(read_limit: usize, list_limit: usize) -> FsDriver {
        FsDriver {
            read_limit,
            list_limit,
            scan_limit: MAX_LIST_SCAN,
        }
    }

    /// 走査予算([`MAX_LIST_SCAN`])を差し替える。テストと、将来ホスト側から
    /// 調整したくなった場合のため。
    pub fn with_scan_limit(mut self, scan_limit: usize) -> FsDriver {
        self.scan_limit = scan_limit;
        self
    }

    pub fn read(&self, root: &Path, rel: &str) -> Result<Vec<u8>, FsError> {
        let (dir, name) = self.locate_existing(root, rel)?;
        let mut file = dir.open_read(&name)?;

        // 上限判定は開いた後の fd から取る。開く前の `metadata` を信じると
        // 判定した相手と読む相手が別物になりうる。
        let size = file
            .metadata()
            .map_err(|e| FsError::Io(e.to_string()))?
            .len();
        if size as usize > self.read_limit {
            return Err(FsError::TooLarge(format!(
                "{rel} is {size} bytes, over the {} byte read limit; use read-range",
                self.read_limit
            )));
        }

        // `read_to_end` は上限を見ないので、`take` で上限 + 1 バイトに
        // 制限してから読む。`fstat` の後にファイルが伸びても(`append` に
        // サイズ上限は無い)、上限を超えて確保することは無い。
        let mut buf = Vec::with_capacity(size as usize);
        (&mut file)
            .take(self.read_limit as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| FsError::Io(e.to_string()))?;
        if buf.len() > self.read_limit {
            // 開いてから読み終えるまでに伸びた場合。
            return Err(FsError::TooLarge(format!(
                "{rel} grew past the {} byte read limit while being read",
                self.read_limit
            )));
        }
        Ok(buf)
    }

    pub fn read_range(
        &self,
        root: &Path,
        rel: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, FsError> {
        if len as usize > self.read_limit {
            return Err(FsError::TooLarge(format!(
                "requested {len} bytes, over the {} byte read limit",
                self.read_limit
            )));
        }

        let (dir, name) = self.locate_existing(root, rel)?;
        let mut file = dir.open_read(&name)?;
        let size = file
            .metadata()
            .map_err(|e| FsError::Io(e.to_string()))?
            .len();
        if offset >= size {
            // 末尾より後ろは「読めるものが無い」だけでエラーにしない。
            return Ok(Vec::new());
        }

        file.seek(SeekFrom::Start(offset))
            .map_err(|e| FsError::Io(e.to_string()))?;
        let want = std::cmp::min(len as u64, size - offset) as usize;
        let mut buf = vec![0u8; want];
        let mut filled = 0;
        while filled < want {
            match file.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Err(FsError::Io(e.to_string())),
            }
        }
        buf.truncate(filled);
        Ok(buf)
    }

    pub fn stat(&self, root: &Path, rel: &str) -> Result<Entry, FsError> {
        let (dir, name) = self.locate_existing(root, rel)?;
        // `open_read` 経由なので、シンボリックリンクも、通常ファイル以外
        // (ディレクトリ・FIFO・デバイス・ソケット)も stat できない。
        // `list` が返すのは通常ファイルだけなので、`stat` の対象も揃う。
        let file = dir.open_read(&name)?;
        let meta = file.metadata().map_err(|e| FsError::Io(e.to_string()))?;
        Ok(Entry {
            path: rel.to_string(),
            size: meta.len(),
            modified: modified_secs(&meta),
        })
    }

    /// `prefix` 配下を再帰的に列挙する。ファイルのみを返し、ディレクトリ
    /// 自体は含めない。`prefix` が空文字ならルート直下から。
    ///
    /// 走査は最初から最後まで fd 基準で行う(起点は `openat::Dir::open`、
    /// 降下は親 fd 相対の `openat2`)。パス文字列を組み立てて `read_dir` を
    /// 呼ぶと、ディレクトリを見つけてから開くまでの間にシンボリックリンクへ
    /// 差し替えられ、ルート外のファイル名・サイズ・更新時刻が「ルート配下の
    /// 相対パス」として返りうる。`prefix` 自体がリンクの場合も同じ理由で
    /// 拒否される(`read` と同じ扱い)。
    ///
    /// シンボリックリンクは、リンク先がルート内でも列挙しない。
    ///
    /// 走査は深さ優先で、子へ降りる前に兄弟の fd を開かない。同時に保持する
    /// fd はツリーの**深さ**に比例する(幅ではない)。幅に比例させると、
    /// ディレクトリだけの浅く広いツリー(プラグインが `write` を繰り返すだけで
    /// 作れる)に対して `list` 1 回でプロセス全体の fd を食い潰せてしまう —
    /// `list_limit` は通常ファイルしか数えないので、それでは止まらない。
    /// 深さには [`MAX_LIST_DEPTH`] の上限を掛ける。
    pub fn list(&self, root: &Path, prefix: &str) -> Result<Vec<Entry>, FsError> {
        // 1・2 段目 + 3 段目。`prefix` がリンクならここで `InvalidPath`。
        let base = openat::Dir::open(root, prefix)?;

        let mut entries = Vec::new();
        let mut scanned = 0usize;
        self.walk(&base, prefix, 0, &mut entries, &mut scanned)?;
        Ok(entries)
    }

    /// [`FsDriver::list`] の再帰本体。`dir` の fd は再帰の間だけ生きており、
    /// 子から戻った時点で `drop` される。
    ///
    /// `scanned` は「見たディレクトリエントリの総数」。返す通常ファイルだけを
    /// 数える `list_limit` と違い、ディレクトリ・シンボリックリンク・FIFO も
    /// 数える([`MAX_LIST_SCAN`] 参照)。
    fn walk(
        &self,
        dir: &openat::Dir,
        dir_path: &str,
        depth: usize,
        entries: &mut Vec<Entry>,
        scanned: &mut usize,
    ) -> Result<(), FsError> {
        if depth >= MAX_LIST_DEPTH {
            return Err(FsError::TooLarge(format!(
                "{dir_path:?} is deeper than the {MAX_LIST_DEPTH} directory level limit"
            )));
        }

        // 子へ降りる前に、このディレクトリの一覧を確定させる。降りている
        // 間このディレクトリの dirent ストリームを開いたままにしないため。
        // 予算判定は `entries` の中(dirent を読み進めながら)で行う。ここで
        // 返ってきた `Vec` を数えたのでは、単一ディレクトリに大量のエントリを
        // 置かれたときに予算が効かない。
        let mut subdirectories = Vec::new();
        for item in dir.entries(scanned, self.scan_limit)? {
            let relative = if dir_path.is_empty() {
                item.name.clone()
            } else {
                format!("{dir_path}/{}", item.name)
            };

            match item.kind {
                openat::EntryKind::RegularFile => {}
                openat::EntryKind::Directory => {
                    // ここではまだ開かない(開くと幅に比例した fd を抱える)。
                    subdirectories.push((item.name, relative));
                    continue;
                }
                // シンボリックリンク・FIFO・デバイスは列挙しない。
                openat::EntryKind::Other => continue,
            }

            entries.push(Entry {
                path: relative,
                size: item.size,
                modified: item.modified,
            });
            if entries.len() > self.list_limit {
                return Err(FsError::TooLarge(format!(
                    "more than {} entries under {dir_path:?}",
                    self.list_limit
                )));
            }
        }

        for (name, relative) in subdirectories {
            // `open_dir` は親 fd 相対に `O_NOFOLLOW` で開くので、列挙してから
            // 開くまでにリンクへ差し替えられていれば `None` になって
            // 読み飛ばされる。
            if let Some(child) = dir.open_dir(&name)? {
                self.walk(&child, &relative, depth + 1, entries, scanned)?;
                // ここで `child` が drop され、次の兄弟を開く前に fd が返る。
            }
        }
        Ok(())
    }

    /// 原子的に書き込む。同一ディレクトリに tmp を作って `rename` する
    /// ので、読み手が半端な内容を見ることはない。
    ///
    /// tmp は `O_CREAT | O_EXCL | O_NOFOLLOW` で作る。tmp 名を先回りして
    /// シンボリックリンクとして置かれても、リンクを辿って書くことはない
    /// (同じディレクトリへ書ける別プロセスからの古典的な攻撃)。
    ///
    /// tmp 名は呼び出しごとに一意なので([`tmp_name`])、居座りが起きるのは
    /// 「攻撃者がその一意な名前を当てて先回りした」場合だけ。その場合は
    /// 素直に失敗させる(消して作り直すと、同一パスへの並行 `write` 同士が
    /// 互いの tmp を消し合う競合をわざわざ作ってしまう)。
    pub fn write(&self, root: &Path, rel: &str, bytes: &[u8]) -> Result<(), FsError> {
        let (dir, name) = self.locate_for_write(root, rel)?;
        let tmp = tmp_name(&name);

        let mut file = dir
            .create_new(&tmp)?
            .ok_or_else(|| FsError::Io(format!("{tmp} already exists while writing {rel}")))?;

        if let Err(e) = file.write_all(bytes) {
            drop(file);
            let _ = dir.unlink(&tmp);
            return Err(FsError::Io(e.to_string()));
        }
        drop(file);

        dir.rename(&tmp, &name).inspect_err(|_| {
            let _ = dir.unlink(&tmp);
        })
    }

    /// 追記する。原子的ではない(ログ用途では途中で切れても後ろに足される
    /// だけなので許容する)。
    ///
    /// 「リンクかどうか確認してから開く」のではなく `O_APPEND | O_CREAT |
    /// O_NOFOLLOW` で一度に開く。確認と open の間に差し替えられる隙が
    /// 無いようにするため。
    pub fn append(&self, root: &Path, rel: &str, bytes: &[u8]) -> Result<(), FsError> {
        let (dir, name) = self.locate_for_write(root, rel)?;
        let mut file = dir.open_append(&name)?;
        file.write_all(bytes)
            .map_err(|e| FsError::Io(e.to_string()))
    }

    pub fn delete(&self, root: &Path, rel: &str) -> Result<(), FsError> {
        let (dir, name) = self.locate_existing(root, rel)?;
        dir.unlink(&name)
    }

    /// 既存パスを 1・2 段目まで通し、親ディレクトリのハンドルと最終要素名を
    /// 返す。ディレクトリを作りはしない。
    fn locate_existing(&self, root: &Path, rel: &str) -> Result<(openat::Dir, String), FsError> {
        // 存在しないものは `NotFound`、触ってはいけないものは `InvalidPath`。
        // この区別は `path` 側で付いているので、ここでは潰さない。
        path::resolve_existing(root, rel)?;
        let (parent_rel, name) = split_parent(rel)?;
        Ok((openat::Dir::open(root, &parent_rel)?, name))
    }

    /// 書き込み先を 1・2 段目まで通し、親ディレクトリのハンドルと最終要素名を
    /// 返す。親が無ければ作る。
    ///
    /// 返す `name` は「まだ何であるか分からない」最終要素なので、必ず
    /// [`openat::Dir`] 経由で開くこと。`Dir` の外へパスを組み立てて出さない
    /// のがこの関数の役目でもある。
    fn locate_for_write(&self, root: &Path, rel: &str) -> Result<(openat::Dir, String), FsError> {
        path::resolve_parent(root, rel)?;
        let (parent_rel, name) = split_parent(rel)?;
        Ok((openat::Dir::open(root, &parent_rel)?, name))
    }
}

/// `rel` を(親ディレクトリの相対パス, 最終要素名)に割る。構文検証は
/// [`path::validate_relative`] に委ねる。
fn split_parent(rel: &str) -> Result<(String, String), FsError> {
    let mut components = path::validate_relative(rel)?;
    let name = components
        .pop()
        .ok_or_else(|| FsError::InvalidPath("path must name a file".into()))?;
    Ok((components.join("/"), name))
}

/// 原子的書き込みで使う tmp の名前。同じディレクトリの中に作るので
/// `rename` が同一ファイルシステム内に収まる。
///
/// **呼び出しごとに一意でなければならない。** pid だけだと、同じプロセス内の
/// 2 スレッド(同じディレクトリを共有する 2 プラグイン、あるいは 1 プラグイン
/// の再入)が同一パスへ `write` したときに同じ tmp 名を奪い合い、片方の
/// `rename` が ENOENT で落ちる — しかも `write` が `not-found` を返すという、
/// WIT の意味論としても誤った結果になる。プロセス内で単調増加するカウンタを
/// 足して、その競合ごと無くす。
fn tmp_name(name: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(".{name}.tmp.{}.{seq}", std::process::id())
}

fn modified_secs(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const READ_LIMIT: usize = 8 * 1024 * 1024;

    fn driver() -> FsDriver {
        FsDriver::new(READ_LIMIT, 10_000)
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();

        d.write(dir.path(), "notes/state.json", b"{\"seen\":1}").unwrap();
        let got = d.read(dir.path(), "notes/state.json").unwrap();

        assert_eq!(got, b"{\"seen\":1}");
        assert!(dir.path().join("notes").join("state.json").is_file());
    }

    #[test]
    fn write_is_atomic_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();

        d.write(dir.path(), "a.txt", b"first").unwrap();
        d.write(dir.path(), "a.txt", b"second").unwrap();

        assert_eq!(d.read(dir.path(), "a.txt").unwrap(), b"second");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name != "a.txt")
            .collect();
        assert!(leftovers.is_empty(), "unexpected leftovers: {leftovers:?}");
    }

    #[test]
    fn append_extends_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();

        d.append(dir.path(), "log.txt", b"one\n").unwrap();
        d.append(dir.path(), "log.txt", b"two\n").unwrap();

        assert_eq!(d.read(dir.path(), "log.txt").unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn read_range_returns_a_slice_and_tolerates_offsets_past_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        d.write(dir.path(), "a.txt", b"0123456789").unwrap();

        assert_eq!(d.read_range(dir.path(), "a.txt", 2, 3).unwrap(), b"234");
        assert_eq!(d.read_range(dir.path(), "a.txt", 8, 100).unwrap(), b"89");
        assert!(d.read_range(dir.path(), "a.txt", 50, 10).unwrap().is_empty());
    }

    #[test]
    fn read_over_the_limit_is_too_large_but_read_range_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let d = FsDriver::new(16, 10_000);
        d.write(dir.path(), "big.bin", &[7u8; 64]).unwrap();

        assert!(matches!(
            d.read(dir.path(), "big.bin").expect_err("over the read limit"),
            FsError::TooLarge(_)
        ));
        assert_eq!(d.read_range(dir.path(), "big.bin", 0, 16).unwrap().len(), 16);
        assert!(matches!(
            d.read_range(dir.path(), "big.bin", 0, 17)
                .expect_err("range longer than the limit"),
            FsError::TooLarge(_)
        ));
    }

    #[test]
    fn stat_reports_size_and_list_is_recursive_over_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        d.write(dir.path(), "a.txt", b"abc").unwrap();
        d.write(dir.path(), "logs/b.txt", b"de").unwrap();

        assert_eq!(d.stat(dir.path(), "a.txt").unwrap().size, 3);

        let mut listed: Vec<String> = d
            .list(dir.path(), "")
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        listed.sort();
        assert_eq!(listed, vec!["a.txt".to_string(), "logs/b.txt".to_string()]);

        let scoped: Vec<String> = d
            .list(dir.path(), "logs")
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(scoped, vec!["logs/b.txt".to_string()]);
    }

    #[test]
    fn list_over_the_entry_limit_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let d = FsDriver::new(READ_LIMIT, 3);
        for i in 0..4 {
            d.write(dir.path(), &format!("f{i}.txt"), b"x").unwrap();
        }

        assert!(matches!(
            d.list(dir.path(), "").expect_err("over the entry limit"),
            FsError::TooLarge(_)
        ));
    }

    #[test]
    fn delete_removes_a_file_and_missing_files_report_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        d.write(dir.path(), "a.txt", b"x").unwrap();

        d.delete(dir.path(), "a.txt").unwrap();
        assert!(!dir.path().join("a.txt").exists());
        assert!(matches!(
            d.delete(dir.path(), "a.txt").expect_err("already gone"),
            FsError::NotFound(_)
        ));
    }

    #[test]
    fn every_operation_refuses_to_escape_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let d = driver();

        assert!(d.read(dir.path(), "../secret").is_err());
        assert!(d.read(dir.path(), "link").is_err());
        assert!(d.stat(dir.path(), "link").is_err());
        assert!(d.write(dir.path(), "escape/evil.txt", b"x").is_err());
        assert!(d.append(dir.path(), "escape/evil.txt", b"x").is_err());
        assert!(d.delete(dir.path(), "link").is_err());
        assert!(d.list(dir.path(), "escape").is_err());

        // 外のファイルが一切変化していないこと。
        assert_eq!(fs::read(outside.path().join("secret")).unwrap(), b"secret");
        assert!(!outside.path().join("evil.txt").exists());
    }

    /// ルート内のシンボリックリンクは、外を指していなくても拒否する
    /// (`RESOLVE_NO_SYMLINKS` 相当の意図的な制約。設計書に明記済み)。
    #[test]
    fn symlinks_inside_the_root_are_rejected_too() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        d.write(dir.path(), "real.txt", b"x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("alias")).unwrap();

        assert!(d.read(dir.path(), "alias").is_err());
    }

    /// 検証と open の間にシンボリックリンクへ差し替えられても、ルート外の
    /// ファイルを書き換えられないこと。`openat2` 経路の意義そのもの。
    #[test]
    fn swapping_the_target_for_a_symlink_cannot_write_outside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        fs::write(&victim, b"original").unwrap();
        let d = driver();

        // 事前に通常ファイルとして作らせ、その後リンクへ差し替える。
        d.write(dir.path(), "target.txt", b"x").unwrap();
        fs::remove_file(dir.path().join("target.txt")).unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join("target.txt")).unwrap();

        let _ = d.write(dir.path(), "target.txt", b"overwritten");

        assert_eq!(
            fs::read(&victim).unwrap(),
            b"original",
            "a symlink swap must never let a write reach outside the root"
        );
    }

    /// 同じディレクトリに書ける第三者が tmp の名前を先回りしてシンボリック
    /// リンクとして置いた場合。tmp を `O_CREAT | O_EXCL | O_NOFOLLOW` で
    /// 作らないと、ここでルート外へ本文が書き出されてしまう。
    ///
    /// tmp 名は呼び出しごとに一意なので、攻撃者はまず名前を当てられない。
    /// それでも当てられた場合(このテストは次に使われる連番を先読みして
    /// 当てにいく)、`O_EXCL | O_NOFOLLOW` がリンクを開くことを拒む。
    #[test]
    fn a_pre_planted_symlink_at_the_temp_name_cannot_be_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        fs::write(&victim, b"original").unwrap();
        let d = driver();

        // 攻撃者がこれから使われうる tmp 名を先取りしてルート外を指すリンクを
        // 置く。連番は他のテストと共有のカウンタなので、この後に使われる値を
        // 幅を持たせて狙う(1 本でも当たれば `O_EXCL` 経路が実際に踏まれる)。
        let probe = tmp_name("target.txt");
        let (prefix, seq) = probe.rsplit_once('.').unwrap();
        let seq: u64 = seq.parse().unwrap();
        let planted: Vec<std::path::PathBuf> = (1..=8)
            .map(|i| dir.path().join(format!("{prefix}.{}", seq + i)))
            .collect();
        for link in &planted {
            std::os::unix::fs::symlink(&victim, link).unwrap();
        }

        let _ = d.write(dir.path(), "target.txt", b"overwritten");

        assert_eq!(
            fs::read(&victim).unwrap(),
            b"original",
            "the temp file must never be opened through a symlink"
        );
        // 仕掛けられたリンクは辿られないだけでなく、辿らずに書き潰されても
        // いないこと(リンクのまま残っているのが正しい)。
        for link in &planted {
            assert!(
                link.symlink_metadata().unwrap().file_type().is_symlink(),
                "{link:?} must still be the planted symlink"
            );
        }
    }

    /// tmp 名は呼び出しごとに一意であること。I3 の回帰ガード
    /// (pid だけだと同一プロセス内の 2 スレッドが衝突する)。
    #[test]
    fn temp_names_are_unique_per_call() {
        let names: std::collections::HashSet<String> =
            (0..1000).map(|_| tmp_name("x.bin")).collect();
        assert_eq!(names.len(), 1000, "temp names must be unique per call");
    }

    /// `append` も同じ。確認してから開くのでは間に合わないので、
    /// 既存がリンクなら open 自体が失敗しなければならない。
    #[test]
    fn appending_through_a_pre_planted_symlink_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        fs::write(&victim, b"original").unwrap();
        let d = driver();

        std::os::unix::fs::symlink(&victim, dir.path().join("log.txt")).unwrap();

        let err = d
            .append(dir.path(), "log.txt", b"appended")
            .expect_err("appending through a symlink must be refused");
        assert!(matches!(err, FsError::InvalidPath(_)), "got {err:?}");
        assert_eq!(fs::read(&victim).unwrap(), b"original");
    }

    /// `prefix` にルート内のシンボリックリンクディレクトリを渡した `list` は、
    /// 同じリンクに対する `read` と同じく拒否されること。`list` だけ
    /// シンボリックリンクを解決する、という不整合を固定で防ぐ。
    #[test]
    fn list_refuses_a_symlinked_prefix_exactly_like_read_does() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        d.write(dir.path(), "real/x.txt", b"x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("alias")).unwrap();

        let read_err = d
            .read(dir.path(), "alias/x.txt")
            .expect_err("read through a symlinked directory must be refused");
        let list_err = d
            .list(dir.path(), "alias")
            .expect_err("list through a symlinked directory must be refused too");

        assert!(matches!(read_err, FsError::InvalidPath(_)), "{read_err:?}");
        assert!(matches!(list_err, FsError::InvalidPath(_)), "{list_err:?}");
    }

    /// 再帰走査の途中に現れたシンボリックリンクディレクトリを辿らないこと。
    /// 辿ると、ルート外のファイル名・サイズ・更新時刻が「ルート配下の相対
    /// パス」として返ってしまう。
    #[test]
    fn list_does_not_descend_into_symlinked_directories_it_meets_while_recursing() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
        let d = driver();

        d.write(dir.path(), "sub/real.txt", b"x").unwrap();
        // ルート外を指すリンクと、ルート内を指すリンクの両方。
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("sub"), dir.path().join("alias")).unwrap();

        let mut listed: Vec<String> = d
            .list(dir.path(), "")
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        listed.sort();

        assert_eq!(
            listed,
            vec!["sub/real.txt".to_string()],
            "list must neither follow symlinked directories nor report the links themselves"
        );
    }

    /// `list` が同時に抱える fd はツリーの**幅**に比例してはならない。
    ///
    /// 幅に比例すると、ディレクトリだけの浅く広いツリー(プラグインが
    /// `write` を繰り返すだけで作れる。`list_limit` は通常ファイルしか
    /// 数えないので発火しない)に対して `list` 1 回でプロセス全体の fd を
    /// 食い潰せる。fd テーブルはプロセス共有なので、デーモンの他の機能
    /// (WebSocket の accept など)まで巻き添えになる。
    ///
    /// 走査中に別スレッドから `/proc/self/fd` を数えて、ピークが幅に
    /// 比例しないことを確認する(Linux 前提)。
    #[test]
    fn list_does_not_hold_one_file_descriptor_per_sibling_directory() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;

        const WIDTH: usize = 500;

        fn open_fds() -> usize {
            fs::read_dir("/proc/self/fd")
                .map(|d| d.count())
                .unwrap_or(0)
        }

        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        for i in 0..WIDTH {
            d.write(dir.path(), &format!("d{i:04}/x.txt"), b"x").unwrap();
        }

        let baseline = open_fds();
        let running = Arc::new(AtomicBool::new(true));
        let peak = Arc::new(AtomicUsize::new(0));
        let samples = Arc::new(AtomicUsize::new(0));

        let sampler = {
            let running = Arc::clone(&running);
            let peak = Arc::clone(&peak);
            let samples = Arc::clone(&samples);
            std::thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    peak.fetch_max(open_fds(), Ordering::Relaxed);
                    samples.fetch_add(1, Ordering::Relaxed);
                    std::thread::yield_now();
                }
            })
        };

        let listed = d.list(dir.path(), "").unwrap();
        running.store(false, Ordering::Relaxed);
        sampler.join().unwrap();

        assert_eq!(listed.len(), WIDTH);
        assert!(
            samples.load(Ordering::Relaxed) >= 5,
            "the sampler never got to run; this test would not detect a regression"
        );

        // 深さ優先なら同時に開くのは「深さぶん + 数本」で足りる。幅に
        // 比例していれば WIDTH 本前後まで伸びる。
        let peak = peak.load(Ordering::Relaxed);
        assert!(
            peak < baseline + WIDTH / 10,
            "list held {peak} fds (baseline {baseline}) while walking {WIDTH} sibling \
             directories; it must not open one per sibling"
        );
    }

    /// ルート内に FIFO があっても、`read` / `stat` がブロックしてはならない。
    ///
    /// `open(O_RDONLY)` は writer が現れるまで返らないので、`O_NONBLOCK` 無しで
    /// 開くとプラグイン専用スレッドが恒久的に固まる(ホスト呼び出し中は
    /// エポック割り込みが効かない)。FIFO はプラグイン自身では作れないが、
    /// 承認したディレクトリに外部の主体が置くことはできる。
    #[test]
    fn a_fifo_inside_the_root_is_refused_instead_of_blocking() {
        let dir = tempfile::tempdir().unwrap();
        make_fifo(&dir.path().join("pipe"));

        let root = dir.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let d = FsDriver::new(READ_LIMIT, 10_000);
            let read = d.read(&root, "pipe").map(|b| b.len());
            let stat = d.stat(&root, "pipe").map(|e| e.size);
            let _ = tx.send((read, stat));
        });

        let (read, stat) = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("read/stat on a FIFO must not block");
        assert!(read.is_err(), "read on a FIFO must fail, got {read:?}");
        assert!(stat.is_err(), "stat on a FIFO must fail, got {stat:?}");
    }

    /// `append` も `read` と同じ理由でブロックしてはならない。
    ///
    /// reader のいない FIFO に対する `open(O_WRONLY)` は永久に返らない。
    /// `read` を塞いだのと完全に同じ故障モードなので、同じ形で塞ぐ。
    #[test]
    fn appending_to_a_fifo_inside_the_root_is_refused_instead_of_blocking() {
        let dir = tempfile::tempdir().unwrap();
        make_fifo(&dir.path().join("pipe"));

        let root = dir.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let d = FsDriver::new(READ_LIMIT, 10_000);
            let _ = tx.send(d.append(&root, "pipe", b"x"));
        });

        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("append to a FIFO must not block");
        assert!(result.is_err(), "append to a FIFO must fail, got {result:?}");
    }

    /// 走査予算は、1 ディレクトリを読み終えてからではなく**読みながら**
    /// 効かなければならない。
    ///
    /// `entries()` が dirent を全件読み、1 件ごとに `statat` を発行して
    /// `Vec` に積んでから返していると、「単一ディレクトリに大量のエントリ」に
    /// 予算が効かない(プラグインは `write` の繰り返しでそれを作れる)。
    /// ここではエラーメッセージが報告する走査件数で、全件走査していないことを
    /// 見る。
    #[test]
    fn the_scan_budget_stops_inside_a_single_huge_directory() {
        const FILES: usize = 5_000;
        const BUDGET: usize = 8;

        let dir = tempfile::tempdir().unwrap();
        for i in 0..FILES {
            fs::write(dir.path().join(format!("f{i:05}.txt")), b"x").unwrap();
        }
        let d = FsDriver::new(READ_LIMIT, 10_000).with_scan_limit(BUDGET);

        let err = d
            .list(dir.path(), "")
            .expect_err("a single huge directory must be cut off by the scan budget");
        let FsError::TooLarge(message) = &err else {
            panic!("expected TooLarge, got {err:?}");
        };

        // メッセージは実際に走査した件数を `scanned=N` で報告する。予算 + 1 を
        // 超えていたら、積み終わってから判定している(= 全件 statat している)
        // ということ。
        let scanned: usize = message
            .split("scanned=")
            .nth(1)
            .and_then(|rest| {
                rest.split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|digits| digits.parse().ok())
            })
            .unwrap_or_else(|| panic!("scan budget error must report `scanned=N`: {message}"));
        assert!(
            scanned <= BUDGET + 1,
            "list walked {scanned} of {FILES} entries before hitting a budget of {BUDGET}; \
             the budget must stop the scan while reading, not after"
        );
    }

    /// `list_limit` は「返す通常ファイルの数」しか数えないので、ディレクトリや
    /// FIFO しか含まないツリーはいくら大きくても上限に触れない。走査した
    /// エントリの総数にも予算を持たせ、超えたら打ち切ること。
    #[test]
    fn a_huge_tree_without_files_is_cut_off_by_the_scan_budget() {
        let dir = tempfile::tempdir().unwrap();
        let d = FsDriver::new(READ_LIMIT, 10_000).with_scan_limit(16);

        // 通常ファイルを 1 つも含まないツリー。`list_limit` は決して発火しない。
        for i in 0..64 {
            fs::create_dir(dir.path().join(format!("d{i:03}"))).unwrap();
        }

        let err = d
            .list(dir.path(), "")
            .expect_err("a huge file-less tree must be cut off by the scan budget");
        assert!(matches!(err, FsError::TooLarge(_)), "got {err:?}");

        // 予算の内側は通ること(予算が実質ゼロになっていないこと)。
        let small = tempfile::tempdir().unwrap();
        d.write(small.path(), "a.txt", b"x").unwrap();
        assert_eq!(d.list(small.path(), "").unwrap().len(), 1);
    }

    /// 同一パスへの並行 `write` は、どれも失敗してはならない。
    ///
    /// tmp 名にプロセス内で一意な要素が無いと、同じディレクトリを共有する
    /// 2 スレッドが同じ tmp を奪い合い、片方の `rename` が ENOENT
    /// (= `not-found`)で落ちる。`write` が `not-found` を返すのは WIT の
    /// 意味論としても誤り。
    #[test]
    fn concurrent_writes_to_the_same_path_all_succeed_and_never_interleave() {
        const SIZE: usize = 4 * 1024 * 1024;
        const ROUNDS: usize = 40;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

        let handles: Vec<_> = [b'a', b'b']
            .into_iter()
            .map(|byte| {
                let root = root.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let d = FsDriver::new(READ_LIMIT, 10_000);
                    let payload = vec![byte; SIZE];
                    barrier.wait();
                    let mut errors = Vec::new();
                    for _ in 0..ROUNDS {
                        if let Err(e) = d.write(&root, "x.bin", &payload) {
                            errors.push(e.to_string());
                        }
                    }
                    errors
                })
            })
            .collect();

        let errors: Vec<String> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("writer thread should not panic"))
            .collect();
        assert!(
            errors.is_empty(),
            "{} of {} concurrent writes failed: {:?}",
            errors.len(),
            2 * ROUNDS,
            &errors[..errors.len().min(3)]
        );

        // 最終内容はどちらかの書き手の完全な内容であること(混ざっていない)。
        let final_bytes = driver().read(dir.path(), "x.bin").unwrap();
        assert_eq!(final_bytes.len(), SIZE);
        let first = final_bytes[0];
        assert!(first == b'a' || first == b'b');
        assert!(
            final_bytes.iter().all(|b| *b == first),
            "the surviving file mixes bytes from both writers"
        );

        // tmp の残骸が無いこと。
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name != "x.bin")
            .collect();
        assert!(leftovers.is_empty(), "unexpected leftovers: {leftovers:?}");
    }

    /// `list` が返す名前は、必ず他の操作にもそのまま渡せること。
    ///
    /// バックスラッシュ・制御文字を含む名前は `path::validate_relative` が
    /// 拒否するので、列挙しておきながら `read` できない、という不整合になる。
    #[test]
    fn list_only_returns_names_the_other_operations_accept() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();
        d.write(dir.path(), "ok.txt", b"x").unwrap();
        fs::write(dir.path().join("we\\ird"), b"x").unwrap();
        fs::write(dir.path().join("new\nline"), b"x").unwrap();

        let listed: Vec<String> = d
            .list(dir.path(), "")
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();

        assert_eq!(listed, vec!["ok.txt".to_string()]);
        for name in &listed {
            d.read(dir.path(), name)
                .unwrap_or_else(|e| panic!("list returned {name:?} but read refused it: {e}"));
        }
    }

    fn make_fifo(path: &Path) {
        use rustix::fs::{FileType, Mode};
        rustix::fs::mknodat(
            rustix::fs::CWD,
            path,
            FileType::Fifo,
            Mode::from_bits_truncate(0o644),
            0,
        )
        .expect("mkfifo");
    }

    /// 深すぎるツリーは再帰が止まらないので、深さの上限で拒否する。
    #[test]
    fn a_tree_deeper_than_the_depth_limit_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver();

        let deep = format!("{}x.txt", "d/".repeat(MAX_LIST_DEPTH + 2));
        d.write(dir.path(), &deep, b"x").unwrap();

        assert!(matches!(
            d.list(dir.path(), "")
                .expect_err("a tree deeper than the limit must be refused"),
            FsError::TooLarge(_)
        ));

        // 上限のすぐ内側は通ること(上限が実質ゼロになっていないこと)。
        let shallow = tempfile::tempdir().unwrap();
        let ok = format!("{}x.txt", "d/".repeat(MAX_LIST_DEPTH - 1));
        d.write(shallow.path(), &ok, b"x").unwrap();
        assert_eq!(d.list(shallow.path(), "").unwrap().len(), 1);
    }
}

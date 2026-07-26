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
}

impl FsDriver {
    pub fn new(read_limit: usize, list_limit: usize) -> FsDriver {
        FsDriver {
            read_limit,
            list_limit,
        }
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

        let mut buf = Vec::with_capacity(size as usize);
        file.read_to_end(&mut buf)
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
        // `open_read` 経由なので、シンボリックリンクは stat も拒否される。
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
    /// シンボリックリンクは(リンク先がルート内でも)列挙しない。
    pub fn list(&self, root: &Path, prefix: &str) -> Result<Vec<Entry>, FsError> {
        let root_canonical = path::canonical_root(root)?;
        let base = if prefix.is_empty() {
            root_canonical.clone()
        } else {
            path::resolve_existing(root, prefix)?
        };

        let mut entries = Vec::new();
        let mut stack = vec![base];
        while let Some(dir) = stack.pop() {
            let read_dir = std::fs::read_dir(&dir).map_err(|e| FsError::Io(e.to_string()))?;
            for item in read_dir {
                let item = item.map_err(|e| FsError::Io(e.to_string()))?;
                // `metadata` はリンク先を見てしまうので、種別は
                // `file_type`(lstat 相当)で判定してリンクを先に弾く。
                let file_type = item.file_type().map_err(|e| FsError::Io(e.to_string()))?;
                if file_type.is_symlink() {
                    continue;
                }
                let path = item.path();
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    // FIFO やデバイスファイルは列挙しない。
                    continue;
                }
                let meta = item.metadata().map_err(|e| FsError::Io(e.to_string()))?;
                let relative = path
                    .strip_prefix(&root_canonical)
                    .map_err(|_| FsError::InvalidPath("entry escapes the granted root".into()))?
                    .to_string_lossy()
                    .to_string();
                entries.push(Entry {
                    path: relative,
                    size: meta.len(),
                    modified: modified_secs(&meta),
                });
                if entries.len() > self.list_limit {
                    return Err(FsError::TooLarge(format!(
                        "more than {} entries under {prefix:?}",
                        self.list_limit
                    )));
                }
            }
        }
        Ok(entries)
    }

    /// 原子的に書き込む。同一ディレクトリに tmp を作って `rename` する
    /// ので、読み手が半端な内容を見ることはない。
    ///
    /// tmp は `O_CREAT | O_EXCL | O_NOFOLLOW` で作る。tmp 名を先回りして
    /// シンボリックリンクとして置かれても、リンクを辿って書くことはない
    /// (同じディレクトリへ書ける別プロセスからの古典的な攻撃)。
    pub fn write(&self, root: &Path, rel: &str, bytes: &[u8]) -> Result<(), FsError> {
        let (dir, name) = self.locate_for_write(root, rel)?;
        let tmp = tmp_name(&name);

        let mut file = match dir.create_new(&tmp)? {
            Some(file) => file,
            None => {
                // tmp の名前に既に何かが居座っている(前回の異常終了の残骸、
                // あるいは攻撃者が先回りして置いたシンボリックリンク)。
                // `unlink` はリンクを辿らずエントリ自体を取り除くので、
                // 消してから作り直す。消した直後にまた置かれたら、
                // `O_EXCL` がもう一度弾くので素直に失敗させる。
                dir.unlink(&tmp)?;
                dir.create_new(&tmp)?.ok_or_else(|| {
                    FsError::Io(format!("{tmp} was recreated while writing {rel}"))
                })?
            }
        };

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
fn tmp_name(name: &str) -> String {
    format!(".{name}.tmp.{}", std::process::id())
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
    use std::path::Path;

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
    #[test]
    fn a_pre_planted_symlink_at_the_temp_name_cannot_be_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        fs::write(&victim, b"original").unwrap();
        let d = driver();

        // 攻撃者が tmp の名前を先取りしてルート外を指すリンクを置く。
        std::os::unix::fs::symlink(&victim, dir.path().join(tmp_name("target.txt"))).unwrap();

        let _ = d.write(dir.path(), "target.txt", b"overwritten");

        assert_eq!(
            fs::read(&victim).unwrap(),
            b"original",
            "the temp file must never be opened through a symlink"
        );
        // 仕掛けられたリンクは(取り除かれたにせよ)残っていてはならない。
        assert!(
            dir.path()
                .join(tmp_name("target.txt"))
                .symlink_metadata()
                .is_err(),
            "the planted symlink must not survive the write"
        );
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

    fn _assert_path_type(_: &Path) {}
}

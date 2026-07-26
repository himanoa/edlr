//! `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` によるパス解決の
//! 3 段目。検証と `open` の間にシンボリックリンクを差し替えられても、
//! カーネルがルート配下から出ることを拒否する(TOCTOU 対策)。
//!
//! `openat2` は Linux 5.6 以降。使えない環境では、ルートの fd から
//! **1 要素ずつ** `openat(O_NOFOLLOW)` で降りるフォールバックを使う。
//! 名前に `/` を含まない 1 要素を親の fd 基準で `O_NOFOLLOW` 付きに開く
//! 限り、シンボリックリンクも `..` も踏めないので、この経路でも配下から
//! 出られない。加えて呼び出し側は事前に `path` モジュールの構文検証と
//! 配下チェック(1・2 段目)を通している。
//!
//! このモジュールの外へは [`Dir`] だけを見せる。ファイル名から
//! `PathBuf` を組み立てて `std::fs` へ渡す経路を呼び出し側に作らせない
//! ためで、`Dir` 経由で触れるものはすべてシンボリックリンクを踏まない
//! ことが保証される。

use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::Path;

use rustix::fs::{AtFlags, FileType, Mode, OFlags};

use crate::path;
use crate::FsError;

/// 新規ファイルのパーミッション(umask が更に絞る)。
const FILE_MODE: u32 = 0o644;

/// ディレクトリを開くときのフラグ。
fn dir_oflags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// ルート配下のディレクトリ 1 つを表すハンドル。
///
/// 保持するのは fd だけで、パス文字列は持たない。ここから触れるものは
/// すべてこの fd 基準に解決されるため、`Dir` を得た後にパスの途中要素を
/// 差し替えられても影響を受けない。
pub struct Dir {
    fd: OwnedFd,
    /// `openat2` が使えるか。使えない環境では `openat(O_NOFOLLOW)` に落ちる。
    confined: bool,
}

/// ディレクトリエントリの種別。プラットフォーム固有の型を外へ出さない
/// ための最小限の分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    RegularFile,
    Directory,
    /// シンボリックリンク・FIFO・デバイスなど。列挙も走査もしない。
    Other,
}

/// [`Dir::entries`] が返すディレクトリエントリ。
pub struct DirEntry {
    /// エントリ名(パス要素 1 つ)。
    pub name: String,
    /// `lstat` 相当で見た種別。シンボリックリンクは [`EntryKind::Other`]。
    pub kind: EntryKind,
    /// 通常ファイルのときだけ意味がある。
    pub size: u64,
    /// Unix epoch 秒。負値(1970 年より前)は `None`。
    pub modified: Option<u64>,
}

impl Dir {
    /// `root` 配下の `rel_dir`(空文字ならルート自身)を開く。
    ///
    /// `rel_dir` は既に存在していなければならない。1・2 段目の検証は
    /// `path` モジュールに委ねる — このモジュールがパス検証を持たない
    /// ようにするため。
    pub fn open(root: &Path, rel_dir: &str) -> Result<Dir, FsError> {
        let canonical_root = path::canonical_root(root)?;
        if !rel_dir.is_empty() {
            // 1・2 段目。3 段目はこの下の `openat2` / `openat`。
            path::resolve_existing(root, rel_dir)?;
        }

        let root_fd = rustix::fs::open(&canonical_root, dir_oflags(), Mode::empty())
            .map_err(|e| errno_to_error(e, "<root>"))?;

        // `openat2` に渡すパスは空文字を許さないので、ルート自身は "." で指す。
        let probe = if rel_dir.is_empty() { "." } else { rel_dir };
        match openat2_beneath(&root_fd, probe, dir_oflags(), Mode::empty()) {
            Ok(Some(fd)) => Ok(Dir { fd, confined: true }),
            // カーネルが `openat2` を持たない。ルートの fd から 1 要素ずつ
            // `O_NOFOLLOW` で降りる。
            Ok(None) => {
                let mut fd = root_fd;
                if !rel_dir.is_empty() {
                    for component in path::validate_relative(rel_dir)? {
                        check_component(&component)?;
                        // `O_NOFOLLOW` なので、途中の要素がリンクなら ELOOP。
                        // 1 要素ずつなので `..` も混ざりようがない。
                        fd = rustix::fs::openat(
                            &fd,
                            component.as_str(),
                            dir_oflags(),
                            Mode::empty(),
                        )
                        .map_err(|e| errno_to_error(e, &component))?;
                    }
                }
                Ok(Dir {
                    fd,
                    confined: false,
                })
            }
            Err(e) => Err(errno_to_error(e, rel_dir)),
        }
    }

    /// 読み取り用に**通常ファイルだけ**を開く。
    ///
    /// `O_NONBLOCK` を付けて開き、`fstat` で通常ファイルでなければ拒否する。
    /// ルート内に FIFO があると `open(O_RDONLY)` は writer が現れるまで返らず
    /// (`O_NOFOLLOW` / `RESOLVE_NO_SYMLINKS` は FIFO を止めない)、ホスト
    /// 呼び出し中はエポック割り込みが効かないので、プラグイン専用スレッドが
    /// 恒久的に固まってしまう。FIFO・デバイス・ソケット・ディレクトリは
    /// この機能で読む対象ではないので、まとめて `InvalidPath` にする。
    ///
    /// 判定は「開いた後の fd」に対する `fstat` で行う。開く前に `statat` で
    /// 確かめると、確かめた相手と開く相手が別物になりうる(TOCTOU)。
    /// 通常ファイルと確認できたら `O_NONBLOCK` は落とす — Linux は通常
    /// ファイルに対してこのフラグを無視するが、以後の `read` の意味論を
    /// 普通のファイルと完全に同じに保つため。
    pub fn open_read(&self, name: &str) -> Result<File, FsError> {
        let file =
            exists_is_impossible(self.open_with(name, OFlags::RDONLY | OFlags::NONBLOCK, false))?;
        ensure_regular_file(file, name)
    }

    /// 新規作成専用に開く。tmp ファイル用。
    ///
    /// 既に何か(通常ファイルでもシンボリックリンクでも)が居れば
    /// `Ok(None)` を返す。`O_EXCL` なので、居座っているものを開いて
    /// しまうことは無い。
    pub fn create_new(&self, name: &str) -> Result<Option<File>, FsError> {
        self.open_with(name, OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL, true)
    }

    /// 追記用に開く。無ければ作る。既存がシンボリックリンクなら拒否する
    /// (`O_NOFOLLOW` / `RESOLVE_NO_SYMLINKS`)。確認してから開くのでは
    /// なく、開く操作そのものが拒否するので TOCTOU にならない。
    ///
    /// [`Dir::open_read`] と同じく `O_NONBLOCK` を付けて開き、通常ファイル
    /// でなければ拒否する。reader のいない FIFO に対する `open(O_WRONLY)` は
    /// 永久に返らず、`read` と**完全に同じ故障モード**(プラグイン専用
    /// スレッドが恒久的に固まる)になるため。`write` は `O_CREAT | O_EXCL` の
    /// tmp を経由するので、既存の FIFO を開くことはない。
    pub fn open_append(&self, name: &str) -> Result<File, FsError> {
        let file = exists_is_impossible(self.open_with(
            name,
            OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE | OFlags::NONBLOCK,
            true,
        ))?;
        ensure_regular_file(file, name)
    }

    /// 直下のサブディレクトリを開く。ディレクトリでない、シンボリック
    /// リンクである、既に消えている場合は `Ok(None)`(列挙の途中で
    /// 出会うのは普通のことなので、エラーにせず読み飛ばさせる)。
    pub fn open_dir(&self, name: &str) -> Result<Option<Dir>, FsError> {
        check_component(name)?;
        let opened = if self.confined {
            openat2_beneath(&self.fd, name, dir_oflags(), Mode::empty())
                .and_then(|fd| fd.ok_or(rustix::io::Errno::NOSYS))
        } else {
            rustix::fs::openat(&self.fd, name, dir_oflags(), Mode::empty())
        };

        match opened {
            Ok(fd) => Ok(Some(Dir {
                fd,
                confined: self.confined,
            })),
            // ELOOP: シンボリックリンク。ENOTDIR: ディレクトリではない。
            // ENOENT: 列挙してから開くまでに消えた。いずれも読み飛ばす。
            Err(rustix::io::Errno::LOOP)
            | Err(rustix::io::Errno::NOTDIR)
            | Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(e) => Err(errno_to_error(e, name)),
        }
    }

    /// 直下のエントリを列挙する。`.` と `..` は含めない。
    ///
    /// 種別とメタデータは `statat(AT_SYMLINK_NOFOLLOW)` で取る。名前が
    /// 指す先がリンクへ差し替えられていても、リンク自身の情報しか返らない
    /// (リンク先のメタデータが漏れない)。
    ///
    /// `scanned` は呼び出し側(`list` の走査全体)と共有する「見たエントリの
    /// 総数」で、`limit` を超えた時点で **読みながら** 打ち切って
    /// `TooLarge` を返す。全件読んでから判定してはならない: `statat` は
    /// 1 件ごとに発行され、`Vec` も 1 件ごとに伸びるので、単一ディレクトリに
    /// 大量のエントリを置かれると(プラグインは `write` の繰り返しでそれを
    /// 作れる)判定に届く前に数十秒のブロックと数百 MB の確保になる。
    ///
    /// 予算超過は `.`/`..` を除いた 1 件を読み進めるたびに見る。`statat` を
    /// 発行する**前**に判定するので、超過分のコストは 1 件も払わない。
    pub fn entries(&self, scanned: &mut usize, limit: usize) -> Result<Vec<DirEntry>, FsError> {
        let dir =
            rustix::fs::Dir::read_from(&self.fd).map_err(|e| errno_to_error(e, "<directory>"))?;

        let mut entries = Vec::new();
        for entry in dir {
            let entry = entry.map_err(|e| errno_to_error(e, "<directory>"))?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            *scanned += 1;
            if *scanned > limit {
                return Err(FsError::TooLarge(format!(
                    "directory listing exceeded the {limit} entry scan budget (scanned={scanned})"
                )));
            }
            let name = match std::str::from_utf8(name) {
                Ok(name) => name.to_string(),
                // UTF-8 でない名前は相対パスとして返せないので列挙しない。
                Err(_) => continue,
            };
            // 列挙した名前は、そのまま他の操作(`read` など)に渡せなければ
            // ならない。バックスラッシュや制御文字を含む名前は
            // `path::validate_relative` が拒否するので、列挙もしない —
            // 「`list` の結果は常に他の操作で使える」という契約を保つため。
            if !path::is_valid_component(&name) {
                continue;
            }

            let stat = match rustix::fs::statat(&self.fd, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            {
                Ok(stat) => stat,
                // 列挙してから statat するまでに消えた。
                Err(rustix::io::Errno::NOENT) => continue,
                Err(e) => return Err(errno_to_error(e, &name)),
            };

            let kind = match FileType::from_raw_mode(stat.st_mode as rustix::fs::RawMode) {
                FileType::RegularFile => EntryKind::RegularFile,
                FileType::Directory => EntryKind::Directory,
                _ => EntryKind::Other,
            };
            entries.push(DirEntry {
                name,
                kind,
                size: stat.st_size as u64,
                modified: u64::try_from(stat.st_mtime as i64).ok(),
            });
        }
        Ok(entries)
    }

    /// 同一ディレクトリ内で名前を付け替える。`rename(2)` は最終要素の
    /// シンボリックリンクを辿らずリンク自体を置き換えるので、宛先が
    /// リンクへ差し替えられていてもリンク先には触れない。
    pub fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        check_component(from)?;
        check_component(to)?;
        rustix::fs::renameat(&self.fd, from, &self.fd, to).map_err(|e| errno_to_error(e, to))
    }

    /// ディレクトリエントリを 1 つ取り除く。シンボリックリンクは辿らず、
    /// リンクそのものを取り除く。
    pub fn unlink(&self, name: &str) -> Result<(), FsError> {
        check_component(name)?;
        rustix::fs::unlinkat(&self.fd, name, AtFlags::empty()).map_err(|e| errno_to_error(e, name))
    }

    /// 実際に開く。`O_EXCL` 付きで既に何かが存在した場合だけ `Ok(None)`。
    fn open_with(&self, name: &str, oflags: OFlags, create: bool) -> Result<Option<File>, FsError> {
        check_component(name)?;
        let oflags = oflags | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mode = if create {
            Mode::from_bits_truncate(FILE_MODE)
        } else {
            Mode::empty()
        };

        let opened = if self.confined {
            // ディレクトリを開けた時点で `openat2` は使えているので、
            // 「未対応」が返ることは無い。返ったなら拘束が効いていない
            // ということなので、黙って緩めず ENOSYS のまま失敗させる。
            openat2_beneath(&self.fd, name, oflags, mode)
                .and_then(|fd| fd.ok_or(rustix::io::Errno::NOSYS))
        } else {
            rustix::fs::openat(&self.fd, name, oflags, mode)
        };

        match opened {
            Ok(fd) => Ok(Some(File::from(fd))),
            Err(rustix::io::Errno::EXIST) => Ok(None),
            Err(e) => Err(errno_to_error(e, name)),
        }
    }
}

/// 開いた fd が通常ファイルであることを確認し、`O_NONBLOCK` を落とす。
///
/// 判定は**開いた後の fd** に対する `fstat` で行う。開く前に `statat` で
/// 確かめると、確かめた相手と開く相手が別物になりうる(TOCTOU)。
/// `O_NONBLOCK` を落とすのは、Linux が通常ファイルに対してこのフラグを無視する
/// とはいえ、以後の `read`/`write` の意味論を普通のファイルと完全に同じに
/// 保つため(ベストエフォート。失敗しても通常ファイルなら挙動は変わらない)。
fn ensure_regular_file(file: File, name: &str) -> Result<File, FsError> {
    let stat = rustix::fs::fstat(&file).map_err(|e| errno_to_error(e, name))?;
    if FileType::from_raw_mode(stat.st_mode as rustix::fs::RawMode) != FileType::RegularFile {
        return Err(FsError::InvalidPath(format!(
            "{name:?} is not a regular file"
        )));
    }
    if let Ok(flags) = rustix::fs::fcntl_getfl(&file) {
        let _ = rustix::fs::fcntl_setfl(&file, flags - OFlags::NONBLOCK);
    }
    Ok(file)
}

/// `O_EXCL` を付けていない open で `Ok(None)`(= 既に存在する)が返ることは
/// 無い。万一返ったら、静かに扱わずエラーにする。
fn exists_is_impossible(opened: Result<Option<File>, FsError>) -> Result<File, FsError> {
    opened?.ok_or_else(|| FsError::Io("open reported EEXIST without O_EXCL".into()))
}

/// `openat2(dirfd, rel, ..., RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`。
///
/// `openat2` を持たないカーネルでは `Ok(None)`(呼び出し側がフォールバック
/// する合図)。それ以外の失敗は errno のまま返し、意味付けは呼び出し側に任せる
/// (`EEXIST` や `ENOTDIR` を区別したい呼び出し側があるため)。
#[cfg(target_os = "linux")]
fn openat2_beneath(
    dirfd: &impl std::os::fd::AsFd,
    rel: &str,
    oflags: OFlags,
    mode: Mode,
) -> Result<Option<OwnedFd>, rustix::io::Errno> {
    use rustix::fs::ResolveFlags;

    match rustix::fs::openat2(
        dirfd,
        rel,
        oflags,
        mode,
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    ) {
        Ok(fd) => Ok(Some(fd)),
        // カーネルが openat2 を持たない(Linux 5.6 未満)。EINVAL は
        // 古いカーネルが `open_how` のサイズや未知のフラグを弾いた場合。
        Err(rustix::io::Errno::NOSYS)
        | Err(rustix::io::Errno::OPNOTSUPP)
        | Err(rustix::io::Errno::INVAL) => Ok(None),
        Err(other) => Err(other),
    }
}

#[cfg(not(target_os = "linux"))]
fn openat2_beneath(
    _dirfd: &impl std::os::fd::AsFd,
    _rel: &str,
    _oflags: OFlags,
    _mode: Mode,
) -> Result<Option<OwnedFd>, rustix::io::Errno> {
    Ok(None)
}

/// `Dir` のメソッドが受け取ってよいのは検証済みのパス要素 1 つだけ。
/// 呼び出し側が誤ってサブパスを渡しても、ここで止まる。
fn check_component(name: &str) -> Result<(), FsError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(FsError::InvalidPath(format!(
            "{name:?} is not a single path component"
        )));
    }
    Ok(())
}

fn errno_to_error(errno: rustix::io::Errno, what: &str) -> FsError {
    match errno {
        rustix::io::Errno::NOENT => FsError::NotFound(what.to_string()),
        // ELOOP: 最終要素がシンボリックリンク(`O_NOFOLLOW` / `NO_SYMLINKS`)。
        // EXDEV: `RESOLVE_BENEATH` がルート外への脱出を止めた。
        rustix::io::Errno::LOOP | rustix::io::Errno::XDEV => FsError::InvalidPath(format!(
            "{what:?} resolves through a symlink or escapes the granted root"
        )),
        // ENOTDIR: ディレクトリとして開こうとした相手がディレクトリでは
        // ない。`O_NOFOLLOW | O_DIRECTORY` でシンボリックリンクを開いた
        // 場合、Linux は ELOOP ではなく ENOTDIR を返す。
        rustix::io::Errno::NOTDIR => FsError::InvalidPath(format!(
            "{what:?} is not a directory (a symlink opened with O_NOFOLLOW reports this too)"
        )),
        other => FsError::Io(other.to_string()),
    }
}

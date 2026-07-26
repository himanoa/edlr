//! `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` によるパス解決の
//! 3 段目。検証と `open` の間にシンボリックリンクを差し替えられても、
//! カーネルがルート配下から出ることを拒否する(TOCTOU 対策)。
//!
//! `openat2` は Linux 5.6 以降。使えない環境では `O_NOFOLLOW` 付きの
//! 通常 open にフォールバックする。フォールバック経路でも、呼び出し側は
//! 事前に `path` モジュールの構文検証と配下チェック(1・2 段目)を通して
//! いる — 3 段目はその代わりではなく、上乗せの拘束である。
//!
//! このモジュールの外へは [`Dir`] だけを見せる。ファイル名から
//! `PathBuf` を組み立てて `std::fs` へ渡す経路を呼び出し側に作らせない
//! ためで、`Dir` 経由で開いた fd はすべてシンボリックリンクを踏まない
//! ことが保証される。

use std::fs::File;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};

use crate::path;
use crate::FsError;

/// 新規ファイルのパーミッション(umask が更に絞る)。
const FILE_MODE: u32 = 0o644;

/// ルート配下のディレクトリ 1 つを表すハンドル。
///
/// ここから開いたファイルは、`openat2` が使える環境ではカーネルレベルで
/// このディレクトリのルート配下に拘束され、シンボリックリンクは一切
/// 辿られない。使えない環境では `O_NOFOLLOW` 付きの通常 open になる。
pub struct Dir {
    /// `openat2` が使える環境でのみ `Some`。ディレクトリの fd。
    fd: Option<std::os::fd::OwnedFd>,
    /// 正規化済みのディレクトリパス。フォールバック経路とエラー表示に使う。
    path: PathBuf,
}

impl Dir {
    /// `root` 配下の `rel_dir`(空文字ならルート自身)を開く。
    ///
    /// `rel_dir` は既に存在していなければならない。1・2 段目の検証は
    /// `path` モジュールに委ねる — このモジュールがパス検証を持たない
    /// ようにするため。
    pub fn open(root: &Path, rel_dir: &str) -> Result<Dir, FsError> {
        let canonical_root = path::canonical_root(root)?;
        let canonical = if rel_dir.is_empty() {
            canonical_root.clone()
        } else {
            path::resolve_existing(root, rel_dir)?
        };

        let fd = open_dir_beneath(&canonical_root, rel_dir)?;
        Ok(Dir {
            fd,
            path: canonical,
        })
    }

    /// 読み取り用に開く。ディレクトリにも使える(`stat` 用)。
    pub fn open_read(&self, name: &str) -> Result<File, FsError> {
        exists_is_impossible(self.open_with(name, OFlags::RDONLY, false))
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
    pub fn open_append(&self, name: &str) -> Result<File, FsError> {
        exists_is_impossible(self.open_with(
            name,
            OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE,
            true,
        ))
    }

    /// 同一ディレクトリ内で名前を付け替える。`rename(2)` は最終要素の
    /// シンボリックリンクを辿らずリンク自体を置き換えるので、宛先が
    /// リンクへ差し替えられていてもリンク先には触れない。
    pub fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        check_component(from)?;
        check_component(to)?;
        match &self.fd {
            Some(fd) => rustix::fs::renameat(fd, from, fd, to).map_err(|e| errno_to_error(e, to)),
            None => std::fs::rename(self.path.join(from), self.path.join(to))
                .map_err(|e| io_to_error(e, to)),
        }
    }

    /// ディレクトリエントリを 1 つ取り除く。シンボリックリンクは辿らず、
    /// リンクそのものを取り除く。
    pub fn unlink(&self, name: &str) -> Result<(), FsError> {
        check_component(name)?;
        match &self.fd {
            Some(fd) => rustix::fs::unlinkat(fd, name, rustix::fs::AtFlags::empty())
                .map_err(|e| errno_to_error(e, name)),
            None => std::fs::remove_file(self.path.join(name)).map_err(|e| io_to_error(e, name)),
        }
    }

    /// 実際に開く。`O_EXCL` 付きで既に何かが存在した場合だけ `Ok(None)`。
    fn open_with(
        &self,
        name: &str,
        oflags: OFlags,
        create: bool,
    ) -> Result<Option<File>, FsError> {
        check_component(name)?;
        let oflags = oflags | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mode = if create {
            Mode::from_bits_truncate(FILE_MODE)
        } else {
            Mode::empty()
        };

        match &self.fd {
            Some(fd) => match openat2_beneath(fd, name, oflags, mode) {
                Ok(Some(fd)) => Ok(Some(File::from(fd))),
                // ディレクトリを開けた時点で `openat2` は使えているので、
                // `Ok(None)`(カーネル未対応)へ来ることは無い。来たなら
                // 拘束が効いていないということなので、黙って緩めず拒否する。
                Ok(None) => Err(FsError::Io(
                    "openat2 became unavailable mid-operation".into(),
                )),
                Err(rustix::io::Errno::EXIST) => Ok(None),
                Err(e) => Err(errno_to_error(e, name)),
            },
            None => {
                // フォールバック: 事前検証済みのディレクトリの中を
                // `O_NOFOLLOW` 付きで開く。最終要素がシンボリックリンクなら
                // カーネルが `ELOOP` を返す。
                use std::os::unix::fs::OpenOptionsExt;
                let mut options = std::fs::OpenOptions::new();
                options
                    .read(!oflags.contains(OFlags::WRONLY))
                    .write(oflags.contains(OFlags::WRONLY))
                    .append(oflags.contains(OFlags::APPEND))
                    .create(oflags.contains(OFlags::CREATE) && !oflags.contains(OFlags::EXCL))
                    .create_new(oflags.contains(OFlags::EXCL))
                    .mode(FILE_MODE)
                    .custom_flags(OFlags::NOFOLLOW.bits() as i32);
                match options.open(self.path.join(name)) {
                    Ok(file) => Ok(Some(file)),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
                    Err(e) => Err(io_to_error(e, name)),
                }
            }
        }
    }
}

/// `openat2` でルート配下に拘束して開く。`openat2` の無いカーネルでは
/// `None` を返し、呼び出し側がフォールバックする。
fn open_dir_beneath(
    canonical_root: &Path,
    rel_dir: &str,
) -> Result<Option<std::os::fd::OwnedFd>, FsError> {
    let root_fd = match rustix::fs::open(
        canonical_root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(e) => return Err(errno_to_error(e, "<root>")),
    };

    // `openat2` に渡すパスは空文字を許さないので、ルート自身は "." で指す。
    let rel = if rel_dir.is_empty() { "." } else { rel_dir };
    openat2_beneath(
        &root_fd,
        rel,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| errno_to_error(e, rel))
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
/// (`EEXIST` を区別したい呼び出し側があるため)。
#[cfg(target_os = "linux")]
fn openat2_beneath(
    dirfd: &impl std::os::fd::AsFd,
    rel: &str,
    oflags: OFlags,
    mode: Mode,
) -> Result<Option<std::os::fd::OwnedFd>, rustix::io::Errno> {
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
) -> Result<Option<std::os::fd::OwnedFd>, rustix::io::Errno> {
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
        other => FsError::Io(other.to_string()),
    }
}

fn io_to_error(error: std::io::Error, what: &str) -> FsError {
    match error.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound(what.to_string()),
        _ => match error.raw_os_error() {
            Some(code)
                if code == rustix::io::Errno::LOOP.raw_os_error()
                    || code == rustix::io::Errno::XDEV.raw_os_error() =>
            {
                FsError::InvalidPath(format!(
                    "{what:?} resolves through a symlink or escapes the granted root"
                ))
            }
            _ => FsError::Io(error.to_string()),
        },
    }
}

//! パス検証。**この機能のサンドボックス境界そのもの**なので、
//! ここを緩めると任意のファイルへの読み書きが通ってしまう。
//!
//! 検証は 3 段で、このモジュールは 1 段目(構文)と 2 段目(正規化後の
//! 配下チェック)を担う。3 段目(`openat2` によるカーネルレベルの拘束)は
//! `crate::openat` にある。

use std::path::{Path, PathBuf};

use crate::FsError;

/// 相対パスを構文レベルで検証し、要素列に分解する。
///
/// ファイルシステムに一切触らないので、ここで弾けるものは必ずここで弾く
/// (触ってから判断する経路を減らすほど、競合の余地が減る)。
pub fn validate_relative(rel: &str) -> Result<Vec<String>, FsError> {
    if rel.is_empty() {
        return Err(FsError::InvalidPath("path must not be empty".into()));
    }
    if rel.contains('\0') {
        return Err(FsError::InvalidPath("path must not contain NUL".into()));
    }
    if rel.chars().any(|c| c.is_control()) {
        return Err(FsError::InvalidPath(
            "path must not contain control characters".into(),
        ));
    }
    if rel.contains('\\') {
        return Err(FsError::InvalidPath(
            "path must not contain a backslash".into(),
        ));
    }
    if rel.starts_with('/') {
        return Err(FsError::InvalidPath("path must be relative".into()));
    }

    let mut components = Vec::new();
    for part in rel.split('/') {
        match part {
            "" => {
                return Err(FsError::InvalidPath(
                    "path must not contain empty components".into(),
                ))
            }
            "." | ".." => {
                return Err(FsError::InvalidPath(format!(
                    "path must not contain a {part:?} component"
                )))
            }
            other => components.push(other.to_string()),
        }
    }
    Ok(components)
}

/// `root` を正規化する。設定時に一度だけ行い、以後の比較の基準にする。
pub fn canonical_root(root: &Path) -> Result<PathBuf, FsError> {
    root.canonicalize()
        .map_err(|e| FsError::InvalidPath(format!("root is unusable: {e}")))
}

/// `path` が `root`(正規化済み)の配下にあることを確認する。
fn ensure_inside(root: &Path, path: &Path) -> Result<(), FsError> {
    if path == root || path.starts_with(root) {
        return Ok(());
    }
    Err(FsError::InvalidPath(
        "resolved path escapes the granted root".into(),
    ))
}

/// 既存パスを解決する(読み取り・stat・delete 用)。
///
/// `canonicalize` はシンボリックリンクを解決するので、リンクで外を指して
/// いればこの時点で配下チェックに落ちる。存在しない場合は `NotFound`
/// (`InvalidPath` と区別する。呼び出し側が「無い」と「触ってはいけない」を
/// 取り違えないため)。
pub fn resolve_existing(root: &Path, rel: &str) -> Result<PathBuf, FsError> {
    let components = validate_relative(rel)?;
    let root = canonical_root(root)?;

    let mut joined = root.clone();
    for component in &components {
        joined.push(component);
    }

    let resolved = match joined.canonicalize() {
        Ok(resolved) => resolved,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FsError::NotFound(rel.to_string()))
        }
        Err(e) => return Err(FsError::Io(e.to_string())),
    };

    ensure_inside(&root, &resolved)?;
    Ok(resolved)
}

/// 書き込み先の親ディレクトリを解決する。無ければ 1 段ずつ作る。
///
/// 1 段作るごとに配下チェックを行うため、途中のディレクトリがシンボリック
/// リンクで外を指していればその時点で落ちる。戻り値は
/// `(正規化済みの親ディレクトリ, ファイル名)`。
pub fn resolve_parent(root: &Path, rel: &str) -> Result<(PathBuf, String), FsError> {
    let mut components = validate_relative(rel)?;
    let name = components
        .pop()
        .ok_or_else(|| FsError::InvalidPath("path must name a file".into()))?;
    let root = canonical_root(root)?;

    let mut current = root.clone();
    for component in &components {
        current.push(component);
        if !current.exists() {
            std::fs::create_dir(&current).map_err(|e| FsError::Io(e.to_string()))?;
        }
        current = current
            .canonicalize()
            .map_err(|e| FsError::Io(e.to_string()))?;
        ensure_inside(&root, &current)?;
        if !current.is_dir() {
            return Err(FsError::InvalidPath(format!(
                "{component:?} is not a directory"
            )));
        }
    }

    Ok((current, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn plain_relative_paths_are_accepted() {
        assert_eq!(validate_relative("a.txt").unwrap(), vec!["a.txt".to_string()]);
        assert_eq!(
            validate_relative("logs/2026-07.csv").unwrap(),
            vec!["logs".to_string(), "2026-07.csv".to_string()]
        );
    }

    #[test]
    fn syntactically_dangerous_paths_are_rejected() {
        for bad in [
            "",                 // 空
            "/etc/passwd",      // 絶対パス
            "../secret",        // 親へ
            "a/../../secret",   // 途中で親へ
            "./a",              // カレント
            "a/./b",            // 途中にカレント
            "a//b",             // 空要素
            "a/",               // 末尾スラッシュ
            "a\\b",             // バックスラッシュ
            "a\0b",             // NUL
            "a\nb",             // 制御文字
        ] {
            assert!(
                validate_relative(bad).is_err(),
                "{bad:?} must be rejected by syntax validation"
            );
        }
    }

    #[test]
    fn existing_file_inside_the_root_resolves() {
        let dir = root();
        fs::write(dir.path().join("a.txt"), b"hi").unwrap();

        let resolved = resolve_existing(dir.path(), "a.txt").expect("inside the root");
        assert!(resolved.starts_with(dir.path().canonicalize().unwrap()));
    }

    #[test]
    fn symlink_pointing_outside_the_root_is_rejected() {
        let dir = root();
        let outside = root();
        fs::write(outside.path().join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();

        let err = resolve_existing(dir.path(), "link")
            .expect_err("a symlink escaping the root must be rejected");
        assert!(matches!(err, FsError::InvalidPath(_)));
    }

    #[test]
    fn symlinked_directory_component_pointing_outside_is_rejected() {
        let dir = root();
        let outside = root();
        fs::create_dir(outside.path().join("d")).unwrap();
        fs::write(outside.path().join("d").join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path().join("d"), dir.path().join("d")).unwrap();

        let err = resolve_existing(dir.path(), "d/secret")
            .expect_err("a symlinked directory component escaping the root must be rejected");
        assert!(matches!(err, FsError::InvalidPath(_)));
    }

    #[test]
    fn missing_file_reports_not_found_not_invalid_path() {
        let dir = root();
        let err = resolve_existing(dir.path(), "nope.txt").expect_err("missing file");
        assert!(matches!(err, FsError::NotFound(_)));
    }

    #[test]
    fn resolve_parent_creates_intermediate_directories_inside_the_root() {
        let dir = root();
        let (parent, name) =
            resolve_parent(dir.path(), "logs/2026/07.csv").expect("nested write target");
        assert_eq!(name, "07.csv");
        assert!(parent.starts_with(dir.path().canonicalize().unwrap()));
        assert!(parent.is_dir());
    }

    #[test]
    fn resolve_parent_refuses_to_follow_a_symlinked_directory_out_of_the_root() {
        let dir = root();
        let outside = root();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();

        let err = resolve_parent(dir.path(), "escape/evil.txt")
            .expect_err("writing through an escaping symlink must be rejected");
        assert!(matches!(err, FsError::InvalidPath(_)));
    }

    /// ハードリンクは「別の inode を指すシンボリックリンク」ではなく、既に
    /// root 配下に実体として存在する新しいディレクトリエントリなので、
    /// `resolve_existing` から見て特別扱いする理由が無い(そのリンクを
    /// root 内へ作れた時点で呼び出し元は既に元ファイルへアクセスできていた
    /// はず)。ここでは「拒否されない」ことだけを確認する。
    #[test]
    fn hardlinked_file_inside_the_root_resolves_like_a_normal_file() {
        let dir = root();
        let outside = root();
        let original = outside.path().join("secret");
        fs::write(&original, b"secret").unwrap();
        fs::hard_link(&original, dir.path().join("linked")).unwrap();

        let resolved = resolve_existing(dir.path(), "linked").expect("hard link resolves");
        assert!(resolved.starts_with(dir.path().canonicalize().unwrap()));
    }

    /// 極端に長いパス要素(ENAMETOOLONG)は `Io` にせよ何にせよエラーで
    /// 止まらなければならない。ここで確認したいのは panic しないことと、
    /// 誤って root 配下に無いパスを `Ok` として返さないこと。
    #[test]
    fn an_absurdly_long_component_is_rejected_not_silently_resolved() {
        let dir = root();
        let long_name = "a".repeat(4096);

        let err = resolve_existing(dir.path(), &long_name)
            .expect_err("an overlong path component must not resolve successfully");
        assert!(matches!(err, FsError::NotFound(_) | FsError::InvalidPath(_) | FsError::Io(_)));
    }

    /// 末尾のドット(`"a."`)は `.` コンポーネントそのものではないので、
    /// 普通のファイル名として構文検証を通るべき(過剰に弾かない)。
    #[test]
    fn trailing_dot_in_a_filename_is_a_plain_component_not_a_dot_component() {
        assert_eq!(
            validate_relative("a.").unwrap(),
            vec!["a.".to_string()]
        );
    }
}

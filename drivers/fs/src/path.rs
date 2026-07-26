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

/// パス要素 1 つが [`validate_relative`] の規則を満たすか。
///
/// `list` が列挙した名前を篩うために使う。「列挙された名前はそのまま他の
/// 操作へ渡せる」という契約を、規則を二重に書かずに保つため
/// (`validate_relative` そのものに通し、要素が 1 つだけであることも確認する)。
pub fn is_valid_component(name: &str) -> bool {
    matches!(validate_relative(name), Ok(components) if components.len() == 1)
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
/// リンクで外を指していればその時点で落ちる。失敗した場合、**この呼び出しが
/// 作成したディレクトリだけ**を後始末する(逆順に `rmdir`。既存だったものは
/// 一切触らない)。そうしないと、必ず失敗する要求を繰り返すだけでルート配下に
/// 任意のディレクトリツリーを作れてしまう(ルート外へは出られないので
/// Critical ではないが、望ましくない副作用)。
///
/// 戻り値は `(正規化済みの親ディレクトリ, ファイル名)`。
///
/// **`name`(ファイル名)は構文検証しか通っていない。** 親ディレクトリと違い、
/// 最終要素がシンボリックリンクかどうかはここでは確認しない
/// (「親を解決する」関数としての契約上、最終要素はまだ存在しないことが
/// 前提のため)。呼び出し側が `parent.join(name)` を組み立てて
/// `std::fs::write`/`OpenOptions` へそのまま渡すと、既存の
/// `root/<name>` が外を指すシンボリックリンクだった場合にサンドボックスの
/// 外へ書き込めてしまう。**実際のオープンは `O_NOFOLLOW`(または
/// `openat2` の `RESOLVE_NO_SYMLINKS`)を使って `parent` に対する openat 相当で
/// 行い、シンボリックリンクなら開かずに拒否すること。**
pub fn resolve_parent(root: &Path, rel: &str) -> Result<(PathBuf, String), FsError> {
    let mut components = validate_relative(rel)?;
    let name = components
        .pop()
        .ok_or_else(|| FsError::InvalidPath("path must name a file".into()))?;
    let root = canonical_root(root)?;

    let mut created: Vec<PathBuf> = Vec::new();
    let mut current = root.clone();
    for component in &components {
        current.push(component);

        // まだ無ければ作る。「作ったかどうか」を覚えておき、後で失敗したら
        // 自分が作った分だけ `rollback` で消す(元から存在したディレクトリは
        // 消さない)。
        let just_created = !current.exists();
        if just_created {
            if let Err(e) = std::fs::create_dir(&current) {
                rollback(&created);
                return Err(FsError::Io(e.to_string()));
            }
        }

        let canon = match current.canonicalize() {
            Ok(c) => c,
            Err(e) => {
                rollback(&created);
                return Err(FsError::Io(e.to_string()));
            }
        };
        if let Err(e) = ensure_inside(&root, &canon) {
            rollback(&created);
            return Err(e);
        }
        if !canon.is_dir() {
            rollback(&created);
            return Err(FsError::InvalidPath(format!(
                "{component:?} is not a directory"
            )));
        }

        current = canon;
        if just_created {
            created.push(current.clone());
        }
    }

    Ok((current, name))
}

/// `resolve_parent` がこの呼び出しの途中で作成したディレクトリを、深い方から
/// 順に取り除く。取り除けなくても(他プロセスとの競合など)無視する —
/// ベストエフォートの後始末であり、これ自体が失敗要因になってはならない。
/// 元から存在していたディレクトリはここには含まれないので、絶対に触らない。
fn rollback(created: &[PathBuf]) {
    for dir in created.iter().rev() {
        let _ = std::fs::remove_dir(dir);
    }
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

    /// 途中まで新規ディレクトリを作ってから失敗するケース。
    ///
    /// シンボリックリンクによる脱出は、脱出用のリンクを新規作成される
    /// ディレクトリの内側に事前配置できない(親がまだ無いので当然置けない)
    /// ため、この呼び出しより前に成功したディレクトリ作成があった上で
    /// さらに深い場所が失敗する状況を作れない。代わりに、1 段掘り進めて
    /// 新規ディレクトリを 1 つ作った直後に、次の要素名を
    /// `NAME_MAX`(255 バイト)超えにして `ENAMETOOLONG` で失敗させ、
    /// 「既に成功して作ったディレクトリ」が後始末されることを確認する。
    #[test]
    fn resolve_parent_rolls_back_directories_it_created_before_failing() {
        let dir = root();

        // "a" は呼び出し前から存在する既存ディレクトリ。ロールバックで
        // 消えてはいけない。
        fs::create_dir(dir.path().join("a")).unwrap();

        let too_long = "x".repeat(300);
        let rel = format!("a/newdir/{too_long}/evil.txt");

        let err = resolve_parent(dir.path(), &rel)
            .expect_err("an overlong component must fail directory creation");
        assert!(matches!(err, FsError::Io(_)));

        // "newdir" はこの呼び出しが作って、同じ呼び出しの失敗で片付けた
        // はずなので残っていない。
        assert!(
            !dir.path().join("a").join("newdir").exists(),
            "resolve_parent must roll back directories it created before failing"
        );
        // "a" は呼び出し前から存在していたので消えていない。
        assert!(
            dir.path().join("a").exists(),
            "resolve_parent must never remove a directory that pre-dates the call"
        );
    }

    /// `ensure_inside` は `Path::starts_with`(コンポーネント単位の比較)を
    /// 使っているため、`root` と `root-evil` のような文字列プレフィックスが
    /// 一致するだけの兄弟ディレクトリへ誤って「配下」と判定しない。将来
    /// 誰かが文字列比較へ書き換えてしまう回帰を防ぐための固定テスト。
    #[test]
    fn sibling_directory_with_a_prefix_matching_root_name_is_not_treated_as_inside() {
        let dir = root();
        let mut evil_name = dir.path().as_os_str().to_os_string();
        evil_name.push("-evil");
        let evil_path = PathBuf::from(evil_name);
        fs::create_dir(&evil_path).expect("create sibling dir with prefix-colliding name");
        fs::write(evil_path.join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(evil_path.join("secret"), dir.path().join("link")).unwrap();

        let err = resolve_existing(dir.path(), "link")
            .expect_err("a prefix-colliding sibling directory must not be treated as inside root");
        assert!(matches!(err, FsError::InvalidPath(_)));

        let _ = fs::remove_dir_all(&evil_path);
    }

    /// ルート**内**を指す相対シンボリックリンクは、`canonicalize` が解決した
    /// 結果がそのままルート配下に留まるので拒否されない。これは意図的な
    /// 仕様として固定する(次タスクの `openat2`/`RESOLVE_NO_SYMLINKS` 側の
    /// 挙動と揃える必要がある)。
    #[test]
    fn relative_symlink_pointing_inside_the_root_resolves_ok() {
        let dir = root();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("target.txt"), b"hi").unwrap();
        std::os::unix::fs::symlink(
            Path::new("sub/target.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();

        let resolved = resolve_existing(dir.path(), "link.txt")
            .expect("a symlink that stays inside the root must resolve, not be rejected");
        assert_eq!(
            resolved,
            dir.path().canonicalize().unwrap().join("sub/target.txt")
        );
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

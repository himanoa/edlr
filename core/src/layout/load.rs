//! `dir/layout.kdl` / `dir/layout.json` の読み込み(I/O はここだけ)。
//!
//! `load_manifest` と同じ「I/O はモジュールの端に 1 つ」の流儀。ただし
//! manifest と違い **どんな失敗もロードを止めない**: 戻り値は常に
//! `(Option<Layout>, Vec<LayoutWarning>)` で、呼び出し側(runner)が
//! warning を tracing に落とす。

use std::fs;
use std::path::Path;

use super::{from_json_str, kdl::from_kdl_str, Layout, LayoutWarning};

/// `dir` から layout を読む。ファイルが無ければ `(None, [])`。
///
/// `.kdl` と `.json` が両方あれば `.kdl` を採用し [`LayoutWarning::BothFilesPresent`]。
/// 読み込み・パース失敗は `(None, [ParseFailed])` に落とす(採用した側が
/// 壊れていても、もう一方へはフォールバックしない — 「どちらが使われるか」を
/// 状況依存にしないため)。
pub fn load_layout(dir: &Path) -> (Option<Layout>, Vec<LayoutWarning>) {
    let kdl_path = dir.join("layout.kdl");
    let json_path = dir.join("layout.json");
    let mut warnings = Vec::new();

    let (path, is_kdl) = match (kdl_path.is_file(), json_path.is_file()) {
        (true, true) => {
            warnings.push(LayoutWarning::BothFilesPresent);
            (kdl_path, true)
        }
        (true, false) => (kdl_path, true),
        (false, true) => (json_path, false),
        (false, false) => return (None, warnings),
    };

    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) => {
            warnings.push(LayoutWarning::ParseFailed(e.to_string()));
            return (None, warnings);
        }
    };

    let parsed = if is_kdl {
        from_kdl_str(&content).map_err(|e| e.to_string())
    } else {
        from_json_str(&content).map_err(|e| e.to_string())
    };
    match parsed {
        Ok((layout, mut parse_warnings)) => {
            warnings.append(&mut parse_warnings);
            (Some(layout), warnings)
        }
        Err(e) => {
            warnings.push(LayoutWarning::ParseFailed(e));
            (None, warnings)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Node;

    fn write(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn missing_files_yield_none_without_warnings() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_layout(dir.path()), (None, vec![]));
    }

    #[test]
    fn reads_kdl() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "layout.kdl",
            "section \"基本\" { field \"voice\" }",
        );
        let (layout, warnings) = load_layout(dir.path());
        assert_eq!(warnings, vec![]);
        let layout = layout.unwrap();
        assert_eq!(layout.sections[0].title, "基本");
        assert_eq!(
            layout.sections[0].children,
            vec![Node::Field {
                field: "voice".into()
            }]
        );
    }

    #[test]
    fn reads_json() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "layout.json",
            r#"{ "sections": [{ "title": "基本", "children": [{ "field": "voice" }] }] }"#,
        );
        let (layout, warnings) = load_layout(dir.path());
        assert_eq!(warnings, vec![]);
        assert_eq!(layout.unwrap().sections[0].title, "基本");
    }

    #[test]
    fn prefers_kdl_when_both_exist() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "layout.kdl", "section \"KDL側\"");
        write(
            dir.path(),
            "layout.json",
            r#"{ "sections": [{ "title": "JSON側" }] }"#,
        );
        let (layout, warnings) = load_layout(dir.path());
        assert_eq!(warnings, vec![LayoutWarning::BothFilesPresent]);
        assert_eq!(layout.unwrap().sections[0].title, "KDL側");
    }

    #[test]
    fn parse_failure_yields_none_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "layout.kdl", "section \"未閉じ {");
        let (layout, warnings) = load_layout(dir.path());
        assert_eq!(layout, None);
        assert!(matches!(warnings[0], LayoutWarning::ParseFailed(_)));
    }
}

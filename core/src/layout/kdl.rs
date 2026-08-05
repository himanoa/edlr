//! `layout.kdl` → [`Layout`] の変換。
//!
//! KDL の文法エラーは `Err`(呼び出し側が layout 全体を捨てて warn)。
//! 文法としては正しいが語彙に無いノード・引数は [`LayoutWarning`] に
//! 拾って読み飛ばす(lenient)。
//!
//! 語彙(v1):
//!
//! ```kdl
//! section "接続" description="サーバへの接続設定" {
//!     field "endpoint"
//!     section "詳細" {
//!         field "timeout"
//!     }
//! }
//! ```

use kdl::{KdlDocument, KdlNode};

use super::{Layout, LayoutWarning, Node, Section};

/// `layout.kdl` の中身をモデルへ変換する。
pub fn from_kdl_str(content: &str) -> Result<(Layout, Vec<LayoutWarning>), kdl::KdlError> {
    let doc: KdlDocument = content.parse()?;
    let mut warnings = Vec::new();
    let sections = doc
        .nodes()
        .iter()
        .filter_map(|node| match convert_node(node, &mut warnings) {
            Some(Node::Section(section)) => Some(section),
            Some(Node::Field { .. }) => {
                // トップレベルの field はセクションに属せない。v1 では
                // 「セクションだけが最上位」なので未知ノード扱いで捨てる。
                warnings.push(LayoutWarning::UnknownNode("field (top-level)".into()));
                None
            }
            None => None,
        })
        .collect();
    Ok((Layout { sections }, warnings))
}

/// KDL ノード 1 つを [`Node`] へ変換する。語彙外なら警告を積んで `None`。
fn convert_node(node: &KdlNode, warnings: &mut Vec<LayoutWarning>) -> Option<Node> {
    match node.name().value() {
        "section" => convert_section(node, warnings).map(Node::Section),
        "field" => {
            let Some(key) = first_string_arg(node) else {
                warnings.push(LayoutWarning::UnknownNode(
                    "field (no string argument)".into(),
                ));
                return None;
            };
            warn_extra_positional_args(node, "field", warnings);
            Some(Node::Field { field: key })
        }
        other => {
            warnings.push(LayoutWarning::UnknownNode(other.to_string()));
            None
        }
    }
}

fn convert_section(node: &KdlNode, warnings: &mut Vec<LayoutWarning>) -> Option<Section> {
    let Some(title) = first_string_arg(node) else {
        warnings.push(LayoutWarning::SectionWithoutTitle);
        return None;
    };
    warn_extra_positional_args(node, "section", warnings);
    let description_entry = node
        .entries()
        .iter()
        .find(|e| e.name().map(|n| n.value()) == Some("description"));
    let description = description_entry
        .and_then(|e| e.value().as_string())
        .map(str::to_string);
    if description_entry.is_some() && description.is_none() {
        warnings.push(LayoutWarning::UnknownNode(
            "description (not a string)".into(),
        ));
    }
    // title(位置引数)と description 以外の名前付き引数は語彙に無い。
    for entry in node.entries() {
        if let Some(name) = entry.name() {
            if name.value() != "description" {
                warnings.push(LayoutWarning::UnknownNode(name.value().to_string()));
            }
        }
    }
    let children = node
        .children()
        .map(|doc| {
            doc.nodes()
                .iter()
                .filter_map(|child| convert_node(child, warnings))
                .collect()
        })
        .unwrap_or_default();
    Some(Section {
        title,
        description,
        children,
    })
}

/// ノードの最初の「名前なし文字列引数」を返す(`section "接続"` の `"接続"`)。
fn first_string_arg(node: &KdlNode) -> Option<String> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
        .map(str::to_string)
}

/// 2つ目以降の位置引数は語彙に無い。黙って捨てず警告を積む。
fn warn_extra_positional_args(node: &KdlNode, name: &str, warnings: &mut Vec<LayoutWarning>) {
    let positional = node.entries().iter().filter(|e| e.name().is_none()).count();
    if positional > 1 {
        warnings.push(LayoutWarning::UnknownNode(format!(
            "{name} (extra positional arguments)"
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::from_json_str;

    const KDL: &str = r#"
section "接続" description="サーバへの接続設定" {
    field "endpoint"
    section "詳細" {
        field "timeout"
    }
}
section "読み上げ" {
    field "voice"
}
"#;

    #[test]
    fn parses_sections_fields_and_nesting() {
        let (layout, warnings) = from_kdl_str(KDL).unwrap();
        assert_eq!(warnings, vec![]);
        assert_eq!(layout.sections.len(), 2);
        assert_eq!(layout.sections[0].title, "接続");
        assert_eq!(
            layout.sections[0].description.as_deref(),
            Some("サーバへの接続設定")
        );
        assert_eq!(
            layout.sections[0].children[0],
            Node::Field {
                field: "endpoint".into()
            }
        );
        match &layout.sections[0].children[1] {
            Node::Section(inner) => assert_eq!(inner.title, "詳細"),
            other => panic!("expected nested section, got {other:?}"),
        }
    }

    #[test]
    fn kdl_and_json_yield_the_same_layout() {
        // スペックの要求: 同じ内容の KDL と JSON は同じ Layout になる。
        let json = r#"{
            "sections": [
                {
                    "title": "接続",
                    "description": "サーバへの接続設定",
                    "children": [
                        { "field": "endpoint" },
                        { "title": "詳細", "children": [{ "field": "timeout" }] }
                    ]
                },
                { "title": "読み上げ", "children": [{ "field": "voice" }] }
            ]
        }"#;
        let (from_kdl, _) = from_kdl_str(KDL).unwrap();
        let (from_json, _) = from_json_str(json).unwrap();
        assert_eq!(from_kdl, from_json);
    }

    #[test]
    fn unknown_node_is_skipped_with_warning() {
        let (layout, warnings) = from_kdl_str(
            r#"
tab "x"
section "基本" {
    field "voice"
    column "y"
}
"#,
        )
        .unwrap();
        assert_eq!(layout.sections.len(), 1);
        assert_eq!(layout.sections[0].children.len(), 1);
        assert_eq!(
            warnings,
            vec![
                LayoutWarning::UnknownNode("tab".into()),
                LayoutWarning::UnknownNode("column".into()),
            ]
        );
    }

    #[test]
    fn section_without_title_is_skipped_with_warning() {
        let (layout, warnings) = from_kdl_str("section { field \"voice\" }").unwrap();
        assert_eq!(layout.sections, vec![]);
        assert_eq!(warnings, vec![LayoutWarning::SectionWithoutTitle]);
    }

    #[test]
    fn field_without_string_argument_is_skipped_with_warning() {
        let (layout, warnings) = from_kdl_str("section \"基本\" { field }").unwrap();
        assert_eq!(layout.sections[0].children, vec![]);
        assert_eq!(
            warnings,
            vec![LayoutWarning::UnknownNode("field (no string argument)".into())]
        );
    }

    #[test]
    fn non_string_description_is_dropped_with_warning() {
        let (layout, warnings) = from_kdl_str("section \"基本\" description=123").unwrap();
        assert_eq!(layout.sections[0].description, None);
        assert_eq!(
            warnings,
            vec![LayoutWarning::UnknownNode("description (not a string)".into())]
        );
    }

    #[test]
    fn extra_positional_args_are_ignored_with_warning() {
        let (layout, warnings) =
            from_kdl_str("section \"基本\" \"余分\" { field \"voice\" \"余分\" }").unwrap();
        assert_eq!(layout.sections[0].title, "基本");
        assert_eq!(
            layout.sections[0].children,
            vec![Node::Field {
                field: "voice".into()
            }]
        );
        assert_eq!(
            warnings,
            vec![
                LayoutWarning::UnknownNode("section (extra positional arguments)".into()),
                LayoutWarning::UnknownNode("field (extra positional arguments)".into()),
            ]
        );
    }

    #[test]
    fn syntax_error_is_err() {
        assert!(from_kdl_str("section \"未閉じ {").is_err());
    }
}

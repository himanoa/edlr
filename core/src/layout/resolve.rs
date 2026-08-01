//! パース済み [`Layout`] を manifest の `[[settings]]` に突き合わせて掃除する。
//!
//! - 存在しないキーへの `field` 参照 → 捨てて [`LayoutWarning::UnknownFieldKey`]
//! - 同じキーの 2 回目以降の参照 → 捨てて [`LayoutWarning::DuplicateFieldKey`]
//! - どこからも参照されなかったキー → 末尾の暗黙セクション
//!   (title 「その他」)に宣言順で入れる。**layout の書き忘れで設定項目が
//!   UI から消えることはない**(スペックのエラー処理表)。
//!
//! ロード時に一度だけ呼ぶ。settings の宣言は manifest 由来で不変なので、
//! 解決結果をエントリに持たせて使い回せる。

use std::collections::HashSet;

use crate::manifest::SettingField;

use super::{Layout, LayoutWarning, Node, Section};

/// 暗黙セクションの見出し。UI にそのまま表示される。
pub const IMPLICIT_SECTION_TITLE: &str = "その他";

pub fn resolve(layout: Layout, settings: &[SettingField]) -> (Layout, Vec<LayoutWarning>) {
    let known: HashSet<&str> = settings.iter().map(|f| f.key()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut warnings = Vec::new();

    let mut sections: Vec<Section> = layout
        .sections
        .into_iter()
        .map(|s| clean_section(s, &known, &mut seen, &mut warnings))
        .collect();

    let leftovers: Vec<Node> = settings
        .iter()
        .map(SettingField::key)
        .filter(|key| !seen.contains(*key))
        .map(|key| Node::Field {
            field: key.to_string(),
        })
        .collect();
    if !leftovers.is_empty() {
        sections.push(Section {
            title: IMPLICIT_SECTION_TITLE.to_string(),
            description: None,
            children: leftovers,
        });
    }

    (Layout { sections }, warnings)
}

fn clean_section(
    section: Section,
    known: &HashSet<&str>,
    seen: &mut HashSet<String>,
    warnings: &mut Vec<LayoutWarning>,
) -> Section {
    let children = section
        .children
        .into_iter()
        .filter_map(|node| match node {
            Node::Field { field } => {
                if !known.contains(field.as_str()) {
                    warnings.push(LayoutWarning::UnknownFieldKey(field));
                    None
                } else if !seen.insert(field.clone()) {
                    warnings.push(LayoutWarning::DuplicateFieldKey(field));
                    None
                } else {
                    Some(Node::Field { field })
                }
            }
            Node::Section(inner) => {
                Some(Node::Section(clean_section(inner, known, seen, warnings)))
            }
        })
        .collect();
    Section {
        children,
        ..section
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Vec<SettingField> {
        vec![
            SettingField::String {
                key: "endpoint".into(),
                label: "Endpoint".into(),
                default: String::new(),
            },
            SettingField::String {
                key: "voice".into(),
                label: "Voice".into(),
                default: String::new(),
            },
            SettingField::Boolean {
                key: "verbose".into(),
                label: "Verbose".into(),
                default: false,
            },
        ]
    }

    fn field(key: &str) -> Node {
        Node::Field { field: key.into() }
    }

    fn section(title: &str, children: Vec<Node>) -> Section {
        Section {
            title: title.into(),
            description: None,
            children,
        }
    }

    #[test]
    fn unknown_key_is_dropped_with_warning() {
        let layout = Layout {
            sections: vec![section("基本", vec![field("endpoint"), field("typo-key")])],
        };
        let (resolved, warnings) = resolve(layout, &settings());
        assert_eq!(resolved.sections[0].children, vec![field("endpoint")]);
        assert!(warnings.contains(&LayoutWarning::UnknownFieldKey("typo-key".into())));
    }

    #[test]
    fn duplicate_keeps_first_occurrence_only() {
        let layout = Layout {
            sections: vec![
                section("A", vec![field("voice")]),
                section("B", vec![field("voice")]),
            ],
        };
        let (resolved, warnings) = resolve(layout, &settings());
        assert_eq!(resolved.sections[0].children, vec![field("voice")]);
        assert_eq!(resolved.sections[1].children, vec![]);
        assert!(warnings.contains(&LayoutWarning::DuplicateFieldKey("voice".into())));
    }

    #[test]
    fn unreferenced_keys_go_to_implicit_trailing_section() {
        let layout = Layout {
            sections: vec![section("基本", vec![field("voice")])],
        };
        let (resolved, warnings) = resolve(layout, &settings());
        assert_eq!(warnings, vec![]);
        let last = resolved.sections.last().unwrap();
        assert_eq!(last.title, IMPLICIT_SECTION_TITLE);
        // 宣言順(endpoint, verbose)で入る。
        assert_eq!(last.children, vec![field("endpoint"), field("verbose")]);
    }

    #[test]
    fn fully_covered_layout_gets_no_implicit_section() {
        let layout = Layout {
            sections: vec![section(
                "全部",
                vec![field("endpoint"), field("voice"), field("verbose")],
            )],
        };
        let (resolved, _) = resolve(layout, &settings());
        assert_eq!(resolved.sections.len(), 1);
    }

    #[test]
    fn nested_section_references_count_as_seen() {
        let layout = Layout {
            sections: vec![section(
                "外",
                vec![Node::Section(section("内", vec![field("voice")]))],
            )],
        };
        let (resolved, _) = resolve(layout, &settings());
        // voice は入れ子側で消費済みなので、暗黙セクションには入らない。
        let last = resolved.sections.last().unwrap();
        assert_eq!(last.children, vec![field("endpoint"), field("verbose")]);
    }
}

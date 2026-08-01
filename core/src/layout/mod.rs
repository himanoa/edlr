//! 設定フォームの見せ方(layout)の正準モデルと変換。
//!
//! データ構造(何があるか)は manifest の `[[settings]]` が持ち、ここは
//! 「どう見せるか」だけを扱う。manifest と違い **lenient**: 不備は
//! `LayoutWarning` として戻り値で返し、パースを打ち切らない。ログ出力は
//! 命令的な呼び出し側(runner)の仕事で、このモジュールは tracing を呼ばない。
//!
//! フォーマットは 2 つ(スペック
//! `docs/superpowers/specs/2026-08-01-settings-layout-design.md` 参照):
//!
//! - `layout.kdl` — 手書き向け([`kdl`] モジュール)
//! - `layout.json` — ツール生成向け。この Rust モデルの serde 表現そのもので、
//!   RPC 応答の `layout` フィールドとも同形

use std::fmt;

pub mod kdl;
pub mod load;
pub mod resolve;

/// layout ファイル 1 枚分の正準モデル。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Layout {
    pub sections: Vec<Section>,
}

/// 見出し付きセクション。入れ子可。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Section {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub children: Vec<Node>,
}

/// セクションの子。`{"field": "key"}` か、入れ子の `Section`。
///
/// untagged なので **`Field` を先に**置く(`{"field": ...}` は `Section` に
/// マッチしない — `title` が無いため — が、判定順を型で明示しておく)。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum Node {
    Field { field: String },
    Section(Section),
}

/// パース・解決中に見つかった不備。呼び出し側が warn ログに落とす。
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutWarning {
    /// KDL/JSON に知らないノード名・キーがあった(無視した)。
    UnknownNode(String),
    /// `section` に title(最初の文字列引数)が無かった(そのノードを捨てた)。
    SectionWithoutTitle,
    /// 存在しない settings キーへの `field` 参照(捨てた)。
    UnknownFieldKey(String),
    /// 同じ settings キーが複数回参照された(最初の出現だけ残した)。
    DuplicateFieldKey(String),
    /// `.kdl` と `.json` が両方あった(`.kdl` を採用した)。
    BothFilesPresent,
    /// ファイルは読めたがパースに失敗した(layout 全体を捨てた)。
    ParseFailed(String),
}

impl fmt::Display for LayoutWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutWarning::UnknownNode(name) => {
                write!(f, "layout: unknown node or key {name:?} is ignored")
            }
            LayoutWarning::SectionWithoutTitle => {
                write!(f, "layout: section without a title is ignored")
            }
            LayoutWarning::UnknownFieldKey(key) => {
                write!(
                    f,
                    "layout: field {key:?} is not declared in [[settings]]; ignored"
                )
            }
            LayoutWarning::DuplicateFieldKey(key) => {
                write!(
                    f,
                    "layout: field {key:?} is referenced more than once; keeping the first"
                )
            }
            LayoutWarning::BothFilesPresent => {
                write!(
                    f,
                    "layout: both layout.kdl and layout.json exist; using layout.kdl"
                )
            }
            LayoutWarning::ParseFailed(e) => write!(f, "layout: parse failed: {e}"),
        }
    }
}

/// `layout.json` の中身をモデルへ読み込む。
///
/// serde は未知キーを黙って無視するため、**トップレベルの**未知キーだけ事前に
/// `serde_json::Value` を歩いて `UnknownNode` 警告に拾う(深追いはしない —
/// lenient の目的は「書き間違いに気付けること」で、網羅的なスキーマ検証ではない)。
///
/// `sections` 配下(`Section`/`Node` の入れ子)の未知キーは拾わない。`Node` は
/// untagged なので、そこでの書き間違いは 2 通りに転ぶ: `Field`/`Section` の
/// どちらにもマッチしなければ `serde_json::from_value` がエラーになり
/// `ParseFailed` として layout 全体を捨てる。たまたまどちらかにマッチして
/// しまえば(例: 余分なキーがあっても既知のフィールドが揃っている)、警告なしで
/// 黙って無視される。
pub fn from_json_str(content: &str) -> Result<(Layout, Vec<LayoutWarning>), serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(content)?;
    let mut warnings = Vec::new();
    if let Some(obj) = value.as_object() {
        for key in obj.keys() {
            if key != "sections" {
                warnings.push(LayoutWarning::UnknownNode(key.clone()));
            }
        }
    }
    let layout: Layout = serde_json::from_value(value)?;
    Ok((layout, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrips_nested_sections() {
        let json = r#"{
            "sections": [
                {
                    "title": "接続",
                    "description": "サーバへの接続設定",
                    "children": [
                        { "field": "endpoint" },
                        { "title": "詳細", "children": [{ "field": "timeout" }] }
                    ]
                }
            ]
        }"#;
        let (layout, warnings) = from_json_str(json).unwrap();
        assert_eq!(warnings, vec![]);
        assert_eq!(layout.sections.len(), 1);
        let section = &layout.sections[0];
        assert_eq!(section.title, "接続");
        assert_eq!(section.description.as_deref(), Some("サーバへの接続設定"));
        assert_eq!(
            section.children[0],
            Node::Field {
                field: "endpoint".into()
            }
        );
        match &section.children[1] {
            Node::Section(inner) => {
                assert_eq!(inner.title, "詳細");
                assert_eq!(
                    inner.children,
                    vec![Node::Field {
                        field: "timeout".into()
                    }]
                );
            }
            other => panic!("expected nested section, got {other:?}"),
        }
    }

    #[test]
    fn json_unknown_top_level_key_warns_but_parses() {
        let json = r#"{ "sections": [], "sectons": [] }"#;
        let (layout, warnings) = from_json_str(json).unwrap();
        assert_eq!(layout.sections, vec![]);
        assert_eq!(warnings, vec![LayoutWarning::UnknownNode("sectons".into())]);
    }

    #[test]
    fn json_parse_failure_is_err() {
        assert!(from_json_str("{ not json").is_err());
    }

    #[test]
    fn serializes_back_to_same_shape() {
        // RPC 応答は Layout の serde 表現をそのまま使う。description 無しの
        // セクションでフィールドごと消えること(`skip_serializing_if`)を固定。
        let layout = Layout {
            sections: vec![Section {
                title: "基本".into(),
                description: None,
                children: vec![Node::Field {
                    field: "voice".into(),
                }],
            }],
        };
        let json = serde_json::to_value(&layout).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "sections": [
                    { "title": "基本", "children": [{ "field": "voice" }] }
                ]
            })
        );
    }
}

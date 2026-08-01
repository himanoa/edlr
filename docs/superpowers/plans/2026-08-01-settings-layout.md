# 設定画面レイアウト(layout ファイル)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** プラグイン/ドライバのディレクトリに任意で置ける `layout.kdl` / `layout.json` を読み、設定フォームをセクション付きで描画できるようにする(スペック: `docs/superpowers/specs/2026-08-01-settings-layout-design.md`)。

**Architecture:** core に純粋モジュール `layout/` を新設(モデル・KDL/JSON 変換・参照解決。すべて値イン値アウト、警告は戻り値)。runner がロード時にファイルを読んで解決済み `Layout` を registry のエントリに格納し、`plugins/list` / `drivers/list` の各エントリに `layout` フィールド(無ければ `null`)として同梱。UI は `layout` があればセクション描画、無ければ現行の平坦描画。

**Tech Stack:** Rust(serde / serde_json / `kdl` crate 6 系を新規依存)、React + TypeScript + vitest(ui/frontend)。

## Global Constraints

- layout の不備は**プラグインのロードを一切妨げない**。常に「警告 + フォールバック」(スペックのエラー処理表に従う)
- 純粋モジュール `layout/` では `tracing` を呼ばない。警告は `Vec<LayoutWarning>` で返し、ログ出力は命令的な呼び出し側(runner)が行う(`.claude/rules/pure-imperative-boundary.md`)
- ファイル I/O は `load_manifest` と同じ流儀で `layout` モジュールの端(`load.rs`)にだけ置く
- 既存の統合テストは消さない・書き換えない(`.claude/rules/testing.md`)。`PluginEntry` / `DriverEntry` のフィールド追加でコンパイルが壊れるテスト箇所への `layout: None` 追記は例外(挙動を変えないため)
- コミットメッセージは既存の慣行(日本語、`feat(scope): ...` / `fix(...): ...`)に合わせる
- cargo コマンドはこの worktree 内で並走させない(CLAUDE.md)。サブエージェント並列前には `cargo fetch` を一度実行する
- 作業中にタスク範囲外の問題を見つけたら `git issues` で起票する(CLAUDE.md)

---

### Task 1: 純粋モジュール `layout` — モデルと JSON 読み込み

**Files:**
- Create: `core/src/layout/mod.rs`
- Modify: `core/src/lib.rs`(`pub mod layout;` を追加。既存の `pub mod manifest;` などと同じ並びに)

**Interfaces:**
- Produces:
  - `layout::Layout { sections: Vec<Section> }`
  - `layout::Section { title: String, description: Option<String>, children: Vec<Node> }`
  - `layout::Node`(`Field { field: String }` | `Section(Section)` の untagged enum)
  - `layout::LayoutWarning`(enum、`Display` 実装)
  - `layout::from_json_str(&str) -> Result<(Layout, Vec<LayoutWarning>), serde_json::Error>`

- [ ] **Step 1: モデルと失敗テストを書く**

`core/src/layout/mod.rs` を作成:

```rust
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
                write!(f, "layout: field {key:?} is not declared in [[settings]]; ignored")
            }
            LayoutWarning::DuplicateFieldKey(key) => {
                write!(f, "layout: field {key:?} is referenced more than once; keeping the first")
            }
            LayoutWarning::BothFilesPresent => {
                write!(f, "layout: both layout.kdl and layout.json exist; using layout.kdl")
            }
            LayoutWarning::ParseFailed(e) => write!(f, "layout: parse failed: {e}"),
        }
    }
}

/// `layout.json` の中身をモデルへ読み込む。
///
/// serde は未知キーを黙って無視するため、トップレベルと `sections` 配下の
/// 未知キーだけ事前に `serde_json::Value` を歩いて `UnknownNode` 警告に拾う
/// (深追いはしない — lenient の目的は「書き間違いに気付けること」で、
/// 網羅的なスキーマ検証ではない)。
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
            Node::Field { field: "endpoint".into() }
        );
        match &section.children[1] {
            Node::Section(inner) => {
                assert_eq!(inner.title, "詳細");
                assert_eq!(inner.children, vec![Node::Field { field: "timeout".into() }]);
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
                children: vec![Node::Field { field: "voice".into() }],
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
```

この時点では `pub mod kdl;` / `pub mod load;` / `pub mod resolve;` が未作成でコンパイルが通らないため、**Step 1 ではこの 3 行をコメントアウトしておき、後続タスクで 1 行ずつ解除する**。

`core/src/lib.rs` に `pub mod layout;` を追加(アルファベット順の並びに合わせて `journal` と `logs` の間)。

- [ ] **Step 2: テストが失敗する(コンパイルできない)ことを確認**

Run: `cargo test -p edlr-core layout::`
Expected: この時点で `Layout` 未定義などのコンパイルエラー、または(モデルまで書いた後なら)テストが FAIL。「書いたテストが最初に赤い」ことを一度は確認する。

- [ ] **Step 3: テストが通ることを確認**

Run: `cargo test -p edlr-core layout::`
Expected: 4 テストすべて PASS

- [ ] **Step 4: コミット**

```bash
git add core/src/layout/mod.rs core/src/lib.rs
git commit -m "feat(layout): 設定フォームレイアウトの正準モデルと layout.json 読み込みを追加"
```

---

### Task 2: `kdl` crate 依存追加と `layout.kdl` 変換

**Files:**
- Modify: `core/Cargo.toml`(`[dependencies]` に `kdl = "6"` を追加)
- Create: `core/src/layout/kdl.rs`
- Modify: `core/src/layout/mod.rs`(`pub mod kdl;` のコメントアウト解除)

**Interfaces:**
- Consumes: Task 1 の `Layout` / `Section` / `Node` / `LayoutWarning`
- Produces: `layout::kdl::from_kdl_str(&str) -> Result<(Layout, Vec<LayoutWarning>), ::kdl::KdlError>`

- [ ] **Step 1: 依存を追加して fetch**

`core/Cargo.toml` の `[dependencies]` に `kdl = "6"` を追加し、`cargo fetch` を実行。

- [ ] **Step 2: 失敗するテストごと実装ファイルを書く**

`core/src/layout/kdl.rs` を作成:

```rust
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
    let mut sections = Vec::new();
    for node in doc.nodes() {
        match convert_node(node, &mut warnings) {
            Some(Node::Section(section)) => sections.push(section),
            Some(Node::Field { .. }) => {
                // トップレベルの field はセクションに属せない。v1 では
                // 「セクションだけが最上位」なので未知ノード扱いで捨てる。
                warnings.push(LayoutWarning::UnknownNode("field (top-level)".into()));
            }
            None => {}
        }
    }
    Ok((Layout { sections }, warnings))
}

/// KDL ノード 1 つを [`Node`] へ変換する。語彙外なら警告を積んで `None`。
fn convert_node(node: &KdlNode, warnings: &mut Vec<LayoutWarning>) -> Option<Node> {
    match node.name().value() {
        "section" => convert_section(node, warnings).map(Node::Section),
        "field" => {
            let key = first_string_arg(node)?;
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
    let description = node
        .entries()
        .iter()
        .find(|e| e.name().map(|n| n.value()) == Some("description"))
        .and_then(|e| e.value().as_string())
        .map(str::to_string);
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
            Node::Field { field: "endpoint".into() }
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
    fn syntax_error_is_err() {
        assert!(from_kdl_str("section \"未閉じ {").is_err());
    }
}
```

`core/src/layout/mod.rs` の `pub mod kdl;` を解除。

注意: `kdl` crate 6 系の API(`KdlDocument`/`KdlNode`/`KdlEntry` のメソッド名)は上のコードの想定と違う可能性がある。コンパイルエラーになったら `cargo doc -p kdl` か docs.rs で 6 系の API を確認して合わせること。**テストの期待値は変えない**。

- [ ] **Step 3: テストが失敗することを確認**

Run: `cargo test -p edlr-core layout::kdl`
Expected: 実装前(またはビルド調整中)は FAIL / コンパイルエラー

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core layout::`
Expected: Task 1 の分も含め全 PASS

- [ ] **Step 5: コミット**

```bash
git add core/Cargo.toml Cargo.lock core/src/layout/kdl.rs core/src/layout/mod.rs
git commit -m "feat(layout): kdl crate を導入し layout.kdl の変換を追加"
```

---

### Task 3: 参照解決(`resolve`)— 不正参照の除去・重複除去・「その他」補完

**Files:**
- Create: `core/src/layout/resolve.rs`
- Modify: `core/src/layout/mod.rs`(`pub mod resolve;` の解除)

**Interfaces:**
- Consumes: Task 1 のモデル、`crate::manifest::SettingField`(`key()` メソッドを使う)
- Produces: `layout::resolve::resolve(Layout, &[SettingField]) -> (Layout, Vec<LayoutWarning>)`

- [ ] **Step 1: 失敗するテストごと実装ファイルを書く**

`core/src/layout/resolve.rs` を作成:

```rust
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
        .map(|key| Node::Field { field: key.to_string() })
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
    Section { children, ..section }
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
```

`core/src/layout/mod.rs` の `pub mod resolve;` を解除。

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-core layout::resolve`
Expected: 実装前は FAIL / コンパイルエラー

- [ ] **Step 3: テストが通ることを確認**

Run: `cargo test -p edlr-core layout::`
Expected: 全 PASS

- [ ] **Step 4: コミット**

```bash
git add core/src/layout/resolve.rs core/src/layout/mod.rs
git commit -m "feat(layout): settings キーへの参照解決と暗黙「その他」セクション補完を追加"
```

---

### Task 4: ファイル読み込みの端 `load_layout`

**Files:**
- Create: `core/src/layout/load.rs`
- Modify: `core/src/layout/mod.rs`(`pub mod load;` の解除)

**Interfaces:**
- Consumes: Task 1〜2 の `from_json_str` / `kdl::from_kdl_str`
- Produces: `layout::load::load_layout(dir: &Path) -> (Option<Layout>, Vec<LayoutWarning>)` — **resolve は呼ばない**(呼び出し側が manifest とあわせて呼ぶ)

- [ ] **Step 1: 失敗するテストごと実装ファイルを書く**

`core/src/layout/load.rs` を作成:

```rust
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
        write(dir.path(), "layout.kdl", "section \"基本\" { field \"voice\" }");
        let (layout, warnings) = load_layout(dir.path());
        assert_eq!(warnings, vec![]);
        let layout = layout.unwrap();
        assert_eq!(layout.sections[0].title, "基本");
        assert_eq!(
            layout.sections[0].children,
            vec![Node::Field { field: "voice".into() }]
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
```

`core/src/layout/mod.rs` の `pub mod load;` を解除。

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-core layout::load`
Expected: 実装前は FAIL / コンパイルエラー

- [ ] **Step 3: テストが通ることを確認**

Run: `cargo test -p edlr-core layout::`
Expected: 全 PASS

- [ ] **Step 4: コミット**

```bash
git add core/src/layout/load.rs core/src/layout/mod.rs
git commit -m "feat(layout): layout.kdl / layout.json のファイル読み込みを追加"
```

---

### Task 5: registry / runner 配線(plugin 側)

**Files:**
- Modify: `core/src/registry/plugin.rs`(`PluginEntry` / `PluginInfo` に `layout` 追加、`Registry::list` で伝搬。**同ファイル内テストの `registry.push(PluginEntry { ... })` 全箇所に `layout: None,` を追記**)
- Modify: `core/src/runner/plugin.rs`(ロード時に `load_layout` + `resolve` を呼び、warn ログを出し、エントリへ格納)

**Interfaces:**
- Consumes: `layout::load::load_layout`, `layout::resolve::resolve`, `layout::{Layout, LayoutWarning}`
- Produces: `PluginEntry.layout: Option<Layout>`, `PluginInfo.layout: Option<Layout>`(Task 7 の RPC 整形が読む)

- [ ] **Step 1: registry の list テストを先に書く**

`core/src/registry/plugin.rs` のテストモジュールに追加(既存テストの `registry.push(PluginEntry { ... })` の作法を流用。`plain_manifest` 相当のヘルパーが同ファイルにあるのでそれに合わせる):

```rust
#[test]
fn list_carries_layout_through() {
    // Registry::list が entry の layout をそのまま PluginInfo へ載せることの固定。
    let registry = test_registry(); // 既存テストと同じ構築ヘルパーを使う
    let layout = crate::layout::Layout {
        sections: vec![crate::layout::Section {
            title: "基本".into(),
            description: None,
            children: vec![crate::layout::Node::Field { field: "voice".into() }],
        }],
    };
    registry.push(PluginEntry {
        manifest: manifest_with_settings(), // voice を持つ既存ヘルパー流用
        state: PluginState::Running,
        layout: Some(layout.clone()),
        // 残りのフィールドは既存テストのボイラープレートに合わせる
        ..
    });
    let infos = registry.list();
    assert_eq!(infos[0].layout, Some(layout));
}
```

(`..` の部分は既存テストの `PluginEntry` 構築をそのまま写す。`PluginInfo` に `PartialEq` が無い場合はフィールド単位で assert する。)

- [ ] **Step 2: コンパイルが通らない/テストが失敗することを確認**

Run: `cargo test -p edlr-core registry::plugin`
Expected: `layout` フィールド未定義でコンパイルエラー

- [ ] **Step 3: 実装**

1. `PluginEntry` に追加:

```rust
    /// `layout.kdl` / `layout.json` 由来の解決済みレイアウト。無ければ None
    /// (UI は平坦フォームで描画する)。ロード時に一度だけ解決する
    /// (`crate::layout::resolve` — settings の宣言は不変なので使い回せる)。
    pub layout: Option<crate::layout::Layout>,
```

2. `PluginInfo` にも `pub layout: Option<crate::layout::Layout>,` を追加。
3. `Registry::list` のスナップショット取得を `(entry.manifest.clone(), entry.state.clone())` から `(entry.manifest.clone(), entry.state.clone(), entry.layout.clone())` に広げ、`PluginInfo { layout, .. }` へ詰める。
4. 同ファイル内テストの `registry.push(PluginEntry {` 全箇所(8 箇所前後)に `layout: None,` を追記。
5. `core/src/runner/plugin.rs` の `load_and_run_plugin` 冒頭(`let entry_path = ...` の直後)に:

```rust
    let (layout, layout_warnings) = crate::layout::load::load_layout(dir);
    let (layout, layout_warnings) = match layout {
        Some(parsed) => {
            let (resolved, mut resolve_warnings) =
                crate::layout::resolve::resolve(parsed, &manifest.settings);
            let mut all = layout_warnings;
            all.append(&mut resolve_warnings);
            (Some(resolved), all)
        }
        None => (None, layout_warnings),
    };
    for warning in &layout_warnings {
        tracing::warn!(plugin = %manifest.id, "{warning}");
    }
```

6. `registry.push(PluginEntry { ... })`(runner/plugin.rs:379 付近)に `layout,` を追加。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core`
Expected: 全 PASS(既存テストの挙動不変)

- [ ] **Step 5: コミット**

```bash
git add core/src/registry/plugin.rs core/src/runner/plugin.rs
git commit -m "feat(registry): プラグインのロード時に layout を読み込み PluginInfo へ伝搬"
```

---

### Task 6: registry / runner 配線(driver 側)

**Files:**
- Modify: `core/src/registry/driver.rs`(`DriverEntry` / `DriverInfo` に `layout` 追加、`DriverRegistry::list` で伝搬。テストの `registry.push(DriverEntry {` 全箇所に `layout: None,`)
- Modify: `core/src/runner/driver.rs`(ロード時に `load_layout` + `resolve`、warn ログ、`registry.push(DriverEntry { ... })`(247 行付近)へ `layout,`)

**Interfaces:**
- Consumes: Task 5 と同じ layout API。resolve へ渡す settings は `manifest.settings`(`DriverManifest` も `settings: Vec<SettingField>` を持つ)
- Produces: `DriverInfo.layout: Option<crate::layout::Layout>`

- [ ] **Step 1: Task 5 の Step 1〜3 を driver 側で対称に行う**

テストも対称に 1 本(`DriverRegistry::list` が layout を運ぶことの固定)。`PluginEntry` と `DriverEntry` は意図的に対称な構造なので、Task 5 の変更をそのまま写す。runner 側の warn ログは `tracing::warn!(driver = %manifest.id, "{warning}")`。

- [ ] **Step 2: テストが通ることを確認**

Run: `cargo test -p edlr-core`
Expected: 全 PASS

- [ ] **Step 3: コミット**

```bash
git add core/src/registry/driver.rs core/src/runner/driver.rs
git commit -m "feat(registry): ドライバのロード時にも layout を読み込み DriverInfo へ伝搬"
```

---

### Task 7: RPC 応答へ `layout` を追加(plugins/list / drivers/list)

**Files:**
- Modify: `core/src/server/rpc_plugins.rs`(`plugin_entry_json` に 1 行)
- Modify: `core/src/server/rpc_drivers.rs`(`driver_entry_json` に 1 行)
- Modify: `core/src/server/tests.rs`(pin テスト追加)

**Interfaces:**
- Consumes: `PluginInfo.layout` / `DriverInfo.layout`(Task 5・6)
- Produces: `plugins/list` の各エントリの `"layout"` フィールド(`Layout` の serde JSON、無ければ `null`)。UI(Task 8)はこの形を読む

- [ ] **Step 1: pin テストを書く**

`core/src/server/tests.rs` に、既存の list 系テストの作法(WS/HTTP 経由の呼び出しヘルパー)に合わせて追加:

```rust
#[tokio::test]
async fn plugins_list_includes_layout_or_null() {
    // layout 無しプラグイン → "layout": null が明示的に載る
    // (フィールド省略ではなく null。UI が「無い」を undefined と区別しない
    //  で済むように、スペックの「無ければ null」を JSON 形として固定する)。
    // 既存の plugins/list テストのセットアップを流用し、応答 JSON に対して:
    //   assert_eq!(plugin_json["layout"], serde_json::Value::Null);
    // layout 有りの場合は registry へ layout 付き PluginEntry を push して:
    //   assert_eq!(plugin_json["layout"]["sections"][0]["title"], "基本");
}
```

(具体のセットアップは `core/src/server/tests.rs` 内の既存 `plugins/list` テストを開いてそれに合わせる。**新しいテスト基盤を発明しない**。)

`"layout": null` を明示するため、`plugin_entry_json` では `serde_json::json!` マクロ内に足す(`Option` の serialize は `None` → `null` になるのでそのままで良い)。

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-core server::tests::plugins_list_includes_layout`
Expected: FAIL(`layout` キーが存在しない)

- [ ] **Step 3: 実装**

`rpc_plugins.rs` の `plugin_entry_json` の `json!` マクロへ追加:

```rust
        "layout": info.layout,
```

`rpc_drivers.rs` の `driver_entry_json` にも同様に `"layout": info.layout,`。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core`
Expected: 全 PASS

- [ ] **Step 5: コミット**

```bash
git add core/src/server/rpc_plugins.rs core/src/server/rpc_drivers.rs core/src/server/tests.rs
git commit -m "feat(rpc): plugins/list と drivers/list の各エントリに layout を同梱"
```

---

### Task 8: UI — 型と PluginForm のセクション描画

**Files:**
- Modify: `ui/frontend/src/types/plugin.ts`(Layout 型を追加、`PluginInfo` / `DriverInfo` に `layout` を追加)
- Modify: `ui/frontend/src/components/PluginForm.tsx`(layout があればセクション描画)
- Test: `ui/frontend/src/components/PluginForm.test.tsx`

**Interfaces:**
- Consumes: Task 7 の RPC 形(`layout: { sections: [...] } | null`)
- Produces:

```ts
export interface LayoutSection {
  title: string;
  description?: string;
  children: LayoutNode[];
}
export type LayoutNode = { field: string } | LayoutSection;
export interface Layout {
  sections: LayoutSection[];
}
// FormPlugin が layout?: Layout | null を含むようになる
```

- [ ] **Step 1: 失敗するテストを書く**

`PluginForm.test.tsx` に既存テストの作法(render + plugin オブジェクトの組み立て)で追加:

```tsx
describe("layout", () => {
  const settings: SettingField[] = [
    { type: "string", key: "endpoint", label: "Endpoint", default: "" },
    { type: "string", key: "voice", label: "Voice", default: "" },
  ];

  it("layout があればセクション見出しと説明を描画する", () => {
    render(
      <PluginForm
        plugin={{
          id: "p1",
          settings,
          values: {},
          layout: {
            sections: [
              {
                title: "接続",
                description: "サーバへの接続設定",
                children: [{ field: "endpoint" }],
              },
              { title: "読み上げ", children: [{ field: "voice" }] },
            ],
          },
        }}
        onChange={async () => {}}
      />,
    );
    expect(screen.getByRole("heading", { name: "接続" })).toBeInTheDocument();
    expect(screen.getByText("サーバへの接続設定")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "読み上げ" })).toBeInTheDocument();
    expect(screen.getByLabelText("Endpoint")).toBeInTheDocument();
    expect(screen.getByLabelText("Voice")).toBeInTheDocument();
  });

  it("入れ子セクションも描画する", () => {
    render(
      <PluginForm
        plugin={{
          id: "p1",
          settings,
          values: {},
          layout: {
            sections: [
              {
                title: "外",
                children: [
                  { field: "endpoint" },
                  { title: "内", children: [{ field: "voice" }] },
                ],
              },
            ],
          },
        }}
        onChange={async () => {}}
      />,
    );
    expect(screen.getByRole("heading", { name: "内" })).toBeInTheDocument();
    expect(screen.getByLabelText("Voice")).toBeInTheDocument();
  });

  it("layout が null なら従来どおり平坦に描画する", () => {
    render(
      <PluginForm
        plugin={{ id: "p1", settings, values: {}, layout: null }}
        onChange={async () => {}}
      />,
    );
    expect(screen.queryByRole("heading")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Endpoint")).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cd ui/frontend && pnpm test -- PluginForm`
Expected: 新規 3 ケースが FAIL

- [ ] **Step 3: 実装**

1. `types/plugin.ts` に上記 `Layout` / `LayoutSection` / `LayoutNode` を追加し、`PluginInfo` と `DriverInfo`(両方 `settings` を持つ interface)へ `layout: Layout | null;` を追加。
2. `PluginForm.tsx`:
   - `FormPlugin` を `Pick<PluginInfo, "id" | "settings" | "values"> & Partial<Pick<PluginInfo, "secretsSet" | "layout">>` に広げる。
   - フィールド描画を関数に切り出し(既存の `<Field .../>` 呼び出しをそのまま使う)、layout 有りのときは再帰でセクションを描く:

```tsx
  const fieldByKey = new Map(plugin.settings.map((f) => [f.key, f]));

  const renderField = (key: string) => {
    const field = fieldByKey.get(key);
    if (!field) return null; // サーバ側 resolve 済みなので通常来ないが防御
    return (
      <Field
        key={field.key}
        field={field}
        value={values[field.key]}
        isSecretSet={(plugin.secretsSet ?? []).includes(field.key)}
        disabled={savingKey === field.key}
        onChange={(v) => update(field.key, v)}
      />
    );
  };

  const renderSection = (section: LayoutSection, depth: number) => (
    <section key={section.title} className="form-section">
      {depth === 0 ? <h3>{section.title}</h3> : <h4>{section.title}</h4>}
      {section.description && <p className="note">{section.description}</p>}
      {section.children.map((node) =>
        "field" in node ? renderField(node.field) : renderSection(node, depth + 1),
      )}
    </section>
  );
```

   - `return` 内を `plugin.layout ? plugin.layout.sections.map((s) => renderSection(s, 0)) : plugin.settings.map((field) => renderField(field.key))` に。
3. `LayoutSection` の判別は `"field" in node`(`LayoutNode` の Field 側だけが `field` キーを持つ)。

- [ ] **Step 4: テスト全体が通ることを確認**

Run: `cd ui/frontend && pnpm test`
Expected: 既存含め全 PASS(平坦描画のリグレッションが無いこと)

- [ ] **Step 5: コミット**

```bash
git add ui/frontend/src/types/plugin.ts ui/frontend/src/components/PluginForm.tsx ui/frontend/src/components/PluginForm.test.tsx
git commit -m "feat(ui): 設定フォームで layout(セクション・説明文)を描画"
```

---

### Task 9: UI — フォームスタイル刷新

**Files:**
- Modify: `ui/frontend/src/index.css`

**Interfaces:**
- Consumes: Task 8 の `className`(`plugin-form` / `form-section` / `form-row` / `note` / `form-error`)

- [ ] **Step 1: スタイルを書く**

`index.css` の既存トーン(変数・色・余白の流儀)に合わせて:

- `.plugin-form .form-section` — カード化(背景 or 枠線、`padding`、セクション間 `margin`)。入れ子セクション(`.form-section .form-section`)は枠を弱め、左インデントのみ
- `.form-section h3` / `h4` — 見出しのサイズ・余白
- `.form-row` — ラベルと入力の揃え(グリッド or flex で縦ズレを解消)、行間
- `.note` — セクション説明文としても読める余白調整

具体値は既存 CSS の変数・スケールに従う。**新しいカラーパレットや CSS フレームワークを持ち込まない**。

- [ ] **Step 2: 目視確認**

Run: `cd ui/frontend && pnpm dev`(または既存の確認手順)で Plugins ページを開き、layout 有り(Task 10 の例を先取りしてローカルに置いてもよい)/無しの両方の見た目を確認。

- [ ] **Step 3: テストが通ることを確認**

Run: `cd ui/frontend && pnpm test`
Expected: 全 PASS(スタイルのみの変更で壊れないこと)

- [ ] **Step 4: コミット**

```bash
git add ui/frontend/src/index.css
git commit -m "feat(ui): 設定フォームのスタイルを刷新(セクションのカード化・行の整列)"
```

---

### Task 10: 例とドキュメント

**Files:**
- Create: `examples/plugins/tutorial-jump-log-rs/layout.kdl`(チュートリアルプラグインに実例を 1 つ)
- Modify: `docs/plugins.md`(「設定画面のレイアウト(layout.kdl / layout.json)」の節を追加)
- Modify: `docs/drivers.md`(同じ仕組みが使える旨と plugins.md への参照を 1 段落)

**Interfaces:**
- Consumes: これまでの全タスクの成果(語彙・lenient 挙動)

- [ ] **Step 1: 例を書く**

`tutorial-jump-log-rs` の `manifest.toml` の `[[settings]]` を見て、実在するキーだけで `layout.kdl` を書く(存在しないキーを書くと warn ログの実例になってしまう)。キーが 1〜2 個しか無い場合はセクション 1 つ+ description で十分。

- [ ] **Step 2: ドキュメントを書く**

`docs/plugins.md` に節を追加。内容はスペックの要約:

- 置き場所(`manifest.toml` と同じディレクトリ、任意)
- KDL の語彙(`section` / `field`、description)と JSON の同形
- lenient 挙動の表(スペックのエラー処理表を転記)
- 「書き忘れたキーは末尾の「その他」セクションに出る」

- [ ] **Step 3: 動作確認**

Run: `scripts/install-examples`(または既存のインストール手順)で例を配置し、デーモン起動 → UI でセクションが出ることを確認。`cargo test -p edlr-core` も最終確認で全 PASS を確認。

- [ ] **Step 4: コミット**

```bash
git add examples/plugins/tutorial-jump-log-rs/layout.kdl docs/plugins.md docs/drivers.md
git commit -m "docs: 設定画面レイアウト(layout.kdl)のドキュメントとチュートリアル実例を追加"
```

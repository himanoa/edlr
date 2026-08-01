# 設定画面レイアウト(layout ファイル)設計

日付: 2026-08-01
状態: レビュー待ち

## 背景と目的

プラグイン/ドライバの設定フォームは、manifest の `[[settings]]`
(`SettingField` の平坦なリスト)を `PluginForm.tsx` が上から順に汎用
レンダリングするだけで、グループ化・説明文などの表現力がなく、見た目にも
不満がある。

**同じデータ構造(`[[settings]]`)はそのままに、「どう見せるか」だけを
別に記述できる仕組み**を導入する。あわせてフォーム自体のスタイルも刷新する。

## 方針

### manifest とは別ファイルにする

理由は二つ:

1. **関心の分離** — manifest は「何があるか」(データ構造・検証)、
   layout は「どう見せるか」。manifest を肥大化させない。
2. **検証の厳格度の分離** — manifest は壊れていればロード拒否
   (`deny_unknown_fields` の厳格な世界)。layout は**壊れていても
   プラグインは動く**べきで、警告してフォールバックする lenient な世界。
   同じファイルに両方の作法を同居させない。

### 木構造を第一級にする

セクションの入れ子や、将来のタブ・条件表示はいずれも木。平坦な TOML
テーブル連鎖では破綻するため、木を素直に書けるフォーマットを選ぶ。

### フォーマットは KDL と JSON の両対応

- **`layout.kdl`** — 手書き向けの推奨フォーマット。木構造のための
  ドキュメント言語で、コメント可・入れ子が自然(Zellij の layout で実績)。
  `kdl` crate(6.x)を依存に追加する。
- **`layout.json`** — ツール生成・機械出力向け。内部モデル(後述)を
  serde_json で直接 deserialize するだけなので追加コストはほぼない。
- 両方存在したら **`layout.kdl` を優先し、警告ログを出す**(拒否しない)。

### v1 の語彙は最小(YAGNI)

セクション(入れ子可)+説明文のみ。タブ・条件表示(`visible-when`)は
**入れない**。必要とする実プラグインが現れた時点で、ノード/属性の追加と
して互換を保って拡張する。木構造を最初から採っているのはこの拡張余地の
ためであり、v1 で語彙を増やす理由にはしない。

## ファイル形式

プラグイン/ドライバのディレクトリに `manifest.toml` と並べて置く。任意。

```kdl
// layout.kdl
section "接続" description="COEIROINK サーバへの接続設定" {
    field "endpoint"
    field "api-key"
}
section "読み上げ" {
    field "voice"
    field "speed"
    section "詳細" {
        field "pitch"
    }
}
```

```json
// layout.json(同じ内容)
{
  "sections": [
    {
      "title": "接続",
      "description": "COEIROINK サーバへの接続設定",
      "children": [{ "field": "endpoint" }, { "field": "api-key" }]
    },
    {
      "title": "読み上げ",
      "children": [
        { "field": "voice" },
        { "field": "speed" },
        { "title": "詳細", "children": [{ "field": "pitch" }] }
      ]
    }
  ]
}
```

### 語彙(v1)

- `section` — `title`(必須)、`description`(任意)、子として `field` /
  `section` を任意個持つ
- `field` — `[[settings]]` のキーへの参照。データ構造側の情報(型・
  label・default)は一切持たない

### 正準モデル

Rust の struct 一式を唯一の正とする(概形):

```rust
pub struct Layout {
    pub sections: Vec<Section>,
}
pub struct Section {
    pub title: String,
    pub description: Option<String>,
    pub children: Vec<Node>,
}
pub enum Node {
    Field { field: String },
    Section(Section),
}
```

- `layout.json` はこのモデルへの serde_json deserialize
- `layout.kdl` は `kdl` crate でパースした KDL ドキュメントから
  このモデルへ変換
- RPC 応答の `layout` フィールドはこのモデルの JSON 表現(= `layout.json`
  と同形)

## エラー処理(lenient)

layout の不備は**プラグインのロードを一切妨げない**。

| 状況 | 挙動 |
| --- | --- |
| layout ファイルが無い | `layout: null`。UI は現行どおり平坦フォーム |
| パース失敗(KDL/JSON とも) | 警告ログ + `layout: null` → 平坦フォーム |
| 未知のノード名・属性 | 警告ログを出してそのノード/属性を無視、残りは使う |
| 存在しない settings キーへの `field` 参照 | 警告ログを出してその参照を捨て、残りは描画 |
| `.kdl` と `.json` が両方存在 | 警告ログを出して `.kdl` を採用 |
| どのセクションにも載らなかったキー | 末尾の暗黙セクション(「その他」)に自動で入る。**書き忘れで項目が消えることはない** |
| 同じキーが複数回参照された | 警告ログを出して最初の出現だけ残す |

## 処理の流れ

```
plugin dir ──(registry: ロード時に読む)──> Layout(または None + warn)
                                              │
core: layout 純粋モジュール                     │
  - KDL/JSON → Layout 変換                     ▼
  - settings キー参照の解決・掃除      rpc: list 応答の各エントリに layout を同梱
  - 暗黙「その他」セクションの補完              │
                                              ▼
                              ui: PluginForm が layout 有→セクション描画
                                              layout 無→平坦描画
```

### core

- **純粋モジュール `layout`**(新設、`.claude/rules/` の純粋モジュール
  作法に従う): モデル定義、KDL/JSON からの変換、`&[SettingField]` に
  対する参照解決(不正参照の除去・重複除去・「その他」補完)。
  すべて値イン値アウト。警告は戻り値(`Vec<LayoutWarning>` 等)で返し、
  ログ出力は呼び出し側の命令的モジュールが行う。
- **registry**(命令的): プラグイン/ドライバのロード時に
  `layout.kdl` / `layout.json` を読み、layout モジュールに渡す。
  失敗は warn ログ + `None`。

### RPC

- `plugins/list` / `drivers/list` の各エントリ(フィールド定義
  `settings` を既に運んでいる応答)に任意フィールド `layout` を追加。
  無ければ `null`。`get-settings` は生の値マップそのものを返す形
  (キーがユーザー定義)なので、そこには足さない。
- 既存フィールドは不変。UI が古くても新しくても壊れない後方互換な追加。

### UI

- `PluginForm` は `layout` が来ていればセクション(見出し・説明・
  入れ子)付きで描画、`null` なら現行の平坦描画。フィールド単体の
  描画コンポーネント(SecretField 等)はそのまま再利用する。
- あわせて見た目の本丸であるフォームスタイルの刷新(余白・
  タイポグラフィ・セクションのカード化・secret / map の見せ方)を行う。

## テスト戦略

- **layout モジュール(純粋)**: KDL/JSON それぞれの正常系・パース失敗・
  未知ノード・不正参照・重複参照・「その他」補完を値イン値アウトの
  純粋テストで網羅。KDL と JSON が同じ入力内容で同じ `Layout` に
  なることの同値テストを置く。
- **rpc**: layout 付き/無しの get-settings 応答の pin テスト
  (生 JSON 形の固定)。
- **UI**: layout 有→セクション見出しが出る、無→平坦、不正 layout →
  平坦フォールバック、のコンポーネントテスト。
- **registry**: layout ファイルの読み込みと warn フォールバックの結線
  テスト(既存の manifest ロードテストの作法に合わせる)。

## スコープ外(将来の拡張余地)

- タブ、条件表示(`visible-when`)、カラム配置 — 木構造の上への
  ノード/属性追加として設計可能。必要とする実プラグインが出てから。
- edlr 側・ユーザー側での layout の差し替え(オーバーライド)機構。
- アプリ本体 Settings ページ(journal ディレクトリ)への適用 —
  こちらは manifest 駆動ではないため今回の対象外。

---
id: layout-kdl-field-description-pcl1
title: layout.kdl の field/description パースで警告なしに値が消える箇所が複数ある
summary: core/src/layout/kdl.rs の first_string_arg 系で field 引数無し・非文字列 description・2つ目の位置引数が LayoutWarning 無しに黙って消える / 未着手
status: open
labels: bug
created: 2026-08-01T06:05:27Z
updated: 2026-08-01T06:05:59Z
---

## どこで踏んだか

`core/src/layout/kdl.rs` の以下 3 箇所。いずれも `docs/superpowers/specs/2026-08-01-settings-layout-design.md` の
lenient 表(「不備は警告して読み飛ばす」)に反して、`LayoutWarning` を積まずに
値が消える。

1. **`field` に文字列引数が無い**(`convert_node` 47-50行目)

   ```rust
   "field" => {
       let key = first_string_arg(node)?;
       Some(Node::Field { field: key })
   }
   ```

   `first_string_arg` が `None` を返すと `?` でそのまま `None` を返して
   ノードごと消える。呼び出し元の `filter_map` はそれを「語彙外だった」と
   同じ扱いで握りつぶす。`field` は名前として存在するので `UnknownNode` にも
   分類されず、何の警告も残らない。再現: `field` のみ(文字列引数なし)を
   置いた `.kdl` を読ませると、ノードが警告なしで消える。

2. **`description` に非文字列を渡す**(`convert_section` 63-68行目)

   ```rust
   let description = node
       .entries()
       .iter()
       .find(|e| e.name().map(|n| n.value()) == Some("description"))
       .and_then(|e| e.value().as_string())
       .map(str::to_string);
   ```

   `description=123` のように非文字列を渡すと `as_string()` が `None` を返し、
   `description` は「未指定」と区別なく `None` 扱いになる。書いたはずの
   description が黙って消える。

3. **2つ目の位置引数が無視される**(`first_string_arg` 94-100行目)

   ```rust
   fn first_string_arg(node: &KdlNode) -> Option<String> {
       node.entries()
           .iter()
           .find(|e| e.name().is_none())
           .and_then(|e| e.value().as_string())
           .map(str::to_string)
   }
   ```

   `.find` で最初の無名引数だけを見るため、`section "接続" "余分な引数"` の
   ように2つ目以降の位置引数を書いても、警告なしにそのまま無視される。
   タイポで引数を並べて書いてしまっても気付けない。

## なぜ困るか

`from_kdl_str` の doc コメント(1-5行目)は「文法としては正しいが語彙に無い
ノード・引数は `LayoutWarning` に拾って読み飛ばす」と約束しており、スペックの
lenient 表も同様に「警告して読み飛ばす」を前提にしている。上記 3 箇所は
その約束を破って**無警告で**値を消す。プラグイン作者が layout.kdl を書き
間違えても runner の warn ログに何も出ないため、「なぜこの field/section が
表示されない・description が出ない」に気付く手段が無い。

## 直し方の案

- `convert_node` の `"field"` 分岐: `first_string_arg` が `None` を返したら
  `LayoutWarning::UnknownNode("field (no argument)".into())` 相当の警告を積んで
  から `None` を返す(専用の `FieldWithoutKey` variant を足すのも手)。
- `convert_section` の description 抽出: entry は見つかったが `as_string()` が
  `None` の場合に `LayoutWarning::UnknownNode("description (not a string)".into())`
  のような警告を積む。
- `first_string_arg` を使う箇所(`section`/`field` 双方)で、無名引数が2つ以上
  ある場合に警告を積む。`first_string_arg` 自体を「全無名引数を数える」形に
  変えて呼び出し側で判定するか、専用ヘルパーを足す。

いずれも `LayoutWarning` に新しい variant を足す(または既存の `UnknownNode`
を転用する)形で対応できる。3箇所とも `convert_node`/`convert_section` は
`warnings: &mut Vec<LayoutWarning>` を既に受け取っているので配線コスト自体は
小さい。

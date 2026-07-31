---
id: rpc-render-rs-doc-2-edlr-rs-docs-only-pagd
title: rpc/render.rs の doc コメント誤付着2箇所と edlr.rs の旧パス参照(docs-only 修正)
summary: render.rs で bus/schedules の doc が隣の関数に付着(移動前からの既存問題)、bin/edlr.rs:249 が旧 server::bus_result_json を参照。コメントのみの follow-up commit で直す / 未着手
status: closed
labels: docs
created: 2026-07-30T14:19:10Z
updated: 2026-07-31T06:33:15Z
---

## どこで踏んだか

core リファクタ Phase 2 の最終レビューで発見。いずれも挙動に影響しない
doc コメントの問題で、move-only 規律のため Phase 2 では意図的に温存した。

1. `core/src/rpc/render.rs:14-25` — `bus_result_json` の doc ブロックが
   `dashboard_result_json` に付着し、`bus_result_json` が無ドキュメントに
2. `core/src/rpc/render.rs:64-78` — `schedules_result_json` の doc が
   `dropped_result_json` に融合し、`schedules_result_json` が無ドキュメントに
   (どちらも移動前の server.rs 時点から byte-identical に存在した既存問題)
3. `core/src/bin/edlr.rs:249` — コメントが「`server::bus_result_json` の
   ドキュメント参照」と旧所在を指す。現在は `rpc::render::bus_result_json`

## なぜ困るか

次にこのコードを読む人が誤った関数のドキュメントを信じる/存在しない
パスを探すことになる。

## 直し方

コメントのみの follow-up commit を1つ(挙動リスクゼロ)。doc ブロックを
正しい関数の直上に移し、edlr.rs の参照パスを `rpc::render::` に更新する。

## 解決(2026-07-31)

コミット 6e437bb で3箇所とも修正(doc ブロックの付着位置修正 + edlr.rs の参照パス更新、記述内容は保全)。

---
id: wasm32-wasip2-5guz
title: テスト実行に wasm32-wasip2 ターゲットが必要なことが未文書で、未導入時のエラーも分かりにくい
summary: core の統合テストが hello-logger を wasm32-wasip2 でビルドするが、ターゲット未導入だと E0463 で落ちる。セットアップ手順の文書化 or fixture 側での事前チェックが要る / 未着手
status: open
labels: dx, docs
created: 2026-08-01T07:39:18Z
updated: 2026-08-01T07:39:45Z
---

## どこで踏んだか

macOS のクリーンな環境で `cargo test --workspace` を実行したところ、
`core/tests/daemon_signal_shutdown_integration.rs` の
`sigterm_to_daemon_stops_running_sidecars_including_grandchildren` が失敗した。

原因は `core/tests/support/mod.rs` の `valid_plugin_wasm()` が
`examples/plugins/hello-logger` を `--target wasm32-wasip2` でビルドするが、
ツールチェーンにターゲットが入っていなかったこと。エラーは fixture ビルドの
出力に埋もれた `error[E0463]: can't find crate`(itoa / zmij のコンパイル失敗)で、
「`rustup target add wasm32-wasip2` が必要」とは読み取れない。

再現手順:

1. `rustup target remove wasm32-wasip2`(または未導入の環境を用意)
2. `cargo test --workspace`
3. `hello-logger fixture build failed` で panic(support/mod.rs:200)

## なぜ困るか

- セットアップ要件がどこにも書かれていない(docs/plugins.md はプラグイン側の
  ターゲット説明のみで、開発環境の前提としては書かれていない)
- E0463 から rustup target の欠如に辿り着くまでに調査が要る。新しい環境を
  作るたびに同じ調査をやり直すことになる

## 直し方の案

1. `valid_plugin_wasm()` の冒頭で `rustup target list --installed` を確認し、
   無ければ「`rustup target add wasm32-wasip2` を実行せよ」と panic メッセージで
   案内する(fixture を使う全テストが恩恵を受ける)
2. CLAUDE.md か README に開発環境セットアップとして
   `rustup target add wasm32-wasip2` を明記する

1 と 2 は排他ではないので両方やってもよい。

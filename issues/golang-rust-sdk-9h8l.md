---
id: golang-rust-sdk-9h8l
title: golangやRust向けのsdkを作って更新を簡単にできるようにする
summary: sdk/rust(crate edlr-plugin-sdk)・sdk/go(module .../sdk/go)を新設し実装完了。既存 examples 全 9 個(Rust 7 / Go 2)を移行済み
status: open
labels: 
created: 2026-07-29T05:46:52Z
updated: 2026-08-05T04:10:37Z
---

現状の実装だとABIに仕様変更が入るたびにSDK部分のwitを各リポジトリに `cp` で配る必要があり煩雑。golangやRust用のSDKを作ってそれに依存する形にして更新作業を楽にしたい

## 実装

設計: `docs/superpowers/specs/2026-08-05-guest-sdk-design.md`。使い方は
`docs/sdk.md`。

- **配布**: このリポジトリ直参照(crates.io / 独立リポジトリでの公開はしない)。
  - Rust: Cargo git 依存 + tag 指定(`edlr-plugin-sdk = { git = "https://github.com/himanoa/edlr", tag = "sdk/v0.5.0" }`)
  - Go: `github.com/himanoa/edlr/sdk/go` のサブディレクトリモジュール
    (`go get github.com/himanoa/edlr/sdk/go@sdk/go/v0.5.0`)
- **構成**: `sdk/rust/src/{lib.rs,http.rs}`(crate-type = rlib のみ、
  `wit_bindgen::generate!` を内包)、`sdk/go/{wit,gen,edlrplugin}`
  (`wit-bindgen-go` 生成物と WIT 本体をコミット。TinyGo のコンポーネント化が
  ディスク上の WIT ファイルを要求するため `sdk/go/wit/` に `core/wit` の
  コピーを同梱し、`core/tests` の同期テストで一致を機械的に検証)
- **公開面**: Rust は `Plugin` trait(全メソッドにデフォルト実装)+
  `register!` マクロ、Go は `Hooks` 構造体 + `Register` 関数。ABI に export
  が増えても SDK 側にデフォルト実装/no-op を足せば既存プラグインはソース
  互換のまま再ビルドだけで追従できる
- **バージョニング**: SDK バージョン = WIT バージョン。tag は `sdk/v<wit>` /
  `sdk/go/v<wit>`
- **examples 移行**: Rust 7 個(hello-logger / http-caller / busy-loop /
  init-trap / memory-hog / state-reader / tutorial-jump-log-rs)、Go 2 個
  (inara-uploader / tutorial-jump-log-go)を全て SDK 経由へ書き換え。既存の
  統合テストがそのまま SDK 経由のロード・init・イベント配送・submit を
  カバーする

sdk-send-async-response-await-lvn3(await ヘルパー)もあわせて実装し close
済み。

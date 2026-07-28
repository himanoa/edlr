---
id: ci-go-tinygo-9jmo
title: CIが存在せずGo/TinyGoツールチェーンも未固定
status: open
labels: ci, infra
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T15:09:15Z
---

## 問題

リポジトリに `.github/` ディレクトリが無く、`cargo test`(Rust ワークスペース)、`pnpm test`(フロントエンド)、`go test ./...` / `tinygo test -target=wasip1`(Go プラグイン)のいずれも自動実行されない。`examples/plugins/inara-uploader/README.md:248` でもギャップ #6 として「Go を一級市民にするならツールチェーンの固定と CI が要る」と明記されている。

## 対応案

- Rust / frontend / Go plugin のワークフローマトリクスを追加する
- TinyGo / `wit-bindgen-go` / `wasm-tools` のバージョンを散文でなく設定として固定する

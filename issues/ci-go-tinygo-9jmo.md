---
id: ci-go-tinygo-9jmo
title: CIが存在せずGo/TinyGoツールチェーンも未固定
status: closed
labels: ci, infra
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T17:06:44Z
---

## 問題

リポジトリに `.github/` ディレクトリが無く、`cargo test`(Rust ワークスペース)、`pnpm test`(フロントエンド)、`go test ./...` / `tinygo test -target=wasip1`(Go プラグイン)のいずれも自動実行されない。`examples/plugins/inara-uploader/README.md:248` でもギャップ #6 として「Go を一級市民にするならツールチェーンの固定と CI が要る」と明記されている。

## 対応案

- Rust / frontend / Go plugin のワークフローマトリクスを追加する
- TinyGo / `wit-bindgen-go` / `wasm-tools` のバージョンを散文でなく設定として固定する

## 対応しない(2026-07-29)

作者本人しか使わないため、リモートでの自動検証に見合う価値が無いという判断で
クローズする。ローカルで `cargo test --workspace` / `pnpm test` を回す運用を続ける。

代わりに、繰り返し手作業になっていたサンプルのビルドと配置を
`scripts/install-examples.sh` にまとめた(ツールチェーンの導線としては、
このスクリプトが `cargo` / `tinygo` の不在を検出してインストール先の URL を
出す)。CI が必要になったら再オープンすること。

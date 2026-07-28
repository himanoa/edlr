---
id: readme-plugin-abi-0-3-0-0-4-0-leb8
title: READMEのplugin ABIバージョン表記が0.3.0のまま(実体は0.4.0)
status: closed
labels: docs
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T15:44:49Z
---

## 問題

`README.md:99` の Status セクションは WIT パッケージを「currently `@0.3.0`」と記載しているが、スケジューラ対応(`on-schedule` / `on-stop` 追加)後の `core/wit/plugin.wit:1` は `edlr:plugin@0.4.0` を宣言している。Features リストにもスケジューリングの記載が無い。README を見てプラグインを作ると、ロードに失敗する world をターゲットしてしまう。

## 対応案

- ドキュメントを同期する
- 可能なら WIT ファイルからバージョンを grep して README と突き合わせるテスト/CI チェックを追加する

関連: [[ci-go-tinygo-9jmo]]

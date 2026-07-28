---
id: spec-md-sqhq
title: spec.mdの未決定事項セクションが実装済み内容のまま陳腐化
status: open
labels: docs
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T15:09:15Z
---

## 問題

`spec.md:55` の「未決定事項(今後の設計課題)」は、ドライバ capability モデル、マニフェストスキーマ、channel ドライバのセマンティクス、WebSocket プロトコルの4つを未決定として列挙しているが、全て実装・文書化済み(`docs/capabilities.md`、`docs/plugins.md`、`docs/drivers.md`、`core/src/server.rs`)。`README.md:83` が新規参加者を spec.md に誘導しているため、能動的にミスリードしている。

## 対応案

当該セクションを「解決済み + ドキュメントへのリンク」に書き換え、本当に未決の項目だけ残す。

関連: [[readme-plugin-abi-0-3-0-0-4-0-leb8]]

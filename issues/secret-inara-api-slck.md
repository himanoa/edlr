---
id: secret-inara-api-slck
title: secret型設定が無くINARA APIキーが平文で保存・送信される
status: open
labels: security, plugin, settings
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T15:09:15Z
---

## 問題

API キーは `type = "string"` で宣言するしかなく、`<settings-dir>/inara-uploader.json` に平文で保存され、Plugins ページでは素のテキスト入力で表示され、`plugins/list` / `plugins/get-settings` のレスポンスにもそのまま返る。`examples/plugins/inara-uploader/README.md:177` で議論されている通り、`driver-fs` による回避は平文の置き場所を変えるだけでフォルダ全体のアクセス許可まで要求するので、むしろ悪い。

## 対応案

`SettingField` に secret 種別を追加する:

- UI ではマスク表示
- RPC では write-only(読み出しレスポンスから除外)
- ログから redact

変更対象: `core/src/plugin/manifest.rs`、`settings.rs`、`server.rs`、`PluginForm.tsx`

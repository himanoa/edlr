---
id: http-1-disabled-a3xp
title: HTTP遅延1回でプラグインが恒久Disabledになりログも不透明
status: closed
labels: plugin, reliability
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T16:38:36Z
---

## 問題

`HTTP_TIMEOUT` はハードコードの 1.5 秒で、2 秒の `CALL_DEADLINE` を厳密に下回る必要がある(`core/src/plugin/host.rs:91`、`host.rs:859`)。プラグインの JSON マーシャリング + INARA リクエスト1回が予算を超えると、`disable_and_break!`(`core/src/plugin/runner.rs:573`)がプラグインを恒久的に `Disabled` にし、ログは「on-event call failed」だけ。一時的なネットワーク停滞と本物の wasm トラップが区別できず、デーモン再起動なしには回復できない。

## 対応案

- deadline-exceeded とトラップを区別し、前者はリトライ/バックオフまたはストライクカウントにする
- Disabled の理由を UI に表示する

関連(部分的に重複): [[issue-sizx]](非同期実行エンジン)、[[http-driver-9znv]](http-driver 非同期化)

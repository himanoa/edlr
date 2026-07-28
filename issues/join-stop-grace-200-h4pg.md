---
id: join-stop-grace-200-h4pg
title: デーモン終了処理が直列joinでSTOP_GRACEが200秒必要になっている
status: closed
labels: shutdown, performance
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T16:17:45Z
---

## 問題

`ui/src-tauri/src/daemon.rs:52` の `STOP_GRACE` は 200 秒。コンパイル時アサーションが `SIDECAR_SHUTDOWN_GRACE × 20 + DRIVER_CALL_DEADLINE + PLUGIN_ON_STOP_GRACE × 20` を全て直列で見積もる必要があるため。`Registry::shutdown_plugins`(`core/src/plugin/registry.rs:435`)が `JoinHandle` を1つずつ順番に poll しているのが原因で、最悪ケースではデスクトップアプリが終了時に数分間ハングして見えてから SIGKILL される。

## 対応案

- 全プラグインへ先に `Stop` を送ってから、共有デッドライン1つで一括 join する
- sidecar の `finish_stop` も同様に並行化する

最悪ケースが N × grace から ~1 × grace に縮み、`STOP_GRACE` を一桁下げられる。

関連: [[on-stop-graceful-shutdown-flush-e646]]

---
id: on-stop-graceful-shutdown-flush-e646
title: on-stopがワークキュー後方に並びgraceful shutdownのflushがスキップされる
status: open
labels: plugin, reliability
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T15:09:14Z
---

## 問題

`PluginWork::Stop` はイベント/バス配送と同じ bounded 64 スロットのワークチャネルに `try_send` される(`config/src/lib.rs:76`、`core/src/plugin/registry.rs:444`)。プラグインスレッドは先行する全ワークを消化してからでないと `call_on_stop` に到達しない(最悪 63 件 × `CALL_DEADLINE` 2 秒 ≒ 126 秒)。一方 `shutdown_plugins` の待機は 5 秒だけなので、キューが詰まっていると on-stop の flush は事実上スキップされる。

`docs/plugins.md:101` と `examples/plugins/inara-uploader/README.md:167` でも残存制限として明記されている。

## 対応案

`Stop` をキュー経由でなくアウトオブバンドで伝える:

- ランナーループが毎周期チェックする `AtomicBool` の stop フラグ、または
- `work_rx` より優先して select する専用チャネル

これにより `Stop` が保留ワークを追い越し、on-stop grace の意味が回復する。

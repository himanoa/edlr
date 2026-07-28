---
id: journal-vs93
title: ワークキュー溢れでjournalイベントが黙って恒久喪失する
status: closed
labels: plugin, reliability, data-loss
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T16:28:31Z
---

## 問題

`PLUGIN_WORK_QUEUE_CAPACITY` は 64(`core/src/plugin/runner.rs:83`)で、満杯時は warn を出してイベントをドロップする(`runner.rs:717`、`runner.rs:841`)。journal の読み取り位置は配送成否と独立に進むため、この経路で失われたイベントは再起動時の replay でも二度と配送されない(`examples/plugins/inara-uploader/README.md:213` に文書化済みで、replay 中は構造的に発生し得る)。

## 対応案(いずれか、または組み合わせ)

- プラグインごとの acknowledged read position を導入する
- replay 時は bounded blocking / バックプレッシャにする
- 最低限、「dropped N events」カウンタを `plugins/list` に出して損失を可視化する

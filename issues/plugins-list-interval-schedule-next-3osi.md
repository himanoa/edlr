---
id: plugins-list-interval-schedule-next-3osi
title: plugins/listがinterval scheduleのnext発火時刻を捏造して返す
status: closed
labels: scheduler, rpc
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T16:03:59Z
---

## 問題

`build_schedule_infos`(`core/src/plugin/registry.rs:692`)は RPC のたびに `Local::now()` から新しい `ScheduleState` を組み立てるため、interval スケジュールの `next` は常に「now + interval」になり、プラグインスレッドの実際の発火時点と無関係。UI のカウントダウンが意味を持たない(cron は壁時計絶対なので問題なし)。`registry.rs:125-134` で近似であることは明記済み。

## 対応案

ランナーループが更新する `Arc<Mutex<..>>` またはアトミックなタイムスタンプで、スレッドの実際の next-fire を公開し、`plugins/list` はそれを返す。

関連: [[schedule-ui-hh-mm-hdhg]]

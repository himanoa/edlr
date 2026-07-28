---
id: schedule-ui-hh-mm-hdhg
title: Schedule UIがHH:MMのみ表示で日付なし・再描画もされない
status: closed
labels: frontend, scheduler
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T15:49:18Z
---

## 問題

`ui/frontend/src/components/ScheduleSection.tsx:8` の `formatNext` は `next` を素の `HH:MM` で描画する。`cron = "0 9 * * *"` が明日発火するのか5分後なのか区別できない。また値はレンダー時のスナップショットでティックが無いため、即座に陳腐化する。

## 対応案

- 相対表記(「in 42s」)にする、または `next` が今日でない場合に日付を含める
- interval で再レンダーして表示を追従させる

関連: [[plugins-list-interval-schedule-next-3osi]], [[schedulesection-xv4e]]

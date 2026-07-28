---
id: schedulesection-xv4e
title: ScheduleSectionだけテストファイルが無い
status: closed
labels: frontend, test
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T15:49:18Z
---

## 問題

`ui/frontend/src/components/` の兄弟コンポーネント(`BusSection`、`CapabilitySection`、`DashboardSection`、`FilesystemSection`、`PluginForm`、`SidecarSection`、`WidgetFrame`)は全て `.test.tsx` を併設しているが、最新の `ScheduleSection.tsx` だけ無い。間接カバレッジは `pages/Plugins.test.tsx` 経由のみ。

## 対応案

小さな vitest ファイルを追加してコンベンションを回復する:

- 空リスト時の null レンダー
- spec / next のフォーマット
- パース不能な `next` のフォールバック

関連: [[schedule-ui-hh-mm-hdhg]]

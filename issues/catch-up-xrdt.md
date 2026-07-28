---
id: catch-up-xrdt
title: デーモン停止中に過ぎたスケジュールのcatch-up実行が無い
status: closed
labels: scheduler, enhancement
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T17:02:09Z
---

## 問題

スケジュール状態はプラグイン起動時に毎回新規構築され永続化されない(設計上「打ち漏らし(デーモン停止中に過ぎた定刻)の追い掛け実行」はスコープ外、`docs/plugins.md:99`)。`cron = "0 9 * * *"` の日次レポートは、09:00 にデーモンが動いていなかった日は単にスキップされ、ログにも UI にも痕跡が残らない。flush 系スケジュールには問題ないが、レポート系には不適切。

## 対応案

- 最終発火タイムスタンプの永続化
- マニフェストにオプトインの `catch-up = true` フラグを追加

関連: [[interval-schedule-ntp-9xko]]

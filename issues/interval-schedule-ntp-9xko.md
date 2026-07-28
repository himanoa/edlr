---
id: interval-schedule-ntp-9xko
title: interval scheduleが壁時計基準でNTP/サスペンドに脆弱
status: open
labels: scheduler
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T15:09:15Z
---

## 問題

`core/src/plugin/schedule.rs:6-30` は設計(`docs/superpowers/specs/2026-07-28-plugin-scheduler-design.md` の「実装時の変更」)から意図的に逸脱し、interval も cron も `chrono::Local` で追跡している。時計が後方にステップすると次回発火がステップ量ぶん遅延し、前方ステップでは発火が早まって合体する。ラップトップのレジューム後、`interval-seconds = 60` の flush が任意に遅れ得る。

## 対応案

interval エントリは `Instant` で追跡し、cron は壁時計のまま維持する(`next` フィールド用の変換だけ行う)。観測可能な出力を変えずに元設計へ戻せる。

関連: [[catch-up-xrdt]]

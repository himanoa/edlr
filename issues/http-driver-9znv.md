---
id: http-driver-9znv
title: http-driverを非同期化する
status: closed
labels: done
created: 2026-07-28T14:27:36Z
updated: 2026-08-05T06:15:02Z
---

deps: issue-sizx



## 実装完了(2026-08-05)

2 段階で実装済み:

1. **内部 async 化**(async-migration Step 2a): `drivers/http` を async
   `reqwest::Client` + `Handle` 化(コミット 35e743f)、`send_async`
   (リクエスト単位タイムアウト上書き)追加、同期 `send` は block_on ラッパ
2. **ドライバ側 submit/complete**(WIT 0.6.0、本コミット):
   - `world driver` に `on-job-complete` export を追加(ABI 破壊 → 全ゲスト
     再生成。設計 spec: docs/superpowers/specs/2026-08-05-driver-submit-design.md)
   - `DriverCtx::submit_send` を stub から実装へ(プラグイン側と同一
     セマンティクス。`PluginJobs` / `job_result_json` / `submit_timeout` 共用)
   - 作業キューはプラグインの自作キューをジェネリック化して共有
     (`WorkQueue<T>` + admit 注入)。`DriverWork::{Message, JobComplete,
     Disconnected}`
   - `Bus::register_driver` を `MessageSink` trait 受け取りに変更し、
     キュー直結の sink で「publish 満杯 = queue-full を返す(捨てない)」
     契約を保存。ホスト発(sidecar-ready)は満杯でも受け入れ。sink の
     `Drop` が `Disconnected` センチネルを push し、従来のチャネル切断
     終了と同じ挙動を保存

docs: plugins.md のバージョン表、drivers.md「非同期 HTTP」節、README、
sdk 0.6.0(tag sdk/v0.6.0 / sdk/go/v0.6.0)。
テスト: admit 境界・sink の queue-full / Disconnected・submit の同期拒否 /
transport 失敗の非同期到着。workspace 全テスト + clippy 全パス。

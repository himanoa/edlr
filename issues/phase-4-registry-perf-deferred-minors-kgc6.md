---
id: phase-4-registry-perf-deferred-minors-kgc6
title: Phase 4 registry サービス化の後追い残件(perf・構造の deferred minors)
summary: registry 解体(Phase 4)の最終レビューで挙動不変優先のため見送った残件6点 — as_settings_manifest の全 clone、list() のサービス迂回、entry trait 3本、to_driver_error の unreachable、disabled 経路のロック内 read、driver capabilities 読み口の非対称 / 未着手
status: open
labels: refactor
created: 2026-07-30T18:50:00Z
updated: 2026-07-30T18:50:42Z
---

## どこで踏んだか

core リファクタ Phase 4(registry 解体、plan: `docs/superpowers/plans/2026-07-31-core-refactor-phase4.md`)の
タスク別・最終レビューで見つかったが、挙動凍結を優先して意図的に見送った残件。
いずれも挙動には影響しない(レビューで裁定済み)。

1. **`RegistrySubject::as_settings_manifest` が plugin 側で `Manifest` 全 clone**
   (`registry/subject.rs` — identity clone)。fs/sidecar/settings/grants の RPC 頻度
   経路で毎回 deep clone。`Cow<'_, Manifest>` 化 or 明示コメントつき容認を検討。
2. **facade の `list()` がサービスを迂回して store を直叩き**
   (`registry/plugin.rs:449-450`、`registry/driver.rs:236-237`)。そのため両 facade に
   冗長な `settings_store`/`grants_store` フィールドが残る。`SettingsService::effective` /
   `GrantService` 経由に寄せてフィールドを落とす(Phase 5/6)。
3. **entry trait 3本(`FilesystemEntry`/`SidecarEntry`/`SettingsEntry`)が `manifest()` を再宣言**。
   4本目が要る事態になったら base `RegistryEntry` + 拡張 trait に集約する。
4. **`to_driver_error` の `unreachable!`**(`registry/driver.rs:99-109`)— 共有サービスが
   新しい `RegistryError` variant を返すよう変わると潜在 panic。不変条件はコメント化済み。
   コンパイル時に強制するにはエラー enum 分割が要る(Phase 6 候補)。
5. **disabled 経路でロック保持中の disk read 2回**(`start_or_restart_sidecar` — grant/config
   read が Disabled 判定より先)。出力不変だがエラー経路のロック保持が伸びた。
   ホットになるなら early return を戻す。
6. **driver 側に capabilities の読み口がない非対称**(plugin は `Registry::capabilities`、
   driver は `list()` の `DriverInfo` 経由のみ)— 既存の非対称。Phase 6 で文書化 or 統一。

あわせて test 側: `InMemoryGrantStorage`(sidecar.rs test_support)は stale/fingerprint
セマンティクス未実装。stale を試すテストを書く時に拡張する。

## なぜ困るか

放置すると「なぜこうなっているか」が失われ、Phase 5/6 の実装者が再調査するか、
知らずに壊す(特に 4 は panic、2 は挙動変更の入り口になりやすい)。

## 直し方

Phase 5(runner/host)・Phase 6(旧パス削除・仕上げ)の計画に取り込む。
2 と 6 は Phase 6 の facade 仕上げ、1・3・5 は触るついでに、4 はエラー enum 整理と同時に。

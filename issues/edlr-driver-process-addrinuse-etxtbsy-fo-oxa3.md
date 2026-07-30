---
id: edlr-driver-process-addrinuse-etxtbsy-fo-oxa3
title: edlr-driver-process のテストが AddrInUse / ETXTBSY で稀に落ちる(ポート・fork 競合)
summary: cargo test --workspace 実行時に respawn_notifies_ready_again 等が AddrInUse で稀に失敗(単体実行では通る)。固定ポートの衝突が原因とみられる。devserver の ETXTBSY(issue aenf)と同系統の flaky / 未着手
status: open
labels: flaky-test
created: 2026-07-30T15:10:34Z
updated: 2026-07-30T20:41:18Z
---

## どこで踏んだか

core リファクタリング(Phase 1〜3)の各セッションで `cargo test --workspace` を
繰り返し実行した際、`edlr-driver-process` のテスト
(`tests::respawn_notifies_ready_again` など)が
`Address already in use`(AddrInUse)で稀に落ちた。単体実行・全体再実行では
毎回通る。少なくとも3回別々のセッションで観測。

## なぜ困るか

full workspace テストをゲートにしている作業(リファクタのコミット単位検証、
CI 化するなら CI)で偽陽性の失敗が出て、毎回「本物の失敗か flaky か」の
切り分けに再実行が必要になる。

## 原因の見立て

テストが固定ポート(または直前に解放されたポート)で listen するプロセスを
spawn しており、並列実行中の他テストとポールが衝突する。devserver の
ETXTBSY(`cargo-test-workspace-devserver-etxtbsy-aenf`)と同じく
「並列 cargo test + プロセス spawn」起因。

## 直し方の案

- ポートを OS 任せ(bind 0)にして実ポートを読み取る形へテストを直す
- あるいは該当テストに `#[serial]` 相当の直列化(既存依存だけで可能な範囲で)

## 追加観測(2026-07-31, Phase 5 セッション)

- `respawn_notifies_ready_again` が full workspace 実行で1回 transient fail(単体再実行でパス)— 既知パターンの再発
- 新顔: core 統合テスト `sigterm_to_daemon_stops_running_driver_sidecars` も並列 full workspace 実行時に1回だけ transient fail(単体・再実行では安定パス)。driver sidecar の spawn を伴うテストなので同系統(並列 cargo test + プロセス spawn)とみられる。対処するときはこのテストも同じ直し方(bind 0 / 直列化)の対象に含めること

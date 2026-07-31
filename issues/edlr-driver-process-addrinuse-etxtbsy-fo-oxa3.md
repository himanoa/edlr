---
id: edlr-driver-process-addrinuse-etxtbsy-fo-oxa3
title: edlr-driver-process のテストが AddrInUse / ETXTBSY で稀に落ちる(ポート・fork 競合)
summary: cargo test --workspace 実行時に respawn_notifies_ready_again 等が AddrInUse で稀に失敗(単体実行では通る)。固定ポートの衝突が原因とみられる。devserver の ETXTBSY(issue aenf)と同系統の flaky / free_port 廃止と ephemeral range 外ポートへの移動で解消(6d4c6e2, 722a70c)
status: closed
labels: flaky-test
created: 2026-07-30T15:10:34Z
updated: 2026-07-31T12:30:46Z
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

## 対応(2026-07-31)

原因は「bind(0) → drop → 後で再 bind」(free_port)の競合窓と、
ephemeral port range(32768-60999)内の固定ポートが並列テストの bind(0) /
outgoing source port に取られること。commit 6d4c6e2 / 722a70c で:

- drivers/process: free_port() を廃止。respawn テストは bind(0) の listener を
  保持したままそのポートを使い、後から listen する ready テストは range 外の
  固定ポート 28621/28622 に変更
- daemon_signal_shutdown / daemon_config_journal 統合テスト: listen アドレスを
  585xx → 285xx(range 外)へ移動(sigterm_to_daemon_stops_running_driver_sidecars 含む)
- ui devserver: 負ケースの固定ポート 59993 → 29993(range 外)

full workspace テスト3回連続 green を確認。

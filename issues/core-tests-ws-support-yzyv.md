---
id: core-tests-ws-support-yzyv
title: core/tests/ の WS ハーネス重複を support/ へ集約する(リファクタ完了後)
summary: rpc_pin_integration.rs が ws_rpc_integration.rs の WS ハーネス約150行(setup/connect/recv_json/send_rpc と fixture ビルダ)をコピーしている。テスト凍結解除後に support/ へ集約する / 未着手
status: open
labels: test, refactor
created: 2026-07-30T14:19:10Z
updated: 2026-07-30T14:19:49Z
---

## どこで踏んだか

core リファクタ Phase 2 Task 1 で pin テスト
(`core/tests/rpc_pin_integration.rs`)を追加した際、テスト凍結原則により
既存の `ws_rpc_integration.rs` や `core/tests/support/mod.rs` に手を入れ
られなかったため、WS ハーネス(`setup`/`connect`/`recv_json`/`send_rpc`/
`recv_hello`)と `http_caller_wasm` 系 fixture ビルダ約150行をファイル内に
コピーした。

## なぜ困るか

ハーネスの修正(タイムアウト調整・接続方法の変更など)が2箇所への
二重適用になり、片方だけ直すとサイレントに食い違う。

## 直し方

core リファクタリング(全 Phase)完了後、テスト凍結が解けたタイミングで
共通部分を `core/tests/support/` へ移して両ファイルから使う。
`ws_rpc_integration.rs` 側の変更は import 追従のみになるようにする。

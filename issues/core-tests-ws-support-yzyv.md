---
id: core-tests-ws-support-yzyv
title: core/tests/ の WS ハーネス重複を support/ へ集約する(リファクタ完了後)
summary: rpc_pin_integration.rs が ws_rpc_integration.rs の WS ハーネス約150行(setup/connect/recv_json/send_rpc と fixture ビルダ)をコピーしている。テスト凍結解除後に support/ へ集約する / 未着手
status: closed
labels: test, refactor
created: 2026-07-30T14:19:10Z
updated: 2026-07-31T06:33:15Z
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

## 解決(2026-07-31)

コミット 2e6ceb4 で、両ファイルで本体が一致するハーネス(Ws/connect/recv_json/recv_hello/send_rpc と
http_caller fixture 3関数)を tests/support/mod.rs へ集約。`setup` はシグネチャ差
(pin 側は Option<DriverRegistry> を取る)のため意図的に統一せず両側に残した。

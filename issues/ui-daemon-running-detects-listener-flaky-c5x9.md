---
id: ui-daemon-running-detects-listener-flaky-c5x9
title: ui の daemon_running_detects_listener が並列負荷で flaky
summary: ui/src-tauri の daemon_running_detects_listener が cargo test --workspace の並列負荷で稀に落ちる(単独では安定 pass)/ 未着手
status: closed
labels: flaky-test
created: 2026-08-05T06:11:52Z
updated: 2026-08-05T07:09:57Z
---



## どこで踏んだか

http-driver-9znv(ドライバ側 submit)の最終ゲート `cargo test --workspace`
で `ui/src-tauri` の `daemon::tests::daemon_running_detects_listener` が
1 回失敗した。単独再実行(`cargo test -p edlr-ui`)では安定して pass する。
ポート/リスナー系のテストなので、並列実行時のポート競合か接続タイムアウトの
短さが原因とみられる(core 側 issue oxa3 /
daemon-signal-shutdown-integration-works-kay8 と同族)。

## なぜ困るか

フルゲートの grep 条件を満たせず、無関係の変更で再実行判断が要る。

## 直し方の案

- core の同族修正と同じく、固定 sleep/即時判定を条件ポーリングに寄せる
- ポートを ephemeral range 外の一意な固定値にする(oxa3 の教訓)

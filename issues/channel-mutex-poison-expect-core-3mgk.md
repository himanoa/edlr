---
id: channel-mutex-poison-expect-core-3mgk
title: channelドライバのmutex poison処理がexpectでcoreと非一貫
status: closed
labels: hardening, driver
created: 2026-07-28T15:07:27Z
updated: 2026-07-28T15:46:46Z
---

## 問題

`drivers/channel/src/lib.rs` の 8 箇所(104, 118, 140, 170, 191, 287, 294, 304 行)が `.lock().expect("bus state poisoned")` を使っている。一方 `core/src/plugin/registry.rs:439` 周辺は一貫して `.lock().unwrap_or_else(|poisoned| poisoned.into_inner())` で回復する。バスロック下でどこか1箇所 panic すると、以後の全バス操作がデーモンスレッドを abort させる連鎖になり、縮退動作にならない。

## 対応案

`drivers/channel` を core と同じ回復パターン(`into_inner()`)に揃える。小さく機械的なハードニング。

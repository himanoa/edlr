---
id: daemon-signal-shutdown-integration-works-kay8
title: daemon_signal_shutdown_integration が --workspace 並列実行で flaky (sigterm_to_daemon_stops_running_driver_sidecars)
summary: core/tests/daemon_signal_shutdown_integration.rs の sigterm テストが cargo test --workspace 下で時々落ちる / 未着手
status: open
labels: flaky-test
created: 2026-08-05T04:02:55Z
updated: 2026-08-05T04:03:23Z
---

## どこで踏んだか

Guest SDK Task 7(Go ゲスト 2 個の SDK 移行、`examples/plugins/inara-uploader` /
`examples/plugins/tutorial-jump-log-go`)の Step 5 フルゲートで
`cargo test --workspace` を実行したところ、以下が毎回ではないが再現する:

```
cd /mnt/game/caches/src/github.com/himanoa/edlr
cargo test --workspace
```

```
running 2 tests
test sigterm_to_daemon_stops_running_driver_sidecars ... FAILED
test sigterm_to_daemon_stops_running_sidecars_including_grandchildren ... ok

---- sigterm_to_daemon_stops_running_driver_sidecars stdout ----

thread 'sigterm_to_daemon_stops_running_driver_sidecars' panicked at core/tests/daemon_signal_shutdown_integration.rs:471:5:
grandchild <pid> survived daemon SIGTERM; driver sidecars were orphaned
```

同じテストを単独実行(`cargo test --workspace --test daemon_signal_shutdown_integration -p edlr-core -- --test-threads=1`)
すると安定して pass する。`--workspace` の並列実行でワークステーション負荷が
上がったときだけ、SIGTERM 後に子プロセスの終了確認が期限内に間に合わず誤検知
している可能性が高い(タイミング依存のアサーション)。

今回の Task 7 は Go の SDK 移行のみで core/ の Rust コードは一切変更していない
ため、この flaky は今回の変更が原因ではない。フルゲートの grep 条件
(`FAILED|[1-9][0-9]* failed` の出力が空であること)を毎回満たせず、CI やレビュー
のたびに再実行判断が要るのが困る。

## なぜ困るか

- フルゲートを要求する他タスクのたびに「本当に壊れているのか flaky か」を
  都度切り分ける必要があり、無駄なコストになる。
- CI で並列負荷が高いときに同様に誤検知し、無関係な PR がブロックされうる。

## 原因の当たり

`core/tests/daemon_signal_shutdown_integration.rs` 内の SIGTERM 後の
子プロセス生存確認が、固定のタイムアウト/リトライ回数に依存している
(471 行目付近)。`--workspace` 並列実行時の CPU 競合でスケジューリングが
遅延し、期限内に子プロセスの終了を観測できないと誤って FAILED になっていると
見られる。

## 直し方の案

- 生存確認のポーリング間隔・タイムアウトを環境の負荷に応じて緩める
  (固定 sleep ではなく、上限までポーリングする形に寄せる/上限を伸ばす)。
- テストランナー側で、この統合テストだけ `--test-threads=1` 相当に切り離す
  (Cargo.toml か CI 側で該当テストバイナリを直列実行に固定)。

---
id: cargo-test-workspace-devserver-etxtbsy-aenf
title: cargo test --workspace が devserver のテストで ETXTBSY で落ちることがある
summary: tempdir に書いた sh スクリプトを spawn する devserver のテストが、他スレッドの fork と競合して Text file busy で落ちる(単体実行では通る) / 未着手
status: open
labels: flaky-test
updated: 2026-07-29T07:53:14Z
---

## 現象

`cargo test --workspace` を走らせたとき、`edlr-ui` のテストが落ちることがある。

```
---- devserver::tests::stop_dev_server_kills_the_whole_process_group stdout ----
thread '...' panicked at ui/src-tauri/src/devserver.rs:205:77:
called `Result::unwrap()` on an `Err` value:
  Os { code: 26, kind: ExecutableFileBusy, message: "Text file busy" }
```

同じテストを単体で走らせると通る:

```
cargo test -p edlr-ui --bin edlr-ui devserver::tests::stop_dev_server_kills_the_whole_process_group
# → ok
```

## 原因(推定)

このテストは tempdir に `parent.sh` を書き、`chmod 755` してから
`spawn_in_own_process_group` で exec する
(`ui/src-tauri/src/devserver.rs` の `stop_dev_server_kills_the_whole_process_group`)。

同じプロセス内の**別テストスレッドが同時に fork** すると、書き込み用に開いた
fd が子プロセスへ引き継がれ、その fd が閉じられるまで exec 側は ETXTBSY を
受け取る。Rust の `Command::spawn` と「書いたばかりの実行ファイルを exec する」
組み合わせで知られた競合で、テストの並列度に依存して顕在化する。

## なぜ困るか

CI やローカルの `cargo test --workspace` がランダムに赤くなる。落ちたときに
本物の回帰かフレークか判別できず、毎回 re-run して確認する羽目になる。実際、
issue `info-host-log-debug-efeq` の修正をマージした直後の検証で踏み、
単体実行し直して初めてフレークだと分かった。

## 案

1. `spawn_in_own_process_group` が ETXTBSY を返したら短いスリープを挟んで数回
   リトライする(テスト側のヘルパとして。製品コードの spawn は触らない)
2. 一時ファイルではなく `/bin/sh -c '<script>'` を exec する形に変え、実行
   ファイルを新規作成しない
3. このテストだけ `serial_test` などで直列化する(根本原因は残る)

案 2 が競合そのものを無くすので素直。`spawn_in_own_process_group` が
「実行ファイルのパス」を受け取る前提なら、`sh` のパスと `-c` 引数を渡す形に
できるか確認する必要がある。

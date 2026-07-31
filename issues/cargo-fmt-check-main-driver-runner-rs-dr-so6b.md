---
id: cargo-fmt-check-main-driver-runner-rs-dr-so6b
title: cargo fmt --check が main で既に落ちている(driver/runner.rs と drivers/process)
summary: main(efd382e)時点で core/src/driver/runner.rs:158 と drivers/process/src/lib.rs:526,853 が rustfmt 非準拠。cargo fmt で直るだけの drift / 未着手
status: closed
labels: build
created: 2026-07-30T13:04:24Z
updated: 2026-07-31T14:10:51Z
---

## どこで踏んだか

core-refactor Phase 0–1 ブランチ(efd382e..32a092f)の最終レビューで
`cargo fmt --check` を実行したところ、リファクタ由来ではない既存の
違反が main 時点(efd382e)から存在していた:

- `core/src/driver/runner.rs:158` — `tracing::warn!` の1行呼び出しが幅超過
- `drivers/process/src/lib.rs:526` / `:853`

再現: main を checkout して `cargo fmt --check`。

## なぜ困るか

- fmt を CI ゲートに足せない(足すと main が即赤になる)
- 以後のブランチで `cargo fmt` を走らせると無関係な diff が混ざり、
  move-only コミットの規律(1コミット=移動のみ)と干渉する

## 直し方

`cargo fmt` を一度走らせて formatting-only のコミットを1つ作るだけ。
core リファクタリングの move-only コミット群と混ざらないタイミング
(フェーズ境界)で入れるのが安全。

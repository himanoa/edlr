---
id: manifest-tests-rs-rustfmt-move-only-upfj
title: manifest/tests.rs が rustfmt 非準拠(move-only 移動の意図的トレードオフ)
summary: core リファクタ Phase 1 のテスト分離で dedent した manifest/tests.rs の7箇所が cargo fmt --check に落ちる(byte-identical 移動を優先した意図的債務)。フェーズ境界で formatting-only commit を入れて解消する / 未着手
status: closed
labels: build, refactor
created: 2026-07-30T13:08:14Z
updated: 2026-07-31T06:33:15Z
---

## どこで踏んだか

core リファクタ Phase 1(plan: `docs/superpowers/plans/2026-07-30-core-refactor-phase0-1.md` Task 4)で、
`core/src/plugin/manifest.rs` の `mod tests` を `core/src/plugin/manifest/tests.rs` へ丸ごと移動した。
移動の byte-identical 検証を優先して「wrapper 剥がし + 一様4スペース dedent」だけを行ったため、
行が短くなった7箇所(tests.rs:144, 278, 917, 933, 1055, 1228, 1238 付近)で rustfmt が
文の再結合を要求し、`cargo fmt --check` に落ちる。

## なぜ困るか

- 将来誰かが `cargo fmt` を走らせると、凍結中のテストファイルに差分が出て
  「テスト凍結違反に見える diff」が混ざる
- fmt を CI ゲートに入れる際の障害になる(main 側の既存 drift は
  `cargo-fmt-check-main-driver-runner-rs-dr-so6b` として別途起票済み)

## 直し方

フェーズ境界(Phase 2 着手前など)で、formatting-only commit を1つ入れる。
コミットメッセージに「フォーマットのみ・トークン変更ゼロ・テスト凍結の明示的例外」と明記する。
main 側 drift の issue と同じコミットでまとめて解消してもよい。

## 解決(2026-07-31)

Phase 6 の style コミット 91dca61(cargo fmt を全ワークスペースへ適用、issue so6b と同時)で解消済み。
cargo fmt --check は現在差分0(2026-07-31 確認)。

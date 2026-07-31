---
id: moonbit-wit-bindgen-0-45-0-60-na5m
title: MoonBit チュートリアルの wit-bindgen 0.45 ピンを、0.60 系対応後に外す
summary: wit-bindgen 0.60 の moonbit 生成は文字列が 8 バイトずれて文字化けするため 0.45 系にピン留め中。上流解消後にピンを外し再生成する / 未着手
status: open
labels: 
created: 2026-07-31T14:51:58Z
updated: 2026-07-31T14:52:33Z
---

## どこで踏んだか

MoonBit 版プラグインチュートリアル(docs/plugin-tutorial-moonbit.md、
examples/plugins/tutorial-jump-log-mbt)の作成時。wit-bindgen-cli **0.60.0** の
moonbit ターゲットで生成したバインディングでビルドすると、ロードや
イベント配信は通るのに、`host-log` のメッセージや payload が全て文字化けする。

再現: wit-bindgen 0.60.0 で `wit-bindgen moonbit core/wit --world plugin` →
`moon build --target wasm --release`(moon 0.1.20260309)→
`wasm-tools component embed -w plugin-guest --encoding utf16` + `component new`
→ デーモンでロードすると
`INFO ... ￿￿倀tutorial-jump-log sta plugin_id=...` のような出力になる。

## 原因

MoonBit の文字列/配列オブジェクトは先頭 8 バイトがヘッダで、データは
「オブジェクトポインタ + 8」から始まる。

- 0.45 系の生成物: `str2ptr` が `i32.const 8 i32.add` を入れており正しい
- 0.60.0 の生成物: `mbt_ffi_str2ptr` が `local.get 0`(ポインタそのまま)で、
  ヘッダをデータとして渡してしまう(逆方向 `ptr2str` も同様にずれる)

現行 moonc(0.1.20260309)のオブジェクトレイアウトと 0.60 の想定が
食い違っている。上流(bytecodealliance/wit-bindgen の moonbit バックエンド、
または moonc 側の追従)の問題で、edlr 側では直せない。

## いま何をしているか(回避策)

チュートリアルと examples の README で **0.45 系へのピン留め**
(`cargo install wit-bindgen-cli --version 0.45.0`)を指示している。
0.45 系は現行 moonc で警告(unannotated_ffi)が出るため、生成後に
`ffi/moon.pkg.json` の `warn-list` へ `-55` を足す手当ても書いてある。

## 直すとき

新しい wit-bindgen(または moonc)の組み合わせで文字化けが解消したら:

1. examples/plugins/tutorial-jump-log-mbt と examples/drivers/tutorial-tracker-mbt
   のバインディングを `--ignore-stub` 付きで再生成し、実機で文字列が
   化けないことを確認する(docs/plugin-tutorial-moonbit.md 2 章の手順)
2. docs/plugin-tutorial-moonbit.md の 0 章・7 章と両 examples の README から
   0.45 ピンの記述を外す(warn-list -55 の手当てが不要になっていればそれも)

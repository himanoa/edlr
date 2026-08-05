---
id: rust-log-debug-gui-logs-ebie
title: RUST_LOG=debug で GUI の Logs 画面が溢れる(レベルフィルタが無い)
summary: RUST_LOG=debug にすると依存クレートの debug まで GUI へ流れ、容量 256 の broadcast が詰まって本来見たいログが Lagged で落ちる / Logs 画面にレベル絞り込みも無い / 未着手
status: closed
labels: 
created: 2026-07-29T07:49:19Z
updated: 2026-08-05T07:12:33Z
---

## どこで踏んだか

`info-host-log-debug-efeq`(デーモンのログレベルが INFO 固定)の修正で、
閾値を `EnvFilter`(既定 info / `RUST_LOG` で上書き)に変え、GUI 転送用の
`LogLayer` 側の足切り(`level > Level::INFO` で return)も外した。これで
`RUST_LOG=debug` を付ければプラグインの `host-log` debug が stderr にも
GUI の Logs 画面にも出る。

その結果として残る問題が 2 つある(どちらもその修正の範囲外なので着手して
いない)。

## なぜ困るか

1. **`RUST_LOG=debug` は「全クレートの debug」を意味する。** wasmtime /
   hyper / reqwest / notify などの debug が丸ごと `LogLayer` に流れ込む。
   `core/src/logs.rs` の broadcast は容量 256(`CHANNEL_CAPACITY`)なので、
   WS の受信側(`ServerState::attach_log_stream`)が追い付かなければ Lagged
   で黙って捨てられる。**捨てられるのは新旧問わずなので、本来見たかった
   プラグインの debug 行が落ちうる。** 当面の回避策は
   `RUST_LOG=info,edlr_core::plugin::host=debug` のようにターゲットを絞る
   こと(`docs/plugins.md` の「ログレベル」に例を書いた)。

2. **Logs 画面にレベルでの絞り込みが無い。** `ui/frontend/src/pages/Logs.tsx`
   のツールバーは kind(journal/status/log)のトグルと文字列フィルタだけで、
   level は表示するだけ(`.log-level-<level>`)。クライアント側バッファは
   2000 件で頭打ちなので、debug が混ざると数秒前の info/warn が押し出される。

## 案

- GUI へ流す分だけ別のフィルタを持たせる(`LogLayer` に `Filter` を per-layer
  で付ける)。ただし「stderr と GUI が同じものを見る」という今の素直さは失う
- 既定の `RUST_LOG` フォールバックを `info` ではなく
  `info,edlr_core=debug` 相当の「自分のクレートは饒舌」にする案もあるが、
  既定を変える話なので別途判断が要る
- Logs 画面に level トグル(error/warn/info/debug)を足す。1 と独立に効く
- `CHANNEL_CAPACITY`(256)を上げる。詰まりの緩和にはなるが根治ではない

## 該当ファイル

- `core/src/logs.rs`(`LogLayer` / `CHANNEL_CAPACITY` / `env_filter`)
- `core/src/server.rs`(`attach_log_stream`)
- `ui/frontend/src/pages/Logs.tsx`

## 対応(2026-08-05)

Logs 画面に level トグル(error/warn/info/debug/trace)を追加した(案3)。
broadcast 溢れ(問題1)はコード変更なし: docs/plugins.md 記載の
`RUST_LOG=info,edlr_core::plugin::host=debug` のようなターゲット絞りで運用する。
それでも足りない実害が出たら per-layer Filter / CHANNEL_CAPACITY 増で再起票。

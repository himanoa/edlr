---
id: wasmtime-vtrv
title: wasmtime のコンパイルキャッシュを有効化して起動時のプラグインロードを速くする
summary: EpochEngine の Config にキャッシュ未設定で毎起動フルコンパイル。debug 実行では全ロードに 15 秒超(release は 1.4 秒)。Config::cache 有効化で release 1.9s→0.4s / debug 15s+→1.0s に改善 / 実装済み(87413f6)
status: closed
labels: performance
created: 2026-08-05T13:07:30Z
updated: 2026-08-05T13:23:34Z
---

## どこで踏んだか

2026-08-05、プラグイン群を SDK v0.6.1 へ移行して UI 開発フロー
(`target/debug/edlr-ui` → `target/debug/edlr`)で起動したところ、
プラグインのロードが体感で明確に遅かった。

実測(同一の plugins/drivers ディレクトリ: ドライバ 5 + プラグイン 5、
`--settings-dir` 等をスクラッチに向けて起動、ログのタイムスタンプ比較):

- `target/release/edlr`: watching → 全プラグイン init 完了まで **約 1.4 秒**
- `target/debug/edlr`: 同区間が **15 秒超**(10 秒経過時点で 5 個中 2 個)

## なぜ困るか

- ロードは直列(`start.rs` が `ready_rx.recv()` で 1 個ずつ init を待つ)かつ
  `EpochEngine::new()` の `Config` にコンパイルキャッシュ設定が無いため、
  **毎起動、全コンポーネントを Cranelift でフルコンパイル**している
  (`core/src/host/engine.rs:32`、`Component::from_file` は `host/plugin.rs:901`)。
- release では現状 1.4 秒なので実害は小さいが、開発フロー(debug)では
  起動のたびに十数秒待つ。プラグインが増えると release でも線形に伸びる。

## 直し方の案

1. **`Config::cache` を有効化**(wasmtime のディスクキャッシュ)。
   コンパイル結果が XDG キャッシュに永続化され、wasm が変わらない限り
   2 回目以降の起動はデシリアライズだけになる。debug/release とも効く。
   変更は `EpochEngine::new()` の数行で済む見込み。
2. (補) ロードの並列化(spawn を先に全部やってから ready を待つ)。
   直列待ちは init 順序の保証が理由でなければ外せるが、キャッシュ有効化
   だけで開発時の痛みはほぼ消えるので優先度低。

## 結果(2026-08-05)

`Cache::new(CacheConfig::new())`(既定の $XDG_CACHE_HOME/wasmtime)を
`EpochEngine::new()` で有効化(87413f6)。実測: release 1.9s→0.4s、
debug 15s+→1.0s。キャッシュ初期化失敗は warn してキャッシュ無し続行。
案 2(ロード並列化)はキャッシュだけで痛みが消えたため見送り。

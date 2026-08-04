---
id: sdk-send-async-response-await-lvn3
title: ゲスト SDK に submit-http の結果を await できるヘルパーを足す
summary: 非ブロッキング HTTP は submit-http(job-id 返却)+ on-job-complete 配送に決定(2026-08-04、on-message 配送案は破棄)。ゲスト SDK 側で job-id→promise/future に変換し response 型で await できるヘルパーを提供する / issue-sizx 依存・実装未着手
status: open
labels: sdk
created: 2026-08-04T13:52:09Z
updated: 2026-08-04T13:57:25Z
---

## 背景

ゲスト(wasm)から見た host import は素の同期呼び出し(component model の
async ABI 不使用)であるため、ノンブロッキング HTTP が response を直接返す
ことはできない。ホスト側の設計は issue-sizx(submit/complete プロトコル)に
乗せる形で確定した(2026-08-04、`docs/async-migration.md` Step 2 改訂):

- `submit-http(request, timeout-ms) -> result<job-id, driver-error>`(型付き submit)
- 結果は `export on-job-complete` で非同期に届く

※ 当初の「`send-async` + 既存 `on-message` に予約ドライバ名 `"http"` で
JSON+base64 配送」案は破棄済み。ABI 破壊 OK が issue-sizx で決定され、
非破壊であることの価値が消えたため。

しかしプラグイン作者の書き味としては、job-id の突き合わせを毎回手書きするのは
煩雑で、response 型のまま await/継続で受け取りたい。

## やること

host API は動かさず、各言語のゲスト SDK に「`submit-http` の job-id →
promise/future/コールバック登録に変換し、response 型で受け取れる」ヘルパーを
載せる:

- SDK 内部で pending マップ(job-id → 継続)を持ち、`on-job-complete` の
  結果をデコードして解決する
- HTTP 以外の job 種別が将来増えることを想定し、`on-job-complete` の demux は
  種別を問わない形にしておく(HTTP 専用にハードコードしない)
- ユーザー自身が `on-job-complete` を扱いたいケースとの共存方法を決める
  (SDK が握って未知の job-id だけ委譲する等)
- 結果の型(typed か list<u8> か)は issue-sizx の実装に従う。デコードは
  SDK で済ませ、同期 `send` の response 型と同じ形で返す
- 対象言語はチュートリアルにある Rust / TinyGo / MoonBit(SDK の整備状況は
  golang-rust-sdk-9h8l に依存する部分あり)

## 依存・関連

- 前提: issue-sizx(submit/complete プロトコル本体、`on-job-complete` export)が先
- 前提: `submit-http` の実装(`docs/async-migration.md` Step 2b)
- 関連: http-driver-9znv(ホスト内部の async 化 = Step 2a、これは独立に先行可)
- 関連: golang-rust-sdk-9h8l(SDK 自体の新設)

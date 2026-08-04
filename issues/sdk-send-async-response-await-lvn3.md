---
id: sdk-send-async-response-await-lvn3
title: ゲスト SDK に send-async の response を await できるヘルパーを足す
summary: send-async は u64 ID + on-message コールバック配送のまま、ゲスト SDK 側で ID→promise/future に変換して response 型で await できるヘルパーを提供する / 方針決定済み(2026-08-04)・実装未着手
status: open
labels: sdk
created: 2026-08-04T13:52:09Z
updated: 2026-08-04T13:53:05Z
---

## 背景

`docs/async-migration.md` Step 2 の `send-async` は、ゲスト(wasm)から見た host import が
素の同期呼び出し(component model の async ABI 不使用)であるため、response を直接
返せない。返すには HTTP 完了まで host 関数内でゲストスレッドを止めるしかなく、
それは既存の同期 `send` と同じものになる。そのため設計は
「`send-async` → u64 リクエスト ID を即返し、結果は既存 `on-message`
(予約ドライバ名 `"http"`, topic `"response"`, id 相関の JSON payload)で配送」で確定している。

しかしプラグイン作者の書き味としては、ID の突き合わせと `on-message` での
JSON パースを毎回手書きするのは煩雑で、response 型のまま await/継続で
受け取りたいという要求がある。

## やること

host API は動かさず(u64 + on-message のまま)、各言語のゲスト SDK に
「`send-async` の ID → promise/future/コールバック登録に変換し、
response 型で受け取れる」ヘルパーを載せる:

- SDK 内部で pending マップ(id → 継続)を持ち、`on-message(driver: "http",
  topic: "response")` の payload(`{"id", "ok": {status, headers, body_b64}}` /
  `{"id", "err": {kind, message}}`)をデコードして解決する
- ユーザーの `on-message` ハンドラとの共存方法を決める(SDK が "http"/"response"
  を横取りし、それ以外を委譲する等)
- body の base64 デコードまで SDK で済ませ、同期 `send` の response 型と
  同じ形で返す
- 対象言語はチュートリアルにある Rust / TinyGo / MoonBit(SDK の整備状況は
  golang-rust-sdk-9h8l に依存する部分あり)

## 依存・関連

- 前提: Step 2(`send-async` のホスト実装)が先。関連 issue: http-driver-9znv
- 関連: golang-rust-sdk-9h8l(SDK 自体の新設)

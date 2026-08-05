---
id: sdk-send-async-response-await-lvn3
title: ゲスト SDK に submit-http の結果を await できるヘルパーを足す
summary: コールバック式で実装完了(future/promise ではなくゲスト側 pending マップ)。Rust は sdk/rust/src/http.rs、Go は sdk/go/edlrplugin/jobs.go
status: open
labels: sdk
created: 2026-08-04T13:52:09Z
updated: 2026-08-05T04:10:37Z
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

## 実装

設計: `docs/superpowers/specs/2026-08-05-guest-sdk-design.md`。使い方は
`docs/sdk.md`。

**future ではなくコールバックにした理由**: ゲスト(wasm)はシングルスレッド
かつ component model の async ABI を使わない同期実行のため、await 可能な
future/promise を実装しても実行を止めて待つ相手(イベントループ)が存在
しない。素直な形はホストからの次の export 呼び出し(`on-job-complete`)で
起動されるコールバックであり、この形で実装した。

- Rust(`sdk/rust/src/http.rs`): `http::submit(request, timeout_ms,
  callback) -> Result<u64, DriverError>`。job-id → `Box<dyn FnOnce(..)>`
  の pending マップを `thread_local! { RefCell<HashMap<..>> } }` で持つ。
  `register!` の `on-job-complete` shim(`dispatch_job_complete`)が pending
  を引き、無ければ `Plugin::on_job_complete` へ委譲(未知 job-id への委譲)
- Go(`sdk/go/edlrplugin/jobs.go`): `SubmitHTTP(req, timeoutMS, cb) (uint64,
  error)`。pending はゲストがシングルスレッドなので素の `map[uint64]` で
  足りる。`Register` が配線する `dispatchJobComplete` が同様に pending →
  `Hooks.OnJobComplete` の順で解決する
- `result-json`(`{"ok":{status,headers,body-base64}}` /
  `{"err":{kind,message}}`)のデコードは両言語とも値イン値アウトの純関数に
  切り出し、ok/err/base64 破損/JSON 破損のケースを unit テストでカバー
  (`parse_job_result` / `parseJobResult`)
- demux は job 種別に依存せず job-id だけで解決するため、HTTP 以外の job
  種別が将来増えても構造は変わらない
- 実地テストは `examples/plugins/http-caller`(Rust)の `sdk::http::submit`
  呼び出しで兼ねる

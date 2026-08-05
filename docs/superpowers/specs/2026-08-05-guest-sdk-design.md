# ゲスト SDK(Rust / Go)設計

issue: golang-rust-sdk-9h8l(+ sdk-send-async-response-await-lvn3 を包含)
日付: 2026-08-05
状態: 設計承認済み

## 目的

ABI(WIT)が変わるたびに、プラグイン作者が `core/wit` を自リポジトリへ cp
してバインディングを再生成する作業を無くす。プラグインは SDK パッケージに
依存するだけにし、ABI 追従は SDK 側(このリポジトリ)の 1 箇所で済ませる。

あわせて、submit/complete プロトコル(WIT 0.5.0)の job-id 突き合わせを
手書きさせない await ヘルパー(コールバック式)を SDK に載せる
(issue sdk-send-async-response-await-lvn3)。

## 決定事項

1. **配布はこのリポジトリ直参照**。Rust は Cargo git 依存(tag 指定)、
   Go は `github.com/himanoa/edlr/sdk/go` のサブディレクトリモジュール。
   crates.io 公開はしない(必要になったら別 issue)
2. **バインディングは SDK が内包する**。Rust は SDK crate 内の
   `wit_bindgen::generate!` 1 回、Go は `wit-bindgen-go` 生成物
   (`gen/`)をコミット。プラグイン作者は wit-bindgen 系ツールに触らない
3. **await ヘルパーはコールバック式**。ゲストはシングルスレッド同期なので
   future/promise ではなくコールバックが正直な形
4. **既存 examples は全部 SDK へ移行**(Rust 7 個 + Go 2 個)。テスト
   フィクスチャが SDK 経由になり、ABI バンプ時の SDK 追従漏れを CI が
   検出する。MoonBit ゲストとチュートリアル本文は現状維持
5. **SDK バージョン = WIT バージョン**。タグは `sdk/v<wit>` /
   `sdk/go/v<wit>`(Go のサブディレクトリモジュール規約)

## リポジトリ構成

```
sdk/
  rust/          # crate: edlr-plugin-sdk(workspace 非メンバー。examples と同じ扱い)
    src/lib.rs   # generate! + 再エクスポート + Plugin trait + register! マクロ
    src/http.rs  # submit ヘルパー(pending マップ・result-json デコード)
  go/            # module: github.com/himanoa/edlr/sdk/go
    wit/         # core/wit のコピー(同期テストで守る。理由は後述)
    gen/         # wit-bindgen-go 生成物(コミット)
    edlrplugin/  # Plugin 登録の肩代わり + submit ヘルパー
```

## Rust SDK

### 利用者から見た形

```rust
use edlr_plugin_sdk as sdk;

struct MyPlugin;

impl sdk::Plugin for MyPlugin {
    fn on_event(ev: sdk::Event) {
        let request = sdk::http::Request { /* .. */ };
        let _ = sdk::http::submit(request, None, |result| {
            // result: Result<sdk::http::Response, sdk::http::JobError>
            // Response.body は base64 デコード済みの Vec<u8>
        });
    }
    // init / on_message / on_schedule / on_stop / on_job_complete は
    // デフォルト実装(空)あり。必要なものだけ書く
}

sdk::register!(MyPlugin);
```

```toml
[dependencies]
edlr-plugin-sdk = { git = "https://github.com/himanoa/edlr", tag = "sdk/v0.5.0" }
```

### 実装

- `wit_bindgen::generate!({ path: "../../core/wit", world: "plugin",
  pub_export_macro: true, ... })`。Cargo の git 依存はリポジトリ全体を
  checkout するため、crate 外の `core/wit` を相対パスで参照できる
  (コピー不要)。crates.io 公開時はこの前提が壊れるが、公開はスコープ外
- `sdk::Plugin` は SDK 独自 trait。全メソッドにデフォルト実装を持ち、
  bindgen の `Guest`(全 export 必須)との緩衝になる。ABI に export が
  増えたときも、SDK 側にデフォルト実装を足せば既存プラグインは
  ソース互換のまま再ビルドだけで追従できる
- `register!(T)` マクロが shim 型に `Guest` を実装し、bindgen の
  `export!`(`with_types_in` 付き)を呼ぶ。`on-job-complete` の shim は
  まず SDK の pending マップを引き、未知の job-id だけ
  `Plugin::on_job_complete` へ委譲する(自前で `submit_send` を直接叩く
  利用者との共存)
- `http::submit(request, timeout_ms, callback) -> Result<JobId, DriverError>`:
  `driver-http.submit-send` を呼び、job-id をキーに
  `thread_local! { RefCell<HashMap<u64, Box<dyn FnOnce(..)>>> }` へ
  callback を登録する(wasm ゲストはシングルスレッドなので thread_local
  で足りる)
- `result-json` のデコード(`{"ok":{status,headers,body-base64}}` /
  `{"err":{kind,message}}` → `Result<Response, JobError>`)は値イン値アウトの
  純関数に切り出してテストする。依存: serde_json + base64
- demux は job 種別に依存しない(job-id → コールバックの表のみ)。
  HTTP 以外の job が将来増えても構造は変わらない

## Go SDK

### 利用者から見た形

```go
import (
    sdk "github.com/himanoa/edlr/sdk/go/edlrplugin"
)

func init() {
    sdk.Register(sdk.Hooks{
        OnEvent: func(ev sdk.Event) {
            sdk.SubmitHTTP(req, nil, func(result sdk.HTTPResult) { /* .. */ })
        },
        // 未設定のフックは no-op
    })
}
```

### 実装

- `gen/` は `wit-bindgen-go generate` の出力をコミット(モジュールパスは
  `github.com/himanoa/edlr/sdk/go/gen/...`)
- `edlrplugin.Register(Hooks)` が `gen` の `Exports.*` へ全フックを配線する
  (未設定は no-op)。`OnJobComplete` は Rust 同様 pending マップ →
  未知 id は `Hooks.OnJobComplete` へ委譲
- **WIT の同梱が必要**: tinygo のコンポーネント化
  (`tinygo build --wit-package <dir> --wit-world plugin-guest`)は WIT
  ファイルをディスク上に要求するが、Go module の zip はモジュール
  ディレクトリしか含まない。そのため `sdk/go/wit/` に `core/wit` の
  コピーを同梱し、ビルドスクリプトからは
  `go list -m -f '{{.Dir}}' github.com/himanoa/edlr/sdk/go` で場所を引く
- `core/wit` と `sdk/go/wit` の内容一致を core のテスト
  (`wit_version_docs_sync.rs` の隣)で機械的に検証する。cp 忘れ = テスト赤

## examples の移行

- Rust 7 個(hello-logger / http-caller / busy-loop / init-trap /
  memory-hog / state-reader / tutorial-jump-log-rs):
  `wit_bindgen::generate!` を消して `edlr-plugin-sdk` の path 依存に。
  `sdk::Plugin` + `register!` へ書き換え
- Go 2 個(inara-uploader / tutorial-jump-log-go): 各自の `gen/` を消して
  `replace github.com/himanoa/edlr/sdk/go => ../../../sdk/go` で参照。
  build.sh の `--wit-package` は sdk/go/wit を指す
- http-caller の submit 例は `sdk::http::submit`(コールバック式)へ
  書き換え、await ヘルパーの実地テストを兼ねる
- MoonBit(tutorial-jump-log-mbt)は SDK 対象外、現状維持

## ドキュメント

- `docs/sdk.md` 新設: 依存の書き方(git/tag、go get)、`Plugin`/`Hooks` の
  形、submit ヘルパー、ABI バンプ時に利用者がやること(tag を上げて
  再ビルドするだけ)
- チュートリアル 3 本は低レベル経路(生 wit-bindgen)の解説として現状維持。
  冒頭に「手早く書くなら SDK(docs/sdk.md)」への誘導を 1 行足す
- `docs/plugins.md` の「プラグインを新しい core/wit に対して再ビルド」の
  節に SDK 経由の選択肢を追記

## テスト

- SDK 純粋部分の unit テスト:
  - Rust: result-json デコード(ok/err/base64/壊れた JSON)、pending
    demux(登録→解決・未知 id 委譲・二重解決なし)
  - Go: 同等の `go test`(fake の gen 呼び出しは薄く。デコードと
    demux が主対象)
- E2E: 既存統合テスト(plugin_host_integration / bus_integration ほか)が
  移行済み examples をビルドするため、SDK 経由のロード・init・イベント
  配送・submit までカバーされる
- WIT コピー同期テスト(core/tests)

## やらないこと

- crates.io / 独立リポジトリでの配布(必要になったら別 issue)
- MoonBit SDK
- チュートリアル本文の SDK 書き直し
- future/promise 型の await(コールバックで足りる。言語側に async
  ランタイムを持ち込まない)

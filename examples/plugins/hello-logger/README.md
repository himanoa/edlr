# hello-logger

`edlr:plugin@0.2.0` の WIT world (`core/wit/plugin.wit`) を実装したサンプル WASM component プラグイン。

- `init()` — 起動時に `"hello-logger initialized"` を info レベルでログ出力する
- `on-event(ev)` — `host-settings.get-all()` から取得した JSON の `enabled`(省略時・パース不可時は `true` 扱い)が真のときのみ、`"<kind>:<name> <payload-json>"` を info レベルでログ出力する(`name` は `ev.name` が無ければ `"-"`)。デーモン起動前に既に書かれていたイベント(`ev.replay`)は、名前の直後に ` (replay)` が挟まって `"<kind>:<name> (replay) <payload-json>"` になる

このクレートはルートの Cargo workspace には含まれない独立クレート(`Cargo.toml` に空の `[workspace]` テーブルを持つ)。`core/wit` を `wit_bindgen::generate!` の `path` で直接参照しており、WIT ファイルはコピーしない。

ゲスト(プラグイン)がビルド時に対象とすべき world は、言語を問わず **`plugin-guest`**(= `plugin` に WASI の import 一式を足したもの)。この例が `world: "plugin"` を指定しているのは、Rust の `wasm32-wasip2` ターゲットではリンカ(`wasm-component-ld`)が WASI import を自動で足すため — Go/TinyGo ではそうならず、`plugin` を対象にするとコンポーネント化が失敗する(`examples/plugins/inara-uploader/README.md` 参照)。

## Build

```sh
cd examples/plugins/hello-logger
cargo build --target wasm32-wasip2 --release
```

生成物: `target/wasm32-wasip2/release/hello_logger.wasm`

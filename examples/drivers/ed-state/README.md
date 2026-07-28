# ed-state

`edlr:plugin@0.3.0` の WIT world (`core/wit/plugin.wit`) の `driver` を実装したサンプル WASM component ドライバ。

- `init()` — 起動時に `"ed-state driver started"` を info レベルでログ出力する
- `on-message(from, topic, payload)` — `topic` が `"set-system"` のときだけ、`payload` をそのまま retained トピック `"current-system"` として `bus-host.emit` で配り直す。それ以外のトピックは無視する

`ed-state` は自分の `driver.toml` で `set-system`(非 retain)と `current-system`(retain)の 2 トピックを宣言する。プラグインは前者へ `publish` してシステム名を渡し、後者を `subscribe` して配り直された値を受け取る(`examples/plugins/state-reader` 参照)。ドライバは自分を呼び出したプラグインの ID(`from`)を知ってはいるが、`emit` 自体は宛先を指定しない(トピックを購読している全プラグインに配る)。

このクレートはルートの Cargo workspace には含まれない独立クレート(`Cargo.toml` に空の `[workspace]` テーブルを持つ)。`core/wit` を `wit_bindgen::generate!` の `path` で直接参照しており、WIT ファイルはコピーしない。

ゲスト(ドライバ)がビルド時に対象とすべき world は、言語を問わず **`driver-guest`**(= `driver` に WASI の import 一式を足したもの)。素の `driver` は `bus`(プラグイン→ドライバの発行 API)を import しない — ドライバ間の呼び出しを構造的に不可能にするための設計であり、ドライバが持つのは `bus-host`(自分のトピックへの `emit`)だけ。

## Build

```sh
cd examples/drivers/ed-state
cargo build --target wasm32-wasip2 --release
```

生成物: `target/wasm32-wasip2/release/ed_state.wasm`

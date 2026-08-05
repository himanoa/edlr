# ゲスト SDK(Rust / Go)

edlr のプラグインを、`core/wit` を自分のリポジトリへ `cp` したり
`wit-bindgen` を手で回したりせずに書くための SDK。ABI(WIT)が上がったら、
依存の tag を上げて再ビルドするだけで追従できる。

低レベル経路(生 wit-bindgen、`core/wit` を自分で cp)の解説は
[plugin-tutorial-rust.md](plugin-tutorial-rust.md) /
[plugin-tutorial-tinygo.md](plugin-tutorial-tinygo.md) を参照。仕組みを
理解したい場合や SDK が対応していない言語(MoonBit)ではそちらを使う。

## なにが嬉しいか

- WIT バインディングを SDK パッケージが内包している。プラグイン作者は
  `core/wit` の cp も `wit-bindgen` / `wit-bindgen-go` の実行も不要
- ABI バンプ(WIT のバージョンアップ)への追従は、依存の tag を上げて
  再ビルドするだけで済む。SDK 側で追従を 1 箇所に集約している
- `Plugin` trait / `Hooks` は全フックにデフォルト実装(no-op)がある。
  必要なものだけ書けばよく、export が増えても既存プラグインはソース
  互換のまま再ビルドで追従できる

## Rust

### 依存の書き方

```toml
[dependencies]
edlr-plugin-sdk = { git = "https://github.com/himanoa/edlr", tag = "sdk/v0.6.1" }
```

`crate-type = ["rlib"]` の通常の Rust crate なので、プラグイン側は
これまで通り `crate-type = ["cdylib"]` にして `wasm-tools component new`
で仕上げる。

### `Plugin` trait と `register!`

`Plugin` trait は全メソッドにデフォルト実装(空)があるので、必要な
フックだけ実装すればよい。`sdk::register!` でホストへの export を配線する。

```rust
use edlr_plugin_sdk as sdk;
use sdk::host_log::{log, Level};

struct HelloLogger;

impl sdk::Plugin for HelloLogger {
    fn init() {
        log(Level::Info, "hello-logger initialized");
    }

    fn on_event(ev: sdk::Event) {
        let name = ev.name.as_deref().unwrap_or("-");
        log(Level::Info, &format!("{}:{}", ev.kind, name));
    }
}

sdk::register!(HelloLogger);
```

`Plugin` のフック一覧(すべて省略可): `init` / `on_event` / `on_message`
/ `on_schedule` / `on_job_complete` / `on_stop`。`bus` / `driver_fs` /
`driver_http` / `driver_process` / `host_log` / `host_settings` は
`sdk::` 直下に再エクスポートされている。

### `http::submit`(コールバック式)

`sdk::http::submit(request, timeout_ms, callback)` はリクエストを
非同期に投げ、呼び出し自体は即座に job-id を返す(`Result<u64,
DriverError>`。受付が拒否された場合はコールバックを登録せず同期の
`Err` を返す)。結果は同期では返らず、その後のどれかの export 呼び出し
(次の `on_event` などの中)で `callback` が起動される形で届く。

```rust
match sdk::http::submit(request, None, move |result| match result {
    Ok(response) => log(Level::Info, &format!("status {}", response.status)),
    Err(e) => log(Level::Warn, &format!("job failed: {e:?}")),
}) {
    Ok(job_id) => log(Level::Info, &format!("submitted as job {job_id}")),
    Err(e) => log(Level::Warn, &format!("submit failed: {e:?}")),
}
```

`callback` の型は `Result<sdk::http::Response, sdk::http::JobError>`。
`Response.body` は base64 デコード済みの `Vec<u8>`。

SDK は job-id → コールバックの pending マップを内部で持ち、
`on-job-complete` の配送時にそこを引いて解決する。`submit` を経由せず
自前で `driver_http::submit_send` を直接呼んだ job の完了は、pending に
無い id として `Plugin::on_job_complete` へ委譲される(SDK の
コールバックとユーザー実装の `on_job_complete` は共存できる)。

ゲスト(wasm)はシングルスレッドで同期実行のため、future/promise では
なくコールバックを使っている(component model の async ABI は不使用)。

## Go

### 依存の書き方

```
go get github.com/himanoa/edlr/sdk/go@sdk/go/v0.6.1
```

### `Hooks` と `Register`

```go
import (
	sdk "github.com/himanoa/edlr/sdk/go/edlrplugin"
	hostlog "github.com/himanoa/edlr/sdk/go/gen/edlr/plugin/host-log"
)

func init() {
	sdk.Register(sdk.Hooks{
		Init: func() {
			hostlog.Log(hostlog.LevelInfo, "started")
		},
		OnEvent: func(ev sdk.Event) {
			// ...
		},
	})
}

func main() {}
```

`Hooks` の各フィールド(`Init` / `OnEvent` / `OnMessage` / `OnSchedule`
/ `OnJobComplete` / `OnStop`)は未設定(`nil`)なら no-op になる。`main`
は空でよい(TinyGo がコンポーネントをビルドするために要るだけで、
エントリポイントとしては使われない)。

### `SubmitHTTP`

```go
jobID, err := sdk.SubmitHTTP(req, nil, func(resp *sdk.Response, err error) {
	if err != nil {
		// job 失敗
		return
	}
	// resp.Status / resp.Headers / resp.Body(base64 デコード済み)
})
```

Rust の `http::submit` と同じ形: 呼び出しは即座に job-id を返し
(受付拒否時は `err` が非 nil でコールバックは登録されない)、結果は
その後のどれかの export 呼び出しの中でコールバックが起動されて届く。
SDK が pending マップを持ち、`SubmitHTTP` を経由しない job の完了だけ
`Hooks.OnJobComplete` へ委譲される。

### tinygo build

SDK は WIT ファイルを `sdk/go/wit/` に同梱している(Go module の zip は
モジュールディレクトリしか含まないため)。`--wit-package` / `--wit-world`
は `tinygo build -target=wasip2` 自身のフラグ(`wasm-tools component new`
は別途呼ばない、tinygo が内部でコンポーネント化まで行う)。ビルド
スクリプトからは `go list -m` で場所を引く:

```sh
tinygo build -target=wasip2 \
  --wit-package "$(go list -m -f '{{.Dir}}' github.com/himanoa/edlr/sdk/go)/wit" \
  --wit-world plugin-guest \
  -o plugin.wasm ./...
```

## バージョニング

SDK バージョン = WIT バージョンで、tag はそれぞれ:

- Rust: `sdk/v<wit-version>`(例: `sdk/v0.6.1`)
- Go: `sdk/go/v<wit-version>`(例: `sdk/go/v0.6.1`。Go のサブディレクトリ
  モジュール規約)

`core/wit` の ABI が上がったら、SDK 側がバインディングを追従させた上で
対応する tag を打つ。プラグイン作者は依存の tag を上げて再ビルドする
だけでよい。

## 低レベル経路との使い分け

WIT を直接触りたい、SDK が対応していない言語(MoonBit)で書きたい、
あるいは SDK の内部を理解したい場合は、生の `wit-bindgen` /
`wit-bindgen-go` を使う低レベル経路のチュートリアルを参照:
[plugin-tutorial-rust.md](plugin-tutorial-rust.md) /
[plugin-tutorial-tinygo.md](plugin-tutorial-tinygo.md) /
[plugin-tutorial-moonbit.md](plugin-tutorial-moonbit.md)。WIT インター
フェースのリファレンスは [plugins.md](plugins.md) にある。

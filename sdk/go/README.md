# github.com/himanoa/edlr/sdk/go

edlr プラグインを Go で書くための SDK。WIT バインディング(`gen/`)と WIT 本体
(`wit/`)を同梱しているので、プラグイン作者は `core/wit` の cp も
`wit-bindgen-go` の実行も不要。

## インストール

```bash
go get github.com/himanoa/edlr/sdk/go@sdk/go/v0.6.0
```

## tinygo build

```bash
tinygo build -target=wasip2 \
  --wit-package "$(go list -m -f '{{.Dir}}' github.com/himanoa/edlr/sdk/go)/wit" \
  --wit-world plugin-guest \
  -o plugin.wasm ./...
```

詳細な使い方(Hooks / Register / SubmitHTTP の設計、非同期 HTTP の意味論)は
リポジトリの `docs/sdk.md` を参照。

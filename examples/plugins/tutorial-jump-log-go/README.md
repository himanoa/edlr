# tutorial-jump-log-go

[docs/plugin-tutorial-tinygo.md](../../../docs/plugin-tutorial-tinygo.md) で
育てるプラグインの完成形(6 章まで終わった状態)。

FSDJump を拾い、設定でふるいに掛け、`tutorial-tracker-go` ドライバへ訪問先を
publish し、10 秒ごとの定期実行で 1 件ずつ EDSM へ問い合わせる。

判断を持つコードは `jumplog` パッケージにあり、テストできる(`main` は
`//go:wasmimport` を含むためネイティブでリンクできない)。

```
./build.sh                             # plugin.wasm を出力(要 TinyGo)
go test ./...                          # ロジックのテスト
tinygo test -target=wasip1 ./jumplog/  # wasm 上でも走らせる
go vet ./...
```

ドライバ(`examples/drivers/tutorial-tracker-go`)と一緒に入れる:

```
./scripts/install-examples.sh tutorial-jump-log-go tutorial-tracker-go
```

配置後はデーモンの再起動と、GUI の Plugins ページでの承認(HTTP capability と
bus 接続)が要る。

## バインディング

WIT バインディング(`gen/`)は `sdk/go`(`edlrplugin`)が同梱しているので、
このプラグイン側での生成・コミットは不要。再生成が要る場合は
`sdk/go/README.md` を参照。

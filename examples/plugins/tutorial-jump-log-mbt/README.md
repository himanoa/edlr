# tutorial-jump-log-mbt

[docs/plugin-tutorial-moonbit.md](../../../docs/plugin-tutorial-moonbit.md) で
育てるプラグインの完成形(6 章まで終わった状態)。

FSDJump を拾い、設定でふるいに掛け、`tutorial-tracker-mbt` ドライバへ訪問先を
publish し、10 秒ごとの定期実行で 1 件ずつ EDSM へ問い合わせる。

判断を持つコードは `jumplog` パッケージにあり、`moon test` でテストできる
(export の配線は `gen/world/plugin/stub.mbt` にある)。

```
./build.sh                        # plugin.wasm を出力(要 MoonBit + wasm-tools)
moon test --target wasm           # ロジックのテスト
moon check --target wasm          # 型チェック
```

ドライバ(`examples/drivers/tutorial-tracker-mbt`)と一緒に入れる:

```
./scripts/install-examples.sh tutorial-jump-log-mbt tutorial-tracker-mbt
```

配置後はデーモンの再起動と、GUI の Plugins ページでの承認(HTTP capability と
bus 接続)が要る。

## バインディングの再生成

`core/wit` を変えたときだけ必要(生成物はコミットしてある)。
**`--ignore-stub` を忘れると `gen/world/plugin/stub.mbt`(実装本体)が
上書きされる**。また `ffi/moon.pkg.json` の `warn-list` 修正と
`gen/world/plugin/moon.pkg.json` の import 追記も再生成で消えるので、
`git diff` で戻すこと:

```
wit-bindgen moonbit ../../../core/wit --world plugin \
  --derive-show --derive-eq --ignore-stub --out-dir .
```

wit-bindgen-cli は **0.45 系**を使う(`cargo install wit-bindgen-cli --version 0.45.0`)。
0.60 は MoonBit の文字列レイアウトと合わず、ログや payload が文字化けする。

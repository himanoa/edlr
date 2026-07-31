# tutorial-tracker-mbt

[docs/plugin-tutorial-moonbit.md](../../../docs/plugin-tutorial-moonbit.md) の
6 章で作るドライバの完成形。

`visit` トピックで受け取った星系名を数え、`last-system` トピック
(retain 付き)へ最新の訪問先と通算回数の JSON を流す。

```
./build.sh   # driver.wasm を出力(要 MoonBit + wasm-tools)
```

実装は `gen/world/driver/stub.mbt` にある。バインディングの再生成は
プラグイン側(`examples/plugins/tutorial-jump-log-mbt`)と同じ流儀で、
world だけ `driver` に変える:

```
wit-bindgen moonbit ../../../core/wit --world driver \
  --derive-show --derive-eq --ignore-stub --out-dir .
```

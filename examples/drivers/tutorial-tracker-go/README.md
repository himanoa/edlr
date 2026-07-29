# tutorial-tracker-go

[docs/plugin-tutorial-tinygo.md](../../../docs/plugin-tutorial-tinygo.md) の
6 章で作るドライバ。`tutorial-jump-log-go` から `visit` で受け取った星系名を
数え、`last-system`(`retain = true`)へ流し直すだけ。

```
./build.sh   # driver.wasm を出力(要 TinyGo)
./scripts/install-examples.sh tutorial-tracker-go
```

バインディングは**ドライバの world** から生成する(`core/wit` を変えたときだけ):

```
wit-bindgen-go generate --world driver --out gen ../../../core/wit
```

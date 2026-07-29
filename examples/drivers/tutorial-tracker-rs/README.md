# tutorial-tracker-rs

[docs/plugin-tutorial-rust.md](../../../docs/plugin-tutorial-rust.md) の 6 章で
作るドライバ。`tutorial-jump-log-rs` から `visit` で受け取った星系名を数え、
`last-system`(`retain = true`)へ流し直すだけ。

```
cargo build --release --target wasm32-wasip2
./scripts/install-examples.sh tutorial-tracker-rs
```

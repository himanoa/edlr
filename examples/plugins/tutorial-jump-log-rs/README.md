# tutorial-jump-log-rs

[docs/plugin-tutorial-rust.md](../../../docs/plugin-tutorial-rust.md) で育てる
プラグインの完成形(6 章まで終わった状態)。

FSDJump を拾い、設定でふるいに掛け、`tutorial-tracker-rs` ドライバへ訪問先を
publish し、10 秒ごとの定期実行で 1 件ずつ EDSM へ問い合わせる。

```
cargo build --release --target wasm32-wasip2
```

ドライバ(`examples/drivers/tutorial-tracker-rs`)と一緒に入れる:

```
./scripts/install-examples.sh tutorial-jump-log-rs tutorial-tracker-rs
```

配置後はデーモンの再起動と、GUI の Plugins ページでの承認(HTTP capability と
bus 接続)が要る。

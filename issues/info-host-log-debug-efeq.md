---
id: info-host-log-debug-efeq
title: デーモンのログレベルが INFO 固定で host-log の debug がどこにも出ない
summary: デーモンが LevelFilter::INFO を固定で掛けており RUST_LOG が効かないため、プラグインの host-log debug が stderr にも GUI にも出ない / 未着手
status: closed
labels: 
created: 2026-07-29T07:03:44Z
updated: 2026-07-29T07:53:14Z
---

## 現象

プラグイン・ドライバが `host-log` の `debug` レベルで出したログは、**どこにも
表示されない**。stderr にも、GUI の Logs 画面にも出ない。`RUST_LOG=debug` を
付けても変わらない。

`core/src/bin/edlr.rs`:

```rust
tracing_subscriber::registry()
    .with(tracing_subscriber::filter::LevelFilter::INFO)   // ← ここ
    .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
    .with(log_layer)
    .init();
```

レジストリ全体に `LevelFilter::INFO` を掛けているため、環境変数は参照されない。
ホスト側は `WitLevel::Debug => tracing::debug!(...)`(`core/src/plugin/host.rs`)
と素直にマップしているので、プラグインの debug ログはここで落ちる。

## なぜ困るか

プラグイン作者から見ると、**WIT に存在するログレベルの 1 つが黙って無効**に
なっている。「debug で出したのに出ない」は原因が見えず、プラグインが呼ばれて
いないのか、ログが捨てられているのかの区別が付かない。

実際にプラグイン作成チュートリアル(`docs/plugin-tutorial-{rust,tinygo}.md`)を
書く過程で踏んだ。サンプルを debug で書いたところ確認手順が一切成立せず、
読者が見るべきログを info へ書き換えて回避したうえで、両文書に「debug は
出ない」と明記することになった。回避策が「debug を使わない」しかない状態。

`examples/drivers/ed-state` など既存のサンプルにも、実際には表示されない
debug ログが残っている。

## 案

1. `EnvFilter` を使い、既定を INFO のまま `RUST_LOG` で上書きできるようにする
   (最小の変更。開発者は `RUST_LOG=debug` で見えるようになる)
2. デーモンに `--log-level` フラグを足す(GUI から起動される Tauri 経路でも
   指定できる形にするなら、設定側にも口が要る)
3. プラグインのログだけ別扱いにする — プラグインごとにログレベルを設定できると
   「1 つのプラグインだけ debug で追う」ができる。ただし設定の保存先
   (`settings-dir` か grants と同様か)を決める必要がある

GUI へ転送している `LogLayer`(`core/src/logs`)の閾値をどうするかも合わせて
決める。stderr は debug まで出すが GUI は INFO 以上、という分け方もありうる。

## 直すときに一緒に見るもの

- `docs/plugin-tutorial-rust.md` / `docs/plugin-tutorial-tinygo.md` の
  「`host-log` の Debug はどこにも出ない」の記述(2 章)と、
  `examples/plugins/tutorial-jump-log-{rs,go}` / `examples/drivers/tutorial-tracker-{rs,go}`
  の「debug ではなく info」というコメント
- `docs/plugins.md` にはログレベルの説明が無い。挙動を決めたらここに書く

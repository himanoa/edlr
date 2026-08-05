---
id: install-examples-sh-layout-kdl-layout-js-7jkk
title: install-examples.sh が layout.kdl / layout.json をインストール先へコピーしない
summary: install-examples.sh の install_component が manifest.toml/driver.toml と wasm しかコピーせず layout ファイルを配置しないため、examples 配下に layout.kdl を置いても UI に反映されない / 未着手
status: closed
labels: bug
created: 2026-08-01T05:56:41Z
updated: 2026-08-05T07:07:20Z
---

## どこで踏んだか

settings-layout プロジェクトの Task 10(`examples/plugins/tutorial-jump-log-rs/layout.kdl`
を追加するタスク)で、`scripts/install-examples.sh` を使ってサンプルを
配置しデーモンを起動し UI でセクション表示を確認しようとしたところ、
`install_component()`(`scripts/install-examples.sh:130-203`)が
コピーしているのは以下だけだと分かった:

- ビルド成果物(wasm) — `cp "$built" "$dest/$installed_as"`
- manifest.toml / driver.toml — `cp "$src/$descriptor" "$dest/$descriptor"`
- `ui/` ディレクトリ(ダッシュボードウィジェット用)

`layout.kdl` / `layout.json` をコピーする行が無い。そのため
`examples/plugins/tutorial-jump-log-rs/layout.kdl` を追加しても、
`./scripts/install-examples.sh tutorial-jump-log-rs` でインストールした
先のディレクトリには manifest.toml と plugin.wasm しか置かれず、
デーモンが読み込む layout は存在しないまま(`layout: null`)になる。

## なぜ困るか

- ドキュメント(`docs/plugins.md` の「設定画面のレイアウト」節)は
  `layout.kdl` を manifest.toml と同じディレクトリに置けば効くと説明しているが、
  サンプルの標準インストール手順ではその通りにならず、チュートリアルで
  layout の効果を確認しようとした人が「効いていない」と誤解する
- 今後 examples 配下の他プラグイン/ドライバに layout ファイルを足しても、
  同じ理由で無視され続ける

## 直し方の案

`install_component()` の manifest コピー箇所の直後に、`layout.kdl` /
`layout.json` が `$src` に存在すれば `$dest` へコピーする処理を足す
(存在しなければ何もしない、が基本方針)。`ui/` と同様に「存在すれば
コピー、無ければスキップ」でよく、`ui/` のような rm -rf する必要はない
(layout ファイルは単一ファイルで、名前を変えたときの残骸問題が起きにくい)。

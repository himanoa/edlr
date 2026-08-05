---
id: make-packaging-core-edlr-vlxe
title: make packaging の配布物に core の edlr バイナリが含まれない
summary: make packaging(cargo tauri build)は edlr-ui だけをバンドルするため、.deb/.rpm/.AppImage に core の edlr バイナリが入らない / 未着手
status: closed
labels: build
created: 2026-07-29T18:30:19Z
updated: 2026-08-05T07:19:28Z
---

## どこで踏んだか

`Makefile` の `packaging` ターゲット。`cd ui && cargo tauri build` は Tauri アプリ
(`edlr-ui`)だけをビルド・バンドルするので、生成される .deb / .rpm / .AppImage には
`core/src/bin/edlr.rs` の `edlr` バイナリが含まれない。

`make all` / `make install` に core が入っていなかった問題は Makefile 側で修正済み
(`-p edlr-core` と `cargo install --path core` を追加)だが、packaging は未対応。

## なぜ困るか

パッケージからインストールしたユーザーは GUI(edlr-ui)だけを得て、CLI の `edlr` が
手に入らない。ソースから `make install` した場合と配布物で内容がずれる。

## 直し方の案

1. Tauri の externalBin(sidecar)として `edlr` を bundle 設定に追加する。
   ターゲットトリプル付きのファイル名が必要なので、packaging ターゲットで
   `cargo build --release -p edlr-core` → リネームして配置する前処理を入れる。
2. .deb / .rpm については `bundle > linux` の files 設定で
   `target/release/edlr` を `/usr/bin/edlr` に入れる(AppImage は別途検討)。
3. あるいは配布物を分け、`edlr` 単体は `cargo install edlr-core` / 別アーカイブで
   配布する方針にして README に明記する。

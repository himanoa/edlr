---
id: cargo-tauri-build-beforebuildcommand-cwd-khuz
title: cargo tauri build の beforeBuildCommand が cwd 不一致で失敗する
summary: before フックの `pnpm --dir frontend` が cwd=ui/frontend で二重パスになり ENOENT / HookCommand オブジェクト形式(script+cwd)に変更して修正済み
status: closed
labels: build
created: 2026-07-29T18:11:35Z
updated: 2026-07-29T18:15:45Z
---

## どこで踏んだか

`ui/` で `cargo tauri build` を実行すると、beforeBuildCommand の段階で失敗する:

```
Running beforeBuildCommand `pnpm --dir frontend build`
ERROR  ENOENT: no such file or directory, lstat '.../edlr/ui/frontend/frontend'
```

tauri-cli (v2.11.4) は before フックを `ui/` ではなく `ui/frontend`(frontend
ルートとして検出したディレクトリ)を cwd にして実行するため、
`--dir frontend` が `ui/frontend/frontend` に解決されて ENOENT になる。

該当ファイル: `ui/src-tauri/tauri.conf.json` の `build.beforeBuildCommand`
(`beforeDevCommand` の `pnpm --dir frontend dev` も同じ形なので、同様に
壊れている可能性が高い)。

## なぜ困るか

`cargo tauri build` を素で叩くと必ず失敗する。ルートの Makefile では
`--config '{"build":{"beforeBuildCommand":""}}'` でフックを無効化して
フロントエンドビルドを Makefile 側で行う回避策を入れたが、conf 自体は
直っていないので、Makefile を経由しない人・CI は同じ罠を踏む。

## 直し方の案

- cwd が frontend ルートになる前提でコマンドを `pnpm build` / `pnpm dev` に直す
- もしくは cwd に依存しないよう `pnpm --dir <絶対 or conf 相対の正しいパス>` に統一する

どちらにするかは、普段 `tauri dev` をどこから起動しているかに合わせて決めるのがよさそう。

## 対応(2026-07-30)

`tauri.conf.json` の両フックを HookCommand オブジェクト形式に変更して修正:

```json
"beforeBuildCommand": { "script": "pnpm build", "cwd": "../frontend" }
```

(`beforeDevCommand` も同様)。`cwd` は tauri.conf.json のディレクトリ基準で
解決されることを、`ui/` と `ui/src-tauri/` の両方から `cargo tauri build
--no-bundle` を実行して確認済み。どこから起動しても `ui/frontend` で
`pnpm build` が走る。

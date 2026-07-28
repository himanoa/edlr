# edlr 開発メモ

## 並列ビルドのロック競合を避ける

サブエージェントを並列に走らせて cargo を同時実行すると、2種類のファイルロックで
待ちが発生する(過去のセッションで `Blocking waiting for file lock` が800回以上
記録された)。以下を守ること。

1. **エージェントを並列起動する前に、必ず一度 `cargo fetch` を実行する。**
   package cache(`~/.cargo`)のロックは依存のダウンロード時だけ排他になる。
   先にキャッシュを温めておけば、並列ビルドは共有ロックだけで通る。
   worktree を新規作成した直後も同様(Cargo.lock が同じなら fetch 済みのまま)。

2. **同一 worktree 内で cargo コマンドを並走させない。** target/ のロックで
   直列化されるだけで速くならない。どうしても並走が必要な場合は、エージェント
   ごとに `CARGO_TARGET_DIR` を分ける(sccache が入っているのでコンパイル
   キャッシュは target を分けても共有される)。

3. sccache は `~/.cargo/config.toml` でグローバルに有効
   (`rustc-wrapper` + `incremental = false`)。sccache はインクリメンタル
   ビルドをキャッシュできないため、incremental を戻さないこと。

## Issue管理

Issue管理はGitHubではなく git issues のスキルを使ってIssueの作成と閲覧、検索を行うこと

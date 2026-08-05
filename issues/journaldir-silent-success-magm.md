---
id: journaldir-silent-success-magm
title: journalDir クリア後、フォールバックにより問題が silent success に見える
summary: Settings で journalDir をクリアすると、自動検出が外れる環境ではデーモンが空のフォールバックを監視して成功に見える / UI に実効ディレクトリの表示が無い / 未着手
status: open
labels: ui
created: 2026-08-01T08:51:20Z
updated: 2026-08-01T08:51:52Z
---

## どこで踏むか

journal-dir-fallback ブランチ(デーモンが journal dir 未解決時に
`$XDG_DATA_HOME/edlr/journal` を作成して起動するようになった変更)の
最終レビューで指摘された UX ギャップ。

再現手順(Proton 既定パスに journal が無い環境、例: Steam のセカンダリ
ライブラリや macOS):

1. Settings で journalDir を設定して使っている状態から「自動検出に戻す」
   (クリア)を実行する
2. デーモンが再起動し、自動検出に失敗するが、フォールバックディレクトリを
   作成して正常に listen する
3. `wait_until_listening` が成功し、`daemon_error_after_restart(true)` が
   エラーをクリアする(`ui/src-tauri/src/main.rs` の restart 経路)

## なぜ困るか

フォールバック導入前は、この操作をしたユーザーには「ディレクトリを指定して
ください」という明示的なエラーが出ていた。導入後は一見成功するが、デーモンが
監視しているのは空の `~/.local/share/edlr/journal` で、ゲームの journal は
永遠に流れてこない。`snapshot` の `journal_dir` は null のままで、UI には
実効(フォールバック)ディレクトリを示す表示が無いため、ユーザーは異常に
気づけない。

## 直し方の案

- デーモンがフォールバックで起動したことを snapshot(または RPC)に載せ、
  Settings / Dashboard に「フォールバックディレクトリ X を監視中」と表示する
- あわせて、クリア操作の完了メッセージで「自動検出に失敗した場合は
  フォールバックを監視する」旨を伝える

いずれもデーモン→UI の情報伝搬が必要で、フォールバック導入本体とは独立した
機能追加のため別タスクとした。

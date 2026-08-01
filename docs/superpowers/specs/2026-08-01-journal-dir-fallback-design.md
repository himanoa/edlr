# journal dir フォールバック自動作成 — 設計

日付: 2026-08-01
状態: 承認済み

## 背景 / 問題

デーモン(`core/src/bin/edlr.rs`)は journal ディレクトリを
CLI 引数 → config.json の `journalDir` → Proton 既定パスの自動検出
の順で解決し、どれでも解決できないと `exit(1)` する。

Proton 既定パスは Linux + Steam Proton 環境にしか存在しないため、
macOS などの新規環境では config.json を書くまでデーモンが一切起動できず、
初回の UI 起動が「デーモン不在」で始まってしまう。

## 方針

解決順の最終段に「既定フォールバックパスを作成して使う」を追加する。

```
CLI 引数 → config.json → Proton 自動検出 → フォールバック(無ければ作成)
```

これにより journal dir 未解決による `exit(1)` は起きなくなる。
UI 側の変更はなし(初回起動時、UI が spawn したデーモンがフォールバックで
起動するようになる)。

フォールバックパス: `$XDG_DATA_HOME/edlr/journal`、
`$XDG_DATA_HOME` が未設定/空なら `~/.local/share/edlr/journal`。

## 変更内容

### 1. `config/src/lib.rs`(純粋)

`fallback_journal_dir(xdg_data_home: Option<&Path>, home: Option<&Path>) -> Option<PathBuf>`
を追加する。パス計算のみで mkdir はしない(純粋モジュールの作法)。
`config_base` / `state_base` と同じ流儀:

- `xdg_data_home` が Some かつ非空 → `<xdg_data_home>/edlr/journal`
- それ以外で `home` が Some → `<home>/.local/share/edlr/journal`
- 両方 None → None(フォールバック不能)

### 2. `core/src/bin/edlr.rs`(命令的)

`daemon_journal_dir` が `None` を返した場合:

1. `fallback_journal_dir(XDG_DATA_HOME, HOME)` でパスを求める
2. それも `None`(HOME すら無い)なら従来どおりエラーを出して `exit(1)`
3. `fs::create_dir_all` で作成(既存なら no-op)し、
   作成して使うことを `tracing::info!` でログに出す
4. mkdir 失敗時はエラーメッセージを出して `exit(1)`

### 変えないこと

- `daemon_journal_dir` のシグネチャ・挙動(既存テスト不変)
- 「CLI/config で明示されたパスが存在しなければ `exit(1)`」の検証。
  明示設定されたパスの不存在は設定ミスであり、勝手に作らない。
  自動作成の対象はフォールバックパスのみ。
- UI(Tauri シェル・フロントエンド)。config.json の `journalDir` は
  null のままなので Settings タブへの初期誘導は従来どおり残るが、
  デーモンは起動済みなので他の画面も機能する。

## エラーハンドリング

- フォールバックパスの mkdir 失敗(権限など): エラー出力 + `exit(1)`。
  従来の「未解決で exit(1)」と同等の終わり方だが、メッセージは
  作成しようとしたパスと失敗理由を含める。
- HOME も XDG_DATA_HOME も無い環境: 従来どおりのエラーで `exit(1)`。

## テスト

- `config` crate: `fallback_journal_dir` の純粋テスト
  (XDG 優先 / home フォールバック / 空 XDG の無視 / 両方 None)。
- `core` 統合テスト: journal dir を一切与えず HOME を一時ディレクトリに
  向けてデーモンを起動し、フォールバックディレクトリが作成されて
  起動に成功することを確認(`daemon_config_journal_integration.rs` の流儀)。

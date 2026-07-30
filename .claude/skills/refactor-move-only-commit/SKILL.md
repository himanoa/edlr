---
name: refactor-move-only-commit
description: core リファクタリング(docs/superpowers/plans/ の core-refactor 系 plan)の作業中、コードをモジュール間で移動するコミットを作るとき・フェーズを完了するときに必ず使う。move-only コミットの検証、テスト凍結チェック、フェーズゲートの手順。
---

# refactor-move-only-commit

core リファクタリングの移行規律(挙動不変の担保)をコミット単位で守るための手順。

## 原則

- **1コミット = 移動のみ or ロジック変更のみ**。混ぜない
- **テストは凍結**。`core/tests/` と `#[cfg(test)]` ブロックに触ってよいのは
  import 行(use 文・パス)の機械的追従だけ
- 旧パスは `pub use` で温存する(削除は Phase 6 で一括)

## move-only コミットの検証手順

コミット前に、ステージした diff がほぼ全行 moved であることを確認する:

```bash
git diff --cached --color-moved=dimmed-zebra --color=always | less -R
```

moved 行は薄い色(dimmed)で表示される。濃い色の +/- 行が残っていたら
それは移動ではない変更なので、move-only コミットから外す。

非対話環境では、追加行と削除行の多重集合が一致するかで機械的に確認できる:

```bash
diff <(git diff --cached | grep '^+' | grep -v '^+++' | sed 's/^+//' | sort) \
     <(git diff --cached | grep '^-' | grep -v '^---' | sed 's/^-//' | sort)
```

- 出力が空 → 純粋な移動(mod 宣言・use 文の差分だけが出るのは許容。目で確認)
- 差分が出る → その行はロジック変更。別コミットに分離する

## テスト凍結チェック

move-only・logic どちらのコミットでも、コミット前に実行:

```bash
git diff --cached -- core/tests/
git diff --cached -- 'core/src/**' | grep -A3 -B3 'cfg(test)'
```

- `core/tests/` の diff が空、または use 文・パス置換のみであること
- アサーション・テストデータ・テスト名に触る差分が出たら**挙動変更の兆候**。
  コミットせず原因を調べて戻す

## フェーズゲート(コミット単位でも全パス)

```bash
cargo test --workspace
cargo clippy --workspace
```

フェーズ途中のコミットでも両方全パスさせる。落ちた状態でのコミットは不可。

## コミットメッセージ

移動コミットは `refactor(core): xxx を yyy/ へ移動(move-only)` のように
move-only であることを明記する。レビュー時に `--color-moved` で見る目印になる。

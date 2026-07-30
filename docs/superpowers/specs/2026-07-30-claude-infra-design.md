# .claude 設備(rules / agents / CLAUDE.md)設計

日付: 2026-07-30
状態: レビュー待ち

## 目的

core リファクタリング設計([2026-07-30-core-refactoring-design.md](2026-07-30-core-refactoring-design.md))
で定めた**恒久規約**を、リファクタリング完了後も守り続けられるように
`.claude/` 配下へ永続化する。参考元は kanamone/Cannelloni の `.claude` 構成
(rules は1トピック1ファイル + paths フロントマター、agents は領域別サブエージェント定義)。

## スコープ

- **作るもの**: `.claude/rules/`(5ファイル)、`.claude/agents/`(2ファイル)、
  `CLAUDE.md` への追記
- **作らないもの**:
  - hooks(hookify 相当の自動警告)— 今回は見送り
  - 移行作業中だけの規律(move-only コミット、旧パス pub use 温存、テスト凍結)の
    rules 化 — これらは plan 文書(docs/superpowers/plans/)に既載で、
    リファクタリング完了後は不要になるため rules には入れない
- **適用範囲**: rules の paths は core/ に限定する。ただし drivers/ など core 外の
  **新規コード**にも同じ作法を推奨する旨を module-layout.md に一文添える。
  既存 drivers の大ファイルには適用しない(別タスクでリファクタリングするときに拡張)

## 1. `.claude/rules/`(恒久規約のみ、5ファイル)

書式は Cannelloni 流に揃える:

- 冒頭に paths フロントマター(基本は `core/**/*.rs`、testing.md のみテストパスも含む)
- 規約は「禁止 / 必須」を明示し、✅/❌ の Rust コード例を付ける
- 内容はリファクタリング設計 spec の恒久部分の抽出であり、新しい決定は含まない

### module-layout.md

- 機能名モジュール10個の一覧と役割
  - 純粋6: `manifest` / `capability` / `settings` / `schedule` / `rpc` / `journal`
  - 命令的4: `registry` / `runner` / `host` / `server`
- レイヤーディレクトリ(`domain/` `ports/` など)は作らない
- 新規モジュールを足すときの判断基準(機能名で切る・純粋/命令的のどちらかを決める)
- core 外(drivers/ 等)の新規コードにも同じ作法を推奨する旨

### pure-imperative-boundary.md

- 純粋モジュールは命令的モジュール・`std::fs`・ネットワーク・スレッド・`Mutex` を
  import しない(レビューで弾く)
- 依存方向は純粋→純粋のみ(例: `manifest → capability`)
- 副作用(ディスク・プロセス・スレッド・チャネル・wasm・ネットワーク)は
  命令的モジュールへ集める
- 違反を見つけたときの対処: 判断を純関数に抽出して境界の外へ移す

### trait-di.md

- trait は**使う機能のモジュールに置く**。中央 `ports/` ディレクトリは作らない
- 既存4本の一覧: `capability::GrantStorage` / `settings::Storage` /
  `registry::ProcessControl` / `registry::BusPort`
- trait は必要が実証されたときだけ増やす(推測で足さない)
- DI は generics(`struct GrantService<S: Storage>`)、公開面は type alias で隠す
- 時刻は trait にしない。純関数が `now` を引数で受ける(sans-IO 流)
- mockall 等のモックマクロは導入しない。モックは手書き

### procedure-style.md

- 原則は「判断と実行を分ける」の一点
- 手続き中の `if`/`match` の塊は名前のついた純関数(値イン値アウト)に抽出する
- 命令的関数は1関数1画面(〜40行)・ネスト2段まで。early return・ガード節で平らにする
- 読み→判断→書きの順に整える。ロック取得は読みの前・書きの前に整列させる
- 判断結果が複数あるときは小さな構造体で返す(`runner::LoopAction` のパターン)。
  綺麗になる場所でだけ使い、全操作に義務付けない

### testing.md

- 二層戦略:
  - 既存の統合テスト(実ディスク・実スレッド)= 挙動の錨。消さない
  - 新規ロジックは純関数化し、モック or 値の等値比較の純粋テストで書く
- モックは各モジュールの `#[cfg(test)] mod test_support` に手書き
- wasmtime の `Store` などモックしても意味のない部分は具象のまま

## 2. `.claude/agents/`(2体)

Cannelloni の agent 書式を踏襲: frontmatter に `name` / `description`
(`<example>` ブロック付き)/ `tools`、本文に担当範囲と作法。

### pure-module-developer.md

- 担当: `manifest` / `capability` / `settings` / `schedule` / `rpc` / `journal`
- 作法: 値イン値アウトで書く。I/O が必要になったら実装を止めて境界設計を見直す。
  テストファースト(純粋テストが仕様書)
- 必読 rules を列挙(boundary / trait-di / testing)

### imperative-module-developer.md

- 担当: `registry` / `runner` / `host` / `server`
- 作法: procedure-style の遵守。手続き中に判断が膨らんだら純粋モジュール側へ抽出。
  ロック順序・cleanup(shutdown 系)への注意
- 必読 rules を列挙(boundary / procedure-style / testing)

## 3. `CLAUDE.md` への追記

既存の2セクション(並列ビルドのロック競合、Issue 管理)は**変更しない**。
先頭に短いセクションを追加する:

- リポジトリ構成の1段マップ(core / drivers / ui / config と core 内10モジュール一覧)
- 「コーディング規約は `.claude/rules/` を参照。core を触るときは必読」の導線
- レビューで繰り返し指摘された内容は rules に永続化する運用(Cannelloni から輸入)

## 注意点

- rules の内容はリファクタリング**完了後**の姿を書く。現状の core はまだ旧構成
  (`plugin/registry.rs` 等)なので、リファクタリング実施中は plan 文書の移行規律が
  優先される。rules 冒頭にその旨の注記を1行入れる
- `.claude/settings.local.json` や worktrees など既存の `.claude` 配下には触れない

## 検討して採用しなかった案

- **rules を1枚に集約**: paths での出し分けができず、常に全文がコンテキストに載る
- **作法別2枚(pure/imperative)**: trait DI やテストのような横断トピックの置き場に困る
- **サブシステム別 agents(10体前後)**: 規約が作法単位なのでエージェントも同じ境界で
  2体に。保守コストを優先
- **hooks(import 違反の自動警告)**: 今回は見送り。運用してみて手動レビューで
  漏れるようなら追加を検討

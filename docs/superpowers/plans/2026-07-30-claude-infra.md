# .claude 設備(rules / agents / CLAUDE.md)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** core リファクタリング設計の恒久規約を `.claude/rules/`(5ファイル)+ `.claude/agents/`(2体)+ `CLAUDE.md` 追記として永続化する。

**Architecture:** すべて Markdown ドキュメント。rules は Cannelloni 流(1トピック1ファイル、paths フロントマターで `core/**/*.rs` に限定、✅/❌ の Rust 例付き)。agents は frontmatter(name/description/example/tools)+ 本文。コードは書かないのでテストは「内容が spec と一致しているかの目視検証」で代替する。

**Tech Stack:** Markdown のみ。spec は `docs/superpowers/specs/2026-07-30-claude-infra-design.md`。

## Global Constraints

- rules は**恒久規約のみ**。移行規律(move-only コミット・旧パス pub use 温存・テスト凍結)は書かない
- rules の paths は `core/**/*.rs` に限定(testing.md は同じ。core のテストも .rs 内 `#[cfg(test)]` と `core/tests/` のため)
- rules の内容はリファクタリング**完了後**の姿。新しい決定を発明しない(spec と core-refactoring-design から抽出のみ)
- 純粋モジュール6: `manifest` / `capability` / `settings` / `schedule` / `rpc` / `journal`。命令的モジュール4: `registry` / `runner` / `host` / `server`
- trait 4本の正式名: `capability::GrantStorage` / `settings::Storage` / `registry::ProcessControl` / `registry::BusPort`
- CLAUDE.md の既存2セクション(並列ビルドのロック競合、Issue 管理)は変更しない
- `.claude/settings.local.json` / `.claude/worktrees/` には触れない
- 各タスク完了ごとにコミット(日本語 conventional commit、既存履歴の流儀: `docs: ...`)

---

### Task 1: rules — module-layout.md と pure-imperative-boundary.md

**Files:**
- Create: `.claude/rules/module-layout.md`
- Create: `.claude/rules/pure-imperative-boundary.md`

**Interfaces:**
- Produces: `.claude/rules/` ディレクトリ。後続タスクの rules が同じ書式(paths フロントマター + ✅/❌ 例)で並ぶ

- [ ] **Step 1: module-layout.md を作成**

以下の内容で作成する:

````markdown
---
paths:
  - "core/**/*.rs"
---

# モジュール構成と依存方向

> 注: core リファクタリング実施中は docs/superpowers/plans/ の移行規律
> (move-only コミット・旧パス pub use 温存・テスト凍結)が本 rules に優先する。

core は**機能名モジュール**で構成する。レイヤーディレクトリ
(`domain/`、`ports/`、`infra/` など)は作らない。

## モジュール一覧

| モジュール | 種別 | 責務 |
|---|---|---|
| `manifest/` | 純粋 | TOML → Manifest のパースと全体整合の検証(I/O は `load_manifest` だけ端に) |
| `capability/` | 純粋 | capability の要求と承認(Request 型・fingerprint・GrantState + Storage trait) |
| `settings/` | 純粋 | 設定の検証・マージ + Storage trait |
| `schedule/` | 純粋 | 発火計算 + 永続化 |
| `rpc/` | 純粋 | RPC 解釈・JSON 整形(純粋関数群) |
| `journal/` | 純粋 | discovery/parser/position/tailer |
| `registry/` | 命令的 | プラグイン・ドライバの facade と各サービス |
| `runner/` | 命令的 | プラグインスレッドとイベントループ |
| `host/` | 命令的 | wasmtime 配線 |
| `server/` | 命令的 | axum/WS。rpc/ を呼ぶだけの薄い層 |

## 新規モジュールを足すとき

1. **機能名**で切る(`grants/` ではなく `capability/grants.rs` のように、1概念は1モジュールにまとめる)
2. 純粋か命令的かを最初に決める。迷ったら「ディスク・プロセス・スレッド・チャネル・wasm・ネットワークを触るか」で判定する
3. 純粋モジュールにできないか先に検討する(判断を値イン値アウトに切り出せば大半は純粋にできる)

## core 外の新規コード

drivers/ など core 外でも、**新規に書くコード**には同じ作法
(機能名モジュール・判断と実行の分離)を推奨する。既存の大ファイルへの
遡及適用は別タスクで行う。
````

- [ ] **Step 2: pure-imperative-boundary.md を作成**

以下の内容で作成する:

````markdown
---
paths:
  - "core/**/*.rs"
---

# 純粋 / 命令的モジュールの境界

## 禁止

純粋モジュール(`manifest` `capability` `settings` `schedule` `rpc` `journal`)から:

- 命令的モジュール(`registry` `runner` `host` `server`)の import
- `std::fs` / `std::net` / `std::thread` / `std::process` の使用
- `Mutex` / チャネル / スレッド生成

これらが見えたらレビューで弾く。

```rust
// ❌ 純粋モジュール(rpc/)から命令的モジュールを import
use crate::registry::Registry;

pub fn render_status(registry: &Registry) -> serde_json::Value { /* ... */ }

// ✅ 値を受け取り値を返す。registry から値を取り出すのは呼び出し側(server/)の仕事
pub fn render_status(plugins: &[PluginStatus]) -> serde_json::Value { /* ... */ }
```

## 依存方向

- 純粋 → 純粋のみ許可(例: `manifest → capability`)
- 命令的 → 純粋は自由
- 純粋 → 命令的は禁止

## 副作用の置き場

ディスク永続化・Mutex・プロセス起動停止・スレッド・チャネル・wasm 呼び出し・
ネットワークは命令的モジュールへ集める。時間がかかる・失敗しうる・順序が
意味を持つ操作はすべてここ。

## 違反を見つけたら

純粋モジュール内に副作用が必要になったら、実装を止めて境界を見直す:

1. 判断部分を純関数(値イン値アウト)に抽出する
2. 副作用は trait(`capability::GrantStorage` など)越しにするか、命令的モジュール側に移す
````

- [ ] **Step 3: 内容を spec と突き合わせて検証**

`docs/superpowers/specs/2026-07-30-claude-infra-design.md` の「module-layout.md」
「pure-imperative-boundary.md」セクションの項目がすべて含まれているか確認。
モジュール名10個・trait 名が Global Constraints の正式名と一致するか確認。

- [ ] **Step 4: Commit**

```bash
git add .claude/rules/module-layout.md .claude/rules/pure-imperative-boundary.md
git commit -m "docs(claude): モジュール構成と純粋/命令的境界の rules を追加"
```

---

### Task 2: rules — trait-di.md と procedure-style.md

**Files:**
- Create: `.claude/rules/trait-di.md`
- Create: `.claude/rules/procedure-style.md`

**Interfaces:**
- Consumes: Task 1 の書式(paths フロントマター + ✅/❌ 例)
- Produces: trait 4本の一覧(agents 本文から参照される)

- [ ] **Step 1: trait-di.md を作成**

以下の内容で作成する:

````markdown
---
paths:
  - "core/**/*.rs"
---

# trait DI

## trait の置き場

trait は**使う機能のモジュールに置く**。中央の `ports/` ディレクトリは作らない。

既存の境界は4本:

| trait | 置き場所 | 実装者 |
|---|---|---|
| `capability::GrantStorage` | capability/ | `GrantsStore` |
| `settings::Storage` | settings/ | `SettingsStore` |
| `registry::ProcessControl` | registry/ | `ProcessDriver` |
| `registry::BusPort` | registry/ | `edlr_driver_channel::Bus` |

## trait を増やすとき

**必要が実証されたときだけ**増やす。「モックしたいテストが実在する」が実証。
推測で境界を切らない。wasmtime の `Store` などモックしても意味のない部分は
具象のまま使う。

## DI の形

generics で受け、公開面は type alias でジェネリクスを隠す:

```rust
// ✅ 内部は generics
pub struct GrantService<S: GrantStorage> { storage: S, /* ... */ }

// ✅ 公開面は alias で具象を固定
pub type DiskGrantService = GrantService<GrantsStore>;
```

```rust
// ❌ dyn Trait を標準にしない(必要が実証された場所を除く)
pub struct GrantService { storage: Box<dyn GrantStorage>, }
```

## 時刻は trait にしない

純関数が `now` を引数で受ける(sans-IO 流。quinn-proto / str0m と同じ):

```rust
// ✅ now を引数で渡す。テストは任意の時刻を渡すだけ
pub fn next_fire(schedule: &Schedule, now: DateTime<Utc>) -> Option<DateTime<Utc>> { /* ... */ }

// ❌ Clock trait や内部での Utc::now() 呼び出し
pub fn next_fire(schedule: &Schedule, clock: &dyn Clock) -> Option<DateTime<Utc>> { /* ... */ }
```

## モックは手書き

mockall 等のモックマクロ crate は導入しない。モックは各モジュールの
`#[cfg(test)] mod test_support` に手書きする(→ testing.md)。
````

- [ ] **Step 2: procedure-style.md を作成**

以下の内容で作成する:

````markdown
---
paths:
  - "core/**/*.rs"
---

# 手続きを綺麗にする作法

原則は一点: **判断と実行を分ける**。手段は普通の関数抽出で、
新しい機構(Effect enum・interpreter)は導入しない。

## 判断は純関数に抽出する

手続き中の `if`/`match` の塊は、名前のついた純関数(値イン値アウト)に切り出す。
抽出した関数がそのまま純粋テストの対象になる。

```rust
// ❌ 実行の中に判断が埋まっている
fn restart(&mut self, id: &str) -> Result<(), Error> {
    if let Some(inst) = self.instances.get(id) {
        if inst.state == State::Running && !inst.stopping {
            self.stop_process(id)?;
            // ...
        }
    }
    // ...
}

// ✅ 判断を純関数へ。実行側は結果に従うだけ
fn should_restart(inst: Option<&Instance>) -> RestartAction { /* 純関数 */ }

fn restart(&mut self, id: &str) -> Result<(), Error> {
    match should_restart(self.instances.get(id)) {
        RestartAction::StopThenStart => { self.stop_process(id)?; /* ... */ }
        RestartAction::StartOnly => { /* ... */ }
        RestartAction::Noop => Ok(()),
    }
}
```

## 命令的関数は短く・直線的に

- 目安1関数1画面(〜40行)、ネスト2段まで
- 深くなったら early return・ガード節・関数抽出で平らにする

## 読み → 判断 → 書き の順に整える

読み書きが交互に出てくる手続きは、先に読む・真ん中で判断・最後に書く順に
並べ替える。ロック取得も「読みの前・書きの前」に自然と整列する。

## 判断結果が複数あるときは小さな構造体で返す

`runner::LoopAction` が既にやっている形。**そうすると綺麗になる場所でだけ**使い、
全操作に義務付けない。
````

- [ ] **Step 3: 内容を spec と突き合わせて検証**

trait 4本の名前・実装者名が spec と一致するか、「now 引数」「mockall 禁止」
「40行・ネスト2段」「LoopAction」が漏れていないか確認。

- [ ] **Step 4: Commit**

```bash
git add .claude/rules/trait-di.md .claude/rules/procedure-style.md
git commit -m "docs(claude): trait DI と手続きの作法の rules を追加"
```

---

### Task 3: rules — testing.md

**Files:**
- Create: `.claude/rules/testing.md`

**Interfaces:**
- Consumes: trait-di.md の「モックは手書き」(相互参照)

- [ ] **Step 1: testing.md を作成**

以下の内容で作成する:

````markdown
---
paths:
  - "core/**/*.rs"
---

# テスト戦略(二層)

## 二層の役割分担

| 層 | 何を使う | 役割 |
|---|---|---|
| 統合テスト(既存) | 実ディスク・実スレッド | **挙動の錨**。消さない・書き換えない |
| 純粋テスト(新規) | モック or 値の等値比較 | 分解した単位の**仕様書** |

役割が違うので、純粋テストを足しても既存の統合テストは消さない。

## 新規ロジックの書き方

1. まず判断を純関数に抽出する(→ procedure-style.md)
2. その純関数に対して値イン値アウトのテストを書く。時刻が絡むなら `now` を引数で渡す
3. 永続化が絡むなら trait(`GrantStorage` / `Storage` など)のモック越しにテストする

## モックは test_support に手書き

モックは各モジュールの `#[cfg(test)] mod test_support` に手書きする。
mockall 等のマクロ crate は導入しない。

```rust
// ✅ capability/grants.rs 内
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashMap;

    pub struct InMemoryGrantStorage {
        pub grants: HashMap<String, GrantState>,
    }

    impl GrantStorage for InMemoryGrantStorage { /* ... */ }
}
```

## モックしないもの

wasmtime の `Store` などモックしても意味のない部分は具象のまま。
無理に trait 化しない(→ trait-di.md)。
````

- [ ] **Step 2: 内容を spec と突き合わせて検証**

「二層」「錨」「test_support 手書き」「wasmtime Store は具象」が含まれるか確認。

- [ ] **Step 3: Commit**

```bash
git add .claude/rules/testing.md
git commit -m "docs(claude): 二層テスト戦略の rules を追加"
```

---

### Task 4: agents — pure-module-developer と imperative-module-developer

**Files:**
- Create: `.claude/agents/pure-module-developer.md`
- Create: `.claude/agents/imperative-module-developer.md`

**Interfaces:**
- Consumes: Task 1〜3 の rules ファイル名(本文の必読リストで参照)

- [ ] **Step 1: pure-module-developer.md を作成**

以下の内容で作成する:

````markdown
---
name: pure-module-developer
description: |
  core の純粋モジュール(manifest / capability / settings / schedule / rpc / journal)の実装・変更を行うときに使う。値イン値アウトの関数設計と純粋テストが守備範囲。Examples:

  <example>
  Context: capability の検証ロジックを追加したい
  user: "capability の fingerprint 検証に新しいルールを足して"
  assistant: "pure-module-developer エージェントで実装します。"
  <commentary>
  capability/ は純粋モジュールなので pure-module-developer の担当。
  </commentary>
  </example>

  <example>
  Context: RPC 応答の JSON 整形を変えたい
  user: "plugin.list の応答に schedule 情報を足して"
  assistant: "rpc/ の整形は純粋関数群なので pure-module-developer で進めます。"
  <commentary>
  rpc/ の JSON 整形は値イン値アウト。
  </commentary>
  </example>
tools: [Read, Write, Edit, Glob, Grep, Bash]
---

# Pure Module Developer

core の純粋モジュール(`manifest` `capability` `settings` `schedule` `rpc` `journal`)
専任の実装者。

## 作法

- **値イン値アウト**で書く。入力は値、出力は値。時刻は `now` を引数で受ける
- **I/O が必要になったら手を止める**。純粋モジュールに `std::fs` やスレッドを
  持ち込まず、境界設計(trait 越しにするか、命令的モジュール側に置くか)を
  見直してから進む
- **テストファースト**。純粋テストがそのまま仕様書になる。モックは
  `#[cfg(test)] mod test_support` に手書き

## 必読 rules

- `.claude/rules/pure-imperative-boundary.md`
- `.claude/rules/trait-di.md`
- `.claude/rules/testing.md`
````

- [ ] **Step 2: imperative-module-developer.md を作成**

以下の内容で作成する:

````markdown
---
name: imperative-module-developer
description: |
  core の命令的モジュール(registry / runner / host / server)の実装・変更を行うときに使う。副作用の実行(プロセス・スレッド・ディスク・wasm・ネットワーク)と手続きの整理が守備範囲。Examples:

  <example>
  Context: sidecar の再起動処理を変えたい
  user: "sidecar の restart にタイムアウトを足して"
  assistant: "imperative-module-developer エージェントで実装します。"
  <commentary>
  registry/ のプロセス制御は命令的モジュールの担当。
  </commentary>
  </example>

  <example>
  Context: WS ハンドラを追加したい
  user: "server に新しい WS メッセージのハンドラを足して"
  assistant: "server/ の配線は imperative-module-developer で進めます。"
  <commentary>
  server/ は WS/HTTP 配線を担う命令的モジュール。
  </commentary>
  </example>
tools: [Read, Write, Edit, Glob, Grep, Bash]
---

# Imperative Module Developer

core の命令的モジュール(`registry` `runner` `host` `server`)専任の実装者。

## 作法

- **procedure-style を遵守**: 1関数1画面(〜40行)・ネスト2段まで・
  読み→判断→書きの順。命令的関数には短い実行手順の羅列だけを残す
- **判断が膨らんだら抽出する**。手続き中の `if`/`match` の塊は純関数に切り出し、
  可能なら純粋モジュール側へ移す
- **ロックと cleanup に注意**: ロック取得は読みの前・書きの前に整列させる。
  shutdown 系は途中失敗しても資源が残らないか常に確認する
- server/ は rpc/ を呼ぶだけの薄い層に保つ。JSON 整形を server に書かない

## 必読 rules

- `.claude/rules/pure-imperative-boundary.md`
- `.claude/rules/procedure-style.md`
- `.claude/rules/testing.md`
````

- [ ] **Step 3: 検証**

frontmatter の name がファイル名と一致するか、description の example が
純粋/命令的の境界を正しく反映しているか、必読 rules のファイル名が
Task 1〜3 で作った実ファイル名と一致するか確認。

- [ ] **Step 4: Commit**

```bash
git add .claude/agents/pure-module-developer.md .claude/agents/imperative-module-developer.md
git commit -m "docs(claude): 作法別サブエージェント2体を追加"
```

---

### Task 5: CLAUDE.md 追記

**Files:**
- Modify: `CLAUDE.md`(先頭、`# edlr 開発メモ` 見出しの直後に挿入。既存2セクションは変更しない)

**Interfaces:**
- Consumes: Task 1〜4 の全ファイル(導線として参照)

- [ ] **Step 1: CLAUDE.md の先頭にセクションを挿入**

`# edlr 開発メモ` の直後・`## 並列ビルドのロック競合を避ける` の前に以下を挿入:

````markdown
## リポジトリ構成とコーディング規約

- `core/` — デーモン本体。機能名モジュール構成:
  純粋(`manifest` `capability` `settings` `schedule` `rpc` `journal`)+
  命令的(`registry` `runner` `host` `server`)
- `drivers/` — ドライバ群 / `ui/` — Tauri GUI / `config/` — 設定

**core を触るときは `.claude/rules/` を必読**(モジュール構成・純粋/命令的境界・
trait DI・手続きの作法・テスト戦略)。core 外の新規コードにも同じ作法を推奨。
実装は `.claude/agents/` の pure-module-developer / imperative-module-developer に
任せられる。

レビューで繰り返し指摘された内容は、その場限りにせず `.claude/rules/` に
ファイルを足して永続化すること。
````

- [ ] **Step 2: 検証**

既存の「並列ビルドのロック競合を避ける」「Issue 管理」セクションに diff が
ないこと(`git diff CLAUDE.md` で挿入行のみであること)を確認。

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md に構成マップと rules への導線を追記"
```

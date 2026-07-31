# core リファクタリング Phase 6(仕上げ: 解体・旧パス削除・journal 作法揃え)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 挙動を一切変えずに、plugin/・driver/ ディレクトリを解体して残存 10 ファイルを機能名モジュール(`manifest/` `runtime/`(新設)`settings/` `host/` `registry/`)へ移設し、Phase 1–5 で温存してきた旧パス pub use を一括削除して全 use 文を新パスへ置換、journal/tailer の作法揃えとモジュールドキュメント整備で core リファクタリングを完了する。

**Architecture:** 設計根拠は **必読の事前分析** `docs/superpowers/specs/2026-07-31-phase6-finish-analysis.md`(移設先対応表 §1・旧パス棚卸し §2・リスク台帳 §4。以下「分析」)。spec は `docs/superpowers/specs/2026-07-30-core-refactoring-design.md` Phase 6 + ユーザー判断で解体を追加。移動規律は `.claude/skills/refactor-move-only-commit`。

**Tech Stack:** Rust (cargo workspace)。新規依存 crate の追加は禁止。

## Global Constraints

- **挙動不変**: 全タスクを通じて RPC 応答・エラー文字列・副作用順序を変えない。移設は git mv + 機械的 use 追従のみ
- **テスト凍結**: 既存テストは丸ごと移動 or import 追従のみ。**Task 5(一括置換)だけが core/tests/ の use 行書換を許される**(spec の「そのコミットは use 文置換のみ」)。それ以外のタスクは従来どおり凍結
- **旧パス温存は Task 5 まで**: Task 1–4 の移設は旧パス pub use を張って利用側 diff ゼロを保つ。Task 5 で一括削除
- **同綴りの新パスを巻き込まない**(分析 §4 リスク1): `registry::plugin` / `registry::driver` / `runner::plugin` / `runner::driver` / `host::plugin` / `host::driver` は既に新パス。置換対象は `crate::plugin::` / `crate::driver::` / `edlr_core::plugin::` / `edlr_core::driver::` プレフィックスのみ
- **純粋/命令的の境界**(分析 §4 リスク2/3): `runtime/` は純粋(Atomic は可、Mutex/スレッド/ディスク不可)。`rpc/` からの import が純粋→純粋のままであること
- **ゲート**: タスクごとに `cargo test --workspace` 全パス + `cargo clippy --workspace` 警告0
- **並列実行の注意**(CLAUDE.md): サブエージェント並列起動前に `cargo fetch`。cargo 並走禁止。既知 flaky(issue oxa3 / aenf)は単体再実行で確認

---

### Task 1: journal/tailer の作法揃え(move → logic の2コミット)

**Files:**
- Move: `core/src/journal/tailer.rs` → `core/src/journal/tailer/mod.rs` + `core/src/journal/tailer/tests.rs`
- Modify: `core/src/journal/tailer/mod.rs`(判定抽出)

**Interfaces:**
- Consumes: なし
- Produces(分析 §3):

```rust
/// 現ファイル消失時のローテーション先。「次」が無ければ、消えたファイルより
/// 厳密に新しい latest だけ採る(巻き戻すと全再配信になる — 現 poll 78–90 の判定)。
fn rotation_fallback(current: &Path, next: Option<PathBuf>, latest: Option<PathBuf>) -> Option<PathBuf>;

/// バッファから完全な行を切り出す(現 read_new 187–196 の判定)。
/// 戻りは (切り出した行, 未完の残り)。空行は捨て、replay は !caught_up。
fn split_complete_lines(buf: String, caught_up: bool) -> (Vec<JournalLine>, String);
```

- [ ] **Step 1(move-only コミット)**: `git mv` で `journal/tailer/mod.rs` 化し、`#[cfg(test)] mod tests`(202–605行)を丸ごと `journal/tailer/tests.rs` へ分離(`#[cfg(test)] mod tests;` 宣言 + `use super::*` 追従のみ。manifest/ の前例と同じ形)。コミット: `refactor(core): journal/tailer のテストを tests.rs へ分離(移動のみ)`
- [ ] **Step 2(logic コミット)**: 上記2つの判定を純関数に抽出し、`poll`/`read_new` は結果に従うだけにする。I/O・`self.pos`/`self.partial` 更新の順序は不変。tests.rs に純粋テストを追加(rotation_fallback 3本: 次あり/次なし+新しい latest/次なし+古い latest、split_complete_lines 3本: 完全行のみ/未完残り/空行スキップ+replay フラグ)。既存テストは無変更。コミット: `refactor(core): tailer のローテーション先と行切り出しの判定を純関数へ抽出`
- [ ] **Step 3**: 各コミットで `cargo test --workspace` / clippy、move-only 検証(Step 1)

---

### Task 2: manifest のトップレベル化(move-only)

**Files:**
- Move: `core/src/plugin/manifest/mod.rs` → `core/src/manifest/mod.rs`、`core/src/plugin/manifest/tests.rs` → `core/src/manifest/tests.rs`
- Move: `core/src/driver/manifest.rs` → `core/src/manifest/driver.rs`
- Modify: `core/src/lib.rs`(`pub mod manifest;`)/ `core/src/plugin/mod.rs` / `core/src/driver/mod.rs`(旧パス pub use)

**Interfaces:**
- Consumes: なし
- Produces: `crate::manifest::{Manifest, load_manifest, ...}` / `crate::manifest::driver::{DriverManifest, load_driver_manifest}`。旧パス温存:

```rust
// plugin/mod.rs — Phase 6 タスク2で manifest/ へ移動(旧パス互換。削除は本 Phase の Task 5)。
pub use crate::manifest;
// driver/mod.rs — 同様に
pub use crate::manifest::driver as manifest;
```

- [ ] **Step 1**: `git mv` + 配線。`manifest/mod.rs` に `pub mod driver;` を追加(旧 `driver::manifest` の中身は無変更)。`plugin/manifest/mod.rs` 182 の capability 旧パス互換 pub use はファイルごと移動(削除は Task 5)。既存の `pub use manifest::{...}` 便宜再輸出(plugin/mod.rs 27–31)は張り替えのみ
- [ ] **Step 2**: `cargo test --workspace` / clippy、move-only 検証(rename 100%)、テスト凍結チェック(core/tests/ diff 空)
- [ ] **Step 3**: コミット `refactor(core): manifest を manifest/ へトップレベル化(移動のみ、旧パスは pub use 温存)`

---

### Task 3: runtime/ 新設(move-only)

**Files:**
- Move: `core/src/plugin/bus_runtime.rs` → `core/src/runtime/bus.rs`、`core/src/plugin/fs_runtime.rs` → `core/src/runtime/fs.rs`、`core/src/plugin/sidecar_runtime.rs` → `core/src/runtime/sidecar.rs`、`core/src/plugin/dropped.rs` → `core/src/runtime/dropped.rs`
- Create: `core/src/runtime/mod.rs`
- Modify: `core/src/lib.rs` / `core/src/plugin/mod.rs`(旧パス pub use)

**Interfaces:**
- Consumes: なし
- Produces: `crate::runtime::{bus, fs, sidecar, dropped}`。runtime/mod.rs のモジュールドキュメントに分類根拠を書く(分析 §4 リスク2): 「HostCtx と Registry が共有するランタイムバッファの JSON 形式と取りこぼしカウンタ。文字列整形と Atomic カウンタのみで I/O・Mutex・スレッドを持たない純粋モジュール」。旧パス温存:

```rust
// plugin/mod.rs — Phase 6 タスク3で runtime/ へ移動(旧パス互換。削除は本 Phase の Task 5)。
pub use crate::runtime::bus as bus_runtime;
pub use crate::runtime::dropped;
pub use crate::runtime::fs as fs_runtime;
pub use crate::runtime::sidecar as sidecar_runtime;
```

- [ ] **Step 1**: `git mv` + 配線。既存の便宜再輸出(plugin/mod.rs 22/24/40 の `pub use bus_runtime::{...}` 等)は張り替えのみ
- [ ] **Step 2**: `cargo test --workspace` / clippy、move-only 検証、テスト凍結チェック。`rpc/render.rs` の import が純粋→純粋のままであることを確認(分析 §4 リスク3)
- [ ] **Step 3**: コミット `refactor(core): 共有ランタイムバッファ群と DropCounters を runtime/ へ移設(移動のみ、旧パスは pub use 温存)`

---

### Task 4: 残り4ファイルの移設(move-only)

**Files:**
- Move: `core/src/plugin/filesystem.rs` → `core/src/settings/filesystem.rs`、`core/src/plugin/sidecar.rs` → `core/src/settings/sidecar.rs`
- Move: `core/src/plugin/allowlist.rs` → `core/src/host/allowlist.rs`
- Move: `core/src/plugin/select_options.rs` → `core/src/registry/select_options.rs`
- Modify: `core/src/settings/mod.rs` / `core/src/host/mod.rs` / `core/src/registry/mod.rs` / `core/src/plugin/mod.rs`(旧パス pub use)

**Interfaces:**
- Consumes: なし
- Produces: `crate::settings::{filesystem, sidecar}` / `crate::host::allowlist` / `crate::registry::select_options`(pub(crate) のまま)。旧パス温存(plugin/mod.rs に `pub use crate::settings::filesystem;` 等 — select_options は pub(crate) use)

- [ ] **Step 1**: `git mv` + 配線。`host/resolve.rs` の `use crate::plugin::allowlist::check_url` は旧パス pub use で通るため無変更(置換は Task 5)
- [ ] **Step 2**: `cargo test --workspace` / clippy、move-only 検証、テスト凍結チェック
- [ ] **Step 3**: コミット `refactor(core): config store を settings/、allowlist を host/、select_options を registry/ へ移設(移動のみ、旧パスは pub use 温存)`

---

### Task 5: 旧パス pub use の一括削除 + use 文置換(sweep)

**Files:**
- Delete: `core/src/plugin/mod.rs` / `core/src/driver/mod.rs`(この時点で pub use と mod 宣言だけの残骸)
- Modify: `core/src/lib.rs`(`pub mod plugin;` / `pub mod driver;` 削除)、`core/src/manifest/mod.rs`(182 相当の capability 旧パス互換 pub use 削除)、core/src 内の全利用側(約170箇所)+ core/tests(約35箇所)の use 文

**Interfaces:**
- Consumes: Task 2–4 の新パス
- Produces: `crate::plugin::*` / `crate::driver::*` / `edlr_core::plugin::*` / `edlr_core::driver::*` の完全消滅

置換対応表(分析 §1/§2 — これ以外の置換をしない):

| 旧 | 新 |
|---|---|
| `{crate,edlr_core}::plugin::manifest::*`・`::plugin::{Manifest,...}` | `{crate,edlr_core}::manifest::*` |
| `::driver::manifest::*`・`::driver::{DriverManifest, load_driver_manifest}` | `::manifest::driver::*` |
| `::plugin::{bus_runtime,fs_runtime,sidecar_runtime,dropped}::*` | `::runtime::{bus,fs,sidecar,dropped}::*` |
| `::plugin::{filesystem,sidecar}::*`(config store) | `::settings::{filesystem,sidecar}::*` |
| `::plugin::allowlist::*` | `::host::allowlist::*` |
| `::plugin::select_options::*` | `::registry::select_options::*` |
| `::plugin::grants::*` | `::capability::grants::*` |
| `::plugin::{schedule,schedule_store}::*` | `::schedule::*` / `::schedule::store::*` |
| `::plugin::settings::*`(SettingsStore) | `::settings::store::*` |
| `::plugin::host::*` / `::driver::host::*` | `::host::plugin::*` / `::host::driver::*` |
| `::plugin::registry::*` / `::driver::registry::*` | `::registry::plugin::*` / `::registry::driver::*` |
| `::plugin::runner::*` / `::driver::runner::*` | `::runner::plugin::*` / `::runner::driver::*` |
| 便宜再輸出経由(`::plugin::Manifest` 等 plugin/mod.rs の pub use 郡) | 各実体の新パスへ |

- [ ] **Step 1**: 上記対応表で core/src + core/tests の use 文を機械的に置換し、plugin/mod.rs・driver/mod.rs と lib.rs の mod 宣言を削除。**use 文(と mod 宣言の削除)以外の diff を出さない**
- [ ] **Step 2**: 検証: `grep -rn "crate::plugin::\|crate::driver::\|edlr_core::plugin\|edlr_core::driver" core/src core/tests --include="*.rs" | grep -v "^\s*//"` が **0 件**(ドキュメントコメント内の表記は Task 6 で扱うため `//` 行は除外してよいが、コード行は 0 必須)。`cargo test --workspace` 全パス + clippy 0。diff が use/mod 行のみであることを目視 + `git diff --stat`
- [ ] **Step 3**: コミット `refactor(core): 旧パス pub use を一括削除し use 文を新パスへ置換(Phase 1–5 の互換層を撤去)`

---

### Task 6: モジュールドキュメント整備(docs)

**Files:**
- Modify: `core/src/lib.rs`(crate ドキュメントにモジュール一覧)、各モジュールの mod.rs(1〜数行のモジュールドキュメントが無い/古いもの)
- Modify: ソース内ドキュメントコメントの旧パス表記(`crate::plugin::registry::…` 等 — Phase 4/5 で先送りした分)を新パスへ更新
- Modify: `.claude/rules/module-layout.md`(モジュール表: manifest/ トップレベル化・runtime/ 追加・plugin/,driver/ 削除・注記の「リファクタ実施中」文言除去)、`CLAUDE.md`(冒頭のモジュール一覧行を同様に更新)

**Interfaces:**
- Consumes: Task 5 完了後の最終構成
- Produces: ドキュメントと実態の一致

- [ ] **Step 1**: `grep -rn "crate::plugin::\|crate::driver::" core/src --include="*.rs"`(この時点で全てコメント内)を対応表で新パス表記に更新。コメントの内容自体(設計説明)は変えない
- [ ] **Step 2**: 各 mod.rs のモジュールドキュメントを点検し、無い/古いものへ1〜数行の責務説明を追加。module-layout.md と CLAUDE.md の表を更新
- [ ] **Step 3**: `cargo test --workspace` / clippy(ドキュメントのみだが機械的確認)。コミット `docs(core): モジュールドキュメントと旧パス表記を Phase 6 後の構成に更新`

---

### Task 7: Phase 6 完了ゲート(リファクタリング全体の完了確認)

- [ ] **Step 1**: `cargo test --workspace`(全パス・pin 含む)+ clippy 0 + `cargo fmt --check`(既知の main 由来 drift は issue so6b — 増やしていないことだけ確認)
- [ ] **Step 2**: 構成確認: `ls core/src` が分析 §1 末尾の一覧(plugin/・driver/ が無い)と一致。`find core/src -name "*.rs" | xargs wc -l | sort -rn | head` を記録
- [ ] **Step 3**: 旧パス 0 件の再確認(Task 5 Step 2 の grep がコメント含め 0 件になっていること — Task 6 でコメントも更新済みのため)
- [ ] **Step 4**: テスト凍結の総括: `git diff 1a5d0eb..HEAD -- core/tests/ --stat` の diff が use 行のみ(Task 5)+ テスト本数が減っていないこと(`git grep -c -E '#\[(tokio::)?test' | 集計` を base と比較)
- [ ] **Step 5**: spec(2026-07-30-core-refactoring-design.md)の状態を「レビュー待ち」から「完了」に更新し、Phase 0–6 の全フェーズ完了をユーザーへ報告する

---

## Self-Review 済み事項

- spec Phase 6 の3要素(journal 作法揃え = Task 1、旧パス一括削除 + use 置換 = Task 5、モジュールドキュメント整備 = Task 6)+ ユーザー承認済みの解体(Task 2–4)をカバー
- 移設先は全ファイルについて分析 §1 で根拠づけ(rpc→dropped の純粋性制約、allowlist の利用者単一性、select_options の Bus 依存を確認済み)
- テスト凍結の例外(Task 5 のみ use 行書換可)は spec の記述(「そのコミットは use 文置換のみ」)に基づき Global Constraints に明記
- 同綴り新パスの巻き込み防止を置換対応表 + grep ゲートで担保
- logs.rs 等の追加作法揃えは「必要が実証されたら」で見送り(分析 §3)— YAGNI
- 本 Phase でリファクタリング全体が完了するため、Task 7 に spec の状態更新を含めた

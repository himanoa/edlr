# core リファクタリング Phase 3(schedule + settings + capability::grants)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 挙動を一切変えずに、settings と schedule を機能名モジュール(`settings/` `schedule/`)へ移し、settings の検証・マージと grants の失効判定を値イン値アウトの純関数に抽出して、純粋テストを追加する。

**Architecture:** spec は `docs/superpowers/specs/2026-07-30-core-refactoring-design.md`(Phase 3 行)。判断と実行の分離は `.claude/rules/procedure-style.md`、移動規律は `.claude/skills/refactor-move-only-commit`。前段として最終レビュー(Phase 2)推奨の `rpc::render` 純粋テストを追加する。

**Tech Stack:** Rust (cargo workspace)。新規依存 crate の追加は禁止。

## Global Constraints

- **挙動不変**: 設定ファイル・grant ファイルのディスク上のフォーマット、エラーメッセージ文字列(`SettingsError`/`GrantsError` の `Display` 含む)、検証の受理/拒否の境界を1バイトも変えない
- **テスト凍結**: 既存テスト(`core/tests/` と既存 `#[cfg(test)]`)の diff は「空 or import 行のみ or 丸ごと移動」だけ許可。**新規の純粋テスト追加は凍結と矛盾しない**。既存テストが落ちたらテストではなく実装を戻す
- **旧パス温存**: `crate::plugin::settings::*` / `crate::plugin::schedule::*` / `crate::plugin::schedule_store::*` は `pub use` で存続(削除は Phase 6)
- **コミット分離**: 1コミット = 移動のみ か ロジック変更のみ。move-only は multiset 検証を通す
- **ゲート**: `cargo test --workspace` 全パス(pin テスト含む)+ `cargo clippy --workspace` 警告なし(ベースライン 0)
- **モックは推測で作らない**(`.claude/rules/trait-di.md`): 本 Phase の新テストは抽出した純関数への値イン値アウトが主体。`test_support` のインメモリ Storage は「それを使うテストをこの Phase で書く場合」だけ追加する(consumer のジェネリック化は Phase 4 なので、原則不要のはず)
- **純粋モジュールの I/O 例外**: `settings/store.rs`(ディスク実装)を settings/ 配下に置くのは capability/grants.rs と同じ公認例外。issue `rules-capability-grants-rs-i-o-manifest-99dq` に追記して記録する(Task 2 Step 4)
- 新しい命令的コード: 1関数〜40行・ネスト2段・不要な `mut` 禁止
- **並列実行の注意**(CLAUDE.md): サブエージェント並列起動前に `cargo fetch`。同一 worktree 内で cargo を並走させない

## 対象の現状(base: Phase 2 マージ済み main)

| ファイル | 行数 | 内容 |
|---|---|---|
| `core/src/plugin/settings.rs` | 933 | `split_secrets`(純)/ `SettingsStore`(検証 `validate_value`・マージ `effective_locked`・原子的書込)+ tests ~630行 |
| `core/src/plugin/schedule.rs` | 772 | `Clock` / `Fire` / `NextFire` / `ScheduleState`(発火計算。now は `Clock` 引数渡し)+ tests |
| `core/src/plugin/schedule_store.rs` | 192 | スケジュール設定の永続化 |
| `core/src/capability/grants.rs` | 1186 | `GrantsStore`。5系統の `*_state_locked` が同型の失効判定(未保存/不一致/一致)を繰り返す |
| `core/src/rpc/render.rs` | 146 | 純粋レンダラ7本(テストなし — Task 1 で錨を足す) |
| `core/src/settings/mod.rs` | 81 | Phase 0 の `Storage` trait(`use crate::plugin::settings::...`)|

---

### Task 1: `rpc::render` の純粋テスト追加

**Files:**
- Modify: `core/src/rpc/render.rs`(`#[cfg(test)] mod tests` を新設。既存コードには触らない)

**Interfaces:**
- Consumes: `crate::plugin::registry::{BusInfo, DashboardInfo, ScheduleInfo, SidecarInfo, FilesystemInfo}` ほか(全フィールド pub — 直接構築できる)
- Produces: レンダラの JSON 形を固定する値イン値アウトのテスト。Phase 4 以降で Info 型を動かすときの錨

- [ ] **Step 1: テストを書く**

7本のレンダラ各1件以上。**期待値は現在の出力をそのまま書く**(pin テストと同じ精神で、全体等値比較)。例:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::grants::GrantState;

    #[test]
    fn capabilities_json_has_requests_granted_and_stale_grant() {
        let state = GrantState { granted: true, stale: false };
        let json = capabilities_result_json(&[], &state);
        assert_eq!(
            json,
            serde_json::json!({ "requests": [], "granted": true, "staleGrant": false })
        );
    }
}
```

`ScheduleInfo` は `next: chrono::DateTime<Local>` を持つ — テストでは固定時刻を `chrono::DateTime::parse_from_rfc3339("2026-07-30T12:00:00+09:00")` から作って `with_timezone(&chrono::Local)` で渡し、期待値側は `info.next.to_rfc3339()` を差し込む(実行時刻に依存させない)。`SidecarInfo`/`FilesystemInfo` 等は必要なフィールドを列挙して構築する(`Default` を新たに impl するのは禁止)。空スライス系(`bus_result_json(&[])` → `{"bus": []}`)も1件ずつ。

- [ ] **Step 2: テスト実行と clippy**

Run: `cargo test -p edlr-core rpc:: 2>&1 | tail -5` のあと `cargo test --workspace 2>&1 | tail -5` / `cargo clippy --workspace 2>&1 | tail -5`
Expected: 全パス、警告増なし

- [ ] **Step 3: コミット(テスト追加のみ)**

```bash
git add core/src/rpc/render.rs
git commit -m "test(core): rpc::render の純粋テストを追加

Phase 2 最終レビューの推奨対応。レンダラ7本の JSON 形を値イン値アウトの
等値比較で固定する(Phase 4 で Info 型を動かすときの錨)。"
```

---

### Task 2: settings を `settings/store.rs` へ移動(move-only)

**Files:**
- Move: `core/src/plugin/settings.rs` → `core/src/settings/store.rs`
- Modify: `core/src/settings/mod.rs`(`pub mod store;` 追加、trait の import を `store::` に変更)
- Modify: `core/src/plugin/mod.rs`(`mod settings;` を `pub use` に置換)
- 追記: issue `rules-capability-grants-rs-i-o-manifest-99dq`

**Interfaces:**
- Consumes: なし(移動のみ)
- Produces: `settings::store::{split_secrets, SettingsStore, SettingsError}`。旧パス `crate::plugin::settings::*` は `pub use` で存続

- [ ] **Step 1: ファイル移動**

```bash
git mv core/src/plugin/settings.rs core/src/settings/store.rs
```

`store.rs` 内の `use crate::plugin::...` は絶対パスなのでそのまま有効。`#[cfg(test)] mod tests` はファイルごと移動(中身に触らない)。

- [ ] **Step 2: 配線**

`core/src/settings/mod.rs`: `pub mod store;` を追加し、Phase 0 の `use crate::plugin::settings::{SettingsError, SettingsStore};` を `use store::{SettingsError, SettingsStore};` に変更。

`core/src/plugin/mod.rs`: `mod settings;`(または `pub mod settings;`)を削除し、同じ場所に:

```rust
// Phase 3 で settings/store.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::settings::store as settings;
```

`plugin/mod.rs` の個別再エクスポート(`pub use settings::...` があれば)はそのまま残す。

- [ ] **Step 3: テスト実行と move-only 検証**

Run: `cargo test --workspace 2>&1 | tail -5` / `cargo clippy --workspace 2>&1 | tail -5` + multiset 検証(非移動行が配線2ファイル分のみ)
Expected: 全パス。`core/tests/` と `#[cfg(test)]` の diff なし(丸ごと移動を除く)

- [ ] **Step 4: issue へ追記**

`rules-capability-grants-rs-i-o-manifest-99dq` に「Phase 3: settings/store.rs(SettingsStore、std::fs + Mutex)も同じ公認例外」を1段落追記(git-issues の非対話手順)。

- [ ] **Step 5: コミット**

```bash
git add -A core/src/settings core/src/plugin/mod.rs
git commit -m "refactor(core): settings を settings/store.rs へ移動(移動のみ、旧パスは pub use 温存)"
```

---

### Task 3: settings の検証・マージを純関数へ抽出(logic)

**Files:**
- Create: `core/src/settings/validate.rs`
- Modify: `core/src/settings/mod.rs`(`pub mod validate;` 追加)
- Modify: `core/src/settings/store.rs`(委譲に置換)
- Test: `core/src/settings/validate.rs` 内の新規 `#[cfg(test)]`

**Interfaces:**
- Consumes: `crate::plugin::manifest::SettingField`、Task 2 の `store::SettingsError`
- Produces: 以下の純関数(store が委譲で使う):

```rust
/// `value` が `field` の宣言型(および Select の `options`)に適合するか検証する。
pub fn validate_value(field: &SettingField, value: &serde_json::Value) -> Result<(), SettingsError>;

/// manifest の settings 宣言に defaults → 保存値の順で重ねた effective 値を作る。
/// `saved` が `None`(ファイルなし・壊れた JSON・非オブジェクト)なら defaults のみ。
/// 宣言に無い保存 key は無視する。
pub fn effective_values(
    settings: &[SettingField],
    saved: Option<&serde_json::Map<String, serde_json::Value>>,
) -> serde_json::Map<String, serde_json::Value>;
```

- [ ] **Step 1: `validate_value` の移動**

`store.rs` の `SettingsStore::validate_value`(associated fn、`&self` を取らない — 既に純粋)を**本体そのまま** `settings/validate.rs` の `pub fn validate_value` へ移し、store 側の呼び出し(`Self::validate_value(field, value)?` 1箇所)を `crate::settings::validate::validate_value(field, value)?` に置換。エラー variant・文字列は不変。

- [ ] **Step 2: `effective_values` の抽出**

`effective_locked` を「読み(I/O)」と「マージ(純)」に分ける。store 側は:

```rust
fn effective_locked(&self, manifest: &Manifest) -> serde_json::Map<String, serde_json::Value> {
    let saved = fs::read_to_string(self.path_for(manifest))
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|value| match value {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        });
    crate::settings::validate::effective_values(&manifest.settings, saved.as_ref())
}
```

`effective_values` の本体は現行の「defaults を敷く → saved にある宣言済み key だけ上書き」をそのまま移す。**受理/拒否・フォールバックの境界は不変**(既存テスト `effective_falls_back_to_defaults_on_broken_or_non_object_json` 等が凍結のまま通ることが担保)。

- [ ] **Step 3: 純粋テストを書く**

`validate.rs` に値イン値アウトのテストを追加(tempdir 不要)。最低限:
- `validate_value`: Boolean/String/Number/Map/Select(options 一致・不一致・options_from は照合しない)/Secret の受理と拒否、エラー variant の一致(`matches!` ではなく `assert_eq!` で `Display` 文字列まで固定してよい)
- `effective_values`: saved=None → defaults のみ / 宣言外 key の無視 / 上書きの3パターン

- [ ] **Step 4: テスト実行と clippy**

Run: `cargo test -p edlr-core settings 2>&1 | tail -5` のあと `cargo test --workspace 2>&1 | tail -5` / clippy
Expected: 既存 settings テスト(移動済み)が1件も変更されずに通ること

- [ ] **Step 5: コミット(logic)**

```bash
git add core/src/settings
git commit -m "refactor(core): settings の検証・マージを settings/validate.rs の純関数へ抽出

validate_value / effective_values を値イン値アウトにし、store は
読み→判断→書きの順の薄い手続きに。受理/拒否境界とエラー文字列は不変
(凍結済み settings テストが担保)。純粋テストを追加。"
```

---

### Task 4: schedule を `schedule/` モジュールへ移動(move-only)

**Files:**
- Move: `core/src/plugin/schedule.rs` → `core/src/schedule/mod.rs`
- Move: `core/src/plugin/schedule_store.rs` → `core/src/schedule/store.rs`
- Modify: `core/src/lib.rs`(`pub mod schedule;` をアルファベット順に追加)
- Modify: `core/src/plugin/mod.rs`(2つの `mod` 宣言を `pub use` に置換)

**Interfaces:**
- Consumes: なし(移動のみ)
- Produces: `schedule::{Clock, ScheduleState, ScheduleView, ...}` / `schedule::store::*`。旧パス `crate::plugin::schedule` / `crate::plugin::schedule_store` は `pub use` で存続

- [ ] **Step 1: ファイル移動**

```bash
mkdir core/src/schedule
git mv core/src/plugin/schedule.rs core/src/schedule/mod.rs
git mv core/src/plugin/schedule_store.rs core/src/schedule/store.rs
```

`schedule/mod.rs` に `pub mod store;` を追加(この1行と配線のみが非移動行)。両ファイル内の `use crate::...` 絶対パスはそのまま有効。相対 `super::` 参照があれば絶対パスに直し、その行をコミットメッセージに列挙する。

- [ ] **Step 2: 配線**

`core/src/lib.rs` に `pub mod schedule;`(`rpc` と `server` の間ではなく正しいアルファベット位置: `router` < `rpc` < `schedule` < `server`)。

`core/src/plugin/mod.rs` の `mod schedule;` / `mod schedule_store;`(pub の有無は現状に従う)を削除し:

```rust
// Phase 3 で schedule/ へ移動(旧パス互換。削除は Phase 6)。
pub use crate::schedule;
pub use crate::schedule::store as schedule_store;
```

注: `pub use crate::schedule;` は `crate::plugin::schedule` パスを再提供する(Phase 1 の grants と同じ手法)。個別再エクスポートがあればそのまま残す。

- [ ] **Step 3: テスト実行と move-only 検証**

Run: `cargo test --workspace 2>&1 | tail -5` / clippy + multiset 検証
Expected: 全パス。schedule のテスト(mod.rs 内の `mod tests`)は丸ごと移動のみ

- [ ] **Step 4: コミット**

```bash
git add -A core/src/schedule core/src/plugin/mod.rs core/src/lib.rs
git commit -m "refactor(core): schedule を schedule/ モジュールへ移動(移動のみ、旧パスは pub use 温存)"
```

---

### Task 5: grants の失効判定を純関数へ抽出(logic)

**Files:**
- Modify: `core/src/capability/grants.rs`(判定の純関数化 + 新規純粋テスト)

**Interfaces:**
- Consumes: 既存の `GrantState`
- Produces: 以下の純関数。5系統の `*_state_locked` が全てこれに委譲する:

```rust
/// 保存済み grant と現在の fingerprint から承認状態を判定する(純関数)。
///
/// - 要求がない(`current` が `None`)→ `{ granted: false, stale: false }`
/// - 未保存(`saved` が `None`)→ `{ granted: false, stale: false }`
/// - fingerprint 不一致 → `{ granted: false, stale: true }`
/// - 一致 → 保存された `granted` をそのまま(取消保存は `{ false, false }`)
fn resolve_grant(current: Option<&str>, saved: Option<(&str, bool)>) -> GrantState;
```

- [ ] **Step 1: 実装**

`grants.rs` 内(`GrantsStore` の impl 外)に `resolve_grant` を追加し、5つの判定を委譲に置換する。例(`state_locked`):

```rust
fn state_locked(&self, manifest: &Manifest) -> GrantState {
    let current = manifest.capabilities_fingerprint();
    let saved = self.read_saved(manifest);
    resolve_grant(
        current.as_deref(),
        saved
            .as_ref()
            .map(|s| (s.fingerprint.as_str(), s.granted)),
    )
}
```

`sidecar_state_locked` 等の per-name 系は `saved.sidecars.get(name)` の lookup を残し、`entry` を `(entry.fingerprint.as_str(), entry.granted)` にして渡す。**判定順序(current 確認 → saved 確認 → 不一致 → 一致)と各分岐の戻り値は1ビットも変えない**。トップレベル `SavedGrant` の `fingerprint` はデフォルト空文字列 — 空文字列は現在の fingerprint と不一致になり stale 扱い(既存テスト `old_format_fingerprint_on_disk_is_treated_as_stale_not_valid` が凍結のまま通ることが担保)。

- [ ] **Step 2: 純粋テストを書く**

`grants.rs` の既存 `mod tests` には触らず、`resolve_grant` の**直上に置く新しい** `#[cfg(test)] mod resolve_tests` を追加(既存 tests への追記は凍結違反になるため分ける)。4分岐 + 空文字列 fingerprint + 取消保存(granted=false で一致)の6ケースを値イン値アウトで。

- [ ] **Step 3: テスト実行と clippy**

Run: `cargo test -p edlr-core grants 2>&1 | tail -5` のあと `cargo test --workspace 2>&1 | tail -5` / clippy
Expected: 既存 grants テスト(30本超)が1件も変更されずに通ること

- [ ] **Step 4: コミット(logic)**

```bash
git add core/src/capability/grants.rs
git commit -m "refactor(core): grants の失効判定を resolve_grant 純関数へ抽出

5系統の *_state_locked が繰り返していた同型の判定(未保存/不一致/一致)を
1本の値イン値アウト関数に集約。判定順序と境界は不変(凍結済み grants
テストが担保)。純粋テストを追加。"
```

---

### Task 6: Phase 3 完了ゲート

**Files:** なし(検証のみ)

**Interfaces:**
- Consumes: Task 1–5 の成果
- Produces: Phase 4 着手可能な状態の確認記録

- [ ] **Step 1: 全体検証**

```bash
cargo test --workspace 2>&1 | grep "test result" | grep -v " 0 failed"   # 空 = 全パス
cargo clippy --workspace 2>&1 | tail -5
```

- [ ] **Step 2: 行数と配置の記録**

```bash
wc -l core/src/settings/*.rs core/src/schedule/*.rs core/src/capability/grants.rs core/src/rpc/render.rs
ls core/src/plugin/
```

Expected: `plugin/` から settings / schedule / schedule_store が消えている(旧パスは pub use)。数値を報告に含める

- [ ] **Step 3: テスト凍結の最終確認**

```bash
BASE=$(git merge-base main HEAD)
git diff $BASE..HEAD -- core/tests/ --stat        # 空のはず(この Phase では core/tests/ に触らない)
git diff $BASE..HEAD -- 'core/src/' | grep -E '^[-+].*#\[(test|tokio::test|cfg\(test\))' | sort | uniq -c
```

Expected: 増分は新規純粋テスト(render / validate / resolve_grant)のみ。既存分は丸ごと移動で説明がつくこと

- [ ] **Step 4: 報告**

Phase 3 の完了をユーザーに報告し、Phase 4(registry 解体 — 本丸)の計画作成に進む承認を得る。

---

## Self-Review 済み事項

- spec Phase 3 の3要素(判定の純関数化・Storage trait 越しの永続化・純粋テスト追加)→ Task 3/5 が判定抽出と純粋テスト、Task 2/4 がモジュール移動に対応。trait 越しのジェネリック consumer 化は Phase 4 の Registry 分解で行う(spec の trait 表も consumer を Phase 3–4 と記載)
- spec の「モックによる純粋テスト」: 本 Phase の新テストは抽出純関数への値イン値アウトで足りるため、インメモリ mock は追加しない(trait-di.md「必要が実証されたときだけ」に従う意図的判断。Phase 4 のジェネリック化で mock が必要になった時点で test_support に追加)
- 型整合: `validate_value`/`effective_values` のシグネチャは Task 3 Interfaces に固定。`resolve_grant` は Task 5 に固定(grants.rs 内の非公開関数 — 外部 consumer がいないため pub にしない)
- settings/store.rs の I/O は capability/grants.rs と同じ公認例外として issue 追記(Task 2 Step 4)
- schedule は既に `Clock` を引数で受ける sans-IO 形なので移動のみ(発火計算の追加抽出は runner 側と絡む Phase 5 で扱う)
- 旧パス温存の手法は Phase 1 Task 8(grants)で実証済みの `pub use crate::<new> as <old>;` パターン
- Phase 4 以降は別 plan(意図的)

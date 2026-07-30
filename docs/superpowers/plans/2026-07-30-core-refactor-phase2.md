# core リファクタリング Phase 2(rpc + server)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 挙動を一切変えずに、着手前に代表 RPC 応答の pin テストを追加した上で、`server.rs`(1855行)の巨大 match をメソッド単位の小関数に分解し、JSON 整形の純粋関数群を `rpc/` モジュールへ移す。server は WS/HTTP 配線と薄い dispatch だけになる。

**Architecture:** spec は `docs/superpowers/specs/2026-07-30-core-refactoring-design.md`(Phase 2 行)。`rpc/` は純粋モジュール(値イン値アウト)、`server/` は命令的モジュール(Registry 呼び出しと axum/WS 配線)。手順の規律は `.claude/skills/rpc-pin-tests` と `.claude/skills/refactor-move-only-commit` に従う。

**Tech Stack:** Rust (cargo workspace)。新規依存 crate の追加は禁止。

## Global Constraints

- **挙動不変**: RPC 応答 JSON・エラーメッセージ文字列・WS envelope(`{"type":"rpc-result",...}`)を1バイトも変えない。pin テスト(Task 1)が防衛線
- **テスト凍結**: `core/tests/` の既存ファイルと既存 `#[cfg(test)]` の diff は「空 or import 行のみ or 丸ごと移動」だけ許可。**pin テストの新規追加は凍結と矛盾しない**(rpc-pin-tests スキル参照)。pin テストが落ちたら**テストではなく実装を戻す**
- **旧パス温存**: 公開関数(`handle_rpc` / `handle_rpc_with_drivers` / `hello_json` / `event_to_ws_json` / `app` / `serve` / `ServerState`)のパス `crate::server::*` は不変。移動する `*_result_json` / `param_str` は元々 private なので外部互換は不要
- **コミット分離**: 1コミット = 移動のみ(move-only)か ロジック変更のみ。move-only コミットは refactor-move-only-commit スキルの multiset 検証を通すこと
- **ゲート**: 全タスクの完了条件は `cargo test --workspace` 全パス(pin テスト含む)+ `cargo clippy --workspace` 警告なし(ベースライン 0)
- **純粋/命令的境界**: `rpc/` から `Registry`/`DriverRegistry`(サービス)や `std::fs`/`Mutex` を import しない。ただし **plugin/registry 配下のデータ型**(`BusInfo` / `DashboardInfo` / `ScheduleInfo` / `SidecarInfo` / `FilesystemInfo` / `DroppedCounts` / `CapabilityRequest` / `GrantState`)の import は Phase 2 では公認の例外(型の所属整理は Phase 4。既知の rules 乖離 issue `rules-capability-grants-rs-i-o-manifest-99dq` に追記して記録する — Task 3 Step 4)
- **新しい命令的コードは 1関数〜40行・ネスト2段まで**。値の組み立てに不要な `mut` を使わない(`.claude/rules/minimal-mut.md`)
- **並列実行の注意**(CLAUDE.md): サブエージェント並列起動前に `cargo fetch`。同一 worktree 内で cargo コマンドを並走させない

## 対象ファイルの現状(base: main = c324a98)

`core/src/server.rs` 1855行の内訳:

| 行(目安) | 内容 |
|---|---|
| 1–120 | `ServerState` / EventFeeder(replay バッファ)/ `hello_json` / `event_to_ws_json` |
| 125–391 | `handle_rpc` → `handle_rpc_with_drivers`(plugins/* 14メソッドの match) |
| 400–542 | `handle_drivers_rpc`(drivers/* 10メソッドの match) |
| 546–696 | `capabilities_result_json` / `dashboard_result_json` / `bus_result_json` / `dropped_result_json` / `schedules_result_json` / `param_str` / `sidecars_result_json` / `filesystem_result_json`(全て純粋) |
| 700–830 | UI アセット配信 / `app` router / `serve` / `origin_allowed` |
| 832–927 | `client_loop` / `handle_client_message`(WS 配線) |
| 929–1830 | `#[cfg(test)] mod tests`(35テスト中の大半) |
| 1831–1855 | `#[cfg(test)] mod origin_tests` |

---

### Task 1: 代表 RPC 応答の pin テスト追加

**Files:**
- Create: `core/tests/rpc_pin_integration.rs`
- 参照(変更禁止): `core/tests/ws_rpc_integration.rs`(ハーネスの流儀)、`core/tests/support/mod.rs`(fixture ヘルパー)、`core/src/server.rs` の `#[cfg(test)] mod tests` 内 `drivers` 系テスト(~line 1000。DriverRegistry の fixture 構築例)

**Interfaces:**
- Consumes: `edlr_core::server::{handle_rpc_with_drivers, app}`、`support::{sidecar_env, empty_driver_registry, valid_plugin_wasm}` など既存公開ヘルパー
- Produces: Phase 2 全体の防衛線となる pin テスト3本(リファクタリング終了後も残す)

手順の規律は `.claude/skills/rpc-pin-tests/SKILL.md` に従う。要点: **応答 JSON 全体の等値比較**(部分比較にしない)、実行ごとに変わる値(tempdir パス・ポート・`next` タイムスタンプ)だけ変数化する。

- [ ] **Step 1: ベースライン確認**

Run: `cargo test --workspace 2>&1 | grep "test result" | grep -v " 0 failed"; echo "empty means all green"`
Expected: 失敗なし(あれば作業前に報告して停止)

- [ ] **Step 2: 捕捉用テストを書いて実際の JSON を得る**

`core/tests/rpc_pin_integration.rs` を作成。ハーネスは `ws_rpc_integration.rs` の `setup`/`connect`/`recv_hello`/`send_rpc`/`recv_json` と同じ形をこのファイル内に写す(`core/tests/` の各ファイルは独立クレートなので、`support/` に無いヘルパーはコピーしてよい — 既存ファイルへの追記は凍結違反なので不可)。fixture は次の3系統:

1. **`plugins/list`**: `support::sidecar_env("svc", <port>, false)` で sidecar + capabilities を持つプラグイン入り `Registry` を作る(`ws_rpc_integration.rs` の sidecar 系テストの構築をそのまま流用)。sidecars / filesystem / capabilities が実データで埋まり、bus / dashboard / schedules / dropped は空でもキーが pin される
2. **`plugins/get-capabilities` → `plugins/set-capabilities`(granted: true)**: 同じ fixture で grant 遷移後の `{ requests, granted, staleGrant }` を pin
3. **`drivers/list`**: `core/src/server.rs` 内 `mod tests` の drivers 系テスト(`handle_drivers_rpc(&drivers, "list", ...)` を呼んでいるもの、~line 1007)と同じ方法で fixture driver 入りの `DriverRegistry` を構築して pin

まず各テストを `eprintln!("{}", serde_json::to_string_pretty(&response).unwrap());` を入れた形で書き、`cargo test -p edlr-core --test rpc_pin_integration -- --nocapture` を1回実行して生 JSON を得る。

- [ ] **Step 3: 捕捉した JSON をそのまま `json!` リテラルに固定する**

得られた JSON を**削らず**に `serde_json::json!` リテラルとして貼り、全体等値比較にする:

```rust
let expected = serde_json::json!({
    "pluginsDir": plugins_dir_str,   // tempdir パスだけ変数化
    "plugins": [ /* 捕捉した生 JSON をそのまま */ ],
});
assert_eq!(response["result"], expected);
```

WS 経由のテストでは envelope も固定する: `assert_eq!(response["type"], "rpc-result");` と `assert_eq!(response["id"], <送った id>);` を必ず含める。`schedules[].next` のような実行時刻フィールドが fixture に現れた場合のみ、応答から取り出して expected 側へ差し込む(それ以外のフィールドの差し込み・省略は禁止)。

- [ ] **Step 4: テスト実行**

Run: `cargo test -p edlr-core --test rpc_pin_integration 2>&1 | tail -5` のあと `cargo test --workspace 2>&1 | tail -5`
Expected: pin 3本を含め全パス

- [ ] **Step 5: コミット(テスト追加のみ)**

```bash
git add core/tests/rpc_pin_integration.rs
git commit -m "test(core): Phase 2 着手前の RPC 応答 pin テストを追加

plugins/list・capabilities grant 遷移・drivers/list の生 JSON 全体を
等値比較で固定する。server.rs 分解中に応答の形が 1 フィールドでも
変われば落ちる防衛線(リファクタリング終了後も残す)。"
```

---

### Task 2: server.rs のディレクトリ化とテスト分離(move-only)

**Files:**
- Move: `core/src/server.rs` → `core/src/server/mod.rs`
- Create: `core/src/server/tests.rs`(`mod tests` の中身を丸ごと移動)

**Interfaces:**
- Consumes: なし(純粋なファイル移動)
- Produces: 後続タスクが小さい diff で作業できる形。公開パス `crate::server::*` は不変

Phase 1 Task 4(manifest 分離)と同一手順。規律は refactor-move-only-commit スキル。

- [ ] **Step 1: ディレクトリ化**

```bash
mkdir core/src/server
git mv core/src/server.rs core/src/server/mod.rs
```

- [ ] **Step 2: `mod tests` を tests.rs へ移動**

`server/mod.rs` の `#[cfg(test)] mod tests { ... }`(~929行目から ~1830行目)を切り取り `core/src/server/tests.rs` へ。変形は次の2点だけ:
- `mod tests {` と末尾の `}` を剥がす
- 一様4スペース dedent(`sed 's/^    //'` 相当)。`use super::*;` はそのまま

`mod.rs` 側には `#[cfg(test)]\nmod tests;` を残す。

**`mod origin_tests`(1831–1855行、25行)は mod.rs に残す**。tests.rs に入れると `use super::origin_allowed;` の `super` が指す先が変わってしまうため動かさない。

- [ ] **Step 3: テスト数の一致確認とテスト実行**

Run: 移動前後で `grep -c '#\[test\]\|#\[tokio::test\]'` の合計(35)が一致すること。`cargo test -p edlr-core server 2>&1 | tail -5` のあと `cargo test --workspace 2>&1 | tail -5`
Expected: 全パス、pin テスト含む

- [ ] **Step 4: move-only 検証とコミット**

refactor-move-only-commit スキルの multiset 検証(dedent 込みなので、tests.rs 側を4スペース re-indent + wrapper 復元したものが元ファイル該当部と byte 一致することを `diff` で確認する — Phase 1 Task 4 のレビューで使った方法)。

```bash
git add -A core/src/server
git commit -m "refactor(core): server.rs をディレクトリ化しテストを tests.rs へ分離(移動のみ)"
```

---

### Task 3: JSON 整形の純粋関数群を `rpc/` へ移動(move-only)

**Files:**
- Create: `core/src/rpc/mod.rs` / `core/src/rpc/render.rs` / `core/src/rpc/params.rs`
- Modify: `core/src/lib.rs`(`pub mod rpc;` をアルファベット順に追加)
- Modify: `core/src/server/mod.rs`(関数定義を削除し `use` で置換)
- 追記: issue `rules-capability-grants-rs-i-o-manifest-99dq`(データ型 import の例外を記録)

**Interfaces:**
- Consumes: なし(移動のみ)
- Produces: `rpc::render::{capabilities_result_json, dashboard_result_json, bus_result_json, dropped_result_json, schedules_result_json, sidecars_result_json, filesystem_result_json}`(全て `pub` 化)、`rpc::params::param_str`(`pub` 化)。Task 4–5 の分解先が consume する

- [ ] **Step 1: rpc モジュールを作る**

`core/src/rpc/mod.rs`:

```rust
//! RPC 応答の JSON 整形と params 解釈(純粋関数群)。
//!
//! 値イン値アウトのみ。`Registry` などの命令的サービスはここから
//! 参照しない(呼び出すのは server/ の仕事)。
//!
//! 注: `BusInfo` など plugin/registry 配下の**データ型**の import は
//! Phase 2 時点の公認例外(型の所属整理は Phase 4。issue
//! rules-capability-grants-rs-i-o-manifest-99dq 参照)。

pub mod params;
pub mod render;
```

`server/mod.rs` から次を **doc コメントごと byte-identical に**移動し、`pub` を付ける(この可視性変更のみ sanctioned):

- → `rpc/render.rs`: `capabilities_result_json` / `dashboard_result_json` / `bus_result_json` / `dropped_result_json` / `schedules_result_json` / `sidecars_result_json` / `filesystem_result_json`(7本)
- → `rpc/params.rs`: `param_str`(1本)

各ファイルの `use` は移動した関数が要る分だけ元ファイルから持ってくる(型は `crate::plugin::...` の絶対パスで書かれているので基本そのまま動く)。

- [ ] **Step 2: server 側を use で配線**

`server/mod.rs` の関数があった場所に:

```rust
// Phase 2 で rpc/ へ移動(server 内の呼び出しと tests の `use super::*`
// がそのまま解決するよう、この use を温存する)。
use crate::rpc::params::param_str;
use crate::rpc::render::{
    bus_result_json, capabilities_result_json, dashboard_result_json, dropped_result_json,
    filesystem_result_json, schedules_result_json, sidecars_result_json,
};
```

`server/tests.rs` は `use super::*;` 経由でこれらが見え続けるので**一切触らない**。`lib.rs` に `pub mod rpc;` を追加。

- [ ] **Step 3: テスト実行と move-only 検証**

Run: `cargo test --workspace 2>&1 | tail -5` / `cargo clippy --workspace 2>&1 | tail -5`
Expected: 全パス(pin・server tests 含む)、警告増なし。multiset 検証で非移動行が「新ファイルヘッダ + pub 付与 + use 置換 + lib.rs 1行」だけであることを確認

- [ ] **Step 4: issue へ例外を追記**

`git issues` で `rules-capability-grants-rs-i-o-manifest-99dq` の本文末尾に「Phase 2: rpc/render.rs も plugin/registry 配下のデータ型を import する(同じ公認例外。Phase 4 で型の所属を整理)」の1段落を追記(git-issues スキルの非対話手順で)。

- [ ] **Step 5: コミット**

```bash
git add core/src/rpc core/src/server/mod.rs core/src/lib.rs
git commit -m "refactor(core): RPC の JSON 整形と params 解釈を rpc/ へ移動(移動のみ)"
```

---

### Task 4: plugins/* の match をメソッド単位の小関数へ分解(logic)

**Files:**
- Create: `core/src/server/rpc_plugins.rs`
- Modify: `core/src/server/mod.rs`(match の各 arm を1行の関数呼び出しに)
- Modify: `core/src/rpc/params.rs`(`param_bool` / `param_object` を追加)

**Interfaces:**
- Consumes: Task 3 の `rpc::render::*` / `rpc::params::*`
- Produces: `server::rpc_plugins` 内の per-method 関数群(下記シグネチャ)。Task 5 が同じ形を drivers に適用する

- [ ] **Step 1: params ヘルパーを rpc/params.rs に追加**

既存の繰り返しパターンと**同一のエラーメッセージ文字列**を生成するヘルパー2本(`param_str` と同じ流儀):

```rust
/// `params` から `key` の bool 値を取り出す。無い・bool でない場合は `Err`。
pub fn param_bool(params: &serde_json::Value, key: &str) -> Result<bool, String> {
    params
        .get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("params.{key} must be a bool"))
}

/// `params` から `key` のオブジェクト値を取り出す。無い・オブジェクトでない場合は `Err`。
pub fn param_object<'a>(
    params: &'a serde_json::Value,
    key: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    params
        .get(key)
        .and_then(|v| v.as_object())
        .ok_or_else(|| format!("params.{key} must be an object"))
}
```

既存の生成文字列は `"params.granted must be a bool"` / `"params.values must be an object"` 等なので `format!("params.{key} ...")` で完全一致する。`params.config` の取り出しは `.cloned().ok_or_else(|| "params.config must be an object")` + `serde_json::from_value` の2段で、エラー文字列が `param_object` と同型なのは「無い場合」だけ。**config の取り出しは現行コードのまま各関数に残す**(ヘルパー化すると「object でない場合」のエラーパスが変わるため)。

- [ ] **Step 2: rpc_plugins.rs に per-method 関数を切り出す**

`server/mod.rs` の `handle_rpc_with_drivers` 内 plugins match(14 arm)の各 arm 本体を、`core/src/server/rpc_plugins.rs` の関数に移す。命名は method 名の snake_case。**本体のロジック・エラー文字列は1文字も変えない**(params 取り出しを Step 1 のヘルパーに置き換えるのは、生成文字列が同一なので可)。代表2本の完成形:

```rust
//! `plugins/*` / `dashboard/*` RPC のメソッド別ハンドラ(params 解釈 →
//! Registry 呼び出し → JSON 整形)。dispatch は `super::handle_rpc_with_drivers`。

use crate::plugin::registry::Registry;
use crate::rpc::params::{param_bool, param_str};
use crate::rpc::render::*;

pub(super) fn set_bus_grant(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let driver = param_str(params, "driver")?;
    let granted = param_bool(params, "granted")?;
    registry
        .set_bus_grant(plugin, driver, granted)
        .map_err(|e| e.to_string())?;
    // `set_sidecar_grant`/`set_filesystem_grant` と同じ流儀: 1 件だけ
    // の grant state を返すのではなく、その plugin の bus 一覧全体を
    // 返す(UI が 1 往復でリスト全体を更新できるように)。
    let bus = registry.bus(plugin).map_err(|e| e.to_string())?;
    Ok(bus_result_json(&bus))
}

pub(super) fn get_sidecars(
    registry: &Registry,
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plugin = param_str(params, "plugin")?;
    let sidecars = registry.sidecars(plugin).map_err(|e| e.to_string())?;
    Ok(sidecars_result_json(&sidecars))
}
```

対象14本: `list`(plugins/list。現在 ~45 行あるので、1要素分の JSON 組み立てを `fn plugin_entry_json(registry: &Registry, info: ...) -> serde_json::Value` に分けて 40 行制限を守る)/ `set_bus_grant` / `set_dashboard_grant` / `dashboard_list` / `get_settings` / `set_settings` / `get_capabilities` / `set_capabilities` / `get_sidecars` / `set_sidecar_config` / `set_sidecar_grant` / `sidecar_control` / `get_filesystem` / `set_filesystem_config` / `set_filesystem_grant`。

arm 側は全て1行になる:

```rust
"plugins/set-bus-grant" => rpc_plugins::set_bus_grant(registry, params),
```

`server/mod.rs` に `mod rpc_plugins;` を追加。Task 3 で置いた `use crate::rpc::render::...` のうち mod.rs 側で使われなくなったものは、tests が `super::*` 経由で参照している分だけ `#[cfg(test)] use ...` に付け替え、それ以外は削除する(clippy unused-imports を出さない)。

- [ ] **Step 3: テスト実行**

Run: `cargo test --workspace 2>&1 | tail -5` / `cargo clippy --workspace 2>&1 | tail -5`
Expected: 全パス。**pin テストと server/tests.rs のエラーメッセージ系テスト(`params.granted must be a bool` 等)が未変更のまま通ることが挙動不変の証明**

- [ ] **Step 4: コミット(logic)**

```bash
git add core/src/server core/src/rpc/params.rs
git commit -m "refactor(core): plugins/* RPC をメソッド単位の小関数へ分解

params 解釈 → Registry 呼び出し → JSON 整形の3段を関数ごとに揃え、
dispatch の match は1行 arm だけにする。エラーメッセージ・応答 JSON は
不変(pin テストと既存テストが担保)。"
```

---

### Task 5: drivers/* の match を同じ形で分解(logic)

**Files:**
- Create: `core/src/server/rpc_drivers.rs`
- Modify: `core/src/server/mod.rs`(`handle_drivers_rpc` の各 arm を1行に)

**Interfaces:**
- Consumes: Task 3 の `rpc::render::*` / Task 4 の `rpc::params::*`
- Produces: `server::rpc_drivers` 内の per-method 関数群(Task 4 と同型)

- [ ] **Step 1: rpc_drivers.rs に per-method 関数を切り出す**

`handle_drivers_rpc` の10 arm を Task 4 と同じ形で `core/src/server/rpc_drivers.rs` へ。第一引数は `drivers: &DriverRegistry`。対象: `list` / `get_settings` / `set_settings` / `set_capabilities` / `set_sidecar_config` / `set_sidecar_grant` / `sidecar_control` / `set_filesystem_config` / `set_filesystem_grant`。`list` の1要素組み立ても `driver_entry_json` に分ける。`handle_drivers_rpc` 自体(prefix 剥がし後 dispatch)と `unknown method: drivers/{other}` のエラーは mod.rs に残す。

`sidecar_control` の action パース(`"start" | "stop" | "restart" | other => Err(unknown action: {other})`)は plugins 側と同一コードが2箇所になるが、**Phase 2 では共通化しない**(plugin/driver の同型コード共通化は Phase 4 のジェネリック化で扱う。ここで独自に共通化すると Phase 4 の設計を先取りして衝突する)。

- [ ] **Step 2: テスト実行**

Run: `cargo test --workspace 2>&1 | tail -5` / `cargo clippy --workspace 2>&1 | tail -5`
Expected: 全パス(pin の drivers/list が形の不変を担保)

- [ ] **Step 3: コミット(logic)**

```bash
git add core/src/server
git commit -m "refactor(core): drivers/* RPC をメソッド単位の小関数へ分解"
```

---

### Task 6: Phase 2 完了ゲート

**Files:** なし(検証のみ)

**Interfaces:**
- Consumes: Task 1–5 の成果
- Produces: Phase 3 着手可能な状態の確認記録

- [ ] **Step 1: 全体検証**

```bash
cargo test --workspace 2>&1 | grep "test result" | grep -v " 0 failed"   # 空 = 全パス
cargo clippy --workspace 2>&1 | tail -5
```

- [ ] **Step 2: 行数の変化を記録**

```bash
wc -l core/src/server/*.rs core/src/rpc/*.rs
```

Expected: `server/mod.rs` が 1855 行から大きく減っている(目安: mod.rs ~450、tests.rs ~900、rpc_plugins ~250、rpc_drivers ~180、rpc/ ~200)。数値を報告に含める

- [ ] **Step 3: テスト凍結の最終確認**

```bash
BASE=$(git merge-base main HEAD)  # ブランチが main 由来でない場合は Phase 2 開始コミットを使う
git diff $BASE..HEAD -- core/tests/ --stat        # rpc_pin_integration.rs の追加のみのはず
git diff $BASE..HEAD -- 'core/src/**' | grep -E '^[-+].*#\[(test|tokio::test|cfg\(test\))' | sort | uniq -c
```

Expected: `core/tests/` の diff は pin テスト新規追加のみ。`#[test]` の増減は server tests の丸ごと移動で説明がつくこと

- [ ] **Step 4: 報告**

Phase 2 の完了をユーザーに報告し、Phase 3(schedule + settings + capability::grants)の計画作成に進む承認を得る。

---

## Self-Review 済み事項

- spec Phase 2 の3要素(pin テスト → match 分解 → `*_result_json` を rpc/ へ、server は配線だけ)→ Task 1 / 4–5 / 3 が対応
- rpc/ の純粋性: render/params は値イン値アウトのみ。データ型 import の rules 乖離は既存 issue に追記して記録(Task 3 Step 4)
- 型整合: Task 4–5 が使う `param_bool`/`param_object` は Task 4 Step 1 で定義。`rpc::render::*` の pub 化は Task 3。`pub(super)` の per-method 関数は server/mod.rs の match からのみ呼ばれる
- エラー文字列の同一性: `param_bool`/`param_object` の `format!` が既存リテラルと一致することを確認済み。config 取り出しはヘルパー化しない理由を明記
- `origin_tests` を動かさない理由(`use super::origin_allowed;` の super が変わる)を Task 2 に明記
- plugins/list・drivers/list の 40 行制限対応(`plugin_entry_json` / `driver_entry_json` 分離)を明記
- sidecar action パースの重複は Phase 4 送りと明記(共通化の先取り禁止)
- Phase 3 以降は別 plan(spec の全 Phase をこの計画が覆うわけではない — 意図的)

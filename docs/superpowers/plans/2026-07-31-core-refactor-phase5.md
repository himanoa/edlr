# core リファクタリング Phase 5(runner + host)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 挙動を一切変えずに、`plugin/runner.rs`(1407行)/`driver/runner.rs`(487行)を `runner/` へ、`plugin/host.rs`(1483行)/`driver/host.rs`(735行)を `host/` へ再配置し、wasmtime 配線(engine/ticker)を `HostCtx` から分離、resolve/check 系とループの strikes 判定を純関数に抽出して plugin/driver の同型コードを共通化する。

**Architecture:** 設計根拠は **必読の事前分析** `docs/superpowers/specs/2026-07-31-phase5-runner-host-analysis.md`(責務インベントリ・同型対応表・リスク台帳。以下「分析」)。spec は `docs/superpowers/specs/2026-07-30-core-refactoring-design.md` Phase 5。移動規律は `.claude/skills/refactor-move-only-commit`。

**Tech Stack:** Rust (cargo workspace)。新規依存 crate の追加は禁止。mockall 禁止(モックは手書き)。

## Global Constraints

- **挙動不変**: エラーメッセージ文字列(分析 §5 リスク3 の 10 種 — "no such sidecar: {name}" / "sidecar not granted: {name}" / "sidecar {name} has no executable configured" / "no such root: {root}" / "filesystem root not granted: {root}" / "root {root} has no directory configured" / "root {root} is read-only" / "capability not granted" / "bus access to {driver} is not granted" / "{driver}/{topic} is not in this plugin's granted bus topics")・副作用の順序(分析 §5 リスク1: bus 登録→スレッド起動、Disabled→bus.disable、購読登録→retained 送信、stop_flag 検査→キュー読み、take_due は Timeout 時のみ)・`Drop` の順序(ticker 停止→`stop_all`)を一切変えない
- **bindgen! の world 別型は共有しない**: `to_wit_*`/`bus_error_to_wit` 等の WIT 写像は複製のまま残す(driver/host.rs 267–273 の既存注記どおり)。共有するのは判定(値イン値アウト)だけ
- **テスト凍結**: 既存テストは丸ごと移動 or import 追従のみ。新規テスト追加は可
- **旧パス温存**: `crate::plugin::runner` / `crate::driver::runner` / `crate::plugin::host` / `crate::driver::host` と全 pub/pub(crate) シグネチャ・WIT 再輸出(plugin/host.rs 46–70)は不変(削除は Phase 6)。`bin/edlr.rs` / `registry/` / `logs.rs` / `core/tests/` の diff はゼロ or import 行のみ
- **コミット分離**: 1コミット = 移動のみ or ロジックのみ。「move+logic」タスクは (a) plugin 側 move-only → (b) driver 側を載せる logic の2コミットに分ける
- **`run_plugin_thread` / `run_driver_thread` は共通化禁止**(分析 §2 — strikes 復帰・schedule・stop 経路の非対称は意図的)。runner の初期バッファ組み立ては **registry の refresh 系と統合しない**(runner.rs 288–293 の意図的重複)
- **ゲート**: タスクごとに `cargo test --workspace` 全パス + `cargo clippy --workspace` 警告0
- **DI の形**: generics + 具象。stores は disk 具象のまま(モック consumer が実証されるまで trait 化しない — trait-di.md)
- 新しい命令的コード: 1関数〜40行・ネスト2段・不要 mut 禁止
- **並列実行の注意**(CLAUDE.md): サブエージェント並列起動前に `cargo fetch`。同一 worktree で cargo 並走禁止。既知 flaky: driver-process AddrInUse(issue oxa3)/ devserver ETXTBSY(issue aenf)は単体再実行で確認

---

### Task 1: 錨テストの追加(driver ctx の判定 pin)

**Files:**
- Modify: `core/src/driver/host.rs`(既存 `mod tests` 内に新テスト関数を追加。既存3テストに触らない)

**Interfaces:**
- Consumes: 既存の `test_driver_ctx` ヘルパー(driver/host.rs 715)— sidecars_json / filesystem_json を差し込めるようコピーして改変した別ヘルパーを足す(既存ヘルパーは変更しない)
- Produces: Task 5(resolve 共通化)の防衛線

- [x] **Step 1: driver ctx の resolve_sidecar / resolve_root / send 判定テストを追加**

分析 §5 リスク4: driver 側 ctx の許可判定に direct テストが無い。plugin/host.rs の同種テスト(`ensure_started_without_grant_is_permission_denied` 1252 / `unknown_root_is_reported_as_such` 1159 / `empty_effective_hosts_means_nothing_is_permitted` 1047 の流儀)を driver ctx 用に写して最低5本:

- `ensure_started` — 未承認 → `PermissionDenied`、未知名 → `UnknownSidecar`、command 空 → `NotConfigured`
- `read` — 未知 root → `UnknownRoot`、read mode への `write` → `PermissionDenied`
- `send` — hosts 空 → `PermissionDenied`

エラーメッセージ文字列も `matches!` でなく中身まで assert する(例: `format!("no such sidecar: {name}")` との等値)— Task 5 で文字列組み立てが resolve.rs へ移った後も byte 同一であることの pin。

- [x] **Step 2: テスト・clippy・コミット**

Run: `cargo test --workspace 2>&1 | tail -5` / `cargo clippy --workspace 2>&1 | tail -5`

```bash
git add core/src/driver/host.rs
git commit -m "test(core): Phase 5 着手前の錨を追加(driver ctx の resolve/permission 判定 pin)

driver 側 DriverCtx の resolve_sidecar / resolve_root / send 許可判定には
direct テストが無かった。Task 5 の判定共通化でエラー文字列を変えない
ための防衛線。"
```

---

### Task 2: runner を `runner/` へ再配置(move-only)

**Files:**
- Move: `core/src/plugin/runner.rs` → `core/src/runner/plugin.rs`
- Move: `core/src/driver/runner.rs` → `core/src/runner/driver.rs`
- Create: `core/src/runner/mod.rs`
- Modify: `core/src/lib.rs`(`pub mod runner;`)/ `core/src/plugin/mod.rs` / `core/src/driver/mod.rs`(旧パス pub use)

**Interfaces:**
- Consumes: なし(先頭タスク)
- Produces: `crate::runner::plugin::start_plugins` / `crate::runner::driver::start_drivers`。旧パス `crate::plugin::runner` / `crate::driver::runner` は Phase 4 Task 9 と同じ形で存続:

```rust
// plugin/mod.rs
// Phase 5 タスク2で runner/plugin.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::runner::plugin as runner;
// driver/mod.rs も同様に pub use crate::runner::driver as runner;
```

- [x] **Step 1**: `git mv` + 配線。ファイル内の相互参照(`crate::plugin::runner::` 表記のドキュメントコメントは触らなくてよい — コメント更新は Phase 6)。`registry/supervisor.rs` の `use crate::plugin::runner::PluginWork` は旧パス pub use で通るため無変更
- [x] **Step 2**: `cargo test --workspace` / clippy。move-only 検証(`git diff --cached --color-moved=dimmed-zebra`、非移動行は mod 宣言・use 文のみ)。テスト凍結チェック(`core/tests/` diff 空)
- [x] **Step 3**: コミット `refactor(core): runner を runner/ へ再配置(移動のみ、旧パスは pub use 温存)`

---

### Task 3: host を `host/` へ再配置(move-only)

**Files:**
- Move: `core/src/plugin/host.rs` → `core/src/host/plugin.rs`
- Move: `core/src/driver/host.rs` → `core/src/host/driver.rs`
- Create: `core/src/host/mod.rs`
- Modify: `core/src/lib.rs`(`pub mod host;`)/ `core/src/plugin/mod.rs` / `core/src/driver/mod.rs`(旧パス pub use)

**Interfaces:**
- Consumes: なし(Task 2 と独立)
- Produces: `crate::host::plugin::{PluginHost, HostCtx, PluginInstance, PluginCallError, ...}` / `crate::host::driver::{DriverHost, DriverCtx, DriverInstance}`。旧パス `crate::plugin::host` / `crate::driver::host` を pub use で温存(WIT 再輸出 `WitSidecarError` 等は移動先ファイル内にそのまま残るので、モジュール pub use だけで旧パスが通る)

- [x] **Step 1**: `git mv` + 配線。`bindgen!({path: "wit"})` は CARGO_MANIFEST_DIR 相対なので無変更で通る(分析 §5 リスク6 — ビルドで確認)。`SIDECAR_SHUTDOWN_GRACE` の `edlr_config` 参照・const assert(93/57)もそのまま移動
- [x] **Step 2**: テスト・clippy・move-only 検証・テスト凍結チェック(`core/tests/driver_http_integration.rs` / `plugin_host_integration.rs` が import 無変更で通ること)
- [x] **Step 3**: コミット `refactor(core): host を host/ へ再配置(移動のみ、旧パスは pub use 温存)`

---

### Task 4: wasmtime 配線の分離 — EpochEngine + SharedDrivers(move → logic の2コミット)

**Files:**
- Create: `core/src/host/engine.rs`、`core/src/host/drivers.rs`
- Modify: `core/src/host/mod.rs` / `core/src/host/plugin.rs` / `core/src/host/driver.rs`

**Interfaces:**
- Consumes: Task 3 の再配置
- Produces(分析 §3):

```rust
// host/engine.rs — wasmtime Engine + epoch ticker の所有(具象・trait 化しない)
pub(crate) struct EpochEngine { engine: Engine, ticker_stop: Arc<AtomicBool> }
impl EpochEngine {
    pub(crate) fn new() -> anyhow::Result<EpochEngine>;   // Config(component_model + epoch_interruption)+ ticker スレッド起動
    pub(crate) fn engine(&self) -> &Engine;
    pub(crate) fn stop_ticker(&self);                     // ticker_stop.store(true, Relaxed)
}
pub(crate) fn deadline_ticks(duration: Duration) -> u64;  // 現 plugin/driver の同名関数(同一実装)
// EPOCH_TICK_INTERVAL 定数もここへ

// host/drivers.rs — http/process/fs の共有 Arc 3点
pub(crate) struct SharedDrivers { http, process, fs }
impl SharedDrivers {
    pub(crate) fn new(http_timeout: Duration) -> anyhow::Result<SharedDrivers>;  // plugin 1.5s / driver 25s を注入
    pub(crate) fn http(&self) -> Arc<HttpDriver>; /* process() / fs() 同様 */
}
```

**厳守**: `EpochEngine` に `Drop` を付けない。各 host の `Drop` は現行の順序(`stop_ticker()` → `process_driver.stop_all()`)を明示的に呼ぶ(分析 §5 リスク2 — フィールド drop は Drop 本体の後なので、EpochEngine 側に Drop を持たせると順序が反転する)。

- [x] **Step 1(move-only コミット)**: plugin 側の engine 構築+ticker(host/plugin.rs 747–762)・driver 3点構築(764–772)・accessor(786–802)・`deadline_ticks`(842)・`EPOCH_TICK_INTERVAL` を `EpochEngine`/`SharedDrivers` へ抽出し、`PluginHost` は `{ engine: EpochEngine, drivers: SharedDrivers }` を持って委譲(pub シグネチャ不変: `new`/`load`/`http_driver`/`process_driver`/`fs_driver`)。`Drop` の順序を目視確認
- [x] **Step 2(logic コミット)**: driver 側(host/driver.rs 521–546, 557–576, 604–618)も同じ `EpochEngine`/`SharedDrivers` に載せ、旧実装を削除。`DRIVER_HTTP_TIMEOUT` を `SharedDrivers::new` に渡す。`load` は各 host に残す(bindgen 型が world 固有)
- [x] **Step 3**: 各コミットでテスト・clippy。コミット例: `refactor(core): wasmtime 配線を host/engine.rs の EpochEngine へ抽出(移動のみ)` / `refactor(core): DriverHost を EpochEngine/SharedDrivers に統合`

---

### Task 5: resolve/check 判定の純関数化 — host/resolve.rs(logic)

**Files:**
- Create: `core/src/host/resolve.rs`
- Modify: `core/src/host/plugin.rs` / `core/src/host/driver.rs`(委譲)/ `core/src/runner/plugin.rs`(`spawn_bus_subscriber` の判定委譲)/ `core/src/host/mod.rs`

**Interfaces:**
- Consumes: `SidecarRuntimeEntry` / `FsRuntimeEntry` / `BusRuntimeEntry`(既存 runtime 型)、`allowlist::check_url`
- Produces(分析 §4 — エラー文字列は**ここで**組み立てて返し、各 ctx は自 world の WIT variant へ写像するだけ):

```rust
pub(crate) enum SidecarResolveError { Unknown(String), NotGranted(String), NotConfigured(String) }
pub(crate) fn resolve_sidecar(entries: &BTreeMap<String, SidecarRuntimeEntry>, name: &str)
    -> Result<edlr_driver_process::ProcessSpec, SidecarResolveError>;

pub(crate) enum RootResolveError { Unknown(String), NotGranted(String), NotConfigured(String), ReadOnly(String) }
pub(crate) fn resolve_root(entries: &BTreeMap<String, FsRuntimeEntry>, root: &str, need_write: bool)
    -> Result<std::path::PathBuf, RootResolveError>;

pub(crate) fn check_http_permission(hosts: &[String], url: &str) -> Result<(), String>;

pub(crate) enum BusDirection { Publish, Subscribe }   // 現 host/plugin.rs 417 から移す
pub(crate) fn check_bus_permission(entries: &BTreeMap<String, BusRuntimeEntry>, driver: &str, topic: &str, direction: BusDirection)
    -> Result<(), String>;
```

- [x] **Step 1**: 上記4関数を実装し、`HostCtx`(resolve_sidecar 475 / resolve_root 611 / send の許可部 349–361 / check_bus 427)と `DriverCtx`(288 / 390 / 213–225)を委譲に置換。各 enum variant → WIT variant の写像は ctx 側の小関数。**エラー文字列 byte 同一 — Task 1 の錨と plugin 側既存テスト(凍結)が防衛**
- [x] **Step 2**: `runner/plugin.rs` の `spawn_bus_subscriber` still_granted 判定(996–1010)を `check_bus_permission(..., Subscribe).is_ok()` に置換(ドキュメントコメント 966–971 が「`check_bus` と同じ判定材料・同じ判定規則」と明記している対) 
- [x] **Step 3**: `resolve.rs` 内 `#[cfg(test)] mod tests` に純粋テストを追加(各関数 3 本以上: 正常系・拒否系・エラー文字列の等値)
- [x] **Step 4**: テスト・clippy。コミット `refactor(core): host の許可判定を resolve.rs の純関数へ抽出(エラー文字列は判定側で一元化)`

---

### Task 6: runner 初期バッファ組み立ての共通化(logic)

**Files:**
- Create: `core/src/runner/bootstrap.rs`
- Modify: `core/src/runner/mod.rs` / `core/src/runner/plugin.rs` / `core/src/runner/driver.rs`

**Interfaces:**
- Consumes: `registry::subject::RegistrySubject`(Phase 4 Task 4 — `id()` / `sidecars()` / `filesystem()` / `as_settings_manifest()`)、disk stores(具象のまま)
- Produces:

```rust
/// 起動直後の共有 JSON バッファ初期値。plugin/driver 共通部
/// (settings / sidecars / capabilities / filesystem)。bus は plugin 専用
/// なので呼び出し側(runner/plugin.rs)が別途組み立てる。
pub(crate) struct InitialBuffers {
    pub(crate) settings_json: Arc<Mutex<String>>,
    pub(crate) capabilities_json: Arc<Mutex<String>>,
    pub(crate) sidecars_json: Arc<Mutex<String>>,
    pub(crate) filesystem_json: Arc<Mutex<String>>,
}
pub(crate) fn build_initial_buffers<S: RegistrySubject>(
    subject: &S,
    settings_store: &SettingsStore,
    grants_store: &GrantsStore,
    sidecar_config_store: &SidecarConfigStore,
    filesystem_config_store: &FilesystemConfigStore,
) -> InitialBuffers;
```

- [x] **Step 1**: `load_and_run_plugin`(runner/plugin.rs 280–352)と `load_and_run_driver`(runner/driver.rs 179–244)の共通部を `build_initial_buffers` に一本化。plugin の bus_entries(360–373)は plugin 側に残す。**registry の refresh 系(SidecarService::refresh_sidecar_runtime 等)とは統合しない**(Global Constraints)。implicit_http_hosts のマージ順(capability hosts → extend(implicit))を変えない
- [x] **Step 2**: `bootstrap.rs` に純粋寄りのテストを追加(tempdir の disk store を使い、granted/ungranted × 設定有無で JSON 初期値の等値を確認 — 2 本以上。既存の統合テストは凍結のまま)
- [x] **Step 3**: テスト・clippy。コミット `refactor(core): runner の初期バッファ組み立てを RegistrySubject で共通化`

---

### Task 7: ループ判定の拡大 — deadline_verdict(logic)

**Files:**
- Modify: `core/src/runner/plugin.rs`

**Interfaces:**
- Consumes: `CALL_DEADLINE_STRIKES`(既存定数)
- Produces:

```rust
/// 期限超過が strikes 回連続したときの扱い。判定だけを純関数にし、
/// 制御フロー(continue/break)と reason 文字列の組み立ては
/// handle_call_result! マクロに残す。
#[derive(Debug, PartialEq)]
enum DeadlineVerdict { Restart, GiveUp }
fn deadline_verdict(strikes: u32) -> DeadlineVerdict;   // strikes >= CALL_DEADLINE_STRIKES → GiveUp
```

- [x] **Step 1**: `handle_call_result!`(626–650)の `if deadline_strikes >= CALL_DEADLINE_STRIKES` 分岐を `deadline_verdict(deadline_strikes)` の match に置換。reason 文字列・ログ・`load_instance()` 作り直し・`continue`/`break` は 1 行も変えない
- [x] **Step 2**: `next_action_tests` と同じ流儀で `deadline_verdict` の純粋テストを追加(境界 3 本: `STRIKES-1` → Restart、`STRIKES` → GiveUp、`STRIKES+1` → GiveUp)
- [x] **Step 3**: テスト・clippy。コミット `refactor(core): 期限超過 strikes の判定を deadline_verdict 純関数へ抽出`

---

### Task 8: Phase 5 完了ゲート

- [x] **Step 1**: `cargo test --workspace`(全パス・pin 含む)+ clippy 0
- [x] **Step 2**: 行数記録: `wc -l core/src/runner/*.rs core/src/host/*.rs` と旧4ファイル(plugin/runner.rs, driver/runner.rs, plugin/host.rs, driver/host.rs)の残骸が無いこと
- [x] **Step 3**: テスト凍結確認: `git diff e029b6e..HEAD -- core/tests/ --stat` が空 + `#[cfg(test)]` 差分が「丸ごと移動 + 新規追加(Task 1/5/6/7)」で説明できること
- [x] **Step 4**: 不変条件の再確認を報告に含める: (a) `PluginHost`/`DriverHost` の `Drop` が `stop_ticker()` → `stop_all()` の順であること(目視)、(b) driver の `bus.register_driver` がスレッド起動前のままであること、(c) エラー文字列 10 種が resolve.rs に集約され Task 1 の錨が通ること
- [x] **Step 5**: ユーザーへ報告し、Phase 6(仕上げ: journal 等の中規模ファイル + 旧パス一括削除)の計画作成に進む承認を得る

---

## Self-Review 済み事項

- spec Phase 5 の全要素をカバー: 「ループ判定の関数抽出を拡大」= Task 5(check_bus_permission の subscriber 共有)+ Task 7(deadline_verdict)。fire_all_due は既に関数、shutdown 系は Phase 4 で ThreadSupervisor へ移設済み(分析冒頭に明記)。「wasmtime 配線と HostCtx の分離」= Task 4(EpochEngine/SharedDrivers)+ Task 5(判定の純関数化で HostCtx が写像だけに痩せる)
- モジュール表(`.claude/rules/module-layout.md`)が約束する `runner/`・`host/` トップレベル化 = Task 2/3
- bindgen! world 別型の制約により WIT 写像(`to_wit_*` 等)は複製温存 — Global Constraints に明記(既存コードの注記と一致)
- リスク台帳の全項目に手当て: リスク4(driver ctx テスト不在)→ Task 1 で先に錨。リスク2(Drop 順序)→ Task 4 の厳守事項 + Task 8 Step 4
- `run_plugin_thread`/`run_driver_thread` の非共通化・初期バッファと registry refresh の非統合を Global Constraints に明記(意図的重複の温存)
- move+logic タスク(Task 4)の2コミット分割で移動規律とレビュー可能性を両立
- Phase 6 は別 plan(意図的)

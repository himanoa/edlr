# core リファクタリング Phase 4(registry 解体)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 挙動を一切変えずに、`plugin/registry.rs`(3417行)と `driver/registry.rs`(1723行)の神オブジェクトを `EntryTable` / `ThreadSupervisor` / `FilesystemService` / `BusService` / `SidecarService` / `GrantService` に分解し、plugin/driver の同型コード(約1100行)をジェネリック共通化して、`Registry` / `DriverRegistry` を薄い facade にする。

**Architecture:** 設計根拠は **必読の事前分析** `docs/superpowers/specs/2026-07-31-phase4-registry-analysis.md`(責務インベントリ・ロック規律・同型対応表・リスク台帳。以下「分析」)。spec は `docs/superpowers/specs/2026-07-30-core-refactoring-design.md`。移動規律は `.claude/skills/refactor-move-only-commit`。

**Tech Stack:** Rust (cargo workspace)。新規依存 crate の追加は禁止。mockall 禁止(モックは手書き)。

## Global Constraints

- **挙動不変**: RPC 応答 JSON・エラーメッセージ文字列(`RegistryError`/`DriverRegistryError` の `Display`、`"plugin {id} is disabled"` / `"driver {id} is disabled"` の主語差を含む)・grant/設定ファイル形式・副作用の順序(stop→バッファ書換、Disabled→stop、driver set_disabled の bus.disable 先行)を一切変えない
- **ロック規律の維持**(分析 §2): `entries` → runtime-lock map → per-id lock → `capabilities_lock` の一方向順序。3つの per-id map の分離(fail-open 回避)を統合しない。`SidecarService` と `GrantService` は**同一の** `capabilities_lock` Arc を共有する
- **テスト凍結**: 既存テストは丸ごと移動 or import 追従のみ。pin テスト(rpc_pin_integration.rs)が落ちたら実装を戻す。新規テスト追加は可
- **旧パス温存**: `crate::plugin::registry::*` / `crate::driver::registry::*` と全 pub/pub(crate) シグネチャは不変(削除は Phase 6)。`runner.rs` / `bin/edlr.rs` / `server/` の diff はゼロ or import 行のみに保つ
- **コミット分離**: 1コミット = 移動のみ or ロジックのみ。「move+generic」タスクは (a) plugin 側の move-only 抽出 → (b) driver 側を載せる logic の2コミットに分ける
- **`set_disabled` はジェネリック化禁止**(分析 §3 — plugin/driver の非対称は意図的な race 修正)
- **ゲート**: タスクごとに `cargo test --workspace` 全パス + `cargo clippy --workspace` 警告0
- **DI の形**: generics + type alias(`type DiskGrantService = GrantService<GrantsStore>`)。`dyn Trait` を標準にしない。trait は必要が実証された分だけ
- 新しい命令的コード: 1関数〜40行・ネスト2段・不要 mut 禁止
- **並列実行の注意**(CLAUDE.md): サブエージェント並列起動前に `cargo fetch`。cargo の並走禁止。既知 flaky: driver-process AddrInUse(issue oxa3)/ devserver ETXTBSY(issue aenf)は単体再実行で確認

---

### Task 1: 錨テストの追加(render Interval + driver secret 非剥がし pin)

**Files:**
- Modify: `core/src/rpc/render.rs`(既存 `mod tests` に**追記はせず**、同モジュール内に新テスト関数を追加してよい — Phase 3 で足した tests モジュールは凍結対象ではなく本 Phase の追記可。ただし既存テストの変更は禁止)
- Modify: `core/tests/rpc_pin_integration.rs`(新テスト関数の追加のみ。既存3テストに触らない)

**Interfaces:**
- Consumes: 既存ハーネス(rpc_pin_integration.rs 内)
- Produces: Task 8(values 共通化)の防衛線

- [x] **Step 1: `ScheduleSpec` の Interval variant の render テスト**

`schedules_result_json` は現在 Cron のみテスト済み。Interval variant(`display_string()` が `"every {n}s"` 形式 — `core/src/plugin/manifest/mod.rs` の `ScheduleSpec` 定義で正確な variant 名と表示形式を確認して合わせる)のテストを1本追加。固定タイムスタンプの流儀は既存テストと同じ。

- [x] **Step 2: driver settings の secret 非剥がし pin**

分析 §6 リスク4: driver の `values`/`set_values` は plugin と違い secret を剥がさない(**現状この挙動を固定するテストがない**)。`rpc_pin_integration.rs` に、Secret 型 setting を持つ fixture driver で `drivers/set-settings` → 応答に secret の生値がそのまま入ることを whole-JSON 等値で pin するテストを追加。fixture は既存の ed-state driver manifest に `[[settings]]`(kind = "secret")を足した別ディレクトリを組む(既存 fixture 関数はコピーして改変。既存テストに触らない)。

- [x] **Step 3: テスト・clippy・コミット**

Run: `cargo test --workspace 2>&1 | tail -5` / `cargo clippy --workspace 2>&1 | tail -5`

```bash
git add core/src/rpc/render.rs core/tests/rpc_pin_integration.rs
git commit -m "test(core): Phase 4 着手前の錨を追加(schedule Interval の render / driver secret 非剥がしの pin)

driver の values/set-settings は plugin と違い secret を剥がさない。
Task 8 のジェネリック共通化でこの差分を消さないための防衛線。"
```

---

### Task 2: `registry/entries.rs` — 共有 EntryTable(move-only)

**Files:**
- Create: `core/src/registry/entries.rs`
- Modify: `core/src/registry/mod.rs`(`pub(crate) mod entries;`)
- Modify: `core/src/plugin/registry.rs` / `core/src/driver/registry.rs`(委譲)

**Interfaces:**
- Consumes: `PluginEntry` / `DriverEntry`(それぞれの registry に残る)
- Produces: `EntryTable<E>` — 以降の全サービスタスクが consume。API(分析 §5):

```rust
pub(crate) struct EntryTable<E> { entries: Arc<Mutex<Vec<E>>> }
// push / with_entries(スナップショット用クロージャ) / find(id → clone 系)/
// is_disabled / set_state など、両 registry の entries 直接操作を過不足なく吸収する
// (実際のメソッド集合は現行コードの entries 使用箇所から逆算し、増やさない)
pub(crate) struct IdLocks { /* HashMap<String, Arc<Mutex<()>>> の lock_for パターン */ }
```

- [x] **Step 1**: 両 registry の `entries` 直接アクセスと `lock_for` ヘルパー(plugin 951 / driver 482)を洗い出し、`EntryTable<E>` と `IdLocks` に移す。両 registry のフィールドを `EntryTable<PluginEntry>` / `EntryTable<DriverEntry>` と `IdLocks` ×3(sidecar/fs/bus — driver は2)に置換し、全メソッドは委譲で不変。ロック保持区間(entries は clone 取得のみで手放す)を変えない
- [x] **Step 2**: `cargo test --workspace` / clippy。multiset 検証(非移動行は委譲呼び出しへの置換のみ — 「移動のみ」の範囲として、抽出に伴う機械的な `self.entries.lock()` → `self.entries.with(...)` 形の置換は許容し、判断ロジックの変更はゼロであることをレビューで示す)
- [x] **Step 3**: コミット `refactor(core): 両 registry の entries 操作を registry/entries.rs の EntryTable へ抽出(移動のみ)`

---

### Task 3: `registry/supervisor.rs` — ThreadSupervisor(move-only)

**Files:**
- Create: `core/src/registry/supervisor.rs`
- Modify: `core/src/registry/mod.rs` / `core/src/plugin/registry.rs`

**Interfaces:**
- Consumes: Task 2 の EntryTable(不要なら消費しない)
- Produces: `ThreadSupervisor`(具象・trait 化しない)。API は分析 §4 のとおり:

```rust
register_thread(id, work_tx, JoinHandle, stop_flag)   // 現 register_plugin_thread
register_schedule_view(id, ScheduleView)
register_drop_counters(id, Arc<DropCounters>)
dropped_counts(id) -> DroppedCounts
published_schedule(id) -> ...                          // build_schedule_infos の読み側
shutdown_all()                                         // 現 shutdown_plugins の2段(signal → 共有 deadline)
shutdown_bus_subscribers() / shutdown_flag() -> Arc<AtomicBool>
```

- [x] **Step 1**: `plugin_threads` / `PluginThreadHandle`(379–387)/ `PLUGIN_STOP_JOIN_TIMEOUT`・`POLL_INTERVAL`(38–45)/ `bus_subscriber_shutdown` / `drop_counters` / `schedule_views` と対応メソッド本体を supervisor.rs へ移動。**shutdown の2段構造(全スレッドへ signal → 共有 deadline で join)を1行も変えない**(リスク7 — 守るテスト: `shutdown_plugins_*` ≈3211/3249/3294、daemon_signal_shutdown_integration.rs)。Registry は `supervisor: ThreadSupervisor` を持ち全メソッドを委譲(pub(crate) シグネチャ不変 → runner.rs / bin の diff ゼロ)
- [x] **Step 2**: テスト・clippy・multiset 検証
- [x] **Step 3**: コミット `refactor(core): スレッド監督を registry/supervisor.rs の ThreadSupervisor へ抽出(移動のみ)`

---

### Task 4: `registry/filesystem.rs` — FilesystemService(move → generic の2コミット)

**Files:**
- Create: `core/src/registry/filesystem.rs`、`core/src/registry/subject.rs`(RegistrySubject trait)
- Modify: `core/src/registry/mod.rs` / 両 registry

**Interfaces:**
- Consumes: `capability::GrantStorage`(Phase 0 trait — **初の consumer**)、Task 2 の EntryTable/IdLocks
- Produces:
  - `registry::subject::RegistrySubject` trait: `id()` / `sidecars()` / `filesystem()` / `as_settings_manifest(&self) -> Manifest`(plugin は clone)/ `unknown_error(id) -> RegistryError`(`UnknownPlugin` vs `UnknownDriver`)/ `subject_noun() -> &'static str`("plugin"/"driver")
  - `FilesystemService<G: GrantStorage>`: `filesystem` / `set_filesystem_config` / `set_filesystem_grant` / `refresh_filesystem_runtime` / `build_filesystem_infos` を subject ジェネリックで
  - 以降のタスクが同じパターン(subject trait + generic service)を踏襲する

- [x] **Step 1(move-only コミット)**: plugin 側の fs 群(1189–1197, 1262–1367, 726–753)を `FilesystemService`(この時点では `GrantsStore` 具象)として抽出、plugin Registry は委譲。エラー文字列 byte 同一
- [x] **Step 2(logic コミット)**: `RegistrySubject` を導入して `Manifest`(plugin)と `DriverManifest`(driver)に impl、service を `<G: GrantStorage, S: RegistrySubject>` 化して driver 側の fs 群(509–606)も同じ service に載せ、driver Registry の旧実装を削除・委譲に置換。**分析 §3 のとおりこのペアはエラー文字列含め byte 同一** — driver 側の旧コードと service の出力が一致することを、既存 driver fs テスト(凍結)の通過で示す
- [x] **Step 3**: 各コミットでテスト・clippy。コミット例: `refactor(core): filesystem 群を FilesystemService へ抽出(移動のみ)` / `refactor(core): FilesystemService を RegistrySubject でジェネリック化し driver 側を統合`

---

### Task 5: `registry/bus.rs` — BusService(move-only)

**Files:**
- Create: `core/src/registry/bus.rs`
- Modify: `core/src/registry/mod.rs` / `core/src/plugin/registry.rs`

**Interfaces:**
- Consumes: `capability::GrantStorage`、EntryTable/IdLocks、`DriverRegistry`(resolved 判定用 clone)
- Produces: `BusService<G: GrantStorage>`(plugin 専用 — driver 側に bus grant は存在しない。分析 §5)。`bus` / `set_bus_grant` / `refresh_bus_runtime` / `build_bus_infos` / `bus_buffer`

- [x] **Step 1**: 上記を move-only で抽出、委譲化。`bus_runtime_locks` は service 所有。`shutdown_bus_subscribers` は Task 3 で Supervisor に移動済み — ここに含めない
- [x] **Step 2**: テスト・clippy・multiset 検証
- [x] **Step 3**: コミット `refactor(core): bus 群を BusService へ抽出(移動のみ)`

---

### Task 6: `registry/sidecar.rs` — SidecarService(最高リスク。move → generic の2コミット)

**Files:**
- Create: `core/src/registry/sidecar.rs`
- Modify: `core/src/registry/mod.rs` / 両 registry

**Interfaces:**
- Consumes: `capability::GrantStorage`、`registry::ProcessControl`(Phase 0 trait — **初の consumer**)、`RegistrySubject`(Task 4)、EntryTable/IdLocks
- Produces: `SidecarService<G: GrantStorage, P: ProcessControl>`: `sidecars` / `set_sidecar_config` / `set_sidecar_grant` / `control_sidecar` / `stop_all` / `stop_named(id, names)`(facade の `set_disabled` 用)/ `refresh_sidecar_runtime` / `build_sidecar_infos` / `sidecar_info_and_entry` / `sidecar_key`

**リスク台帳(分析 §6)の 1/2/3/5 が全部ここ。厳守事項:**
- `capabilities_lock` は**コンストラクタで注入される共有 Arc**(Task 7 の GrantService と同一実体)。`refresh_sidecar_runtime` の step 3(capabilities_json 書換)の順序・保持区間を不変に
- `control_sidecar` の TOCTOU 対策と `"{noun} {id} is disabled"` の主語は `subject_noun()` で分岐(文字列 byte 同一)
- stop → バッファ書換の順序不変
- 守るテスト: `concurrent_control_sidecar_start_and_grant_revoke_...`(≈2938)、`revoking_filesystem_access_is_not_blocked_...`(≈2675)、`set_capabilities_persists_grant_and_updates_shared_capabilities_json`(≈3022)、`concurrent_set_capabilities_keeps_shared_buffer_...`(≈3188)、driver 側 1227+ — 全て凍結のまま通過すること

- [x] **Step 1(move-only)**: plugin 側 sidecar 群を `SidecarService`(具象)へ抽出・委譲化
- [x] **Step 2(logic)**: `<G, P, S: RegistrySubject>` 化して driver 側(607–830, 218–280, 906)を統合。`ProcessControl` 経由の呼び出しに置換(`ensure_started`/`status`/`stop`/`stop_all` — `stop_detached` は trait 外なので、使用箇所があれば具象 `Arc<ProcessDriver>` のまま残しその旨を報告)
- [x] **Step 3**: 各コミットでテスト・clippy。`refactor(core): sidecar 群を SidecarService へ抽出(移動のみ)` / `refactor(core): SidecarService をジェネリック化し driver 側を統合`

---

### Task 7: `registry/grants.rs` — GrantService + dashboard(move-only)

**Files:**
- Create: `core/src/registry/grants.rs`
- Modify: `core/src/registry/mod.rs` / `core/src/plugin/registry.rs` / `core/src/driver/registry.rs`

**Interfaces:**
- Consumes: `capability::GrantStorage`、EntryTable、共有 `capabilities_lock`(Task 6 と同一 Arc)
- Produces: `GrantService<G: GrantStorage>`: `capabilities` / `set_capabilities`(plugin 版。driver 版の統合は Task 8)/ `effective_hosts` + **dashboard 群**(`dashboard` / `set_dashboard_grant` / `dashboard_widgets_for_ui` / `dashboard_asset_path` / `events_of` / `build_dashboard_infos` — 分析 §5: 実体は grants + is_file 1発なのでここに置く)。`type DiskGrantService = GrantService<GrantsStore>`

- [x] **Step 1**: move-only 抽出・委譲化。`set_capabilities` が live な `sidecars_json` バッファを読む挙動(1116–1124)を**再計算に「直さない」**(リスク2 — 意図的挙動)
- [x] **Step 2**: テスト・clippy・multiset 検証
- [x] **Step 3**: コミット `refactor(core): capabilities/dashboard 群を GrantService へ抽出(移動のみ)`

---

### Task 8: values / set_values / set_capabilities の内部共通化(logic)

**Files:**
- Modify: `core/src/registry/grants.rs`(driver set_capabilities の統合)、新規 or 既存の適所に settings 共通ヘルパー
- Modify: 両 registry(委譲先の切替)

**Interfaces:**
- Consumes: `settings::Storage`(Phase 0 trait — **初の consumer**)、`settings::store::split_secrets`
- Produces: 共通内部関数 + 側ごとの薄い wrapper。**wrapper に残すもの**(分析 §3): plugin 側の `split_secrets` 適用、`RegistryError` vs `DriverRegistryError` の写像、unknown-id エラーの主語

- [x] **Step 1**: plugin `values`/`set_values`(977–1082)と driver 版(329–377)の共通部(effective 取得・update_and_effective 呼び出し)を1本化。**driver 側が secret を剥がさない挙動は Task 1 の pin が防衛** — pin が落ちたら実装を戻す
- [x] **Step 2**: `set_capabilities` の driver 版(379–445)を GrantService に統合(projection は `RegistrySubject::as_settings_manifest`、implicit hosts マージの読み元バッファ挙動は側ごとに現状維持)
- [x] **Step 3**: テスト・clippy。コミット `refactor(core): settings/capabilities の plugin/driver 同型コードを共通化(secret 剥がしとエラー写像は wrapper 温存)`

---

### Task 9: facade を `registry/` へ再配置(move-only)

**Files:**
- Move: `core/src/plugin/registry.rs` → `core/src/registry/plugin.rs`(tests は `registry/plugin/tests.rs` に分離してよい — 丸ごと移動)
- Move: `core/src/driver/registry.rs` → `core/src/registry/driver.rs`(同上)
- Modify: `core/src/registry/mod.rs` / `core/src/plugin/mod.rs` / `core/src/driver/mod.rs`(旧パス pub use)

**Interfaces:**
- Consumes: Task 2–8 の全サービス
- Produces: `registry::plugin::Registry` / `registry::driver::DriverRegistry`。旧パス `crate::plugin::registry` / `crate::driver::registry` は `pub use crate::registry::plugin as registry;` 形で存続(可視性は現行に合わせる — Phase 3 Task 4 の pub(crate) 温存判断と同じ流儀)

- [x] **Step 1**: `git mv` + 配線。この時点で facade に残っているのは分析 §5 の表のとおり(new / list / snapshot / dirs / manifest_of / push / entry_settings / set_disabled / stop_all_sidecars / 委譲群)であることを確認し、残数を報告
- [x] **Step 2**: テスト・clippy・multiset 検証(統合テストの import が無変更で通ること)
- [x] **Step 3**: コミット `refactor(core): Registry facade を registry/ へ再配置(移動のみ、旧パスは pub use 温存)`

---

### Task 10: 判断関数の抽出とモック純粋テスト(logic)

**Files:**
- Modify: `core/src/registry/sidecar.rs` / `grants.rs`(判断の純関数抽出)
- Create: 各サービスの `#[cfg(test)] mod test_support`(手書きモック)

**Interfaces:**
- Consumes: これまでの全成果
- Produces: spec の「モックによる純粋テスト」の初実装。`ProcessControl` / `GrantStorage` の**モック consumer が初めて実在**する

- [x] **Step 1**: sidecar の起動前提判定(granted・executable 設定済み・disabled でない、の判定部)と `stop_named` の対象決定、`effective_hosts` のマージ判定を、名前付き純関数(値イン値アウト)に抽出。命令的関数には手順の羅列だけ残す(procedure-style.md)。**エラー文字列は関数抽出後も呼び出し側で組み立てが変わらないこと**
- [x] **Step 2**: `test_support` に `InMemoryGrantStorage`(HashMap)と `FakeProcessControl`(呼び出し記録 + 固定応答)を手書きし、抽出した判定と service の代表フロー(set_sidecar_grant の grant→refresh、control_sidecar の disabled 拒否)を tempdir・実プロセスなしでテストする(各3本以上)
- [x] **Step 3**: テスト・clippy。コミット `refactor(core): sidecar/grants の判断を純関数へ抽出しモック純粋テストを追加`

---

### Task 11: Phase 4 完了ゲート

- [x] **Step 1**: `cargo test --workspace`(全パス・pin 含む)+ clippy 0
- [x] **Step 2**: 行数記録: `wc -l core/src/registry/*.rs core/src/registry/**/*.rs` と旧2ファイルの残骸が無いこと。facade の行数(目安: plugin facade ~600、driver facade ~300)を報告
- [x] **Step 3**: テスト凍結確認: `git diff $(git merge-base main HEAD)..HEAD -- core/tests/ --stat`(Task 1 の pin 追加のみ)+ `#[cfg(test)]` 差分が「丸ごと移動 + 新規追加」で説明できること
- [x] **Step 4**: ロック規律の再確認: `capabilities_lock` の Arc が SidecarService と GrantService で同一実体であること(コンストラクタの配線を目視)を報告に含める
- [x] **Step 5**: ユーザーへ報告し、Phase 5(runner + host)の計画作成に進む承認を得る

---

## Self-Review 済み事項

- spec Phase 4 の全要素(5サービス分解・facade 薄化・ジェネリック共通化・ThreadSupervisor 境界確定)を Task 2–9 がカバー。spec 未記載だが必須の EntryTable(分析 §5)を Task 2 として先行
- dashboard の置き場(spec に無い)は分析 §5 の判断どおり GrantService へ
- `set_disabled` の非ジェネリック維持・`list`/`snapshot` の facade 残留を Global Constraints と Task 9 に明記
- Phase 0 trait 4本のうち `GrantStorage`(Task 4)/ `ProcessControl`(Task 6)/ `settings::Storage`(Task 8)が consumer を得る。`BusPort` は select_options::resolve の呼び出し形が `list` 内にあり、Phase 4 では facade に残る — consumer 化は必要が生じた時点(推測で切らない)
- リスク台帳の全項目に守るテストを対応付け(リスク4のみテスト不在 → Task 1 で先に pin)
- move+generic タスクの2コミット分割で移動規律とレビュー可能性を両立
- Phase 5 以降は別 plan(意図的)

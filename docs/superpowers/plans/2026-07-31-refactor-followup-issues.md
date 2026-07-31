# リファクタ起票 issue 一掃(pagd / yzyv / 99dq / l051 / kgc6 / upfj)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** core リファクタリング(Phase 0–6)中に起票した6 issue を解消する — docs 修正(pagd)、テストハーネス集約(yzyv)、純粋/命令的境界の逆辺解消と rules 明文化(99dq)、rpc→registry の *Info import 解消(l051)、Phase 4 残件の実装2点+容認4点(kgc6)、fmt 済み確認クローズ(upfj)。

**Architecture:** 各 issue 本文(`git issues show <id>`)が要求仕様。リファクタは完了済みだが、**RPC 応答 JSON・エラー Display 文字列・公開 API の挙動不変は引き続き維持**する。テスト凍結は解除済み — ただし既存テストのアサーション変更は原則せず、変更する場合は理由をレポートに明記。

**Tech Stack:** Rust (cargo workspace)。新規依存 crate 追加禁止。mockall 禁止。

## Global Constraints

- **挙動不変**: RPC 応答 JSON(pin テストが防衛)・エラー Display 文字列 byte 同一・公開関数シグネチャの互換(移動時は pub use)
- **依存方向**: 純粋→命令的 import 禁止 / manifest→capability の一方向(逆辺を作らない)。`.claude/rules/` の規約に従う
- **ゲート**: タスクごとに `cargo test --workspace` 全パス + `cargo clippy --workspace` 警告0 + `cargo fmt --check` 差分0(so6b 解消後の状態を維持)
- **issue クローズ**: 各タスク完了時に該当 issue へ解決内容を追記してクローズ(`GIT_EDITOR=true git issues close <id>`)— Task 6 でまとめてでも可
- **並列実行の注意**(CLAUDE.md): cargo 並走禁止。既知 flaky(oxa3/aenf)は単体再実行で確認

---

### Task 1: pagd — doc コメント修正(docs-only)

**Files:**
- Modify: `core/src/rpc/render.rs`(doc 付着2箇所)/ `core/src/bin/edlr.rs:252`(旧参照)

**Interfaces:** なし(コメントのみ)

- [x] **Step 1**: issue `rpc-render-rs-doc-2-edlr-rs-docs-only-pagd` 本文のとおり: (1) `bus_result_json` の doc ブロックが `dashboard_result_json` に付着 → 正しい関数直上へ移す(render.rs 14–25 相当。現在の行位置は要確認)。(2) `schedules_result_json` の doc が `dropped_result_json` に融合 → 分離して各関数の直上へ。(3) edlr.rs:252 の「`server::bus_result_json` のドキュメント参照」を `rpc::render::bus_result_json` に更新。**doc の記述内容は保全**(付着位置の修正と参照パス更新のみ)
- [x] **Step 2**: `cargo test --workspace` / clippy / fmt --check。コミット: `docs(core): render.rs の doc 付着2箇所を正しい関数へ移し edlr.rs の旧パス参照を更新(issue pagd)`

---

### Task 2: yzyv — WS ハーネス重複を support/ へ集約(test)

**Files:**
- Modify: `core/tests/support/mod.rs`(WS ハーネス関数の追加)
- Modify: `core/tests/rpc_pin_integration.rs` / `core/tests/ws_rpc_integration.rs`(重複定義を削除し support:: 呼び出しへ)

**Interfaces:**
- Produces: `support::ws` 系ヘルパー(`setup` / `connect` / `recv_hello` / `recv_json` / `send_rpc` と fixture ビルダのうち両ファイルで重複しているもの)

- [x] **Step 1**: 両ファイルの重複関数(issue yzyv: 約150行 — setup/connect/recv_json/send_rpc と fixture ビルダ)を diff で特定し、**完全一致しているものだけ** support/mod.rs へ移す(片側にしか無い・微妙に異なるヘルパーは触らない。無理に統一しない)。両テストファイルは `mod support;` 経由で呼ぶ形に置換
- [x] **Step 2**: **テストのアサーション・テストデータ・テスト名は不変**。pin テスト(rpc_pin_integration.rs)が全て従来どおり通ること
- [x] **Step 3**: `cargo test --workspace` / clippy / fmt --check。コミット: `test(core): WS ハーネスの重複を tests/support へ集約(issue yzyv)`

---

### Task 3: 99dq — 境界逆辺の解消と rules 明文化(logic + docs)

**Files:**
- Modify: `core/src/capability/validate.rs` / `core/src/manifest/mod.rs`(逆辺1)
- Modify: `core/src/settings/store.rs` / `core/src/settings/mod.rs` / `core/src/settings/validate.rs`(逆辺2)
- Modify: `.claude/rules/module-layout.md` / `.claude/rules/pure-imperative-boundary.md`(公認例外の明文化)

**Interfaces:**
- Produces: `capability::validate::is_valid_id`(manifest は `pub use crate::capability::validate::is_valid_id;` で旧パス温存)、`settings::SettingsError`(store.rs は `pub use super::SettingsError;` で旧パス温存)

- [x] **Step 1(逆辺1: capability/validate → manifest)**: (a) `is_valid_id` を manifest から capability/validate.rs へ移し、manifest 側は pub use(公開パス `crate::manifest::is_valid_id` 不変)。(b) `validate_bus` / `validate_widget_entry` の戻りを `Result<(), String>` に変え、`ManifestError::BadBus` / `BadDashboard` への wrap は呼び出し側(manifest/mod.rs)で行う — validate_bus 内のエラーは全て BadBus、validate_widget_entry は全て BadDashboard なので `.map_err(ManifestError::BadBus)` 形で機械的に写せる。**エラーメッセージ文字列は1バイトも変えない**(既存 manifest テストが防衛)。これで capability/validate.rs から `use crate::manifest` が消える
- [x] **Step 2(逆辺2: settings/validate → store)**: `SettingsError` enum を settings/store.rs から settings/mod.rs へ移動(Display 含め byte 同一)。store.rs に `pub use super::SettingsError;`(旧パス `crate::settings::store::SettingsError` 温存)。validate.rs は `crate::settings::SettingsError` を使う
- [x] **Step 3(rules 明文化)**: `pure-imperative-boundary.md` に公認例外を追記: 「純粋モジュールの Storage trait に対する**ディスク実装ファイル**(`capability/grants.rs`・`settings/store.rs`・`settings/filesystem.rs`・`settings/sidecar.rs`)は `manifest::load_manifest` と同格の "I/O は端に" の例外。ここ以外の純粋モジュール内 I/O はレビューで弾く」。module-layout.md の capability/settings 行にも同旨の注記
- [x] **Step 4**: `cargo test --workspace`(manifest のエラーメッセージ系テストが全て通ること)/ clippy / fmt --check。コミット: `refactor(core): capability/settings の依存逆辺を解消し純粋境界の公認例外を rules に明文化(issue 99dq)`

---

### Task 4: l051 — *Info 値型を rpc/ へ移設(logic)

**Files:**
- Create: `core/src/rpc/info.rs`
- Modify: `core/src/rpc/mod.rs` / `core/src/rpc/render.rs` / `core/src/registry/plugin.rs` / `core/src/registry/{sidecar,filesystem,bus,grants}.rs` ほか利用側

**Interfaces:**
- Produces: `rpc::info::{SidecarInfo, FilesystemInfo, BusInfo, DashboardInfo, ScheduleInfo}`(render が消費する5つの値型。フィールド不変)。registry/plugin.rs は `pub use crate::rpc::info::{...};` で旧パス(`crate::registry::plugin::SidecarInfo` 等)を温存 — registry(命令的)→ rpc(純粋)の re-export は依存方向として合法

- [x] **Step 1**: render.rs が import する5構造体(registry/plugin.rs 73–135: SidecarInfo/FilesystemInfo/BusInfo/DashboardInfo/ScheduleInfo — doc コメントごと)を `rpc/info.rs` へ移動。フィールドの型は全て純粋または外部 crate の値型(GrantState / 各 Request / 各 Config / `edlr_driver_process::InstanceStatus`)であることを確認しながら移す。`PluginInfo` / `DriverInfo` / `PluginState` は registry の語彙なので**動かさない**
- [x] **Step 2**: registry/plugin.rs に pub use を張り、render.rs の `crate::registry::plugin::*Info` 参照を `crate::rpc::info::*` に更新(これで rpc→registry import が消える)。registry 内のサービス群は旧パス(pub use 経由)のままでよいが、機械的に更新してもよい
- [x] **Step 3**: 検証: `grep -rn "crate::registry\|crate::runner\|crate::host\|crate::server" core/src/rpc/ core/src/manifest/ core/src/capability/ core/src/settings/ core/src/schedule/ core/src/journal/ core/src/runtime/` が **0 件**(純粋→命令的 import の完全消滅)。`cargo test --workspace`(pin テスト含む)/ clippy / fmt --check。コミット: `refactor(core): render が使う *Info 値型を rpc/info.rs へ移設し純粋→命令的 import を解消(issue l051)`

---

### Task 5: kgc6 — Phase 4 残件(実装2点 + 容認・文書化4点)

**Files:**
- Modify: `core/src/registry/plugin.rs` / `core/src/registry/driver.rs`(残件2: list() の service 経由化と冗長フィールド削除)
- Modify: `core/src/registry/subject.rs` / `core/src/registry/{settings,grants}.rs` / `core/src/registry/driver.rs`(残件4: Error 関連型化)
- Modify: `core/src/registry/subject.rs` / `core/src/registry/sidecar.rs` / `core/src/registry/driver.rs`(残件1/5/6 の容認コメント・文書化)

**Interfaces:**
- Produces: `RegistrySubject` に関連型 `type Error`(plugin=RegistryError / driver=DriverRegistryError)と `fn unknown_error(id) -> Self::Error` + Settings/Grants エラーの constructor。共有サービス(SettingsService/GrantService の共通経路)は `Subject::Error` を返し、`to_driver_error`(driver.rs 99–109)は**削除**

- [x] **Step 1(残件2)**: facade の `list()` が settings_store/grants_store を直叩きしている箇所(registry/plugin.rs 449–450 相当 / driver.rs 236–237 相当)を `SettingsService::effective` / `GrantService` 経由に寄せ、facade の冗長フィールド(`settings_store`/`grants_store` — 他で未使用なら)を削除。**list() の出力 JSON は pin テストで不変を確認**
- [x] **Step 2(残件4)**: `RegistrySubject` に `type Error: std::error::Error`(+ `fn unknown_error(id: &str) -> Self::Error`、Settings/Grants 写像用 constructor `fn settings_error(SettingsError) -> Self::Error` / `fn grants_error(GrantsError) -> Self::Error`)を導入し、共有サービスの該当経路を `Subject::Error` 返しに変更。driver facade の `to_driver_error` と `unreachable!` を削除。**エラー Display 文字列不変**(Task 1 Phase 5 の錨 + 既存テストが防衛)
- [x] **Step 3(残件1/5/6 の記録)**: (1) `as_settings_manifest` の plugin 側全 clone に「プロファイル上ホットと実証されるまで容認(Cow 化は複雑さに見合わない)」の doc コメント。(5) `start_or_restart_sidecar` のロック内 disk read に「エラー経路のみ・出力不変のため容認」の doc コメント。(6) driver に capabilities 読み口が無い非対称を registry/driver.rs のモジュール or 該当箇所 doc に明記。(3: entry trait 3本)はコード変更なし — issue クローズ時に「4本目が要るまで対応しない」と記録
- [x] **Step 4**: `cargo test --workspace`(pin 含む)/ clippy / fmt --check。コミット2つに分ける: `refactor(core): facade list() を service 経由に寄せ冗長 store フィールドを削除(issue kgc6 残件2)` / `refactor(core): RegistrySubject::Error 関連型で to_driver_error の unreachable を撤去(issue kgc6 残件4、容認3点のコメント化を含む)`

---

### Task 6: ゲート + issue クローズ

- [x] **Step 1**: `cargo test --workspace` 全パス + clippy 0 + `cargo fmt --check` 0。flaky は単体再実行
- [x] **Step 2**: upfj の確認クローズ: `cargo fmt --check` が manifest/tests.rs を含め差分0(Phase 6 の 91dca61 で解消済み)であることを確認し、その旨を追記してクローズ
- [x] **Step 3**: 6 issue(pagd / yzyv / 99dq / l051 / kgc6 / upfj)それぞれに解決コミットと内容を追記し `GIT_EDITOR=true git issues close <id>`。kgc6 は容認4点の裁定(1: Cow 見送り、3: 条件未達、5: 非ホット容認、6: 文書化)を明記
- [x] **Step 4**: ユーザーへ報告(実装した点・容認と裁定した点の一覧)

---

## Self-Review 済み事項

- 6 issue 全てに対応タスクあり。kgc6 の6残件は issue 自身の判断基準(「ホットになるなら」「4本目が要る事態になったら」等)に従い実装2・容認4に裁定し、裁定を issue に記録する
- 99dq の逆辺解消は「エラー文字列 byte 同一 + 公開パス pub use 温存」で挙動・API 互換を守る。ManifestError 本体は manifest に残す(capability へ動かすのは意味論が逆)
- l051 は移設先を rpc/info.rs に決定(render の消費語彙。runtime/ は host 共有バッファの家なので不適)。PluginState 等 registry の状態語彙は動かさない
- oxa3 / aenf(flaky テスト)はリファクタ起因ではない既存問題のためスコープ外(ユーザーへ報告時に明記)

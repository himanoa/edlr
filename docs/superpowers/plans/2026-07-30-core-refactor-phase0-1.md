# core リファクタリング Phase 0–1 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 挙動を一切変えずに、(Phase 0) DI 用の trait 4本を既存具象型から逆算して定義し、(Phase 1) manifest.rs(3206行)から capability の型・検証・fingerprint を `capability/` モジュールへ分離する。

**Architecture:** spec は `docs/superpowers/specs/2026-07-30-core-refactoring-design.md`。機能名モジュール + 依存方向で層を表現し、旧パスは `pub use` で温存する。テストは凍結(アサーション変更禁止、import 追従のみ可)。

**Tech Stack:** Rust (cargo workspace)。新規依存 crate の追加は禁止(mockall・thiserror 等を入れない)。

## Global Constraints

- **挙動不変**: wire フォーマット・ファイルフォーマット・ログ文字列・エラーメッセージを1バイトも変えない
- **テスト凍結**: `core/tests/` と `#[cfg(test)]` の diff は「空 or import 行のみ or 丸ごと移動」だけ許可。アサーション・テストデータ・テスト名に触ったらその変更は誤り
- **旧パス温存**: モジュール移動時は旧パスに `pub use` を残す(削除は Phase 6)
- **コミット分離**: 1コミット = 移動のみ(move-only)か ロジック変更のみ。移動コミットは `git diff HEAD~1 --color-moved=dimmed-zebra` で移動行が dimmed 表示されることを確認
- **ゲート**: 全タスクの完了条件は `cargo test --workspace` 全パス + `cargo clippy --workspace` 警告なし(既存警告があれば増やさないこと)
- **並列実行の注意**(CLAUDE.md): サブエージェントを並列に走らせる前に `cargo fetch` を一度実行。同一 worktree 内で cargo コマンドを並走させない
- 新しい命令的コードを書く場合: 1関数〜40行・ネスト2段まで

---

### Task 1: `capability` モジュール新設と `GrantStorage` trait(Phase 0)

**Files:**
- Create: `core/src/capability/mod.rs`
- Modify: `core/src/lib.rs`(`pub mod capability;` を追加)
- Test: `core/src/capability/mod.rs` 内の `#[cfg(test)]`

**Interfaces:**
- Consumes: `crate::plugin::grants::{GrantState, GrantsError, GrantsStore}`、`crate::plugin::Manifest`(いずれも既存。この時点では grants.rs はまだ `plugin/` にある — Task 8 で移動)
- Produces: `capability::GrantStorage` trait(Phase 3–4 で `Registry` 系が consume する)。メソッドは `GrantsStore` の公開 API と同名同型

- [ ] **Step 1: ベースライン確認**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: 全パス(失敗があれば作業前に報告して停止)

- [ ] **Step 2: trait とディスク実装への impl を書く**

`core/src/capability/mod.rs` を新規作成:

```rust
//! capability(プラグインが manifest で宣言する要求と、ユーザーによる承認)。
//!
//! 要求(request)と承認(grant)は capability という 1 概念の表と裏なので、
//! 両方をこのモジュール配下で扱う。詳細は
//! `docs/superpowers/specs/2026-07-30-core-refactoring-design.md` を参照。

use crate::plugin::grants::{GrantState, GrantsError, GrantsStore};
use crate::plugin::Manifest;

/// capability 承認の永続化の口。ディスク実装は [`GrantsStore`]。
/// テストではインメモリ実装を注入して、tempdir なしの純粋テストを書く。
///
/// メソッドは `GrantsStore` の公開 API と同名同型(挙動不変で導入するため)。
pub trait GrantStorage {
    fn state(&self, manifest: &Manifest) -> GrantState;
    fn set(&self, manifest: &Manifest, granted: bool) -> Result<GrantState, GrantsError>;
    fn sidecar_state(&self, manifest: &Manifest, name: &str) -> GrantState;
    fn set_sidecar(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError>;
    fn filesystem_state(&self, manifest: &Manifest, name: &str) -> GrantState;
    fn set_filesystem(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError>;
    fn bus_state(&self, manifest: &Manifest, driver: &str) -> GrantState;
    fn set_bus(
        &self,
        manifest: &Manifest,
        driver: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError>;
    fn dashboard_state(&self, manifest: &Manifest, widget: &str) -> GrantState;
    fn set_dashboard(
        &self,
        manifest: &Manifest,
        widget: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError>;
}

impl GrantStorage for GrantsStore {
    fn state(&self, manifest: &Manifest) -> GrantState {
        GrantsStore::state(self, manifest)
    }
    fn set(&self, manifest: &Manifest, granted: bool) -> Result<GrantState, GrantsError> {
        GrantsStore::set(self, manifest, granted)
    }
    fn sidecar_state(&self, manifest: &Manifest, name: &str) -> GrantState {
        GrantsStore::sidecar_state(self, manifest, name)
    }
    fn set_sidecar(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        GrantsStore::set_sidecar(self, manifest, name, granted)
    }
    fn filesystem_state(&self, manifest: &Manifest, name: &str) -> GrantState {
        GrantsStore::filesystem_state(self, manifest, name)
    }
    fn set_filesystem(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        GrantsStore::set_filesystem(self, manifest, name, granted)
    }
    fn bus_state(&self, manifest: &Manifest, driver: &str) -> GrantState {
        GrantsStore::bus_state(self, manifest, driver)
    }
    fn set_bus(
        &self,
        manifest: &Manifest,
        driver: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        GrantsStore::set_bus(self, manifest, driver, granted)
    }
    fn dashboard_state(&self, manifest: &Manifest, widget: &str) -> GrantState {
        GrantsStore::dashboard_state(self, manifest, widget)
    }
    fn set_dashboard(
        &self,
        manifest: &Manifest,
        widget: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        GrantsStore::set_dashboard(self, manifest, widget, granted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// trait 経由でディスク実装を呼べること(= Registry 側をジェネリック化
    /// したとき既存実装がそのまま挿さること)の静的確認。
    fn state_via_trait<S: GrantStorage>(storage: &S, manifest: &Manifest) -> GrantState {
        storage.state(manifest)
    }

    #[test]
    fn grants_store_satisfies_grant_storage() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().to_path_buf());
        let manifest = Manifest {
            id: "cap-trait-check".into(),
            ..Default::default()
        };
        let state = state_via_trait(&store, &manifest);
        assert_eq!(
            state,
            GrantState {
                granted: false,
                stale: false
            }
        );
    }
}
```

注意: `Manifest` が `Default` を実装していない場合は、`core/src/plugin/registry.rs` のテストにある `plain_manifest(id)` と同じ形で全フィールドを列挙して構築する(既存テストの構築コードをコピーする。`Default` の impl を Manifest に足すのは禁止 — 挙動に影響しないが公開 API の変更になるため Phase 6 まで持ち越し)。

- [ ] **Step 3: lib.rs に配線**

`core/src/lib.rs` に `pub mod capability;` を追加(アルファベット順の位置)。

- [ ] **Step 4: テスト実行**

Run: `cargo test -p edlr-core capability 2>&1 | tail -5` のあと `cargo test --workspace 2>&1 | tail -5`
Expected: 新テスト含め全パス

- [ ] **Step 5: clippy**

Run: `cargo clippy --workspace 2>&1 | tail -5`
Expected: 警告が増えていない

- [ ] **Step 6: コミット(logic コミット)**

```bash
git add core/src/capability/mod.rs core/src/lib.rs
git commit -m "refactor(core): capability モジュールを新設し GrantStorage trait を定義

Phase 0: 既存 GrantsStore の公開 API から逆算した trait。まだ consumer は
いない(Phase 3-4 で Registry 系をジェネリック化する際の語彙を先に固定する)。"
```

---

### Task 2: `settings::Storage` trait(Phase 0)

**Files:**
- Create: `core/src/settings/mod.rs`
- Modify: `core/src/lib.rs`(`pub mod settings;` を追加)
- Test: `core/src/settings/mod.rs` 内の `#[cfg(test)]`

**Interfaces:**
- Consumes: `crate::plugin::settings::{SettingsStore, SettingsError}`、`crate::plugin::Manifest`
- Produces: `settings::Storage` trait(Phase 3 で consume)。メソッドは `SettingsStore` の公開 API と同名同型

- [ ] **Step 1: trait と impl を書く**

`core/src/settings/mod.rs` を新規作成:

```rust
//! プラグイン設定の検証・マージと永続化の口。
//!
//! 現状は trait 定義のみ(Phase 0)。Phase 3 で検証・マージの純粋ロジックが
//! `plugin/settings.rs` からここへ移ってくる。

use crate::plugin::settings::{SettingsError, SettingsStore};
use crate::plugin::Manifest;

/// 設定永続化の口。ディスク実装は [`SettingsStore`]。
pub trait Storage {
    /// manifest 由来の defaults に保存済みの値をマージして返す。
    fn effective(&self, manifest: &Manifest) -> serde_json::Map<String, serde_json::Value>;
    /// 検証してから部分適用で保存する(検証は書き込み前に全件)。
    fn update(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), SettingsError>;
    /// `update` と同じ検証・永続化を行い、書き込み後の effective settings を
    /// 同じロック区間内で返す。
    fn update_and_effective(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, SettingsError>;
}

impl Storage for SettingsStore {
    fn effective(&self, manifest: &Manifest) -> serde_json::Map<String, serde_json::Value> {
        SettingsStore::effective(self, manifest)
    }
    fn update(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), SettingsError> {
        SettingsStore::update(self, manifest, values)
    }
    fn update_and_effective(
        &self,
        manifest: &Manifest,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, SettingsError> {
        SettingsStore::update_and_effective(self, manifest, values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effective_via_trait<S: Storage>(
        storage: &S,
        manifest: &Manifest,
    ) -> serde_json::Map<String, serde_json::Value> {
        storage.effective(manifest)
    }

    #[test]
    fn settings_store_satisfies_storage() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(tmp.path().to_path_buf());
        // Task 1 と同じ流儀で Manifest を構築(settings が空なら effective は空)
        let manifest = Manifest {
            id: "settings-trait-check".into(),
            ..Default::default()
        };
        assert!(effective_via_trait(&store, &manifest).is_empty());
    }
}
```

(Task 1 と同じく、`Manifest` に `Default` が無ければ既存テストの構築ヘルパーの形をコピーする。)

- [ ] **Step 2: lib.rs に `pub mod settings;` を追加**

- [ ] **Step 3: テストと clippy**

Run: `cargo test --workspace 2>&1 | tail -5` / `cargo clippy --workspace 2>&1 | tail -5`
Expected: 全パス、警告増なし

- [ ] **Step 4: コミット**

```bash
git add core/src/settings/mod.rs core/src/lib.rs
git commit -m "refactor(core): settings::Storage trait を定義(Phase 0)"
```

---

### Task 3: `registry::ProcessControl` / `registry::BusPort` trait(Phase 0)

**Files:**
- Create: `core/src/registry/mod.rs`
- Modify: `core/src/lib.rs`(`pub mod registry;` を追加)
- Test: `core/src/registry/mod.rs` 内の `#[cfg(test)]`

**Interfaces:**
- Consumes: `edlr_driver_process::{ProcessDriver, ProcessSpec, InstanceStatus, ProcessError}`、`edlr_driver_channel::Bus`
- Produces: `registry::ProcessControl`(Phase 4 で `Registry`/`SidecarService` が consume)、`registry::BusPort`(Phase 3–4 で select options 解決が consume)

- [ ] **Step 1: trait と impl を書く**

`core/src/registry/mod.rs` を新規作成:

```rust
//! プラグイン/ドライバの registry(facade と各サービス)。
//!
//! 現状は trait 定義のみ(Phase 0)。Phase 4 で `plugin/registry.rs` と
//! `driver/registry.rs` の実装がここへ移ってくる。

use edlr_driver_process::{InstanceStatus, ProcessError, ProcessSpec};

/// サイドカープロセス制御の口。実運用の実装は
/// [`edlr_driver_process::ProcessDriver`]。
///
/// `ProcessDriver::stop_detached` は `Arc<Self>` を要求するため trait には
/// 含めない(必要になった時点で `Arc` 前提のメソッドとして追加を検討)。
pub trait ProcessControl {
    fn ensure_started(
        &self,
        key: &str,
        spec: &ProcessSpec,
    ) -> Result<Vec<InstanceStatus>, ProcessError>;
    fn status(&self, key: &str, spec: &ProcessSpec) -> Vec<InstanceStatus>;
    fn stop(&self, key: &str);
    fn stop_all(&self);
}

impl ProcessControl for edlr_driver_process::ProcessDriver {
    fn ensure_started(
        &self,
        key: &str,
        spec: &ProcessSpec,
    ) -> Result<Vec<InstanceStatus>, ProcessError> {
        edlr_driver_process::ProcessDriver::ensure_started(self, key, spec)
    }
    fn status(&self, key: &str, spec: &ProcessSpec) -> Vec<InstanceStatus> {
        edlr_driver_process::ProcessDriver::status(self, key, spec)
    }
    fn stop(&self, key: &str) {
        edlr_driver_process::ProcessDriver::stop(self, key)
    }
    fn stop_all(&self) {
        edlr_driver_process::ProcessDriver::stop_all(self)
    }
}

/// プラグイン間バスへの読み取り口。実運用の実装は
/// [`edlr_driver_channel::Bus`]。
///
/// メソッドは現時点で registry 系が実際に使っている 1 本だけ
/// (`select_options::resolve` が retain 値から select 候補を解決する)。
/// spec の方針どおり、必要が実証されたときだけ増やす。
pub trait BusPort {
    fn retained_for(&self, driver_id: &str, topic: &str) -> Option<Vec<u8>>;
}

impl BusPort for edlr_driver_channel::Bus {
    fn retained_for(&self, driver_id: &str, topic: &str) -> Option<Vec<u8>> {
        edlr_driver_channel::Bus::retained_for(self, driver_id, topic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retained_via_trait<B: BusPort>(bus: &B, driver: &str, topic: &str) -> Option<Vec<u8>> {
        bus.retained_for(driver, topic)
    }

    #[test]
    fn bus_satisfies_bus_port() {
        let bus = edlr_driver_channel::Bus::new();
        assert_eq!(retained_via_trait(&bus, "no-such-driver", "topic"), None);
    }

    /// ProcessDriver が trait を満たすことのコンパイル時確認
    /// (実プロセスを起動しないよう、呼び出しはしない)。
    #[test]
    fn process_driver_satisfies_process_control() {
        fn assert_impl<T: ProcessControl>() {}
        assert_impl::<edlr_driver_process::ProcessDriver>();
    }
}
```

- [ ] **Step 2: lib.rs に `pub mod registry;` を追加**

- [ ] **Step 3: テストと clippy**

Run: `cargo test --workspace 2>&1 | tail -5` / `cargo clippy --workspace 2>&1 | tail -5`
Expected: 全パス、警告増なし

- [ ] **Step 4: コミット**

```bash
git add core/src/registry/mod.rs core/src/lib.rs
git commit -m "refactor(core): ProcessControl / BusPort trait を定義(Phase 0 完了)"
```

---

### Task 4: manifest.rs のディレクトリ化とテスト分離(Phase 1)

**Files:**
- Move: `core/src/plugin/manifest.rs` → `core/src/plugin/manifest/mod.rs`
- Create: `core/src/plugin/manifest/tests.rs`(既存 `mod tests` の中身を丸ごと移動)

**Interfaces:**
- Consumes: なし(純粋なファイル移動)
- Produces: 後続タスクが小さい diff で作業できる形。公開パス(`crate::plugin::manifest::*`)は不変

- [ ] **Step 1: ディレクトリ化**

```bash
mkdir core/src/plugin/manifest
git mv core/src/plugin/manifest.rs core/src/plugin/manifest/mod.rs
```

- [ ] **Step 2: テスト本体を tests.rs へ移動**

`core/src/plugin/manifest/mod.rs` 末尾の `#[cfg(test)] mod tests { ... }`(約 1188 行目以降、`wit_version` 等のテスト補助含むブロック全体)を切り取り、`core/src/plugin/manifest/tests.rs` に貼り付ける。貼り付け時の変形は次の2点だけ:
- ファイル先頭の `mod tests {` と末尾の `}` を剥がす(ファイル自体がモジュールになるため)
- `use super::*;` は**そのまま残す**(`tests.rs` の `super` は `manifest` モジュールを指すので変更不要)

`mod.rs` 側には以下を残す:

```rust
#[cfg(test)]
mod tests;
```

もし `mod tests` 内で `use super::tests::...` のような相互参照や、`#[cfg(test)]` 付き補助関数がテストモジュール外にある場合は、それらの可視性(`pub(crate)` 化など)を最小限で調整し、調整内容をコミットメッセージに列挙する。

- [ ] **Step 3: テスト実行**

Run: `cargo test -p edlr-core manifest 2>&1 | tail -5` のあと `cargo test --workspace 2>&1 | tail -5`
Expected: テスト数が移動前と同一で全パス(`cargo test -p edlr-core manifest 2>&1 | grep -c "test .* ok"` を移動前後で比較)

- [ ] **Step 4: move-only 確認とコミット**

```bash
git add -A core/src/plugin/manifest
git diff --cached --color-moved=dimmed-zebra | less -R   # 移動行が dimmed であることを目視
git commit -m "refactor(core): manifest.rs をディレクトリ化しテストを tests.rs へ分離(移動のみ)"
```

---

### Task 5: capability の Request 型を `capability/request.rs` へ移動(Phase 1)

**Files:**
- Create: `core/src/capability/request.rs`
- Modify: `core/src/capability/mod.rs`(`pub mod request;` 追加)
- Modify: `core/src/plugin/manifest/mod.rs`(型定義を削除し `pub use` に置換)

**Interfaces:**
- Consumes: なし(移動のみ)
- Produces: `capability::request::{CapabilityRequest, SidecarRequest, FilesystemMode, FilesystemRequest, BusRequest, WidgetSize, DashboardWidget}`。旧パス `crate::plugin::manifest::CapabilityRequest` 等は `pub use` で存続

- [ ] **Step 1: 移動対象の確認**

`core/src/plugin/manifest/mod.rs` から以下を**定義ごと**(doc コメント・derive・impl ブロック込みで)移動する。移動前の行番号目安(Task 4 で多少ずれる):

| 型 | 元の行 | 付随する impl |
|---|---|---|
| `CapabilityRequest` | ~185 | なし |
| `SidecarRequest` | ~196 | なし |
| `FilesystemMode` | ~209 | `impl FilesystemMode`(`as_str`/`allows_write`) |
| `FilesystemRequest` | ~235 | なし |
| `BusRequest` | ~247 | なし |
| `WidgetSize` | ~260 | `impl WidgetSize`(`as_str`) |
| `DashboardWidget` | ~284 | なし |

`ScheduleRequest`/`ScheduleSpec`/`SettingField`/`SelectOption`/`OptionsFrom` は**移動しない**(スケジュールと設定は capability ではない。設定型は Phase 3、schedule 型は Phase 3 で扱う)。

- [ ] **Step 2: `capability/request.rs` を作成して貼り付け**

ファイル冒頭:

```rust
//! プラグインが manifest で宣言する capability 要求の型。
//!
//! パースは manifest 側(TOML → これらの型)が担い、ここは型と
//! それ自身の小さな振る舞い(`as_str` 等)だけを持つ。

use serde::Deserialize;
```

(移動した型が使っている import は元ファイルの `use` から必要な分だけ持ってくる。)

`capability/mod.rs` に `pub mod request;` を追加。

- [ ] **Step 3: 旧パスの `pub use` を置く**

`core/src/plugin/manifest/mod.rs` の、型があった場所に:

```rust
// Phase 1 で capability/request.rs へ移動した型の旧パス互換(削除は Phase 6)。
pub use crate::capability::request::{
    BusRequest, CapabilityRequest, DashboardWidget, FilesystemMode, FilesystemRequest,
    SidecarRequest, WidgetSize,
};
```

`plugin/mod.rs` に既存の再エクスポート(`pub use manifest::CapabilityRequest;` など)があれば**そのまま触らない**(pub use の連鎖で自動的に生きる)。

- [ ] **Step 4: テスト実行と move-only 確認**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: 全パス。`core/tests/` と `#[cfg(test)]` の diff が無いことを `git status` で確認

- [ ] **Step 5: コミット**

```bash
git add core/src/capability core/src/plugin/manifest/mod.rs
git diff --cached --color-moved=dimmed-zebra | less -R
git commit -m "refactor(core): capability の Request 型を capability/request.rs へ移動(移動のみ、旧パスは pub use 温存)"
```

---

### Task 6: fingerprint 計算を `capability/fingerprint.rs` へ移動(Phase 1)

**Files:**
- Create: `core/src/capability/fingerprint.rs`
- Modify: `core/src/capability/mod.rs`(`pub mod fingerprint;` 追加)
- Modify: `core/src/plugin/manifest/mod.rs`(`Manifest` のメソッドを委譲に変更)

**Interfaces:**
- Consumes: Task 5 の `capability::request::*`
- Produces: 以下の純関数群。`Manifest::capabilities_fingerprint()` 等の公開メソッドは**シグネチャ不変のまま**これらへ委譲する

```rust
pub fn capabilities(requests: &[CapabilityRequest]) -> Option<String>;
pub fn sidecar(request: &SidecarRequest) -> String;
pub fn filesystem(request: &FilesystemRequest) -> String;
pub fn bus(request: &BusRequest) -> String;
pub fn dashboard(widget: &DashboardWidget) -> String;
```

- [ ] **Step 1: 移動対象の確認**

`core/src/plugin/manifest/mod.rs` から:
- ヘルパー `encode_field`(~613行)と `sha256_hex`(~621行)→ `capability/fingerprint.rs` の非公開関数として移動
- `impl Manifest` 内の `capabilities_fingerprint`(~460)/ `sidecar_fingerprint`(~525)/ `filesystem_fingerprint`(~548)/ `bus_fingerprint`(~564)/ `dashboard_fingerprint`(~592)の**本体**(ハッシュ材料の組み立てロジック)→ 上記シグネチャの関数として移動

各関数のシグネチャ設計: `Manifest` のメソッドは「name/driver/id で該当 request を探す(`sidecar(name)` 等の既存 lookup)→ 見つかったら fingerprint 計算」の2段になっている。**lookup は Manifest に残し、計算だけを移す**。例:

```rust
// manifest/mod.rs(委譲後)
pub fn sidecar_fingerprint(&self, name: &str) -> Option<String> {
    self.sidecar(name)
        .map(crate::capability::fingerprint::sidecar)
}
```

`capabilities_fingerprint` は全 `[[capabilities]]` を材料にするので slice ごと渡す:

```rust
pub fn capabilities_fingerprint(&self) -> Option<String> {
    crate::capability::fingerprint::capabilities(&self.capabilities)
}
```

**ハッシュ材料の組み立て(ソート順・区切り文字・encode_field の適用箇所)は1文字も変えない。** ここが変わると保存済み grant が全ユーザーで一斉に stale になる(挙動変更)。fingerprint の既存テスト(`fingerprint_is_stable_order_independent_and_sensitive_to_content` 等)が凍結のまま通ることが担保。

- [ ] **Step 2: 実装して委譲に置換**

- [ ] **Step 3: テスト実行**

Run: `cargo test -p edlr-core fingerprint 2>&1 | tail -5` のあと `cargo test --workspace 2>&1 | tail -5`
Expected: 全パス(特に fingerprint 系テストが1件も変更されずに通ること)

- [ ] **Step 4: コミット**

```bash
git add core/src/capability core/src/plugin/manifest/mod.rs
git commit -m "refactor(core): fingerprint 計算を capability/fingerprint.rs の純関数へ移動

Manifest のメソッドはシグネチャ不変のまま委譲に変更。ハッシュ材料の
組み立ては未変更(凍結済み fingerprint テストが担保)。"
```

---

### Task 7: capability の検証関数を `capability/validate.rs` へ移動(Phase 1)

**Files:**
- Create: `core/src/capability/validate.rs`
- Modify: `core/src/capability/mod.rs`(`pub mod validate;` 追加)
- Modify: `core/src/plugin/manifest/mod.rs`(呼び出しを `capability::validate::*` に変更)

**Interfaces:**
- Consumes: Task 5 の `capability::request::*`
- Produces: 以下の関数(シグネチャは既存のまま移動):

```rust
pub fn validate_host(host: &str) -> Result<(), String>;
pub fn reject_invisible_chars(field: &str, s: &str) -> Result<(), String>;
pub fn validate_bus(requests: &mut [BusRequest]) -> Result<(), ManifestError>;
pub fn validate_widget_entry(entry: &str) -> Result<(), ManifestError>;
```

- [ ] **Step 1: 移動**

`core/src/plugin/manifest/mod.rs` から `validate_host`(~762)/ `reject_invisible_chars`(~802)/ `validate_bus`(~927)/ `validate_widget_entry`(~1048)を doc コメントごと `capability/validate.rs` へ移動し、`pub` を付ける。`ManifestError` は manifest 側に残る(エラー型の所属は据え置き — 動かすと `Display` 文字列や公開パスに波及しやすい)ので、`validate.rs` は `use crate::plugin::manifest::ManifestError;` で参照する。

**`validate_schedules` と `validate_sidecar`/settings 系の検証は移動しない**(capability ではない)。manifest 内の呼び出し箇所(`load_manifest` 内・`Deserialize` 実装内)を `crate::capability::validate::validate_host(...)` 形式に置換。

- [ ] **Step 2: テスト実行**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: 全パス。検証エラーメッセージ文字列のテスト(`host_without_scheme_is_rejected` 等)が未変更のまま通ること

- [ ] **Step 3: move-only 確認とコミット**

```bash
git add core/src/capability core/src/plugin/manifest/mod.rs
git diff --cached --color-moved=dimmed-zebra | less -R
git commit -m "refactor(core): capability の検証関数を capability/validate.rs へ移動(移動のみ)"
```

---

### Task 8: `plugin/grants.rs` を `capability/grants.rs` へ移動(Phase 1)

**Files:**
- Move: `core/src/plugin/grants.rs` → `core/src/capability/grants.rs`
- Modify: `core/src/capability/mod.rs`(`pub mod grants;` 追加、Task 1 の `use crate::plugin::grants::...` を `grants::...` に変更)
- Modify: `core/src/plugin/mod.rs`(`mod grants;` を削除し `pub use` に置換)

**Interfaces:**
- Consumes: Task 1 の `capability::GrantStorage`(import 元が変わるだけ)
- Produces: `capability::grants::{GrantState, GrantsStore, GrantsError}`。旧パス `crate::plugin::grants::*` は `pub use` で存続

- [ ] **Step 1: ファイル移動**

```bash
git mv core/src/plugin/grants.rs core/src/capability/grants.rs
```

`capability/grants.rs` 内の `use crate::plugin::Manifest;` はそのまま有効(絶対パスなので)。`capability/mod.rs` に `pub mod grants;` を追加し、Task 1 で書いた `use crate::plugin::grants::{...};` を `use grants::{GrantState, GrantsError, GrantsStore};` に変更。

- [ ] **Step 2: 旧パス互換**

`core/src/plugin/mod.rs` の `mod grants;`(または `pub mod grants;`)を削除し、同じ場所に:

```rust
// Phase 1 で capability/grants.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::capability::grants;
```

`plugin/mod.rs` に `pub use grants::GrantState;` のような個別再エクスポートがあれば、それは**そのまま残す**(上の `pub use` 経由で解決される)。

- [ ] **Step 3: テスト実行**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: 全パス。`use` 文以外の diff が `core/tests/` と `#[cfg(test)]` に無いこと

- [ ] **Step 4: move-only 確認とコミット**

```bash
git add -A core/src/capability core/src/plugin/mod.rs
git diff --cached --color-moved=dimmed-zebra | less -R
git commit -m "refactor(core): grants を capability/grants.rs へ移動(移動のみ、旧パスは pub use 温存)"
```

---

### Task 9: Phase 1 完了ゲート

**Files:**
- Modify: `docs/superpowers/specs/2026-07-30-core-refactoring-design.md`(進捗の追記のみ・任意)

**Interfaces:**
- Consumes: Task 1–8 の成果
- Produces: Phase 2 着手可能な状態の確認記録

- [ ] **Step 1: 全体検証**

```bash
cargo fetch
cargo test --workspace 2>&1 | tail -10
cargo clippy --workspace 2>&1 | tail -5
```

Expected: 全テストパス、clippy 警告増なし

- [ ] **Step 2: 行数の変化を記録**

```bash
wc -l core/src/plugin/manifest/mod.rs core/src/plugin/manifest/tests.rs core/src/capability/*.rs
```

Expected: `manifest/mod.rs` が元の 3206 行から大きく減っている(目安: 本体 ~900 行、tests ~2000 行、capability 側 ~400 行)。数値を報告に含める

- [ ] **Step 3: テスト凍結の最終確認**

```bash
git diff d79676d..HEAD -- core/tests/ | head -50
git diff d79676d..HEAD -- 'core/src/**' | grep -E '^[-+].*#\[(test|cfg\(test\))' | head
```

Expected: `core/tests/` の diff が空。`#[cfg(test)]` ブロックの変更が「丸ごと移動 + import 行」のみであることを説明できる状態

- [ ] **Step 4: 報告**

Phase 0–1 の完了をユーザーに報告し、Phase 2(rpc + server)の計画作成に進む承認を得る。

---

## Self-Review 済み事項

- spec の Phase 0(trait 4本・impl のみ・consumer なし)→ Task 1–3 が対応
- spec の Phase 1(manifest + capability: テスト分離・Request 型/検証/fingerprint の移動)→ Task 4–8 が対応。grants の capability 配下への移動(spec の構成図)→ Task 8
- 型整合: `GrantStorage`(Task 1)の import 変更を Task 8 Step 1 に明記。`validate.rs` が参照する `ManifestError` の所属は manifest 側に据え置きと明記
- fingerprint 移動(Task 6)は move-only ではなく委譲化を含むため logic コミット扱い。ハッシュ材料不変の注意を明記
- Phase 2 以降の計画は Phase 1 完了後に別ファイルで作成する(spec の全 Phase をこの計画が覆うわけではない — 意図的)

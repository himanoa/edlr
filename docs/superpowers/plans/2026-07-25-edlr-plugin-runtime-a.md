# edlr プラグインランタイム(Plan A)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** WIT 定義・wasmtime component ホスト・マニフェスト・イベント配信・設定永続化・サンプルプラグインを実装し、実 wasm プラグインがイベントを受けてログを出せる状態にする。

**Architecture:** 設計書 `docs/superpowers/specs/2026-07-25-edlr-plugin-runtime-design.md` のとおり。core に `plugin/` モジュール(manifest / settings / host / runner / registry)。サンプルは `examples/plugins/hello-logger`(wasm32-wasip2、統合テストのフィクスチャ兼用)。

**Tech Stack:** wasmtime(component-model, 最新安定版を `cargo add` で導入)、toml + serde、wit-bindgen(ゲスト側)。

## Global Constraints

- カーネルは panic しない。プラグインのロード失敗・trap は当該プラグインのみ disabled 化(warn ログ)し、監視コア・他プラグインに波及させない
- WIT は設計書の `edlr:plugin@0.1.0` を一字一句そのまま使う(`core/wit/plugin.wit`)
- manifest の id は `[a-z0-9-]+` かつディレクトリ名と一致必須。検証エラーはそのプラグインだけスキップ + warn
- epoch interruption で 1 call のデッドライン既定 2 秒(定数 `CALL_DEADLINE`)。超過 trap → disabled
- 設定は `<settings-dir>/<id>.json`。`get-all` は defaults マージ済み JSON を返す。壊れた保存 JSON は defaults 扱い
- **wasmtime の API はバージョンで変わる**。計画中のホスト側コードはスケルトンであり、`bindgen!` や Linker まわりは導入した実バージョンの API に合わせて最小限の適応をしてよい(挙動要件・シグネチャ意図は変えない)。適応した場合はレポートに差分理由を書く
- コミットメッセージ末尾に `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

---

### Task 1: WIT 定義とマニフェスト(parse + 検証)

**Files:**
- Create: `core/wit/plugin.wit`(設計書の WIT を一字一句)
- Create: `core/src/plugin/mod.rs`, `core/src/plugin/manifest.rs`
- Modify: `core/src/lib.rs`(`pub mod plugin;`)、`core/Cargo.toml`(`toml = "0.8"`, `serde = { version = "1", features = ["derive"] }` を追加)

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SettingField {
    Boolean { key: String, label: String, default: bool },
    String  { key: String, label: String, default: String },
    Number  { key: String, label: String, default: f64 },
    Select  { key: String, label: String, default: String, options: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub entry: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub settings: Vec<SettingField>,
}

pub fn load_manifest(dir: &Path) -> Result<Manifest, ManifestError>; // dir/manifest.toml
pub fn matches_event(events: &[String], event: &crate::event::Event) -> bool;
impl SettingField { pub fn key(&self) -> &str; pub fn default_value(&self) -> serde_json::Value; }
```

- `ManifestError`(thiserror 不使用、手書き enum + Display 可): `Io`, `Parse`, `IdMismatch`(dir 名と不一致), `BadId`(`[a-z0-9-]+` 以外), `MissingEntry`(entry ファイル不存在), `DuplicateKey`(settings key 重複)
- `matches_event`: `"*"` は全 journal、`"status"` は Status、それ以外は journal のイベント名完全一致

- [ ] **Step 1: 失敗するテストを書く**(manifest.rs 内 `#[cfg(test)]`。tempdir に manifest.toml + entry ファイルを書いて検証)

テストケース(全て実装すること):
1. 正常系: 全フィールド + settings 4 型がパースされる(TOML 例は設計書の表に準拠して自作)
2. `id` がディレクトリ名と不一致 → `IdMismatch`
3. `id = "Bad_ID"` → `BadId`
4. `entry` のファイルが無い → `MissingEntry`
5. settings key 重複 → `DuplicateKey`
6. TOML 構文エラー → `Parse`
7. `matches_event`: `["*"]` は journal 全部に true / status に false、`["status"]` はその逆、`["FSDJump"]` は該当のみ、空リストは全 false

- [ ] **Step 2: 失敗確認** — `cargo test -p edlr-core plugin::manifest` → FAIL(未定義)
- [ ] **Step 3: 実装**(スケルトンどおり。検証は load_manifest 内で実施)
- [ ] **Step 4: `cargo test -p edlr-core plugin::manifest` → PASS、`cargo test --workspace` → PASS**
- [ ] **Step 5: Commit** — `feat(core): plugin manifest schema, validation, and event filter`

---

### Task 2: 設定ストア

**Files:**
- Create: `core/src/plugin/settings.rs`
- Modify: `core/src/plugin/mod.rs`

**Interfaces:**
- Produces:

```rust
pub struct SettingsStore { dir: PathBuf }
impl SettingsStore {
    pub fn new(dir: PathBuf) -> Self;
    /// defaults(manifest 由来)に保存値をマージした JSON オブジェクトを返す。
    /// ファイル不在・壊れ JSON・非オブジェクトは defaults のみ。
    pub fn effective(&self, manifest: &Manifest) -> serde_json::Map<String, serde_json::Value>;
    /// 部分更新して保存。dir が無ければ作成。未知 key は Err(UnknownKey)。
    pub fn update(&self, manifest: &Manifest, values: &serde_json::Map<String, serde_json::Value>) -> Result<(), SettingsError>;
}
```

- [ ] **Step 1: 失敗するテストを書く**。ケース: defaults のみ / 保存値が defaults を上書き / 壊れ JSON → defaults / update 後に effective へ反映(ファイル実在確認) / 未知 key → Err / settings-dir 不存在でも update が作成して成功
- [ ] **Step 2: 失敗確認** → **Step 3: 実装** → **Step 4: PASS 確認(workspace 全体も)**
- [ ] **Step 5: Commit** — `feat(core): plugin settings store with defaults merge`

---

### Task 3: サンプルプラグイン hello-logger(ゲスト側)

**Files:**
- Create: `examples/plugins/hello-logger/Cargo.toml`, `src/lib.rs`, `wit/`(core/wit/plugin.wit のコピーでなく `../../../core/wit` を参照できないため、wit-bindgen の `path` 指定で core/wit を直接参照する)
- Create: `examples/plugins/hello-logger/README.md`(ビルド手順)
- Modify: ルート `Cargo.toml` は変更しない(hello-logger は独立 crate、`[workspace]` 空テーブルで分離)

**Interfaces:**
- Produces: `examples/plugins/hello-logger/target/wasm32-wasip2/release/hello_logger.wasm`(component)。`init` で "hello-logger initialized" を info ログ、`on-event` で `enabled` 設定(get-all の JSON から読む。既定 true)が true のときのみ `"<kind>:<name> <payload-json>"` を info ログ

実装スケルトン(`src/lib.rs`、wit-bindgen のバージョンに合わせて適応可):

```rust
wit_bindgen::generate!({ path: "../../../core/wit", world: "plugin" });

use exports::... // 生成物に合わせる
struct HelloLogger;

impl Guest for HelloLogger {
    fn init() {
        edlr::plugin::host_log::log(edlr::plugin::host_log::Level::Info, "hello-logger initialized");
    }
    fn on_event(ev: Event) {
        let settings = edlr::plugin::host_settings::get_all();
        let enabled = serde_json::from_str::<serde_json::Value>(&settings)
            .ok().and_then(|v| v.get("enabled").and_then(|b| b.as_bool())).unwrap_or(true);
        if enabled {
            let name = ev.name.as_deref().unwrap_or("-");
            edlr::plugin::host_log::log(Level::Info, &format!("{}:{} {}", ev.kind, name, ev.payload_json));
        }
    }
}
export!(HelloLogger);
```

Cargo.toml: `crate-type = ["cdylib"]`、依存 `wit-bindgen`(最新)+ `serde_json`。`[workspace]` 空テーブル。manifest.toml はここには置かない(テストが tempdir に生成する)。

- [ ] **Step 1: 実装してビルド** — `cd examples/plugins/hello-logger && cargo build --target wasm32-wasip2 --release`
  Expected: `hello_logger.wasm` 生成。`wasm-tools` は使わず、wasm32-wasip2 ターゲットが直接 component を出すことを前提にする(出力が component か不安なら後続 Task 4 の統合テストで判明する)
- [ ] **Step 2: README にビルドコマンドを記載、`.gitignore` に `examples/plugins/*/target/` を追記**
- [ ] **Step 3: Commit** — `feat(examples): hello-logger sample plugin (wasm component)`

---

### Task 4: wasmtime ホスト(ロード・host 関数・epoch デッドライン)

**Files:**
- Create: `core/src/plugin/host.rs`
- Modify: `core/src/plugin/mod.rs`、`core/Cargo.toml`(`cargo add wasmtime --features component-model` 相当。既定 features で component-model が有効なら追加 feature 不要)

**Interfaces:**
- Produces:

```rust
pub struct PluginInstance { /* Store, bindings, ... */ }
pub struct HostCtx {
    pub plugin_id: String,
    pub settings_json: Arc<std::sync::Mutex<String>>, // effective 設定の JSON 文字列
}
pub struct PluginHost { engine: wasmtime::Engine /* epoch ticker 込み */ }
impl PluginHost {
    pub fn new() -> anyhow::Result<PluginHost>; // epoch ticker スレッド(100ms 間隔で increment_epoch)を起動
    pub fn load(&self, wasm_path: &Path, ctx: HostCtx) -> anyhow::Result<PluginInstance>;
}
impl PluginInstance {
    pub const CALL_DEADLINE: Duration = Duration::from_secs(2);
    pub fn call_init(&mut self) -> anyhow::Result<()>;
    pub fn call_on_event(&mut self, kind: &str, timestamp: Option<&str>, name: Option<&str>, payload_json: &str) -> anyhow::Result<()>;
}
```

- host-log 実装: `tracing::info!/warn!/...` に `plugin_id` を付けて出力
- host-settings 実装: `ctx.settings_json` の現在値を返す(runner が更新できるよう Arc<Mutex>)
- 各 call 前に `store.set_epoch_deadline(CALL_DEADLINE 相当の tick 数)` を設定。超過は trap → Err
- `bindgen!` マクロで WIT からホストバインディング生成(`core/wit` 参照)。API 差異は適応可(Global Constraints 参照)

- [ ] **Step 1: 失敗する統合テストを書く**(`core/tests/plugin_host_integration.rs`)

```text
fixture: テスト先頭で std::process::Command により examples/plugins/hello-logger を
`cargo build --target wasm32-wasip2 --release` でビルド(すでに成果物があれば cargo が no-op)。
wasm パスを取得して以下を検証:

1. load → call_init が Ok。(ログ内容の検証は不要。呼べることが要件)
2. call_on_event(kind="journal", name=Some("FSDJump"), payload_json="{}") が Ok
3. settings_json を {"enabled": false} に書き換えても call_on_event が Ok(挙動分岐はゲスト内部)
4. 存在しない wasm パス → load が Err(panic しない)
5. デッドライン: 無限ループする最小 wasm(後述)で call_on_event が Err になり、
   呼び出しが CALL_DEADLINE + 数秒以内に返ってくる(wall-clock を assert)
```

無限ループ wasm: hello-logger と並置で `examples/plugins/busy-loop/`(on-event で `loop {}`)を作り同様にビルドする(このタスクで作成、マニフェスト不要)。

- [ ] **Step 2: 失敗確認** → **Step 3: 実装**(wasmtime API に適応しつつ要件どおり)
- [ ] **Step 4: `cargo test -p edlr-core --test plugin_host_integration` PASS、workspace 全体 PASS**
- [ ] **Step 5: Commit** — `feat(core): wasmtime component host with epoch deadline`

---

### Task 5: レジストリとランナー(走査→ロード→購読→配信→disabled 化)

**Files:**
- Create: `core/src/plugin/registry.rs`, `core/src/plugin/runner.rs`
- Modify: `core/src/plugin/mod.rs`

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum PluginState { Running, Disabled { reason: String } }

/// 後続の RPC(Plan B)が読む共有ビュー。
#[derive(Clone)]
pub struct Registry(Arc<std::sync::Mutex<Vec<PluginEntry>>>);
pub struct PluginEntry {
    pub manifest: Manifest,
    pub state: PluginState,
    pub settings_json: Arc<std::sync::Mutex<String>>, // HostCtx と共有
}
impl Registry {
    pub fn snapshot(&self) -> Vec<(Manifest, PluginState)>;
    pub fn entry_settings(&self, id: &str) -> Option<Arc<std::sync::Mutex<String>>>;
    pub fn set_disabled(&self, id: &str, reason: String);
}

/// plugins_dir を走査し、各プラグインをロードして専用タスクで駆動する。
/// 戻り値の Registry は起動直後から snapshot 可能。
pub fn start_plugins(
    plugins_dir: &Path,
    settings_store: SettingsStore,
    router: &crate::router::Router,
    host: PluginHost,
) -> Registry;
```

動作要件:
- plugins_dir 不存在 → 空 Registry で正常(info ログ)
- manifest 検証エラー → そのディレクトリをスキップ(warn、Registry には載せない)
- ロード成功 → `init()` 呼び出し(Err なら Disabled で登録、タスクは起動しない)
- 各プラグインタスク: `router.subscribe()` → `matches_event` フィルタ → `call_on_event`(spawn_blocking で呼ぶ。wasm 実行は同期のため)。Err(trap 含む)→ `set_disabled` してタスク終了
- Lagged は warn して継続。wasm 呼び出しはプラグイン内直列・プラグイン間並行

- [ ] **Step 1: 失敗する統合テストを書く**(`core/tests/plugin_runner_integration.rs`)

```text
tempdir を plugins_dir にして hello-logger の wasm + 正しい manifest.toml
(events = ["FSDJump"])を配置し:
1. start_plugins → snapshot に Running で 1 件
2. router.publish(FSDJump) → (検証手段としてログを拾うのは不安定なので)
   busy-loop プラグインを events=["*"] で並置 → publish 後しばらくして
   snapshot で busy-loop が Disabled になる & hello-logger は Running のまま
3. 壊れた manifest のディレクトリを混ぜても他プラグインは正常ロード
4. plugins_dir 不存在 → 空 snapshot
```

- [ ] **Step 2: 失敗確認** → **Step 3: 実装** → **Step 4: 全テスト PASS**
- [ ] **Step 5: Commit** — `feat(core): plugin registry and per-plugin event runner`

---

### Task 6: bin 結線と仕上げ

**Files:**
- Modify: `core/src/bin/edlr.rs`, `README.md`

**Interfaces:**
- Produces: CLI `--plugins-dir <PATH>` / `--settings-dir <PATH>`。既定はそれぞれ `$XDG_CONFIG_HOME/edlr/plugins` / `$XDG_CONFIG_HOME/edlr/settings`(XDG_CONFIG_HOME 未設定時は `~/.config/edlr/...`)。`PluginHost::new()` 失敗は warn してプラグイン機能なしで継続

- [ ] **Step 1: 実装**(`config.rs` に `default_config_subdir(home: &Path, sub: &str) -> PathBuf` を追加しユニットテスト。main で start_plugins を結線し、Registry は後続 Plan B 用に server へ渡せるよう `ServerState` に持たせず、いったん main 内変数に保持して `let _registry = ...` で明示)
- [ ] **Step 2: スモーク** — scratch の plugins-dir に hello-logger を配置して `cargo run -p edlr-core --bin edlr -- --journal-dir <scratch> --plugins-dir <...> --settings-dir <...>`、journal に FSDJump を追記し、stderr に hello-logger のログが出ることを確認
- [ ] **Step 3: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`(hello-logger / busy-loop 側も `cargo fmt` `cargo clippy`)**
- [ ] **Step 4: README の構成・使い方にプラグインの節を追記**(plugins-dir レイアウト、hello-logger のビルドと配置手順)
- [ ] **Step 5: Commit** — `feat(core): wire plugin runtime into edlr daemon`

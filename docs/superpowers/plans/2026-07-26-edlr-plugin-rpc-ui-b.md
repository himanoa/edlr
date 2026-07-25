# edlr プラグイン WS RPC + UI 接続(Plan B)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** WS プロトコルのクライアント→サーバ方向を開通して `plugins/list` / `plugins/get-settings` / `plugins/set-settings` を提供し、フロントの Plugins 画面をモックから本物のプラグインデータに置き換える。

**Architecture:** Plan A の `Registry` を RPC が使える形に拡張(SettingsStore を保持し、値の取得・検証付き更新・共有 settings_json の反映を担う)→ `ServerState` に `Option<Registry>` を持たせ、`/ws` の受信メッセージを RPC として処理 → フロントは Plugins 画面専用の RPC クライアントで接続。設計書: `docs/superpowers/specs/2026-07-25-edlr-plugin-runtime-design.md`

**Tech Stack:** 既存(axum ws / serde_json / React + TS + vitest)。新規依存なし。

## Global Constraints

- RPC のワイヤ形式は設計書どおり:
  - client→server: `{"type":"rpc","id":<number>,"method":"<name>","params":{…}}`
  - server→client: `{"type":"rpc-result","id":<number>,"result":<value>}` / `{"type":"rpc-error","id":<number>,"error":"<message>"}`
- `plugins/list` の result は `{ pluginsDir, plugins: [{ id, name, version, description, state, reason?, settings, values }] }`
- 未知 method・params 不正・未知 plugin・未知 設定 key・型不一致 → `rpc-error`(接続は維持)
- `type` が `rpc` でないメッセージ、パース不能なメッセージは従来どおり**無視**(切断しない)
- `id` が無い/数値でない `rpc` は無視(応答先が定まらないため)
- プラグイン機能が無効(PluginHost 初期化失敗)の場合、RPC は `rpc-error` で "plugins unavailable" を返す
- 設定更新後は共有 `settings_json` を更新し、次回の `host-settings.get-all` に反映されること
- サーバは panic しない。イベント配信は RPC 処理でブロックされないこと
- コミットメッセージ末尾に `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

---

### Task 1: Registry の RPC 対応拡張

**Files:**
- Modify: `core/src/plugin/registry.rs`, `core/src/plugin/runner.rs`
- Test: `core/src/plugin/registry.rs` 内ユニット + `core/tests/plugin_runner_integration.rs` に 1 ケース追加

**Interfaces:**
- Produces(`registry.rs`):

```rust
/// RPC 応答用のプラグイン情報スナップショット。
pub struct PluginInfo {
    pub manifest: Manifest,
    pub state: PluginState,
    pub values: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug)]
pub enum RegistryError { UnknownPlugin(String), Settings(crate::plugin::SettingsError) }
// Display + std::error::Error を実装

impl Registry {
    pub fn plugins_dir(&self) -> &Path;
    pub fn list(&self) -> Vec<PluginInfo>;
    pub fn values(&self, id: &str) -> Result<serde_json::Map<String, serde_json::Value>, RegistryError>;
    /// 検証 → 永続化 → 共有 settings_json 更新。更新後の全値を返す。
    pub fn set_values(&self, id: &str, values: &serde_json::Map<String, serde_json::Value>)
        -> Result<serde_json::Map<String, serde_json::Value>, RegistryError>;
}
```

実装要件:
- `Registry` に `settings_store: SettingsStore`(または `Arc<SettingsStore>`)と `plugins_dir: PathBuf` を保持させる。`start_plugins` は現在 `SettingsStore` を消費しているので、Registry に移して保持する
- `values()` は SettingsStore の `effective(&manifest)` を返す(共有文字列ではなくストア由来を正とする)
- `set_values()`: `SettingsStore::update` で検証 + 永続化 → 新しい effective を計算 → 該当エントリの `settings_json`(`Arc<Mutex<String>>`)を新しい JSON 文字列で上書き → 新 effective を返す
- 既存の `snapshot()` / `entry_settings()` / `set_disabled()` は互換を保つ(呼び出し元があるため)

- [ ] **Step 1: 失敗するテストを書く**

`registry.rs` ユニット(Registry を直接構築できるようテスト用コンストラクタが必要なら `#[cfg(test)]` で用意する。プラグインを 1 件も持たない Registry でも `values`/`set_values` の未知 ID パスは検証できる):
1. 未知 ID → `values()` が `RegistryError::UnknownPlugin`
2. 未知 ID → `set_values()` が `RegistryError::UnknownPlugin`

`core/tests/plugin_runner_integration.rs` に追加(hello-logger を使う):
3. `start_plugins` 後、`registry.list()` が 1 件返り、`values["enabled"]` がマニフェスト default と一致する
4. `set_values(id, {"enabled": false})` が Ok を返し、(a) 戻り値と `values()` の両方で false になり、(b) `entry_settings(id)` の共有文字列をパースしても `"enabled": false` になっている(= プラグインから見える)
5. `set_values(id, {"nope": 1})` が Err(Settings 経由の未知 key)で、既存の値が変わらない

- [ ] **Step 2: 失敗確認** → **Step 3: 実装** → **Step 4: `cargo test --workspace` PASS**
- [ ] **Step 5: Commit** — `feat(core): registry settings access for rpc`

---

### Task 2: WS RPC ハンドラ

**Files:**
- Modify: `core/src/server.rs`
- Test: `core/tests/ws_rpc_integration.rs`(新規)

**Interfaces:**
- Produces:
  - `ServerState::new(router: &Router, registry: Option<Registry>) -> ServerState`(既存呼び出し元は `None` を渡すよう更新)
  - `pub fn handle_rpc(registry: Option<&Registry>, method: &str, params: &serde_json::Value) -> Result<serde_json::Value, String>`(純関数。テスト容易性のため公開)
  - クライアント受信ループで `{"type":"rpc",...}` を検出したら `handle_rpc` を呼び、`rpc-result` / `rpc-error` を同じソケットに返す

`handle_rpc` の仕様:
- `"plugins/list"`: params 不要。`{ pluginsDir, plugins: [...] }` を返す。各要素の `state` は `"running"` / `"disabled"`、disabled のときのみ `reason` を含める。`settings` はマニフェストの設定スキーマ(serde でそのままシリアライズ)、`values` は現在値
- `"plugins/get-settings"`: `params.plugin` が文字列でなければ Err("params.plugin must be a string")。未知 ID は Err。成功時は values オブジェクト
- `"plugins/set-settings"`: `params.plugin`(文字列)と `params.values`(オブジェクト)が必要。検証エラーはそのまま Err メッセージへ。成功時は更新後の values オブジェクト
- registry が `None` の場合はすべて Err("plugins unavailable")
- 未知 method → Err(`unknown method: <name>`)

受信ループ要件:
- RPC 処理は同期的に完了させてよい(短時間)。ただしイベント配信タスクをブロックしないよう、既存の `tokio::select!` 構造を壊さないこと
- `id` が数値でない/欠落している rpc メッセージは無視(応答しない)
- RPC 以外のメッセージは従来どおり無視

- [ ] **Step 1: 失敗するテストを書く**(`core/tests/ws_rpc_integration.rs`)

Plan A の `plugin_runner_integration.rs` のフィクスチャビルド手法と `ws_integration.rs` の接続ヘルパを流用し、hello-logger を配置した tempdir で `start_plugins` → `ServerState::new(&router, Some(registry))` → サーバ起動:
1. `plugins/list` → result が `pluginsDir` を含み、`plugins` が 1 件、`id == "hello-logger"`、`state == "running"`、`settings` が配列、`values.enabled` が default
2. `plugins/get-settings` (plugin=hello-logger) → values が返る / 未知 plugin → `rpc-error`
3. `plugins/set-settings` で `{"enabled": false}` → `rpc-result` の values が false、続けて `get-settings` しても false(永続化されている)
4. `plugins/set-settings` で未知 key → `rpc-error`、その後 `get-settings` の値が変わっていない
5. 未知 method → `rpc-error`
6. 不正メッセージ(`"not json"` と `{"type":"nonsense"}`)を送っても接続が維持され、その後の `plugins/list` が成功する
7. RPC 応答とイベント配信が同一ソケットで多重化されること: `plugins/list` を送る前後に `router.publish(...)` してイベントも届き、rpc-result も届く(受信メッセージを type で振り分けて両方確認)
8. registry が `None` の ServerState では `plugins/list` が `rpc-error`("plugins unavailable")

- [ ] **Step 2: 失敗確認** → **Step 3: 実装** → **Step 4: 全テスト PASS**
- [ ] **Step 5: Commit** — `feat(core): plugins rpc over websocket`

---

### Task 3: bin 結線

**Files:**
- Modify: `core/src/bin/edlr.rs`

**Interfaces:**
- Consumes: Task 1・2 の成果

要件: `start_plugins` の戻り値 `Registry` を `ServerState::new(&router, registry.clone())` に渡す(`Registry` は `Clone`)。プラグイン機能が無効な場合は `None`。既存の起動順(プラグイン → サーバ/監視)と warn-and-continue の挙動を維持する。

- [ ] **Step 1: 実装**
- [ ] **Step 2: `cargo test --workspace` / clippy / fmt clean**
- [ ] **Step 3: スモーク** — Plan A のスモーク手順(scratch の plugins/journal/settings dir、`--listen 127.0.0.1:18137`)でデーモンを起動し、`websocat` は使えない前提なので簡易 Rust or curl では WS を張れないため、**代わりに `cargo test --test ws_rpc_integration` が通ることをもって結線検証**とし、デーモン起動時に plugins が load される既存ログ(hello-logger initialized)が出ることだけ確認する
- [ ] **Step 4: Commit** — `feat(core): expose plugin registry to the websocket server`

---

### Task 4: フロント RPC クライアント

**Files:**
- Create: `ui/frontend/src/rpc.ts`, `ui/frontend/src/rpc.test.ts`

**Interfaces:**
- Produces:

```ts
export type RpcResponse =
  | { type: "rpc-result"; id: number; result: unknown }
  | { type: "rpc-error"; id: number; error: string };

/** サーバからの 1 メッセージを RPC 応答としてパースする。RPC 応答でなければ null。 */
export function parseRpcResponse(data: string): RpcResponse | null;

export class RpcClient {
  constructor(url: string, timeoutMs?: number); // 既定 5000
  call<T = unknown>(method: string, params?: unknown): Promise<T>; // rpc-error は reject(Error(message))
  close(): void;
}
```

実装要件:
- 単一 WebSocket を開き、`id` を 1 から採番して pending Map で応答を突き合わせる
- 接続確立前の `call` は接続完了後に送る(キューする)
- タイムアウトで reject し pending から除去
- ソケットが閉じたら pending をすべて reject
- イベントメッセージ(`type: "event"` / `"hello"`)は無視する

- [ ] **Step 1: 失敗するテストを書く**(`rpc.test.ts`。`WebSocket` をテスト用のフェイククラスに差し替えて検証する。jsdom には WebSocket がないか実接続できないため、`vi.stubGlobal("WebSocket", FakeWebSocket)` で制御する)
  1. `parseRpcResponse`: rpc-result / rpc-error をパース、`{"type":"event",...}` と壊れた JSON は null
  2. `call` が送信フレームに `{type:"rpc", id, method, params}` を含み、対応する rpc-result で resolve する
  3. rpc-error は Error(message) で reject する
  4. 2 つの `call` を並行に出し、応答が逆順で返っても正しく対応付く
  5. 応答が来なければタイムアウトで reject する(フェイクタイマー使用可)
  6. ソケット close で pending が reject される

- [ ] **Step 2: 失敗確認**(`cd ui/frontend && pnpm test`。node が PATH に無ければ `mise exec -- pnpm test`)→ **Step 3: 実装** → **Step 4: PASS**
- [ ] **Step 5: Commit** — `feat(ui): websocket rpc client`

---

### Task 5: Plugins 画面を本物のデータへ

**Files:**
- Modify: `ui/frontend/src/pages/Plugins.tsx`, `ui/frontend/src/components/PluginForm.tsx`, `ui/frontend/src/index.css`
- Delete: `ui/frontend/src/mock/plugins.ts`, `ui/frontend/src/lib/settings.ts` とそれぞれのテスト(`src/lib/settings.test.ts`)
- Create: `ui/frontend/src/types/plugin.ts`(モックの型を昇格させたもの)
- Modify: `ui/frontend/src/components/PluginForm.test.tsx`(新しい props に合わせて書き換え)
- Create: `ui/frontend/src/pages/Plugins.test.tsx`

**Interfaces:**
- Produces(`types/plugin.ts`。マニフェストのシリアライズ形に一致させること):

```ts
export type SettingField =
  | { type: "boolean"; key: string; label: string; default: boolean }
  | { type: "string"; key: string; label: string; default: string }
  | { type: "number"; key: string; label: string; default: number }
  | { type: "select"; key: string; label: string; default: string; options: string[] };

export interface PluginInfo {
  id: string; name: string; version: string; description: string;
  state: "running" | "disabled"; reason?: string;
  settings: SettingField[];
  values: Record<string, unknown>;
}
export interface PluginsList { pluginsDir: string; plugins: PluginInfo[] }
```

- `PluginForm` は props を `{ plugin: PluginInfo; onChange: (key: string, value: unknown) => Promise<void> }` に変更。localStorage 依存を除去。保存中は入力を disabled にし、失敗時はフォーム内にエラーメッセージを表示して値を元に戻す
- `Plugins` 画面: マウント時に `plugins/list` を呼ぶ。読み込み中・エラー・0 件(`pluginsDir` を案内文に表示)の 3 状態を出し分ける。各プラグインに state バッジ(disabled は `reason` を併記)。設定変更は `plugins/set-settings` を呼び、返ってきた values で表示を更新する

- [ ] **Step 1: 失敗するテストを書く**
  - `PluginForm.test.tsx`(書き換え): 4 型のコントロールが描画される / boolean を切り替えると `onChange(key, value)` が呼ばれる / onChange が reject するとエラー表示が出て値が戻る
  - `Plugins.test.tsx`(新規、RpcClient をモックする): ローディング → 一覧表示 / 0 件のとき pluginsDir を含む案内文 / RPC 失敗時にエラー表示 / disabled プラグインに reason が表示される / 設定変更で `plugins/set-settings` が正しい引数で呼ばれ、返り値で表示が更新される
- [ ] **Step 2: 失敗確認** → **Step 3: 実装(モック関連ファイルの削除を含む)** → **Step 4: `pnpm test && pnpm build` PASS**
- [ ] **Step 5: Commit** — `feat(ui): plugins screen backed by live rpc data`

---

### Task 6: 仕上げ

**Files:**
- Modify: `README.md`、必要なら `ui/README.md`

- [ ] **Step 1: 全検証** — `cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cd ui/frontend && pnpm test && pnpm build`
- [ ] **Step 2: 手動 E2E スモーク** — scratch に hello-logger を配置してデーモンを `--listen 127.0.0.1:18137 --ui-dir ui/frontend/dist` で起動(事前に `pnpm build`)。`ui/frontend/vite.config.ts` の proxy は 8137 固定なので、ここでは **daemon が配信する dist** を使い、ブラウザ起動はせず `curl -s http://127.0.0.1:18137/ | head -c 200` で index.html が返ることだけ確認する(RPC 自体は Task 2 の統合テストで検証済み)。終了後にプロセス・ポートが残っていないことを確認する
- [ ] **Step 3: README にプラグイン設定 UI と RPC メソッドの節を追記**(3 メソッドのワイヤ形式、UI での設定変更がデーモンに保存されること)
- [ ] **Step 4: Commit** — `chore: document plugin rpc and settings ui`

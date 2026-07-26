# edlr capability モデル + HTTP ドライバ Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** マニフェストでの capability 宣言、UI 承認による付与、呼び出し時のホスト許可判定、そして実際に通信できる HTTP ドライバを実装する。

**Architecture:** 設計書 `docs/superpowers/specs/2026-07-26-edlr-capability-http-driver-design.md` のとおり。要求はマニフェスト、承認は grants ストア、判定はドライバのホスト実装内(`HostCtx` から読む)。

**Tech Stack:** 既存 + reqwest(blocking)、url。

## Global Constraints

- 許可判定はスキーム + ホスト + ポートの完全一致(ポートはスキーム既定値に正規化、ホストは大文字小文字無視、サブドメインのワイルドカードなし、パスは無視)
- 未承認でもプラグインは通常起動する。未承認・許可外の `send` は `permission-denied`(trap ではない)
- プラグインは自分の ID も許可リストも渡さない。判定に使う値は `HostCtx`(インスタンス固有)からのみ読む
- 承認・取消は再起動なしで次の呼び出しから有効(共有バッファ経由)
- マニフェストの要求が変われば承認は自動失効(要求ハッシュ照合)
- HTTP: リダイレクト追従なし、タイムアウト既定 10 秒、ボディ上限既定 8 MiB
- カーネルは panic しない。既存の全テストを壊さない
- コミットメッセージ末尾に `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

---

### Task 1: マニフェストの capability 宣言と検証

**Files:**
- Modify: `core/src/plugin/manifest.rs`

**Interfaces:**

```rust
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CapabilityRequest {
    Http { hosts: Vec<String>, reason: String },
}

// Manifest に追加
#[serde(default)] pub capabilities: Vec<CapabilityRequest>,

impl Manifest {
    /// 要求一式の安定ハッシュ(grants の失効判定に使う)。要求が空なら None。
    pub fn capabilities_fingerprint(&self) -> Option<String>;
}
```

- `load_manifest` の検証に追加: 各 `hosts` 要素が `http://` または `https://` で始まり URL としてパース可能でホストを持つこと(パス・クエリ付きは検証エラー)、`hosts` が空でないこと、`reason` が空でないこと。新しい `ManifestError` バリアント(`BadCapability(String)`)を追加
- 未知 `kind` は serde の tagged enum が弾く → `Parse` エラー
- `capabilities_fingerprint`: 要求を正規化(ホストは正規化して昇順ソート)した文字列から安定ハッシュを作る。ハッシュ関数は依存追加を避けるため `std::collections::hash_map::DefaultHasher` ではなく**内容そのものの正規化文字列**を使ってよい(安定性が要件、暗号強度は不要)。実装は自由だが「同じ要求 → 同じ値、要求が変われば別の値、実行ごとに変わらない」ことをテストで示すこと

- [ ] **Step 1: 失敗するテストを書く**(manifest.rs 内)
  1. capabilities 付きマニフェストがパースされる(hosts / reason が読める)
  2. `capabilities` 省略時は空 Vec
  3. 未知 kind → エラー
  4. スキームなしホスト(`api.example.com`)→ `BadCapability`
  5. パス付きホスト(`https://api.example.com/v1`)→ `BadCapability`
  6. `hosts = []` → `BadCapability`
  7. `reason = ""` → `BadCapability`
  8. fingerprint: 同一内容の 2 つのマニフェストで一致、ホスト順が違っても一致、ホストを 1 つ足すと不一致、要求なしなら None
- [ ] **Step 2: 失敗確認** → **Step 3: 実装** → **Step 4: `cargo test --workspace` PASS**
- [ ] **Step 5: Commit** — `feat(core): capability declarations in plugin manifests`

---

### Task 2: grants ストア

**Files:**
- Create: `core/src/plugin/grants.rs`
- Modify: `core/src/plugin/mod.rs`

**Interfaces:**

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct GrantState { pub granted: bool, pub stale: bool }

pub struct GrantsStore { /* dir + 内部 Mutex(SettingsStore と同じ流儀) */ }
impl GrantsStore {
    pub fn new(dir: PathBuf) -> Self;
    /// 保存された承認を読み、manifest の現在の fingerprint と照合する。
    /// 未保存 → { granted: false, stale: false }
    /// 保存済みだが fingerprint 不一致 → { granted: false, stale: true }
    pub fn state(&self, manifest: &Manifest) -> GrantState;
    /// 承認/取消を保存する。granted=true のとき現在の fingerprint を一緒に保存する。
    pub fn set(&self, manifest: &Manifest, granted: bool) -> Result<GrantState, GrantsError>;
}
```

- 保存形式: `{"granted": true, "fingerprint": "<value>"}`。壊れた JSON は未保存扱い
- 書き込みは `SettingsStore` と同じく内部 Mutex + temp ファイル + rename の原子的書き込み(`core/src/plugin/settings.rs` の実装に倣う)
- 要求が無いマニフェスト(fingerprint が None)に対しては常に `{ granted: false, stale: false }` を返し、`set` は Ok で何も保存しない

- [ ] **Step 1: 失敗するテストを書く**
  1. 未保存 → granted=false, stale=false
  2. `set(true)` 後に state が granted=true, stale=false、ファイルが実在する
  3. fingerprint が変わる(hosts を足したマニフェストで問い合わせる)と granted=false, stale=true
  4. `set(false)` で取消できる(その後 granted=false, stale=false)
  5. 壊れた JSON → 未保存扱い(granted=false, stale=false)
  6. 要求のないマニフェストは常に granted=false, stale=false で `set` しても失敗しない
  7. dir 不存在でも `set` が作成して成功する
- [ ] **Step 2: 失敗確認** → **Step 3: 実装** → **Step 4: PASS**
- [ ] **Step 5: Commit** — `feat(core): plugin capability grants store`

---

### Task 3: WIT の driver-http と許可判定(ネットワークなし)

**Files:**
- Modify: `core/wit/plugin.wit`, `core/src/plugin/host.rs`, `core/src/plugin/runner.rs`, `core/src/plugin/registry.rs`
- Create: `core/src/plugin/allowlist.rs`
- Create: `examples/plugins/http-caller/`(フィクスチャ)
- Modify: `core/tests/plugin_host_integration.rs`

**Interfaces:**

```rust
// allowlist.rs
/// 許可リストに対して URL を判定する。許可されていれば Ok(()) を返す。
pub fn check_url(granted_hosts: &[String], url: &str) -> Result<(), String>;
```

- 判定規則は Global Constraints のとおり。実装のヒント: `url` crate でパースし、`scheme`・`host_str`(小文字化)・`port_or_known_default` を比較する
- `HostCtx` に `capabilities_json: Arc<Mutex<String>>` を追加(設定と同じ流儀)。形は `{"granted": bool, "hosts": ["https://..."]}`。runner が起動時に GrantsStore + Manifest から作り、Registry が更新できるよう共有する
- WIT に設計書の `driver-http` を追加し、`world plugin` に `import driver-http;` を足す
- `driver-http.send` のホスト実装(このタスクではネットワークを呼ばない): capabilities_json を読み、granted でなければ `permission-denied("capability not granted")`、URL が許可外なら `permission-denied(理由)`、許可されていれば **このタスクでは** `transport("not implemented")` を返す(Task 4 で実処理に置き換える)
- `Registry` に `set_capabilities(id, granted) -> Result<GrantState, RegistryError>` を追加(GrantsStore を保持し、共有 capabilities_json も更新する)

`examples/plugins/http-caller` フィクスチャ: `on-event` で `driver-http.send` を 1 回呼び、結果(Ok / Err の種別とメッセージ)を `host-log` に出す。呼び先 URL は設定値 `url`(既定 `https://api.example.com/ping`)から読む。既存フィクスチャと同じ独立 crate 構成。

- [ ] **Step 1: 失敗するテストを書く**
  - `allowlist.rs` 単体: 完全一致で許可 / スキーム違いは拒否 / ポート違いは拒否 / 既定ポート明示(`https://a.example.com:443`)は省略形と一致 / 大文字ホストは一致 / サブドメインは拒否 / パス付き URL でもホスト一致なら許可 / 不正 URL は拒否 / 非 http(s) スキームは拒否 / granted リストが空なら拒否
  - `core/tests/plugin_host_integration.rs` に統合: http-caller を未承認(capabilities_json が granted=false)でロードして `on-event` を呼ぶと Ok で返る(trap しない)こと。granted=true + 許可ホストでは `transport("not implemented")` 相当になること(ログではなく、ホスト側で観測できる形にするのが難しいので、**プラグインの返り値ではなくホスト実装を直接呼ぶ単体テスト**でも可。実 wasm 経由では「trap しない」ことだけ確認する)
- [ ] **Step 2: 失敗確認** → **Step 3: 実装** → **Step 4: `cargo test --workspace` PASS**
- [ ] **Step 5: Commit** — `feat(core): driver-http wit interface and host allowlist enforcement`

---

### Task 4: HTTP ドライバの実処理

**Files:**
- Modify: `drivers/http/Cargo.toml`, `drivers/http/src/lib.rs`, `core/Cargo.toml`, `core/src/plugin/host.rs`
- Create: `core/tests/driver_http_integration.rs`

**Interfaces:**

```rust
// drivers/http
pub struct HttpDriver { /* reqwest::blocking::Client */ }
pub struct HttpRequest { pub method: String, pub url: String, pub headers: Vec<(String, String)>, pub body: Option<Vec<u8>> }
pub struct HttpResponse { pub status: u16, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
pub enum HttpError { InvalidRequest(String), Transport(String) }
impl HttpDriver {
    pub fn new(timeout: Duration, max_body_bytes: usize) -> Result<Self, HttpError>;
    pub fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;
}
```

- reqwest は `blocking` + `rustls-tls` を有効化し、`redirect(reqwest::redirect::Policy::none())` を設定する
- ボディ上限: レスポンス読み取り時に上限超過なら `Transport`。実装はストリーミング読みで上限打ち切りが望ましいが、`Content-Length` 事前チェック + 読み取り後サイズ確認でも可
- `core` は `edlr-driver-http` に依存し、`driver-http.send` のホスト実装を Task 3 のスタブから実処理へ置き換える。既定値は定数(`HTTP_TIMEOUT: 10s`、`HTTP_MAX_BODY: 8 MiB`)
- 許可判定は引き続きホスト実装側(ドライバ crate は判定を持たない)

- [ ] **Step 1: 失敗するテストを書く**(`core/tests/driver_http_integration.rs`)

テスト用 HTTP サーバはローカルに立てる(axum は既に依存にあるので、`127.0.0.1:0` で bind して使う)。ケース:
  1. 許可済みホスト(テストサーバのアドレス)への GET が 200 と本文を返す
  2. POST でボディとヘッダが往復する(サーバ側でエコーする)
  3. 許可外ホストは `permission-denied`(実際に接続しないこと。サーバを立てずに判定だけで弾かれる)
  4. リダイレクトを返すエンドポイントで 3xx がそのまま返る(追従しない)
  5. 上限超過ボディで `transport` エラー
  6. 接続不能アドレスで `transport` エラー(panic しない)

- [ ] **Step 2: 失敗確認** → **Step 3: 実装** → **Step 4: PASS(`cargo test --workspace`)**
- [ ] **Step 5: Commit** — `feat(drivers): http driver with timeout, size cap, and no redirects`

---

### Task 5: RPC と Registry 拡張

**Files:**
- Modify: `core/src/plugin/registry.rs`, `core/src/server.rs`
- Modify: `core/tests/ws_rpc_integration.rs`

**Interfaces:**
- `Registry::capabilities(id) -> Result<(Vec<CapabilityRequest>, GrantState), RegistryError>`
- RPC `plugins/get-capabilities` / `plugins/set-capabilities`(設計書の形)
- `plugins/list` の各要素に `capabilities: { requests, granted, staleGrant }` を追加

- [ ] **Step 1: 失敗するテストを書く**(ws_rpc_integration に追加。http-caller フィクスチャを使う)
  1. `plugins/list` の要素に capabilities が含まれ、初期は granted=false, staleGrant=false、requests に宣言が入る
  2. `plugins/get-capabilities` が同じ内容を返す / 未知 plugin は rpc-error
  3. `plugins/set-capabilities` `{granted: true}` で granted=true になり、`get-capabilities` と `list` にも反映される
  4. `set-capabilities` `{granted: false}` で取消できる
  5. params 不正(plugin 欠落 / granted が bool でない)は rpc-error
- [ ] **Step 2: 失敗確認** → **Step 3: 実装** → **Step 4: PASS**
- [ ] **Step 5: Commit** — `feat(core): capability rpc methods`

---

### Task 6: 承認 UI

**Files:**
- Modify: `ui/frontend/src/types/plugin.ts`, `ui/frontend/src/pages/Plugins.tsx`, `ui/frontend/src/index.css`
- Create: `ui/frontend/src/components/CapabilitySection.tsx`, `ui/frontend/src/components/CapabilitySection.test.tsx`
- Modify: `ui/frontend/src/pages/Plugins.test.tsx`

**Interfaces:**

```ts
export interface CapabilityRequest { kind: "http"; hosts: string[]; reason: string }
export interface Capabilities { requests: CapabilityRequest[]; granted: boolean; staleGrant: boolean }
// PluginInfo に capabilities: Capabilities を追加
```

- `CapabilitySection` props: `{ capabilities: Capabilities; onToggle: (granted: boolean) => Promise<void> }`
- 要求が空なら何も描画しない
- 各要求の kind・reason・ホスト一覧を表示
- 承認トグル。未承認時は「未承認 — このプラグインは外部通信できません」、`staleGrant` 時は「要求が変わったため再承認が必要」を警告として表示
- トグル失敗時はエラー表示 + 状態を元に戻す(PluginForm と同じ流儀)
- Plugins 画面は `plugins/set-capabilities` を呼び、返った状態で表示を更新する

- [ ] **Step 1: 失敗するテストを書く**
  - `CapabilitySection.test.tsx`: 要求なしで何も描画しない / kind・hosts・reason が出る / 未承認の注意文が出る / staleGrant の警告が出る / トグルで onToggle(true) が呼ばれる / onToggle が reject したらエラー表示 + トグルが元に戻る
  - `Plugins.test.tsx` 追加: capability セクションが表示され、トグルで `plugins/set-capabilities` が正しい引数で呼ばれ、返り値で表示が更新される
- [ ] **Step 2: 失敗確認** → **Step 3: 実装** → **Step 4: `pnpm test && pnpm build` PASS**
- [ ] **Step 5: Commit** — `feat(ui): capability approval section`

---

### Task 7: 結線と仕上げ

**Files:**
- Modify: `core/src/bin/edlr.rs`, `core/src/config.rs`, `README.md`
- Modify: `examples/plugins/hello-logger/README.md`(必要なら)

- [ ] **Step 1: CLI に `--grants-dir` を追加**(既定 `<config>/edlr/grants`。`config::config_subdir` を使う)。`GrantsStore` を作って `start_plugins` に渡し、Registry が保持する形に結線する
- [ ] **Step 2: 全検証** — `cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cd ui/frontend && pnpm test && pnpm build`(node が無ければ `mise exec -- pnpm`)。フィクスチャ crate も個別に clippy を通す
- [ ] **Step 3: E2E スモーク** — scratch に http-caller を配置(`url` 設定をローカルの簡易サーバに向ける)。デーモンを起動して journal にイベントを追記し、(a) 未承認では `permission-denied` のログが出ること、(b) `plugins/set-capabilities` 相当の grants ファイルを置いて再起動せずに承認が効くかは UI 経由でないと難しいので、**grants ファイルを事前に置いた状態で起動して通信が成功する**ことを確認する。実際の通信先は `python3 -m http.server` などローカルに立てる。終了後にプロセス・ポートが残っていないことを確認する
- [ ] **Step 4: README にドライバ capability の節を追記** — マニフェストの `[[capabilities]]` 書式、承認フロー(UI)、HTTP ドライバの制約(リダイレクト不追従・タイムアウト・サイズ上限・完全一致)、`--grants-dir`
- [ ] **Step 5: Commit** — `feat(core): wire grants store and document capabilities`

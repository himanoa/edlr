# ダッシュボードウィジェット実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** プラグインが manifest の `[[dashboard]]` で宣言した HTML ウィジェットを、grant 済みのものだけ Dashboard タブに iframe で表示する。

**Architecture:** 既存 capability(bus)の declare→grant→検証パターンを丸ごと踏襲。manifest 宣言 → GrantsStore 永続化(fingerprint で staleGrant)→ Registry が `DashboardInfo` を構築 → server.rs が RPC(`plugins/set-dashboard-grant`, `dashboard/list`)+ 静的アセット配信(`/plugin-ui/...`、grant 必須・トラバーサル拒否・CSP付与)→ フロントは Plugins 画面に DashboardSection(grant UI)、Dashboard 画面に iframe グリッド + postMessage ブリッジ。

**Tech Stack:** Rust (axum 0.8, tower-http 0.6), React 18 + TypeScript + vitest。

**Spec:** `docs/superpowers/specs/2026-07-28-edlr-dashboard-widgets-design.md`

## Global Constraints

- ウィジェット `id` は `[a-z0-9-]+`、プラグイン内で一意。`size` は `small|medium|large`。
- `entry` はプラグインディレクトリ内相対パス。`..`・絶対パスはロード時拒否。entry ファイル不在は起動を止めず `resolved: false`。
- 未 grant / stale grant のウィジェットアセットは 404。パストラバーサルも 404。
- iframe は `sandbox="allow-scripts"` のみ(`allow-same-origin` 禁止)。
- アセットレスポンスに CSP: `default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'none'`。
- postMessage プロトコル: widget→親 `edlr:ready` / `edlr:height`、親→widget `edlr:init` / `edlr:event`。イベント転送は manifest `events` のマッチ分のみ(`*`=全journal、`status`=Status、他は完全一致、空=なし)。
- bus retained 値の転送(`edlr:bus`)は v1 スコープ外。
- TDD: 各タスクは失敗するテストを先に書き、失敗を確認してから実装する。
- `Manifest` 構造体へのフィールド追加は、リテラル構築しているテスト(manifest.rs / grants.rs / registry.rs 内)に `dashboard: vec![]` を追加して回す。
- cargo コマンドは並走させない(CLAUDE.md)。

---

### Task 1: Manifest に `[[dashboard]]` を追加

**Files:**
- Modify: `core/src/plugin/manifest.rs`

**Interfaces:**
- Produces: `DashboardWidget { id, title, entry, size }`, `WidgetSize::{Small,Medium,Large}`(`as_str()` → "small"/"medium"/"large")、`Manifest.dashboard: Vec<DashboardWidget>`、`Manifest::dashboard_widget(&self, id) -> Option<&DashboardWidget>`、`Manifest::dashboard_fingerprint(&self, id) -> Option<String>`、`ManifestError::BadDashboard(String)`

- [ ] **Step 1: 失敗するテストを書く**(`manifest.rs` の `mod tests` に追加)

```rust
#[test]
fn dashboard_section_parses_and_validates() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("widgety");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.wasm"), b"\0asm").unwrap();
    std::fs::write(
        plugin.join("manifest.toml"),
        r#"
id = "widgety"
name = "W"
version = "0.1.0"
entry = "plugin.wasm"

[[dashboard]]
id = "status"
title = "Status"
entry = "ui/status/index.html"
size = "medium"
"#,
    )
    .unwrap();
    let manifest = load_manifest(&plugin).unwrap();
    assert_eq!(manifest.dashboard.len(), 1);
    let w = manifest.dashboard_widget("status").unwrap();
    assert_eq!(w.title, "Status");
    assert_eq!(w.size, WidgetSize::Medium);
    assert_eq!(w.size.as_str(), "medium");
}

#[test]
fn dashboard_rejects_bad_id_duplicate_and_traversal_entry() {
    // ヘルパー: dashboard セクションだけ差し替えて load する
    fn load_with(section: &str) -> Result<Manifest, ManifestError> {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("widgety");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.wasm"), b"\0asm").unwrap();
        std::fs::write(
            plugin.join("manifest.toml"),
            format!(
                "id = \"widgety\"\nname = \"W\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n{section}"
            ),
        )
        .unwrap();
        load_manifest(&plugin)
    }
    let bad_id = load_with(
        "[[dashboard]]\nid = \"Bad_ID\"\ntitle = \"t\"\nentry = \"ui/a.html\"\nsize = \"small\"\n",
    );
    assert!(matches!(bad_id, Err(ManifestError::BadDashboard(_))));
    let dup = load_with(
        "[[dashboard]]\nid = \"a\"\ntitle = \"t\"\nentry = \"ui/a.html\"\nsize = \"small\"\n\n[[dashboard]]\nid = \"a\"\ntitle = \"t\"\nentry = \"ui/b.html\"\nsize = \"small\"\n",
    );
    assert!(matches!(dup, Err(ManifestError::BadDashboard(_))));
    let traversal = load_with(
        "[[dashboard]]\nid = \"a\"\ntitle = \"t\"\nentry = \"../outside.html\"\nsize = \"small\"\n",
    );
    assert!(matches!(traversal, Err(ManifestError::BadDashboard(_))));
    let absolute = load_with(
        "[[dashboard]]\nid = \"a\"\ntitle = \"t\"\nentry = \"/etc/passwd\"\nsize = \"small\"\n",
    );
    assert!(matches!(absolute, Err(ManifestError::BadDashboard(_))));
    let empty_title = load_with(
        "[[dashboard]]\nid = \"a\"\ntitle = \"  \"\nentry = \"ui/a.html\"\nsize = \"small\"\n",
    );
    assert!(matches!(empty_title, Err(ManifestError::BadDashboard(_))));
}

#[test]
fn dashboard_entry_missing_file_does_not_fail_load() {
    // entry ファイル不在はロード成功(resolved 判定は Registry 側の責務)
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("widgety");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.wasm"), b"\0asm").unwrap();
    std::fs::write(
        plugin.join("manifest.toml"),
        "id = \"widgety\"\nname = \"W\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n[[dashboard]]\nid = \"a\"\ntitle = \"t\"\nentry = \"ui/nonexistent.html\"\nsize = \"large\"\n",
    )
    .unwrap();
    assert!(load_manifest(&plugin).is_ok());
}

#[test]
fn dashboard_fingerprint_changes_with_each_field() {
    fn manifest_with(id: &str, title: &str, entry: &str, size: WidgetSize) -> Manifest {
        Manifest {
            id: "p".into(), name: "P".into(), version: "0".into(),
            description: String::new(), entry: "plugin.wasm".into(),
            events: vec![], settings: vec![], capabilities: vec![],
            sidecars: vec![], filesystem: vec![], bus: vec![],
            dashboard: vec![DashboardWidget {
                id: id.into(), title: title.into(), entry: entry.into(), size,
            }],
        }
    }
    let base = manifest_with("a", "t", "ui/a.html", WidgetSize::Small);
    let fp = base.dashboard_fingerprint("a").unwrap();
    assert_eq!(fp, base.dashboard_fingerprint("a").unwrap());
    assert_ne!(fp, manifest_with("a", "t2", "ui/a.html", WidgetSize::Small).dashboard_fingerprint("a").unwrap());
    assert_ne!(fp, manifest_with("a", "t", "ui/b.html", WidgetSize::Small).dashboard_fingerprint("a").unwrap());
    assert_ne!(fp, manifest_with("a", "t", "ui/a.html", WidgetSize::Large).dashboard_fingerprint("a").unwrap());
    assert!(base.dashboard_fingerprint("missing").is_none());
}
```

- [ ] **Step 2: 失敗を確認** — `cargo test -p edlr-core dashboard` → コンパイルエラー(型・フィールド不在)

- [ ] **Step 3: 実装**

`FilesystemRequest` の隣に追加:

```rust
/// ダッシュボードウィジェットのサイズ(グリッドのカラムスパンに対応)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WidgetSize {
    Small,
    Medium,
    Large,
}

impl WidgetSize {
    pub fn as_str(&self) -> &'static str {
        match self {
            WidgetSize::Small => "small",
            WidgetSize::Medium => "medium",
            WidgetSize::Large => "large",
        }
    }
}

/// プラグインが宣言するダッシュボードウィジェット 1 件(`[[dashboard]]`)。
///
/// `entry` はプラグインディレクトリからの相対パス。ファイルの実在は
/// ロード時には要求しない(未解決は Registry が `resolved: false` として
/// UI バッジで報せる — bus の未解決参照と同じセマンティクス)。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct DashboardWidget {
    pub id: String,
    pub title: String,
    pub entry: String,
    pub size: WidgetSize,
}
```

`Manifest` にフィールド追加(`bus` の下):

```rust
    #[serde(default)]
    pub dashboard: Vec<DashboardWidget>,
```

`ManifestError` に `BadDashboard(String)` variant + Display: `write!(f, "invalid dashboard widget: {msg}")`。

アクセサ + fingerprint(`bus_request`/`bus_fingerprint` の隣):

```rust
    pub fn dashboard_widget(&self, id: &str) -> Option<&DashboardWidget> {
        self.dashboard.iter().find(|w| w.id == id)
    }

    /// grant の stale 判定に使う fingerprint。宣言のどのフィールドが
    /// 変わっても値が変わる(bus_fingerprint と同じ流儀)。
    pub fn dashboard_fingerprint(&self, id: &str) -> Option<String> {
        let widget = self.dashboard_widget(id)?;
        let mut canonical = encode_field("dashboard");
        canonical.push_str(&encode_field(&widget.id));
        canonical.push_str(&encode_field(&widget.title));
        canonical.push_str(&encode_field(&widget.entry));
        canonical.push_str(&encode_field(widget.size.as_str()));
        Some(sha256_hex(&canonical))
    }
```

バリデータ(`validate_filesystem` の流儀):

```rust
pub(crate) fn validate_dashboard(widgets: &mut [DashboardWidget]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for widget in widgets.iter_mut() {
        if !is_valid_id(&widget.id) {
            return Err(ManifestError::BadDashboard(format!(
                "dashboard id must match [a-z0-9-]+: {}",
                widget.id
            )));
        }
        if !seen.insert(widget.id.clone()) {
            return Err(ManifestError::BadDashboard(format!(
                "duplicate dashboard id: {}",
                widget.id
            )));
        }
        let title = widget.title.trim().to_string();
        if title.is_empty() {
            return Err(ManifestError::BadDashboard(
                "dashboard widget requires a non-empty title".to_string(),
            ));
        }
        reject_invisible_chars("title", &title).map_err(ManifestError::BadDashboard)?;
        widget.title = title;
        validate_widget_entry(&widget.entry)?;
    }
    Ok(())
}

/// `entry` がプラグインディレクトリ内に収まる相対パスであることの検証。
/// 絶対パス・`..`・空・ルート/プレフィックス成分を拒否する。
fn validate_widget_entry(entry: &str) -> Result<(), ManifestError> {
    use std::path::Component;
    let path = std::path::Path::new(entry);
    if entry.is_empty()
        || path.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(ManifestError::BadDashboard(format!(
            "dashboard entry must be a relative path inside the plugin directory: {entry}"
        )));
    }
    Ok(())
}
```

`load_manifest` の `validate_bus` の後に `validate_dashboard(&mut manifest.dashboard)?;` を追加。

- [ ] **Step 4: `Manifest` リテラル構築箇所を修正** — `cargo test --workspace` のコンパイルエラー箇所(manifest.rs / grants.rs / registry.rs のテスト内)全てに `dashboard: vec![],` を追加

- [ ] **Step 5: テストが通ることを確認** — `cargo test -p edlr-core` 全パス

- [ ] **Step 6: Commit** — `git commit -m "feat(core): parse and validate [[dashboard]] manifest section"`

---

### Task 2: GrantsStore に dashboard grant を追加

**Files:**
- Modify: `core/src/plugin/grants.rs`

**Interfaces:**
- Consumes: `Manifest::dashboard_fingerprint`
- Produces: `GrantsStore::dashboard_state(&Manifest, widget_id) -> GrantState`、`GrantsStore::set_dashboard(&Manifest, widget_id, granted) -> Result<GrantState, GrantsError>`

- [ ] **Step 1: 失敗するテストを書く**(`grants.rs` の `mod tests`。既存の bus テストを雛形に。fixture の `Manifest` には `dashboard: vec![DashboardWidget { id: "status".into(), title: "S".into(), entry: "ui/index.html".into(), size: WidgetSize::Small }]` を持たせる)

```rust
#[test]
fn dashboard_grant_persists_and_goes_stale_on_fingerprint_change() {
    let dir = tempfile::tempdir().unwrap();
    let store = GrantsStore::new(dir.path().to_path_buf());
    let manifest = manifest_with_dashboard(); // fixture: 上記 widget "status" 入り

    // 初期状態: 未承認・stale でない
    let initial = store.dashboard_state(&manifest, "status");
    assert!(!initial.granted);
    assert!(!initial.stale);

    // 承認 → granted
    let granted = store.set_dashboard(&manifest, "status", true).unwrap();
    assert!(granted.granted);
    assert!(store.dashboard_state(&manifest, "status").granted);

    // manifest 側の宣言が変わる → staleGrant
    let mut changed = manifest.clone();
    changed.dashboard[0].entry = "ui/other.html".into();
    let state = store.dashboard_state(&changed, "status");
    assert!(!state.granted);
    assert!(state.stale);

    // 未宣言 widget は常に false/false
    let unknown = store.dashboard_state(&manifest, "nope");
    assert!(!unknown.granted && !unknown.stale);
}
```

- [ ] **Step 2: 失敗を確認** — `cargo test -p edlr-core dashboard_grant` → メソッド不在でコンパイルエラー

- [ ] **Step 3: 実装** — `SavedGrant` に `#[serde(default)] dashboard: BTreeMap<String, SavedSidecarGrant>` を追加し、`bus_state`/`bus_state_locked`/`set_bus`(grants.rs:311–375)を丸写しして `dashboard_state`/`dashboard_state_locked`/`set_dashboard` を実装(fingerprint 取得だけ `manifest.dashboard_fingerprint(id)` に差し替え、`saved.bus` → `saved.dashboard`)

- [ ] **Step 4: パスを確認** — `cargo test -p edlr-core` 全パス

- [ ] **Step 5: Commit** — `git commit -m "feat(core): persist dashboard widget grants with stale detection"`

---

### Task 3: Registry に DashboardInfo と grant 操作・アセットパス解決を追加

**Files:**
- Modify: `core/src/plugin/registry.rs`

**Interfaces:**
- Consumes: Task 1 の型、Task 2 の GrantsStore API
- Produces:
  - `pub struct DashboardInfo { pub request: DashboardWidget, pub grant: GrantState, pub resolved: bool }`
  - `Registry::dashboard(&self, id) -> Result<Vec<DashboardInfo>, RegistryError>`
  - `Registry::set_dashboard_grant(&self, id, widget, granted) -> Result<Vec<DashboardInfo>, RegistryError>`
  - `Registry::dashboard_asset_path(&self, plugin, widget, rel_path) -> Result<PathBuf, RegistryError>`(grant 必須・トラバーサル拒否込み)
  - `Registry::dashboard_widgets_for_ui(&self)`: 全プラグイン分の `(plugin_id, plugin_name, state, DashboardInfo)` を返す(`dashboard/list` 用)
  - `PluginInfo.dashboard: Vec<DashboardInfo>` フィールド追加
  - `RegistryError::UnknownDashboard(String)`(Display: `unknown dashboard widget: {0}`)、`RegistryError::DashboardNotGranted(String)`(Display: `dashboard widget not granted: {0}`)

- [ ] **Step 1: 失敗するテストを書く**(registry.rs `mod tests`。`test_registry_with_bus_request` の流儀で、`[[dashboard]]` 宣言入り manifest + plugins_dir に entry ファイルを実際に置く fixture `test_registry_with_dashboard()` を作る)

```rust
#[test]
fn dashboard_reports_resolved_only_when_entry_file_exists() {
    let (registry, plugins_dir) = test_registry_with_dashboard(); // widget "status", entry "ui/index.html"
    // entry 不在 → resolved: false
    let infos = registry.dashboard("widgety").unwrap();
    assert_eq!(infos.len(), 1);
    assert!(!infos[0].resolved);
    // entry を置く → resolved: true
    let ui_dir = plugins_dir.join("widgety").join("ui");
    std::fs::create_dir_all(&ui_dir).unwrap();
    std::fs::write(ui_dir.join("index.html"), "<html></html>").unwrap();
    assert!(registry.dashboard("widgety").unwrap()[0].resolved);
}

#[test]
fn set_dashboard_grant_round_trips_and_rejects_unknown_widget() {
    let (registry, plugins_dir) = test_registry_with_dashboard();
    let ui_dir = plugins_dir.join("widgety").join("ui");
    std::fs::create_dir_all(&ui_dir).unwrap();
    std::fs::write(ui_dir.join("index.html"), "<html></html>").unwrap();

    let infos = registry.set_dashboard_grant("widgety", "status", true).unwrap();
    assert!(infos[0].grant.granted);
    let infos = registry.set_dashboard_grant("widgety", "status", false).unwrap();
    assert!(!infos[0].grant.granted);
    let err = registry.set_dashboard_grant("widgety", "nope", true).unwrap_err();
    assert!(matches!(err, RegistryError::UnknownDashboard(w) if w == "nope"));
}

#[test]
fn dashboard_asset_path_requires_grant_and_rejects_traversal() {
    let (registry, plugins_dir) = test_registry_with_dashboard();
    let ui_dir = plugins_dir.join("widgety").join("ui");
    std::fs::create_dir_all(&ui_dir).unwrap();
    std::fs::write(ui_dir.join("index.html"), "<html></html>").unwrap();
    std::fs::write(ui_dir.join("app.js"), "//").unwrap();

    // 未 grant → エラー
    let err = registry.dashboard_asset_path("widgety", "status", "index.html").unwrap_err();
    assert!(matches!(err, RegistryError::DashboardNotGranted(_)));

    registry.set_dashboard_grant("widgety", "status", true).unwrap();
    // 正常系: entry ディレクトリ配下のファイル
    let path = registry.dashboard_asset_path("widgety", "status", "app.js").unwrap();
    assert!(path.ends_with("widgety/ui/app.js"));
    // 空パスは entry ファイル自身
    let path = registry.dashboard_asset_path("widgety", "status", "").unwrap();
    assert!(path.ends_with("widgety/ui/index.html"));
    // トラバーサルは拒否
    assert!(registry.dashboard_asset_path("widgety", "status", "../manifest.toml").is_err());
    assert!(registry.dashboard_asset_path("widgety", "status", "a/../../manifest.toml").is_err());
    assert!(registry.dashboard_asset_path("widgety", "status", "/etc/passwd").is_err());
}
```

- [ ] **Step 2: 失敗を確認** — `cargo test -p edlr-core dashboard` → コンパイルエラー

- [ ] **Step 3: 実装**

```rust
/// UI へ返すダッシュボードウィジェットの状態(`BusInfo` と同じ流儀)。
#[derive(Debug)]
pub struct DashboardInfo {
    pub request: DashboardWidget,
    pub grant: GrantState,
    pub resolved: bool,
}
```

`build_dashboard_infos`(`build_bus_infos` の隣):

```rust
    fn build_dashboard_infos(&self, manifest: &Manifest) -> Vec<DashboardInfo> {
        manifest
            .dashboard
            .iter()
            .map(|widget| {
                let grant = self.grants_store.dashboard_state(manifest, &widget.id);
                let resolved = self
                    .plugins_dir
                    .join(&manifest.id)
                    .join(&widget.entry)
                    .is_file();
                DashboardInfo { request: widget.clone(), grant, resolved }
            })
            .collect()
    }

    pub fn dashboard(&self, id: &str) -> Result<Vec<DashboardInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        Ok(self.build_dashboard_infos(&manifest))
    }

    pub fn set_dashboard_grant(
        &self,
        id: &str,
        widget: &str,
        granted: bool,
    ) -> Result<Vec<DashboardInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        if manifest.dashboard_widget(widget).is_none() {
            return Err(RegistryError::UnknownDashboard(widget.to_string()));
        }
        self.grants_store
            .set_dashboard(&manifest, widget, granted)
            .map_err(RegistryError::Grants)?;
        Ok(self.build_dashboard_infos(&manifest))
    }

    /// `dashboard/list` 用: 全プラグインの grant 済み判定込み一覧。
    pub fn dashboard_widgets_for_ui(&self) -> Vec<(String, String, PluginState, DashboardInfo)> {
        let snapshot: Vec<(Manifest, PluginState)> = {
            let guard = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            guard.iter().map(|e| (e.manifest.clone(), e.state.clone())).collect()
        };
        snapshot
            .into_iter()
            .flat_map(|(manifest, state)| {
                self.build_dashboard_infos(&manifest)
                    .into_iter()
                    .map(move |info| {
                        (manifest.id.clone(), manifest.name.clone(), state.clone(), info)
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// ウィジェットアセットの実ファイルパスを解決する。grant 必須・
    /// entry のディレクトリ外へのトラバーサルは拒否(サーバの
    /// `/plugin-ui/...` ハンドラの心臓部。HTTP 層は薄く保つ)。
    pub fn dashboard_asset_path(
        &self,
        plugin: &str,
        widget: &str,
        rel_path: &str,
    ) -> Result<PathBuf, RegistryError> {
        use std::path::Component;
        let manifest = self.find_manifest(plugin)?;
        let spec = manifest
            .dashboard_widget(widget)
            .ok_or_else(|| RegistryError::UnknownDashboard(widget.to_string()))?;
        let grant = self.grants_store.dashboard_state(&manifest, widget);
        if !grant.granted {
            return Err(RegistryError::DashboardNotGranted(widget.to_string()));
        }
        let entry = self.plugins_dir.join(&manifest.id).join(&spec.entry);
        let base = entry
            .parent()
            .ok_or_else(|| RegistryError::UnknownDashboard(widget.to_string()))?
            .to_path_buf();
        if rel_path.is_empty() {
            return Ok(entry);
        }
        let rel = std::path::Path::new(rel_path);
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(RegistryError::UnknownDashboard(widget.to_string()));
        }
        Ok(base.join(rel))
    }
```

`PluginInfo` に `pub dashboard: Vec<DashboardInfo>` を追加し、`list()` で `let dashboard = self.build_dashboard_infos(&manifest);` を組み込む。`RegistryError` に `UnknownDashboard(String)` / `DashboardNotGranted(String)` + Display。

注意: `PluginState` が `Clone` でなければ snapshot 部は既存 `list()` と同じ手口(clone 済みタプル)に合わせる。

- [ ] **Step 4: パスを確認** — `cargo test -p edlr-core` 全パス(PluginInfo 構築箇所のコンパイル修正含む)

- [ ] **Step 5: Commit** — `git commit -m "feat(core): registry state, grants and asset path resolution for dashboard widgets"`

---

### Task 4: RPC(set-dashboard-grant / dashboard/list)と plugins/list への組み込み

**Files:**
- Modify: `core/src/server.rs`

**Interfaces:**
- Consumes: Task 3 の Registry API
- Produces:
  - RPC `plugins/set-dashboard-grant` params `{plugin, widget, granted}` → `{dashboard: [DashboardEntryJson]}`
  - RPC `dashboard/list` → `{widgets: [{plugin, pluginName, widget, title, url, size, events, resolved, state}]}`(grant 済みのみ。`url` = `/plugin-ui/{plugin}/{widget}/{entryのファイル名}`)
  - `plugins/list` の各要素に `dashboard` 配列(`{id, title, entry, size, granted, staleGrant, resolved}`)

- [ ] **Step 1: 失敗するテストを書く**(server.rs `mod tests`。既存の `plugins/set-bus-grant` テスト(server.rs:893 付近)を雛形に、Task 3 の fixture を `pub(crate)` で再利用)

```rust
#[test]
fn set_dashboard_grant_rpc_returns_full_dashboard_list() {
    let (registry, plugins_dir) = crate::plugin::registry::tests::test_registry_with_dashboard();
    let ui_dir = plugins_dir.join("widgety").join("ui");
    std::fs::create_dir_all(&ui_dir).unwrap();
    std::fs::write(ui_dir.join("index.html"), "<html></html>").unwrap();

    let result = handle_rpc(
        &Some(registry.clone()),
        "plugins/set-dashboard-grant",
        &serde_json::json!({"plugin": "widgety", "widget": "status", "granted": true}),
    )
    .unwrap();
    assert_eq!(result["dashboard"][0]["id"], "status");
    assert_eq!(result["dashboard"][0]["granted"], true);
    assert_eq!(result["dashboard"][0]["resolved"], true);

    let listed = handle_rpc(&Some(registry.clone()), "plugins/list", &serde_json::json!({})).unwrap();
    assert_eq!(listed["plugins"][0]["dashboard"][0]["granted"], true);

    let widgets = handle_rpc(&Some(registry), "dashboard/list", &serde_json::json!({})).unwrap();
    assert_eq!(widgets["widgets"][0]["plugin"], "widgety");
    assert_eq!(widgets["widgets"][0]["widget"], "status");
    assert_eq!(widgets["widgets"][0]["url"], "/plugin-ui/widgety/status/index.html");
    assert_eq!(widgets["widgets"][0]["size"], "small");
}

#[test]
fn dashboard_list_excludes_ungranted_widgets() {
    let (registry, _dir) = crate::plugin::registry::tests::test_registry_with_dashboard();
    let widgets = handle_rpc(&Some(registry), "dashboard/list", &serde_json::json!({})).unwrap();
    assert_eq!(widgets["widgets"].as_array().unwrap().len(), 0);
}
```

(既存テストの RPC 呼び出しヘルパー名が `handle_rpc` と違う場合はそちらに合わせる。)

- [ ] **Step 2: 失敗を確認** — `cargo test -p edlr-core set_dashboard_grant_rpc` → unknown method で失敗

- [ ] **Step 3: 実装** — dispatch に 2 つの match arm を追加:

```rust
        "plugins/set-dashboard-grant" => {
            let plugin = param_str(params, "plugin")?;
            let widget = param_str(params, "widget")?;
            let granted = params
                .get("granted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "params.granted must be a bool".to_string())?;
            let dashboard = registry
                .set_dashboard_grant(plugin, widget, granted)
                .map_err(|e| e.to_string())?;
            Ok(dashboard_result_json(&dashboard))
        }
        "dashboard/list" => {
            let widgets: Vec<serde_json::Value> = registry
                .dashboard_widgets_for_ui()
                .into_iter()
                .filter(|(_, _, _, info)| info.grant.granted)
                .map(|(plugin_id, plugin_name, state, info)| {
                    let entry_file = std::path::Path::new(&info.request.entry)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("index.html");
                    let events = registry
                        .events_of(&plugin_id)
                        .unwrap_or_default();
                    serde_json::json!({
                        "plugin": plugin_id,
                        "pluginName": plugin_name,
                        "widget": info.request.id,
                        "title": info.request.title,
                        "url": format!("/plugin-ui/{plugin_id}/{}/{entry_file}", info.request.id),
                        "size": info.request.size.as_str(),
                        "events": events,
                        "resolved": info.resolved,
                        "state": state,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "widgets": widgets }))
        }
```

`registry.events_of(id)` が無ければ Registry に `pub fn events_of(&self, id) -> Result<Vec<String>, RegistryError>`(`find_manifest(id)?.events.clone()`)を足す。`state` のシリアライズは `plugins/list` と同じ表現("running"/"disabled")に合わせる。

`dashboard_result_json`(`bus_result_json` の隣):

```rust
fn dashboard_result_json(dashboard: &[crate::plugin::registry::DashboardInfo]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = dashboard
        .iter()
        .map(|info| {
            serde_json::json!({
                "id": info.request.id,
                "title": info.request.title,
                "entry": info.request.entry,
                "size": info.request.size.as_str(),
                "granted": info.grant.granted,
                "staleGrant": info.grant.stale,
                "resolved": info.resolved,
            })
        })
        .collect();
    serde_json::json!({ "dashboard": items })
}
```

`plugins/list` の組み立て(server.rs:149–152 付近)に `value["dashboard"] = dashboard_result_json(&info.dashboard)["dashboard"].clone();` を追加。

- [ ] **Step 4: パスを確認** — `cargo test -p edlr-core` 全パス

- [ ] **Step 5: Commit** — `git commit -m "feat(core): dashboard grant and listing RPCs"`

---

### Task 5: アセット配信ルート `/plugin-ui/...` と SDK 配信

**Files:**
- Create: `core/src/plugin_ui_sdk.js`
- Modify: `core/src/server.rs`(`app()` にルート追加 + ハンドラ)

**Interfaces:**
- Consumes: `Registry::dashboard_asset_path`
- Produces:
  - `GET /plugin-ui/{plugin}/{widget}/{*path}` — grant 済みウィジェットのアセットを CSP 付きで配信。未 grant・トラバーサル・不在は 404
  - `GET /plugin-ui-sdk.js` — `window.edlr` SDK(`include_str!` で埋め込み)
- SDK API(widget 作者向け): `edlr.ready()`、`edlr.onEvent(cb)`、`edlr.reportHeight()`(任意)

- [ ] **Step 1: 失敗するテストを書く**(server.rs `mod tests`。axum Router を `tower::ServiceExt::oneshot` で叩く。`tower` が dev-dependencies に無ければ `core/Cargo.toml` へ `tower = { version = "0.5", features = ["util"] }` を dev-dependencies に追加)

```rust
#[tokio::test]
async fn plugin_ui_serves_granted_assets_with_csp_and_404s_everything_else() {
    use tower::ServiceExt;
    let (registry, plugins_dir) = crate::plugin::registry::tests::test_registry_with_dashboard();
    let ui_dir = plugins_dir.join("widgety").join("ui");
    std::fs::create_dir_all(&ui_dir).unwrap();
    std::fs::write(ui_dir.join("index.html"), "<html>w</html>").unwrap();

    let router = crate::router::Router::new(8);
    let state = ServerState::new(&router, Some(registry.clone()), None);
    let app = app(state, None);

    let get = |uri: &str| {
        axum::http::Request::builder().uri(uri).body(axum::body::Body::empty()).unwrap()
    };

    // 未 grant → 404
    let res = app.clone().oneshot(get("/plugin-ui/widgety/status/index.html")).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);

    registry.set_dashboard_grant("widgety", "status", true).unwrap();

    // grant 済み → 200 + CSP + Content-Type
    let res = app.clone().oneshot(get("/plugin-ui/widgety/status/index.html")).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let csp = res.headers().get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(res.headers().get("content-type").unwrap().to_str().unwrap().contains("text/html"));

    // トラバーサル → 404
    let res = app.clone().oneshot(get("/plugin-ui/widgety/status/..%2Fmanifest.toml")).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
    // 不在ファイル → 404
    let res = app.clone().oneshot(get("/plugin-ui/widgety/status/nope.js")).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
    // 未知プラグイン → 404
    let res = app.clone().oneshot(get("/plugin-ui/nope/status/index.html")).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);

    // SDK は grant 不要で配信
    let res = app.clone().oneshot(get("/plugin-ui-sdk.js")).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    assert!(res.headers().get("content-type").unwrap().to_str().unwrap().contains("javascript"));
}
```

- [ ] **Step 2: 失敗を確認** — 404 は SPA fallback に飲まれて index 不在 → 現状は route 不在で失敗するはず。実行して失敗の形を確認

- [ ] **Step 3: SDK ファイルを書く**(`core/src/plugin_ui_sdk.js`)

```javascript
// edlr dashboard widget SDK.
// 使い方: <script src="/plugin-ui-sdk.js"></script> のあと
//   edlr.onEvent((event) => { ... });  // {kind, timestamp?, event?, raw}
//   edlr.ready();                      // 準備完了を親へ通知(これを呼ぶまでイベントは届かない)
(function () {
  "use strict";
  var listeners = [];
  var context = null; // {plugin, widget} — edlr:init で確定

  window.addEventListener("message", function (e) {
    var msg = e.data;
    if (!msg || typeof msg !== "object") return;
    if (msg.type === "edlr:init") {
      context = { plugin: msg.plugin, widget: msg.widget };
      return;
    }
    if (msg.type === "edlr:event") {
      for (var i = 0; i < listeners.length; i++) {
        try {
          listeners[i](msg.event);
        } catch (err) {
          /* widget 側の例外で他のリスナーを止めない */
        }
      }
    }
  });

  window.edlr = {
    ready: function () {
      window.parent.postMessage({ type: "edlr:ready" }, "*");
    },
    onEvent: function (cb) {
      listeners.push(cb);
    },
    reportHeight: function () {
      var h = document.documentElement.scrollHeight;
      window.parent.postMessage({ type: "edlr:height", px: h }, "*");
    },
    context: function () {
      return context;
    },
  };
})();
```

- [ ] **Step 4: ハンドラとルートを実装**(server.rs)

```rust
const PLUGIN_UI_SDK: &str = include_str!("plugin_ui_sdk.js");

/// ウィジェットアセントに付ける CSP。外部ネットワークへの読み込み・
/// fetch を遮断し、自ウィジェットのアセット(相対パス)のみ許可する。
/// iframe 側は opaque origin(sandbox="allow-scripts")なので、
/// 'self' はドキュメント URL のオリジン(このデーモン)を指す。
const WIDGET_CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; \
     style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'none'";

fn content_type_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

async fn plugin_ui_handler(
    axum::extract::State(state): axum::extract::State<ServerState>,
    axum::extract::Path((plugin, widget, path)): axum::extract::Path<(String, String, String)>,
) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    let Some(registry) = state.registry.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // grant チェック・トラバーサル拒否は Registry 側(単体テスト済み)。
    // ファイル IO はブロッキングなので spawn_blocking に逃がす。
    let result = tokio::task::spawn_blocking(move || {
        let file = registry.dashboard_asset_path(&plugin, &widget, &path)?;
        std::fs::read(&file)
            .map(|bytes| (bytes, content_type_for(&file)))
            .map_err(|_| crate::plugin::registry::RegistryError::UnknownDashboard(widget))
    })
    .await;
    match result {
        Ok(Ok((bytes, content_type))) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CONTENT_SECURITY_POLICY, WIDGET_CSP),
            ],
            bytes,
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn plugin_ui_sdk_handler() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        PLUGIN_UI_SDK,
    )
}
```

`app()` を修正(`with_state` より前にルートを足す):

```rust
    let mut app = axum::Router::new()
        .route("/ws", get(ws_handler))
        .route("/plugin-ui-sdk.js", get(plugin_ui_sdk_handler))
        .route("/plugin-ui/{plugin}/{widget}/{*path}", get(plugin_ui_handler))
        .with_state(state);
```

`ServerState.registry` が private なら `pub(crate)` アクセサを追加。axum の `{*path}` は URL デコード済みで渡る(`..%2F` → `../`)ため、トラバーサル検査は Registry 側の Component 検査で受け止まることをテストで確認。

- [ ] **Step 5: パスを確認** — `cargo test -p edlr-core` 全パス

- [ ] **Step 6: Commit** — `git commit -m "feat(core): serve granted dashboard widget assets and postMessage SDK"`

---

### Task 6: フロントの型・RPC ヘルパー・イベントマッチング

**Files:**
- Modify: `ui/frontend/src/types/plugin.ts`
- Modify: `ui/frontend/src/rpc.ts`
- Create: `ui/frontend/src/lib/events.ts`
- Test: `ui/frontend/src/lib/events.test.ts`

**Interfaces:**
- Produces:
  - `types/plugin.ts`: `DashboardWidget { id, title, entry, size: "small"|"medium"|"large", granted, staleGrant, resolved }`、`PluginInfo.dashboard: DashboardWidget[]`、`DashboardListEntry { plugin, pluginName, widget, title, url, size, events, resolved, state }`
  - `rpc.ts`: `setDashboardGrant(pluginId, widget, granted): Promise<{dashboard: DashboardWidget[]}>`、`listDashboard(): Promise<{widgets: DashboardListEntry[]}>`
  - `lib/events.ts`: `matchesEvent(events: string[], entry: LogEntry): boolean`(Rust 側 `matches_event` と同一規則)

- [ ] **Step 1: 失敗するテストを書く**(`ui/frontend/src/lib/events.test.ts`)

```ts
import { describe, expect, it } from "vitest";
import { matchesEvent } from "./events";
import type { LogEntry } from "./filter";

const journal = (event: string): LogEntry => ({
  id: 1, kind: "journal", timestamp: "2026-07-28T00:00:00Z", event, raw: {},
});
const status: LogEntry = { id: 2, kind: "status", raw: {} };

describe("matchesEvent", () => {
  it("matches exact journal event names", () => {
    expect(matchesEvent(["FSDJump"], journal("FSDJump"))).toBe(true);
    expect(matchesEvent(["FSDJump"], journal("Docked"))).toBe(false);
  });
  it("wildcard matches any journal event but not status", () => {
    expect(matchesEvent(["*"], journal("Docked"))).toBe(true);
    expect(matchesEvent(["*"], status)).toBe(false);
  });
  it("status pattern matches only status events", () => {
    expect(matchesEvent(["status"], status)).toBe(true);
    expect(matchesEvent(["status"], journal("status"))).toBe(true); // journal 名 "status" は完全一致でもある
  });
  it("empty list matches nothing", () => {
    expect(matchesEvent([], journal("Docked"))).toBe(false);
    expect(matchesEvent([], status)).toBe(false);
  });
});
```

(注: Rust 実装では journal 側は `e == "*" || e == name` なので、journal イベント名が偶然 "status" の場合もマッチする。TS も同じにする。)

- [ ] **Step 2: 失敗を確認** — `pnpm --dir ui/frontend test` → module 不在で失敗

- [ ] **Step 3: 実装**

`lib/events.ts`:

```ts
import type { LogEntry } from "./filter";

/**
 * manifest の events フィルタが entry にマッチするか。
 * core/src/plugin/manifest.rs の matches_event と同一規則:
 * - "*" は全 journal イベント(status には false)
 * - "status" は status イベントのみ
 * - それ以外は journal イベント名の完全一致
 * - 空リストは常に false
 */
export function matchesEvent(events: string[], entry: LogEntry): boolean {
  if (entry.kind === "journal") {
    return events.some((e) => e === "*" || e === entry.event);
  }
  return events.includes("status");
}
```

`types/plugin.ts` に型追加、`PluginInfo` に `dashboard: DashboardWidget[]`。`rpc.ts` に `setBusGrant` と同形のヘルパー 2 つ:

```ts
  setDashboardGrant(
    pluginId: string,
    widget: string,
    granted: boolean,
  ): Promise<{ dashboard: DashboardWidget[] }> {
    return this.call<{ dashboard: DashboardWidget[] }>("plugins/set-dashboard-grant", {
      plugin: pluginId,
      widget,
      granted,
    });
  }

  listDashboard(): Promise<{ widgets: DashboardListEntry[] }> {
    return this.call<{ widgets: DashboardListEntry[] }>("dashboard/list");
  }
```

- [ ] **Step 4: パスを確認** — `pnpm --dir ui/frontend test` 全パス(tsc は `pnpm --dir ui/frontend build` で確認)

- [ ] **Step 5: Commit** — `git commit -m "feat(ui): dashboard widget types, rpc helpers and event matching"`

---

### Task 7: Plugins 画面の DashboardSection(grant UI)

**Files:**
- Create: `ui/frontend/src/components/DashboardSection.tsx`
- Modify: `ui/frontend/src/pages/Plugins.tsx`
- Test: `ui/frontend/src/components/DashboardSection.test.tsx`

**Interfaces:**
- Consumes: `DashboardWidget` 型、`RpcClient.setDashboardGrant`
- Produces: `<DashboardSection pluginId dashboard onSetGrant />`(`BusSection` と同形。`onSetGrant: (pluginId, widget, granted) => Promise<void>`)

- [ ] **Step 1: 失敗するテストを書く**(`DashboardSection.test.tsx`、`BusSection.test.tsx` を雛形に)

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { DashboardSection } from "./DashboardSection";

const base = {
  id: "status",
  title: "Ship Status",
  entry: "ui/status/index.html",
  size: "medium" as const,
  granted: false,
  staleGrant: false,
  resolved: true,
};

describe("DashboardSection", () => {
  it("renders nothing when no widgets are declared", () => {
    const { container } = render(
      <DashboardSection pluginId="p" dashboard={[]} onSetGrant={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("shows title, size and calls onSetGrant on approve", async () => {
    const onSetGrant = vi.fn();
    render(<DashboardSection pluginId="p" dashboard={[base]} onSetGrant={onSetGrant} />);
    expect(screen.getByText("Ship Status")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("checkbox"));
    expect(onSetGrant).toHaveBeenCalledWith("p", "status", true);
  });

  it("shows unresolved and stale badges", () => {
    render(
      <DashboardSection
        pluginId="p"
        dashboard={[
          { ...base, resolved: false },
          { ...base, id: "b", staleGrant: true },
        ]}
        onSetGrant={vi.fn()}
      />,
    );
    expect(screen.getByText("未解決")).toBeInTheDocument();
    expect(screen.getByText("要再承認")).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: 失敗を確認** — `pnpm --dir ui/frontend test DashboardSection`

- [ ] **Step 3: 実装**(`BusSection.tsx` の構造を踏襲: エントリカード、saving/error state、チェックボックスは server-driven で optimistic 更新なし、`未解決` は grant 不可(`disabled={saving || (!entry.granted && !entry.resolved)}`)。バッジ className は `badge badge-bus-unresolved`/`badge badge-bus-stale` を流用)

- [ ] **Step 4: Plugins.tsx に組み込む** — `handleDashboardGrant`(`handleBusGrant` と同形、`updated.dashboard` で該当 plugin の `dashboard` を差し替え)を追加し、`<BusSection …/>` の下に `<DashboardSection pluginId={p.id} dashboard={p.dashboard} onSetGrant={handleDashboardGrant} />`。`Plugins.test.tsx` のモック RpcClient に `setDashboardGrant` を追加(既存 fixture の PluginInfo に `dashboard: []` を足す)

- [ ] **Step 5: パスを確認** — `pnpm --dir ui/frontend test` 全パス

- [ ] **Step 6: Commit** — `git commit -m "feat(ui): dashboard widget grant section in Plugins page"`

---

### Task 8: Dashboard 画面(iframe グリッド + postMessage ブリッジ)

**Files:**
- Create: `ui/frontend/src/components/WidgetFrame.tsx`
- Modify: `ui/frontend/src/pages/Dashboard.tsx`(全面書き換え)
- Modify: `ui/frontend/src/App.css`(グリッド + カードの最低限のスタイル)
- Test: `ui/frontend/src/components/WidgetFrame.test.tsx`, `ui/frontend/src/pages/Dashboard.test.tsx`

**Interfaces:**
- Consumes: `RpcClient.listDashboard`、`useEventStream`、`matchesEvent`、`DashboardListEntry`
- Produces:
  - `<WidgetFrame entry={DashboardListEntry} entries={LogEntry[]} wsUrlBase={string} />` — 1 ウィジェットのカード。iframe `sandbox="allow-scripts"`、`edlr:ready` 受信後に `edlr:init` → 蓄積分 + 以後のマッチイベントを `edlr:event` で転送、`edlr:height` で iframe 高さ調整
  - `Dashboard` ページ — `dashboard/list` を取得し CSS Grid(3カラム、small=1/medium=2/large=3 スパン、`grid-column: span N`)に配置。0 件時は案内文。`resolved: false` / `state !== "running"` はプレースホルダカード

- [ ] **Step 1: WidgetFrame の失敗するテストを書く**

```tsx
import { act, render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { WidgetFrame } from "./WidgetFrame";
import type { LogEntry } from "../lib/filter";

const entry = {
  plugin: "widgety", pluginName: "W", widget: "status", title: "Status",
  url: "/plugin-ui/widgety/status/index.html", size: "small" as const,
  events: ["FSDJump"], resolved: true, state: "running",
};
const jump: LogEntry = {
  id: 1, kind: "journal", timestamp: "t", event: "FSDJump", raw: { StarSystem: "Sol" },
};
const dock: LogEntry = { id: 2, kind: "journal", timestamp: "t", event: "Docked", raw: {} };

function fakeReady(iframe: HTMLIFrameElement, received: unknown[]) {
  // jsdom の iframe には contentWindow があるので postMessage を差し替えて観測する
  const win = iframe.contentWindow as Window;
  (win as unknown as { postMessage: (msg: unknown) => void }).postMessage = (msg: unknown) =>
    received.push(msg);
  // widget からの edlr:ready を親 window で受けた体にする
  act(() => {
    window.dispatchEvent(
      new MessageEvent("message", { data: { type: "edlr:ready" }, source: win }),
    );
  });
}

describe("WidgetFrame", () => {
  it("sends init and only matching events after ready", () => {
    const received: unknown[] = [];
    const { container, rerender } = render(<WidgetFrame entry={entry} entries={[jump, dock]} />);
    const iframe = container.querySelector("iframe")!;
    expect(iframe.getAttribute("sandbox")).toBe("allow-scripts");
    expect(iframe.getAttribute("src")).toBe(entry.url);

    fakeReady(iframe, received);
    // ready 後: init + 蓄積分のうちマッチする FSDJump のみ
    expect(received[0]).toMatchObject({ type: "edlr:init", plugin: "widgety", widget: "status" });
    expect(received.filter((m) => (m as { type: string }).type === "edlr:event")).toHaveLength(1);

    // 以後の新着もマッチ分のみ転送
    const more: LogEntry = { id: 3, kind: "journal", timestamp: "t", event: "FSDJump", raw: {} };
    rerender(<WidgetFrame entry={entry} entries={[jump, dock, more]} />);
    expect(received.filter((m) => (m as { type: string }).type === "edlr:event")).toHaveLength(2);
  });

  it("does not send anything before ready", () => {
    const received: unknown[] = [];
    const { container } = render(<WidgetFrame entry={entry} entries={[jump]} />);
    const iframe = container.querySelector("iframe")!;
    const win = iframe.contentWindow as Window;
    (win as unknown as { postMessage: (msg: unknown) => void }).postMessage = (msg: unknown) =>
      received.push(msg);
    expect(received).toHaveLength(0);
  });
});
```

- [ ] **Step 2: 失敗を確認** — `pnpm --dir ui/frontend test WidgetFrame`

- [ ] **Step 3: WidgetFrame を実装**

```tsx
import { useEffect, useRef, useState } from "react";
import type { DashboardListEntry } from "../types/plugin";
import type { LogEntry } from "../lib/filter";
import { matchesEvent } from "../lib/events";

const MIN_HEIGHT_PX = 120;
const MAX_HEIGHT_PX = 800;

/**
 * ダッシュボードウィジェット 1 件のカード。
 *
 * - iframe は sandbox="allow-scripts" のみ(opaque origin)。通信は postMessage だけ。
 * - widget からの edlr:ready を受けてから edlr:init → 蓄積済み + 以後の
 *   マッチイベントを edlr:event で送る(ready 前には何も送らない)。
 * - edlr:height で高さを自動調整(暴走防止のため上下限をクランプ)。
 */
export function WidgetFrame({ entry, entries }: {
  entry: DashboardListEntry;
  entries: LogEntry[];
}) {
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const [ready, setReady] = useState(false);
  const [height, setHeight] = useState(240);
  // 転送済み位置。ready 前のイベントは ready 時にまとめて送る
  const sentUpTo = useRef(0);

  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      const win = iframeRef.current?.contentWindow;
      if (!win || e.source !== win) return;
      const msg = e.data as { type?: string; px?: number };
      if (msg?.type === "edlr:ready") {
        win.postMessage(
          { type: "edlr:init", plugin: entry.plugin, widget: entry.widget },
          "*",
        );
        setReady(true);
      } else if (msg?.type === "edlr:height" && typeof msg.px === "number") {
        setHeight(Math.min(MAX_HEIGHT_PX, Math.max(MIN_HEIGHT_PX, msg.px)));
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [entry.plugin, entry.widget]);

  useEffect(() => {
    if (!ready) return;
    const win = iframeRef.current?.contentWindow;
    if (!win) return;
    for (const log of entries.slice(sentUpTo.current)) {
      if (matchesEvent(entry.events, log)) {
        win.postMessage({ type: "edlr:event", event: log }, "*");
      }
    }
    sentUpTo.current = entries.length;
  }, [ready, entries, entry.events]);

  return (
    <iframe
      ref={iframeRef}
      title={`${entry.plugin}/${entry.widget}`}
      src={entry.url}
      sandbox="allow-scripts"
      style={{ width: "100%", height, border: "none" }}
    />
  );
}

export default WidgetFrame;
```

- [ ] **Step 4: WidgetFrame テストのパスを確認** — `pnpm --dir ui/frontend test WidgetFrame`

- [ ] **Step 5: Dashboard ページの失敗するテストを書く**(`Plugins.test.tsx` の流儀で `vi.mock("../rpc")` + `vi.mock("../ws")`)

```tsx
import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import Dashboard from "./Dashboard";

const widgets = [
  { plugin: "widgety", pluginName: "W", widget: "status", title: "Status",
    url: "/plugin-ui/widgety/status/index.html", size: "medium", events: ["*"],
    resolved: true, state: "running" },
  { plugin: "widgety", pluginName: "W", widget: "broken", title: "Broken",
    url: "/plugin-ui/widgety/broken/index.html", size: "small", events: [],
    resolved: false, state: "running" },
];

vi.mock("../ws", () => ({
  defaultWsUrl: () => "ws://test/ws",
  useEventStream: () => ({ entries: [], connection: "open" }),
}));
vi.mock("../rpc", () => ({
  RpcClient: class {
    listDashboard() {
      return Promise.resolve({ widgets });
    }
    close() {}
  },
}));

describe("Dashboard", () => {
  it("renders granted widgets as cards and placeholders for unresolved ones", async () => {
    render(<Dashboard />);
    await waitFor(() => expect(screen.getByText("Status")).toBeInTheDocument());
    expect(document.querySelector("iframe")).not.toBeNull();
    expect(screen.getByText("Broken")).toBeInTheDocument();
    // 未解決はプレースホルダ(iframe を作らない)
    expect(document.querySelectorAll("iframe")).toHaveLength(1);
    expect(screen.getByText(/entry ファイルが見つかりません/)).toBeInTheDocument();
  });

  it("shows guidance when no widgets are granted", async () => {
    widgets.length = 0;
    render(<Dashboard />);
    await waitFor(() =>
      expect(screen.getByText(/承認済みのウィジェットがありません/)).toBeInTheDocument(),
    );
  });
});
```

- [ ] **Step 6: Dashboard ページを実装**

```tsx
import { useEffect, useRef, useState } from "react";
import { RpcClient } from "../rpc";
import { defaultWsUrl, useEventStream } from "../ws";
import type { DashboardListEntry } from "../types/plugin";
import WidgetFrame from "../components/WidgetFrame";

const SPAN: Record<DashboardListEntry["size"], number> = { small: 1, medium: 2, large: 3 };

export default function Dashboard() {
  const [widgets, setWidgets] = useState<DashboardListEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { entries } = useEventStream(defaultWsUrl());
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    const client = new RpcClient(defaultWsUrl());
    client
      .listDashboard()
      .then((res) => {
        if (mountedRef.current) setWidgets(res.widgets);
      })
      .catch((err) => {
        if (mountedRef.current) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      mountedRef.current = false;
      client.close();
    };
  }, []);

  return (
    <section>
      <h1>Dashboard</h1>
      {error && <p className="error">{error}</p>}
      {widgets && widgets.length === 0 && (
        <p>承認済みのウィジェットがありません。Plugins 画面でウィジェットを承認すると、ここに表示されます。</p>
      )}
      <div className="widget-grid">
        {widgets?.map((w) => (
          <article
            key={`${w.plugin}/${w.widget}`}
            className="widget-card"
            style={{ gridColumn: `span ${SPAN[w.size]}` }}
          >
            <h2>{w.title}</h2>
            {w.state !== "running" ? (
              <p className="widget-placeholder">プラグインが停止しています</p>
            ) : !w.resolved ? (
              <p className="widget-placeholder">entry ファイルが見つかりません</p>
            ) : (
              <WidgetFrame entry={w} entries={entries} />
            )}
          </article>
        ))}
      </div>
    </section>
  );
}
```

`RpcClient` に `close()` が無ければ既存の破棄 API 名(Plugins.tsx が使っているもの)に合わせる。CSS(`App.css` へ追記):

```css
.widget-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 1rem;
}
.widget-card {
  border: 1px solid var(--border, #ccc);
  border-radius: 8px;
  padding: 0.75rem;
  min-width: 0;
}
.widget-placeholder {
  color: #888;
}
@media (max-width: 900px) {
  .widget-grid { grid-template-columns: 1fr; }
  .widget-card { grid-column: span 1 !important; }
}
```

- [ ] **Step 7: パスを確認** — `pnpm --dir ui/frontend test` 全パス + `pnpm --dir ui/frontend build`(tsc)

- [ ] **Step 8: Commit** — `git commit -m "feat(ui): dashboard page renders granted widgets with postMessage bridge"`

---

### Task 9: サンプルウィジェットと E2E 確認

**Files:**
- Modify: `examples/plugins/state-reader/manifest.toml`(`[[dashboard]]` 追加)
- Create: `examples/plugins/state-reader/ui/last-jump/index.html`

**注(spec からの逸脱):** spec は hello-logger にサンプルを置くとしていたが、hello-logger には manifest.toml が無い(単なる crate)。manifest を持つ state-reader に置く。

- [ ] **Step 1: サンプルウィジェットを書く**

`manifest.toml` に追記:

```toml
[[dashboard]]
id = "last-jump"
title = "Last Jump"
entry = "ui/last-jump/index.html"
size = "small"
```

`ui/last-jump/index.html`:

```html
<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <style>
      body { font-family: system-ui, sans-serif; margin: 0.5rem; }
      .system { font-size: 1.4rem; font-weight: 600; }
      .muted { color: #888; }
    </style>
  </head>
  <body>
    <div class="system" id="system">—</div>
    <div class="muted" id="time">FSDJump 待ち</div>
    <script src="/plugin-ui-sdk.js"></script>
    <script>
      edlr.onEvent(function (event) {
        if (event.kind !== "journal" || event.event !== "FSDJump") return;
        document.getElementById("system").textContent =
          (event.raw && event.raw.StarSystem) || "?";
        document.getElementById("time").textContent = event.timestamp;
        edlr.reportHeight();
      });
      edlr.ready();
    </script>
  </body>
</html>
```

- [ ] **Step 2: E2E 手動確認**(state-reader のビルド済み wasm が必要。無ければ `~/.config/edlr/plugins` に一時プラグインディレクトリを作って確認)

1. `cargo build -p edlr-core --bin edlr`
2. テスト用 plugins dir を組み立て(id=ディレクトリ名、wasm 実在が必須): `--plugins-dir` で起動
3. `cargo run -p edlr-ui`(vite 道連れ起動)→ Plugins 画面にウィジェット行と grant トグルが出る
4. grant → Dashboard 画面にカードが出る/未 grant では `/plugin-ui/...` が 404(curl で確認)
5. curl でアセットの CSP ヘッダとトラバーサル 404 を確認

- [ ] **Step 3: 全体テスト** — `cargo test --workspace` + `pnpm --dir ui/frontend test` + `pnpm --dir ui/frontend build` 全パス

- [ ] **Step 4: Commit** — `git commit -m "feat(examples): sample last-jump dashboard widget for state-reader"`

---

## Self-Review 結果

- **Spec coverage:** manifest(Task 1)、配信+CSP+sandbox(Task 5、sandbox 属性は Task 8)、ブリッジ+SDK(Task 5/8)、grant/RPC/Plugins 画面(Task 2–4/7)、Dashboard 画面(Task 8)、エラーハンドリング(404=Task 3/5、プレースホルダ=Task 8、ready 前は送らない=Task 8)、テスト(各タスク)、サンプル(Task 9)。スコープ外項目(edlr:bus、並び替え)はどのタスクにも含めない。
- **逸脱:** サンプル配置先を hello-logger → state-reader に変更(Task 9 に理由明記)。
- **型整合:** `DashboardWidget`(Rust/TS 同名)、`DashboardInfo`、`DashboardListEntry`、RPC メソッド名 `plugins/set-dashboard-grant` / `dashboard/list` を全タスクで統一済み。

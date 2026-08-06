# filesystem file target 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `[[filesystem]]` に `target = "file"` を追加し、単一ファイルだけをプラグイン/ドライバに承認できるようにする。

**Architecture:** 既存の `FilesystemRequest` に `target` フィールド(デフォルト directory)を足し、grants/fingerprint/runtime バッファ/RPC/UI の既存経路をそのまま流用する。file ルートはホスト側でゲストパスを `""` のみに制限し、`FsDriver` には「親ディレクトリ + ファイル名」として渡すことでドライバ(`edlr_driver_fs`)は無変更で済ませる。WIT も無変更。

**Tech Stack:** Rust (core, `edlr-core` crate), React + TypeScript + vitest (ui/frontend), Tauri (ui/src-tauri)

**Spec:** `docs/superpowers/specs/2026-08-06-filesystem-file-target-design.md`

## Global Constraints

- コメント・エラーメッセージ以外の識別子は英語、コメントは既存ファイルに合わせて日本語。
- core を触るタスクは `.claude/rules/` の作法に従う(純粋モジュールは値イン値アウト、テストは純粋テスト優先)。
- cargo テストはリポジトリルートから `cargo test -p edlr-core <filter>`。同一 worktree で cargo を並走させない。
- UI テストは `ui/frontend` で `pnpm test`(vitest)。
- 既存の manifest / 旧 filesystem_json バッファ / 既存 grants は**無変更で今まで通り動く**こと(後方互換)。既存 directory ルートの fingerprint を変えてはいけない(全ユーザーの再承認を誘発するため)。

---

### Task 1: FilesystemTarget 型と manifest パース

**Files:**
- Modify: `core/src/capability/request.rs`(`FilesystemRequest` 周辺、59行付近)
- Test: `core/src/manifest/tests.rs`(`parse_fs_manifest` ヘルパ 1173行付近の隣)

**Interfaces:**
- Produces: `crate::capability::request::FilesystemTarget`(`Directory | File`、`as_str() -> &'static str`(`"directory"` / `"file"`)、`Default = Directory`)、`FilesystemRequest.target: FilesystemTarget`

- [ ] **Step 1: 失敗するテストを書く**

`core/src/manifest/tests.rs` の filesystem テスト群(`filesystem_requires_*` 付近)に追加:

```rust
#[test]
fn filesystem_target_defaults_to_directory() {
    let manifest = parse_fs_manifest(
        "[[filesystem]]\nname = \"exports\"\nreason = \"r\"\nmode = \"read\"\n",
    )
    .expect("target is optional");
    assert_eq!(
        manifest.filesystem[0].target,
        crate::capability::request::FilesystemTarget::Directory
    );
}

#[test]
fn filesystem_target_file_parses() {
    let manifest = parse_fs_manifest(
        "[[filesystem]]\nname = \"status\"\nreason = \"r\"\nmode = \"read\"\ntarget = \"file\"\n",
    )
    .expect("file target parses");
    assert_eq!(
        manifest.filesystem[0].target,
        crate::capability::request::FilesystemTarget::File
    );
}

#[test]
fn filesystem_target_rejects_unknown_value() {
    parse_fs_manifest(
        "[[filesystem]]\nname = \"a\"\nreason = \"r\"\nmode = \"read\"\ntarget = \"folder\"\n",
    )
    .expect_err("unknown target must be rejected");
}
```

- [ ] **Step 2: テストが失敗する(コンパイルエラーになる)ことを確認**

Run: `cargo test -p edlr-core filesystem_target`
Expected: FAIL(`FilesystemTarget` が未定義のコンパイルエラー)

- [ ] **Step 3: 最小実装**

`core/src/capability/request.rs` の `FilesystemMode` の下に追加し、`FilesystemRequest` にフィールドを足す:

```rust
/// `[[filesystem]]` の `target`(承認対象がディレクトリか単一ファイルか)。
/// 省略時は directory(既存 manifest の互換のため)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FilesystemTarget {
    #[default]
    Directory,
    File,
}

impl FilesystemTarget {
    /// フィンガープリント・RPC 応答で使う安定した文字列表現。
    pub fn as_str(&self) -> &'static str {
        match self {
            FilesystemTarget::Directory => "directory",
            FilesystemTarget::File => "file",
        }
    }
}
```

`FilesystemRequest` に:

```rust
pub struct FilesystemRequest {
    pub name: String,
    pub reason: String,
    pub mode: FilesystemMode,
    #[serde(default)]
    pub target: FilesystemTarget,
}
```

`FilesystemRequest` を構造体リテラルで組んでいる既存テスト(`core/src/registry/plugin.rs:881` 付近など、`cargo test` のコンパイルエラーで全箇所わかる)に `target: FilesystemTarget::Directory,`(または `Default::default()`)を足す。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core filesystem_target`
Expected: PASS。続けて `cargo test -p edlr-core` 全体も PASS(構造体リテラルの直し漏れ検出)。

- [ ] **Step 5: コミット**

```bash
git add core/src/capability/request.rs core/src/manifest/tests.rs core/src
git commit -m "feat(core): [[filesystem]] に target = file|directory を追加"
```

---

### Task 2: fingerprint に target を含める(file のときだけ)

**Files:**
- Modify: `core/src/capability/fingerprint.rs:82-88`(`fn filesystem`)
- Test: `core/src/manifest/tests.rs`(既存の filesystem fingerprint テスト 1244行付近の隣)

**Interfaces:**
- Consumes: Task 1 の `FilesystemTarget`
- Produces: `fingerprint::filesystem` が `target = "file"` のとき異なる値を返す(directory のときは**従来と同一の値**)

- [ ] **Step 1: 失敗するテストを書く**

`core/src/manifest/tests.rs` に追加:

```rust
#[test]
fn filesystem_fingerprint_changes_when_target_becomes_file() {
    let dir = parse_fs_manifest(
        "[[filesystem]]\nname = \"a\"\nreason = \"r\"\nmode = \"read\"\n",
    )
    .unwrap();
    let file = parse_fs_manifest(
        "[[filesystem]]\nname = \"a\"\nreason = \"r\"\nmode = \"read\"\ntarget = \"file\"\n",
    )
    .unwrap();
    assert_ne!(
        dir.filesystem_fingerprint("a"),
        file.filesystem_fingerprint("a"),
        "directory -> file の変更は再承認を要求しなければならない"
    );
}

#[test]
fn filesystem_fingerprint_for_directory_is_unchanged_from_before_target_existed() {
    // target フィールド導入前の canonical は
    // "filesystem" + name + reason + mode の 4 フィールドだった。directory の
    // fingerprint がこの値のままであること(= 既存の grants を失効させない
    // こと)を、導入前のアルゴリズムで計算した固定値で釘付けする。
    let manifest = parse_fs_manifest(
        "[[filesystem]]\nname = \"a\"\nreason = \"r\"\nmode = \"read\"\n",
    )
    .unwrap();
    // sha256("10:filesystem" + "1:a" + "1:r" + "4:read") -- 実装前に
    // `echo -n '10:filesystem1:a1:r4:read' | sha256sum` で得た値。
    assert_eq!(
        manifest.filesystem_fingerprint("a").unwrap(),
        "19861b2e46585bdee3a0e98f7a87c151036658bcff7c8b8704ca92407fe011fa"
    );
}
```

(固定値は `echo -n '10:filesystem1:a1:r4:read' | sha256sum` の実出力。計画作成時に計算済み。)

- [ ] **Step 2: テストを実行して現状を確認**

Run: `cargo test -p edlr-core filesystem_fingerprint`
Expected: `changes_when_target_becomes_file` が FAIL(まだ target を畳み込んでいないため同値)、`unchanged_from_before` は PASS(現状の釘付け)

- [ ] **Step 3: 実装**

`core/src/capability/fingerprint.rs` の `fn filesystem` を変更:

```rust
pub fn filesystem(request: &FilesystemRequest) -> String {
    let mut canonical = encode_field("filesystem");
    canonical.push_str(&encode_field(&request.name));
    canonical.push_str(&encode_field(&request.reason));
    canonical.push_str(&encode_field(request.mode.as_str()));
    // target は file のときだけ畳み込む。directory で無条件に足すと、この
    // フィールド導入前に承認された既存の grants が全プラグインで一斉に
    // 失効してしまう。4 フィールド形(旧 directory)と 5 フィールド形
    // (file)の衝突は起こらない: mode のエンコードは "4:read" か
    // "10:read-write" のどちらかで、"4:file" で終わる文字列を含み得ない。
    if request.target == crate::capability::request::FilesystemTarget::File {
        canonical.push_str(&encode_field(request.target.as_str()));
    }
    sha256_hex(&canonical)
}
```

(`use` は既存の `super::request::{...}` インポートに `FilesystemTarget` を足してパスを短くしてよい)

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core filesystem_fingerprint`
Expected: 両方 PASS

- [ ] **Step 5: コミット**

```bash
git add core/src/capability/fingerprint.rs core/src/manifest/tests.rs
git commit -m "feat(core): filesystem fingerprint に target を畳み込む(file のみ)"
```

---

### Task 3: FsRuntimeEntry に target を追加してバッファへ流す

**Files:**
- Modify: `core/src/runtime/fs.rs`(`FsRuntimeEntry`、redaction、テスト)
- Modify: `core/src/registry/filesystem.rs:233-241`(`refresh_filesystem_runtime` の組み立て)
- Modify: `core/src/runner/bootstrap.rs:109-115`(初期バッファの組み立て)
- Modify: `core/src/host/resolve.rs:206-213`, `core/src/host/plugin.rs:1251-`, `core/src/host/driver.rs:869-`(テストヘルパ `fs_entry` に `target` フィールド追加)

**Interfaces:**
- Produces: `FsRuntimeEntry.target: String`(`"directory"` / `"file"`。`#[serde(default)]` なので旧バッファでは空文字 = directory 扱い)

- [ ] **Step 1: 失敗するテストを書く**

`core/src/runtime/fs.rs` のテストに追加:

```rust
#[test]
fn missing_target_in_old_buffers_defaults_to_empty() {
    // target フィールド導入前に直列化されたバッファ。空文字は
    // 「file ではない」= directory として扱われる(resolve 側の規則)。
    let parsed = parse_filesystem(
        r#"[{"name":"exports","granted":true,"mode":"read","path":"/tmp/e"}]"#,
    );
    assert_eq!(parsed.get("exports").unwrap().target, "");
}

#[test]
fn target_round_trips_and_survives_redaction() {
    let mut e = entry(false);
    e.target = "file".to_string();
    let parsed = parse_filesystem(&filesystem_json_string(&[e]));
    // 未承認で path は落ちるが、target は mode と同じく承認画面に出る
    // 情報なので残る。
    assert_eq!(parsed.get("exports").unwrap().target, "file");
    assert_eq!(parsed.get("exports").unwrap().path, "");
}
```

- [ ] **Step 2: 失敗を確認**

Run: `cargo test -p edlr-core --lib runtime::fs`
Expected: FAIL(`target` フィールドが無いコンパイルエラー)

- [ ] **Step 3: 実装**

`FsRuntimeEntry` に追加:

```rust
pub struct FsRuntimeEntry {
    pub name: String,
    pub granted: bool,
    pub mode: String,
    #[serde(default)]
    pub path: String,
    /// "directory" / "file"。旧バッファには無いので空文字も directory 扱い。
    #[serde(default)]
    pub target: String,
}
```

`filesystem_json_string` の redaction 分岐(未承認側)に `target: entry.target.clone(),` を追加。テストヘルパ `fn entry` にも `target: "directory".into(),` を追加。

生成箇所 2 つに `target` を足す:

`core/src/registry/filesystem.rs`(`refresh_filesystem_runtime`):

```rust
.map(|info| FsRuntimeEntry {
    name: info.request.name.clone(),
    granted: info.grant.granted,
    mode: info.request.mode.as_str().to_string(),
    path: info.config.path.clone(),
    target: info.request.target.as_str().to_string(),
})
```

`core/src/runner/bootstrap.rs`(初期バッファ):

```rust
FsRuntimeEntry {
    name: request.name.clone(),
    granted,
    mode: request.mode.as_str().to_string(),
    path,
    target: request.target.as_str().to_string(),
}
```

テストヘルパ `fs_entry`(`host/resolve.rs` / `host/plugin.rs` / `host/driver.rs` の 3 箇所)には `target: "directory".to_string(),` を追加(他のコンパイルエラー箇所も同様に directory で埋める)。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core`
Expected: 全 PASS

- [ ] **Step 5: コミット**

```bash
git add core/src
git commit -m "feat(core): filesystem_json バッファに target を載せる"
```

---

### Task 4: resolve — ResolvedRoot と effective_fs_target

**Files:**
- Modify: `core/src/host/resolve.rs`(`resolve_root` 74-98行、テスト)

**Interfaces:**
- Consumes: Task 3 の `FsRuntimeEntry.target`
- Produces:
  - `pub(crate) struct ResolvedRoot { pub path: PathBuf, pub is_file: bool }`
  - `resolve_root(...) -> Result<ResolvedRoot, RootResolveError>`(シグネチャ変更)
  - `pub(crate) fn effective_fs_target(resolved: &ResolvedRoot, guest_path: &str) -> Result<(PathBuf, String), String>` — `FsDriver` に渡す `(root_dir, rel)` を返す。file ルートでは `guest_path == ""` のみ許可し、`(親ディレクトリ, ファイル名)` に分解する。エラーは `String`(呼び出し側が `invalid-path` へ写像)

- [ ] **Step 1: 失敗するテストを書く**

`core/src/host/resolve.rs` のテストに追加。まず既存ヘルパを拡張:

```rust
fn fs_file_entry(granted: bool, mode: &str, path: &str) -> FsRuntimeEntry {
    FsRuntimeEntry {
        name: "status".to_string(),
        granted,
        mode: mode.to_string(),
        path: path.to_string(),
        target: "file".to_string(),
    }
}

#[test]
fn resolve_root_directory_entry_is_not_file() {
    let entries = fs_entries(vec![fs_entry(true, "read", "/tmp/exports")]);
    let resolved = resolve_root(&entries, "exports", false).unwrap();
    assert!(!resolved.is_file);
    assert_eq!(resolved.path, PathBuf::from("/tmp/exports"));
}

#[test]
fn resolve_root_file_entry_is_file() {
    let entries = fs_entries(vec![fs_file_entry(true, "read", "/home/u/Status.json")]);
    let resolved = resolve_root(&entries, "status", false).unwrap();
    assert!(resolved.is_file);
}

#[test]
fn effective_fs_target_directory_passes_guest_path_through() {
    let resolved = ResolvedRoot { path: PathBuf::from("/tmp/exports"), is_file: false };
    let (dir, rel) = effective_fs_target(&resolved, "sub/a.txt").unwrap();
    assert_eq!(dir, PathBuf::from("/tmp/exports"));
    assert_eq!(rel, "sub/a.txt");
}

#[test]
fn effective_fs_target_file_splits_into_parent_and_name() {
    let resolved = ResolvedRoot { path: PathBuf::from("/home/u/Status.json"), is_file: true };
    let (dir, rel) = effective_fs_target(&resolved, "").unwrap();
    assert_eq!(dir, PathBuf::from("/home/u"));
    assert_eq!(rel, "Status.json");
}

#[test]
fn effective_fs_target_file_rejects_nonempty_guest_path() {
    let resolved = ResolvedRoot { path: PathBuf::from("/home/u/Status.json"), is_file: true };
    let err = effective_fs_target(&resolved, "other.json").expect_err("non-empty path on file root");
    assert_eq!(err, "path must be empty for a file root");
}
```

- [ ] **Step 2: 失敗を確認**

Run: `cargo test -p edlr-core --lib host::resolve`
Expected: FAIL(`ResolvedRoot` 未定義のコンパイルエラー)

- [ ] **Step 3: 実装**

```rust
/// `resolve_root` の解決結果。`is_file` は当該ルートが `target = "file"`
/// (単一ファイル承認)かどうか。
#[derive(Debug)]
pub(crate) struct ResolvedRoot {
    pub path: PathBuf,
    pub is_file: bool,
}
```

`resolve_root` の末尾を変更(判定順・エラー文字列は既存のまま):

```rust
    Ok(ResolvedRoot {
        path: PathBuf::from(&entry.path),
        is_file: entry.target == "file",
    })
```

`effective_fs_target` を追加:

```rust
/// `FsDriver` に渡す `(root_dir, rel)` を組み立てる。
///
/// directory ルートはゲストのパスをそのまま相対パスとして通す。file ルート
/// はゲストのパスを `""` に限定し、承認されたファイルパスを「親ディレクトリ
/// + ファイル名」に分解して返す -- `FsDriver` のパス検証・サイズ上限・
/// 原子的書き込みをそのまま流用するため(ドライバ側に file 分岐を持たない)。
pub(crate) fn effective_fs_target(
    resolved: &ResolvedRoot,
    guest_path: &str,
) -> Result<(PathBuf, String), String> {
    if !resolved.is_file {
        return Ok((resolved.path.clone(), guest_path.to_string()));
    }
    if !guest_path.is_empty() {
        return Err("path must be empty for a file root".to_string());
    }
    let parent = resolved.path.parent();
    let name = resolved.path.file_name().and_then(|n| n.to_str());
    match (parent, name) {
        (Some(parent), Some(name)) => Ok((parent.to_path_buf(), name.to_string())),
        _ => Err(format!(
            "configured file path has no parent directory: {}",
            resolved.path.display()
        )),
    }
}
```

この時点では `host/plugin.rs` / `host/driver.rs` の `resolve_root` 呼び出しが型エラーになるので、Task 5 と同一コミットにせず**先にホスト側の写像だけ最小修正**してよい(`let root_path = self.resolve_root(...)?.path;` と一時的に `.path` を付ける)。その場合もテストは通ること。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core`
Expected: 全 PASS

- [ ] **Step 5: コミット**

```bash
git add core/src/host
git commit -m "feat(core): resolve_root が target を返し effective_fs_target を追加"
```

---

### Task 5: host(plugin / driver)の file ルート配線

**Files:**
- Modify: `core/src/host/plugin.rs`(`resolve_root` 764-778行、`DriverFsHost` impl 799-859行、テスト 1251行付近)
- Modify: `core/src/host/driver.rs`(`resolve_root` 434行付近、`DriverFsHost` impl 469-526行、テスト 869行付近)

**Interfaces:**
- Consumes: Task 4 の `ResolvedRoot` / `effective_fs_target`
- Produces: file ルートに対する WIT `driver-fs` の挙動 — `read`/`read-range`/`stat`/`write`/`append` は `path == ""` のみ(非空は `invalid-path`)、`list`/`delete` は常に `invalid-path`

- [ ] **Step 1: 失敗するテストを書く**

`core/src/host/plugin.rs` のテストに、既存の fs テスト(1230行付近の `fs_ctx(&filesystem_json_string(&[fs_entry(...)]))` パターン)と同じ流儀で追加。`filesystem_json_string` は `crate::runtime::fs::filesystem_json_string`:

```rust
fn fs_file_entry(granted: bool, mode: &str, path: &str) -> crate::runtime::fs::FsRuntimeEntry {
    crate::runtime::fs::FsRuntimeEntry {
        name: "status".to_string(),
        granted,
        mode: mode.to_string(),
        path: path.to_string(),
        target: "file".to_string(),
    }
}

#[test]
fn fs_read_on_file_root_reads_the_configured_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("Status.json");
    std::fs::write(&file, b"{\"ok\":true}").unwrap();
    let mut ctx = fs_ctx(&filesystem_json_string(&[fs_file_entry(
        true,
        "read",
        file.to_str().unwrap(),
    )]));
    let bytes = ctx.read("status".to_string(), "".to_string()).unwrap();
    assert_eq!(bytes, b"{\"ok\":true}");
}

#[test]
fn fs_read_on_file_root_rejects_nonempty_path() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("Status.json");
    std::fs::write(&file, b"x").unwrap();
    let mut ctx = fs_ctx(&filesystem_json_string(&[fs_file_entry(true, "read", file.to_str().unwrap())]));
    let err = ctx
        .read("status".to_string(), "Status.json".to_string())
        .expect_err("non-empty path must be rejected");
    assert!(matches!(err, WitFsError::InvalidPath(_)));
}

#[test]
fn fs_list_on_file_root_is_invalid_path() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("Status.json");
    std::fs::write(&file, b"x").unwrap();
    let mut ctx = fs_ctx(&filesystem_json_string(&[fs_file_entry(true, "read", file.to_str().unwrap())]));
    let err = ctx
        .list("status".to_string(), "".to_string())
        .expect_err("list on file root");
    assert!(matches!(err, WitFsError::InvalidPath(_)));
}

#[test]
fn fs_delete_on_file_root_is_invalid_path() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("Status.json");
    std::fs::write(&file, b"x").unwrap();
    let mut ctx =
        fs_ctx(&filesystem_json_string(&[fs_file_entry(true, "read-write", file.to_str().unwrap())]));
    let err = ctx
        .delete("status".to_string(), "".to_string())
        .expect_err("delete on file root");
    assert!(matches!(err, WitFsError::InvalidPath(_)));
    assert!(file.exists());
}

#[test]
fn fs_write_on_file_root_overwrites_the_configured_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("out.csv");
    std::fs::write(&file, b"old").unwrap();
    let mut ctx =
        fs_ctx(&filesystem_json_string(&[fs_file_entry(true, "read-write", file.to_str().unwrap())]));
    ctx.write("status".to_string(), "".to_string(), b"new".to_vec())
        .unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"new");
}
```

- [ ] **Step 2: 失敗を確認**

Run: `cargo test -p edlr-core --lib host::plugin`
Expected: `fs_read_on_file_root_reads_the_configured_file` 等が FAIL(Task 4 の暫定 `.path` のままだと file パスをルートディレクトリ扱いして NotFound/InvalidPath になる)

- [ ] **Step 3: 実装**

`core/src/host/plugin.rs` の `resolve_root` の戻り値を `ResolvedRoot` に変え(写像はそのまま)、各メソッドを配線:

```rust
    fn resolve_root(&self, root: &str, need_write: bool) -> Result<ResolvedRoot, WitFsError> {
        // 中身は従来どおり resolve::resolve_root へ委譲(エラー写像も不変)
    }
```

read / read_range / stat / write / append は共通パターン:

```rust
    fn read(&mut self, root: String, path: String) -> Result<Vec<u8>, WitFsError> {
        let resolved = self.resolve_root(&root, false)?;
        let (dir, rel) = effective_fs_target(&resolved, &path).map_err(WitFsError::InvalidPath)?;
        self.fs_driver.read(&dir, &rel).map_err(to_wit_fs_error)
    }
```

list / delete は file ルートを先に拒否:

```rust
    fn list(&mut self, root: String, prefix: String) -> Result<Vec<WitFsEntry>, WitFsError> {
        let resolved = self.resolve_root(&root, false)?;
        if resolved.is_file {
            return Err(WitFsError::InvalidPath(format!(
                "root {root} is a single file: list is not supported"
            )));
        }
        self.fs_driver
            .list(&resolved.path, &prefix)
            .map(|entries| entries.into_iter().map(to_wit_fs_entry).collect())
            .map_err(to_wit_fs_error)
    }

    fn delete(&mut self, root: String, path: String) -> Result<(), WitFsError> {
        let resolved = self.resolve_root(&root, true)?;
        if resolved.is_file {
            return Err(WitFsError::InvalidPath(format!(
                "root {root} is a single file: delete is not supported"
            )));
        }
        self.fs_driver
            .delete(&resolved.path, &path)
            .map_err(to_wit_fs_error)
    }
```

`append` は file ルートでも許可(mode が read-write なら)。`write` と同じく対象ファイルそのものへの書き込みであり、拒否する理由がない。

`core/src/host/driver.rs` の `DriverFsHost for DriverCtx`(469-526行)にも**同一の変更**を施す(こちらも `resolve::resolve_root` / `effective_fs_target` を共有しているので写像だけ)。driver 側テスト(869行付近の `fs_entry` を使う既存テストの隣)に、plugin 側から `fs_read_on_file_root_reads_the_configured_file` と `fs_list_on_file_root_is_invalid_path` の 2 本を driver の組み立てで写して追加する。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core`
Expected: 全 PASS(既存の directory ルートのテストが不変で通ることが後方互換の証拠)

- [ ] **Step 5: コミット**

```bash
git add core/src/host
git commit -m "feat(core): file ルートの host 配線(path 空のみ・list/delete 拒否)"
```

---

### Task 6: RPC 応答に target を載せる

**Files:**
- Modify: `core/src/rpc/render.rs:127-142`(`filesystem_result_json`)、テストは同ファイル 417行付近

**Interfaces:**
- Consumes: Task 1 の `FilesystemRequest.target`
- Produces: `get-filesystem` / `set-filesystem-*` / `plugins/list` / `drivers/list` の filesystem 各要素に `"target": "directory" | "file"`

- [ ] **Step 1: 失敗するテストを書く**

既存テスト `filesystem_json_includes_populated_root`(412行付近)にアサートを足すか、隣に追加:

```rust
#[test]
fn filesystem_json_includes_target() {
    let roots = vec![FilesystemInfo {
        request: FilesystemRequest {
            name: "status".to_string(),
            reason: "watch".to_string(),
            mode: FilesystemMode::Read,
            target: crate::capability::request::FilesystemTarget::File,
        },
        config: FilesystemConfig { path: String::new() },
        grant: GrantState { granted: false, stale: false },
    }];
    let json = filesystem_result_json(&roots);
    assert_eq!(json["roots"][0]["target"], "file");
}
```

(`GrantState` のフィールドが違う場合は既存テストの組み立てに合わせる。)

- [ ] **Step 2: 失敗を確認**

Run: `cargo test -p edlr-core --lib rpc::render`
Expected: FAIL(`target` キーが無い)

- [ ] **Step 3: 実装**

`filesystem_result_json` の `json!` に 1 行追加:

```rust
"target": info.request.target.as_str(),
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core`
Expected: 全 PASS(server の pin テストが RPC 生 JSON を釘付けしている場合は、期待値へ `"target": "directory"` を足して更新する — 出力仕様の意図的変更)

- [ ] **Step 5: コミット**

```bash
git add core/src
git commit -m "feat(core): filesystem RPC 応答に target を追加"
```

---

### Task 7: UI — ファイルピッカーと表示の出し分け

**Files:**
- Modify: `ui/frontend/src/types/plugin.ts:91-98`(`FilesystemRoot`)
- Modify: `ui/frontend/src/components/FilesystemSection.tsx`
- Modify: `ui/src-tauri/src/main.rs`(`pick_file` コマンド追加、302行付近と `invoke_handler` 352-359行)
- Test: `ui/frontend/src/components/FilesystemSection.test.tsx`

**Interfaces:**
- Consumes: Task 6 の RPC 応答 `target`
- Produces: `FilesystemRoot.target?: "directory" | "file"`(欠落時は directory 扱い)、Tauri コマンド `pick_file`

- [ ] **Step 1: 失敗するテストを書く**

`FilesystemSection.test.tsx` の既存テストの流儀(root オブジェクトの組み立て・render のしかた)に合わせて追加:

```tsx
test("file target のルートはファイルとして表示される", () => {
  render(
    <FilesystemSection
      roots={[
        {
          name: "status",
          reason: "Status.json を監視する",
          mode: "read",
          target: "file",
          granted: false,
          staleGrant: false,
          config: { path: "" },
        },
      ]}
      onConfigChange={async () => {}}
      onGrantChange={async () => {}}
    />,
  );
  expect(screen.getByLabelText("ファイル")).toBeInTheDocument();
  expect(
    screen.getByLabelText("このファイルへのアクセスを承認する"),
  ).toBeInTheDocument();
});

test("target が無い(旧デーモン)ルートはフォルダとして表示される", () => {
  render(
    <FilesystemSection
      roots={[
        {
          name: "exports",
          reason: "r",
          mode: "read",
          granted: false,
          staleGrant: false,
          config: { path: "" },
        },
      ]}
      onConfigChange={async () => {}}
      onGrantChange={async () => {}}
    />,
  );
  expect(screen.getByLabelText("フォルダ")).toBeInTheDocument();
});
```

(既存テストが render ヘルパや props の組み立て関数を持っていればそれを使う。)

- [ ] **Step 2: 失敗を確認**

Run: `cd ui/frontend && pnpm test -- FilesystemSection`
Expected: FAIL(型エラーまたは「ファイル」ラベル不在)

- [ ] **Step 3: 実装**

`types/plugin.ts`:

```ts
export interface FilesystemRoot {
  name: string;
  reason: string;
  mode: "read" | "read-write";
  /** 欠落(旧デーモン)時は directory 扱い */
  target?: "directory" | "file";
  granted: boolean;
  staleGrant: boolean;
  config: FilesystemConfig;
}
```

`FilesystemSection.tsx` の `FilesystemRootCard` 内で `const isFile = root.target === "file";` を作り出し分け:

- `handlePick`: `invoke<string | null>(isFile ? "pick_file" : "pick_directory")`
- パス入力のラベルと `aria-label`: `isFile ? "ファイル" : "フォルダ"`
- 承認チェックのラベルと `aria-label`: `isFile ? "このファイルへのアクセスを承認する" : "このフォルダへのアクセスを承認する"`
- 警告文:
  - file + read-write: `承認すると、このプラグインは選んだファイルを読み取り・上書きできます`
  - file + read: `承認すると、このプラグインは選んだファイルを読み取れます`
  - directory は既存文言のまま
- 未承認文言は既存のまま(`未承認 — このプラグインはファイルにアクセスできません`)

`ui/src-tauri/src/main.rs` に追加(`pick_directory` の隣、実装は `pick_executable` と同じ):

```rust
/// ネイティブのファイル選択ダイアログを開く(プラグインのファイル
/// アクセス設定で target = "file" のルート用)。キャンセル時は None。
#[tauri::command]
async fn pick_file(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_file(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten().map(|path| path.to_string())
}
```

`invoke_handler` の `generate_handler![...]` に `pick_file` を追加。

- [ ] **Step 4: テストが通ることを確認**

Run: `cd ui/frontend && pnpm test`
Expected: 全 PASS。Tauri 側は `cargo check`(`ui/src-tauri` で)が通ること。

- [ ] **Step 5: コミット**

```bash
git add ui/frontend/src ui/src-tauri/src/main.rs
git commit -m "feat(ui): file target のルートをファイルピッカーで設定できるようにする"
```

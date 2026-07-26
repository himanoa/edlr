# サイドカープロセス capability(driver-process)実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** プラグインが `driver-process.ensure-started` を呼ぶと、ユーザーが指定・承認した実行ファイルをホストがサイドカープロセスとして起動し、プラグインは既存の `driver-http` で `127.0.0.1:<port>` のそのプロセスと通信できるようにする。

**Architecture:** 新クレート `drivers/process` がプロセスの所有権(spawn / 監視 / プロセスグループ kill / レート制限)を持ち、`core` 側は既存の capability 機構(manifest 宣言 → grants 承認 → 呼び出し時照合)に sidecar を足す。`drivers/http` と対称。プラグインは PID もハンドルも受け取らず、argv も決められない。

**Tech Stack:** Rust 2021 / wasmtime component model + WIT / libc(プロセスグループ kill)/ tracing / axum WebSocket RPC / React + TypeScript + vitest / Tauri 2。

**設計書:** `docs/superpowers/specs/2026-07-26-edlr-sidecar-process-driver-design.md`

## Global Constraints

- Rust edition 2021。ワークスペースは `Cargo.toml` の `members` に `drivers/process` を追加する
- 新規依存は `libc`(0.2)と `tracing`(既存バージョンに合わせる)のみ。非同期ランタイムを `drivers/process` に持ち込まない(呼び出し元はプラグイン専用 OS スレッド)
- ドキュメントコメントは既存コードにならい日本語(`core/src/plugin/*.rs` の流儀)、セキュリティ上の判断根拠を書くコメントは英語でも可(既存 `host.rs` がそう)
- **`HTTP_TIMEOUT` と違い `driver-process` の各呼び出しはブロックしない**。`PluginInstance::CALL_DEADLINE`(2 秒)を消費する待ち合わせを新たに入れないこと
- サイドカー設定は `<settings-dir>/<plugin-id>.sidecars.json` に保存する(**設計書からの変更点**: 設計書は `<settings-dir>/<plugin-id>.json` の名前空間と書いていたが、`SettingsStore::update` は manifest の `[[settings]]` に無いキーをディスクから間引く実装なので、同じファイルに置くと設定保存のたびに消える)
- テスト実行: Rust は `cargo test`(ワークスペースルート)、フロントエンドは `cd ui/frontend && mise exec -- pnpm test`、Tauri 側は `cd ui/src-tauri && cargo test`(独立ワークスペース)
- 各タスクの最後にコミットする。コミットメッセージは Conventional Commits(`feat(plugin):` / `feat(ui):` / `test:` など)

## File Structure

**新規**

| ファイル | 責務 |
|---|---|
| `drivers/process/Cargo.toml` | `edlr-driver-process` クレート定義 |
| `drivers/process/src/lib.rs` | `ProcessDriver` — spawn / status / stop / stop_all、レート制限、プロセスグループ kill、stdout/stderr のログ転送 |
| `core/src/plugin/sidecar.rs` | `SidecarRequest`(manifest 側)、`SidecarConfig`(ユーザー設定)、`SidecarConfigStore`、ポート採番と検証 |
| `core/src/plugin/sidecar_runtime.rs` | `sidecars_json` 共有バッファの組み立て/パース(`capabilities_json` と同じ流儀) |
| `core/tests/driver_process_integration.rs` | 実 wasm を使わないホスト側統合テスト(承認・暗黙許可・shutdown) |
| `ui/frontend/src/components/SidecarSection.tsx` | サイドカー設定・承認・インスタンス表示 UI |
| `ui/frontend/src/components/SidecarSection.test.tsx` | 同テスト |

**変更**

| ファイル | 変更内容 |
|---|---|
| `core/wit/plugin.wit` | `interface driver-process` 追加、`world plugin` に `import driver-process;` |
| `core/src/plugin/manifest.rs` | `[[sidecar]]` のパース・検証・フィンガープリント |
| `core/src/plugin/grants.rs` | サイドカー単位の grant(`SavedGrant` に `sidecars` を追加、後方互換) |
| `core/src/plugin/host.rs` | `DriverProcessHost` 実装、`HostCtx` に `sidecars_json` と `process_driver`、`capabilities_json` の形を `{"hosts":[...]}` へ変更 |
| `core/src/plugin/registry.rs` | サイドカーの設定/承認/制御 API、`PluginInfo` に sidecar 情報 |
| `core/src/plugin/runner.rs` | 起動時の `sidecars_json` 構築、shutdown での全停止 |
| `core/src/plugin/mod.rs` | 新モジュールの `pub mod` / re-export |
| `core/src/server.rs` | RPC 4 メソッド + `plugins/list` への `sidecars` 追加 |
| `core/src/bin/edlr.rs` | shutdown 時に `Registry::stop_all_sidecars()` を呼ぶ |
| `ui/frontend/src/types/plugin.ts` | `Sidecar` 型 |
| `ui/frontend/src/pages/Plugins.tsx` | `SidecarSection` の配線 |
| `ui/src-tauri/src/main.rs` | `pick_executable` コマンド |
| `README.md` | サイドカー capability の節 |

---

### Task 1: `drivers/process` クレート — ProcessDriver

**Files:**
- Create: `drivers/process/Cargo.toml`, `drivers/process/src/lib.rs`
- Modify: `Cargo.toml`(ワークスペース `members`)

**Interfaces:**
- Consumes: なし(最初のタスク)
- Produces:
  - `edlr_driver_process::ProcessDriver::new(shutdown_grace: Duration, spawn_min_interval: Duration) -> ProcessDriver`
  - `ProcessSpec { command: PathBuf, args: Vec<String>, ports: Vec<u16> }`
  - `InstanceStatus { index: u32, port: u16, running: bool, exit_code: Option<i32> }`
  - `ProcessError::{RateLimited(String), Spawn(String)}`
  - `ProcessDriver::ensure_started(&self, key: &str, spec: &ProcessSpec) -> Result<Vec<InstanceStatus>, ProcessError>`
  - `ProcessDriver::status(&self, key: &str, spec: &ProcessSpec) -> Vec<InstanceStatus>`
  - `ProcessDriver::stop(&self, key: &str)`
  - `ProcessDriver::stop_all(&self)`

- [ ] **Step 1: クレートを作りワークスペースに登録する**

`drivers/process/Cargo.toml`:

```toml
[package]
name = "edlr-driver-process"
version = "0.1.0"
edition = "2021"

[dependencies]
libc = "0.2"
tracing = "0.1"
```

ルート `Cargo.toml` の `members` を次に変更:

```toml
members = ["config", "core", "drivers/http", "drivers/process", "drivers/channel", "ui/src-tauri"]
```

- [ ] **Step 2: 失敗するテストを書く(冪等な起動と status)**

`drivers/process/src/lib.rs` の末尾に:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn driver() -> ProcessDriver {
        ProcessDriver::new(Duration::from_millis(200), Duration::from_millis(0))
    }

    /// 起動しっぱなしになる無害なプロセス。ポートは使わないが、
    /// `{port}` 展開と引数の受け渡しを一緒に確認できる形にしておく。
    fn sleep_spec(ports: Vec<u16>) -> ProcessSpec {
        ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "echo port={port}; sleep 30".to_string()],
            ports,
        }
    }

    #[test]
    fn ensure_started_spawns_one_process_per_port_and_is_idempotent() {
        let driver = driver();
        let spec = sleep_spec(vec![50021, 50022]);

        let first = driver.ensure_started("p/tts", &spec).expect("first start");
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|i| i.running));
        assert_eq!(first[0].port, 50021);
        assert_eq!(first[1].port, 50022);
        assert_eq!(first[1].index, 1);

        let second = driver.ensure_started("p/tts", &spec).expect("second start");
        assert!(second.iter().all(|i| i.running));

        driver.stop("p/tts");
        assert!(driver.status("p/tts", &spec).iter().all(|i| !i.running));
    }
}
```

`lib.rs` の先頭に必要な `use` を置く:

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
```

- [ ] **Step 3: テストが失敗することを確認する**

Run: `cargo test -p edlr-driver-process`
Expected: FAIL(`ProcessDriver` などが未定義でコンパイルエラー)

- [ ] **Step 4: 最小実装を書く**

`drivers/process/src/lib.rs`(テストモジュールの上):

```rust
//! プラグインのサイドカープロセスを起動・監視・確実に停止するドライバ。
//!
//! プロセスの所有権はこのドライバ(= ホスト)にあり、呼び出し元(プラグイン)は
//! PID もハンドルも受け取らない。停止は必ずプロセスグループ単位で行うため、
//! サイドカーが更に子を作っていても孤児が残らない。

/// 起動するプロセスの仕様。`args` 内の `{port}` は各インスタンスの実ポートに展開される。
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessSpec {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub ports: Vec<u16>,
}

/// 1 インスタンスの現在状態。
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceStatus {
    pub index: u32,
    pub port: u16,
    pub running: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug)]
pub enum ProcessError {
    /// 直近の spawn 試行から `spawn_min_interval` が経過していない。
    RateLimited(String),
    /// 実行ファイルが無い、権限が無い等で spawn 自体に失敗した。
    Spawn(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessError::RateLimited(msg) => write!(f, "{msg}"),
            ProcessError::Spawn(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ProcessError {}

/// 起動済みインスタンス 1 件。`child` が `None` なら終了済み。
struct Instance {
    index: u32,
    port: u16,
    child: Option<Child>,
    exit_code: Option<i32>,
}

/// 1 サイドカー(= 1 key)分のインスタンス群と、直近の spawn 試行時刻。
struct Group {
    instances: Vec<Instance>,
    last_spawn_attempt: Option<Instant>,
}

pub struct ProcessDriver {
    groups: Mutex<HashMap<String, Group>>,
    shutdown_grace: Duration,
    spawn_min_interval: Duration,
}

impl ProcessDriver {
    pub fn new(shutdown_grace: Duration, spawn_min_interval: Duration) -> ProcessDriver {
        ProcessDriver {
            groups: Mutex::new(HashMap::new()),
            shutdown_grace,
            spawn_min_interval,
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Group>> {
        self.groups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 生きていないインスタンスだけを spawn し、直後の状態を返す。
    /// 既に全て生きていればレート制限にも掛からず、そのまま成功する。
    pub fn ensure_started(
        &self,
        key: &str,
        spec: &ProcessSpec,
    ) -> Result<Vec<InstanceStatus>, ProcessError> {
        let mut groups = self.lock();
        let group = groups.entry(key.to_string()).or_insert_with(|| Group {
            instances: Vec::new(),
            last_spawn_attempt: None,
        });

        reap(group);
        align_instances(group, spec);

        let needs_spawn = group.instances.iter().any(|i| i.child.is_none());
        if !needs_spawn {
            return Ok(snapshot(group));
        }

        if let Some(last) = group.last_spawn_attempt {
            let elapsed = last.elapsed();
            if elapsed < self.spawn_min_interval {
                return Err(ProcessError::RateLimited(format!(
                    "sidecar spawn rate-limited: retry in {} ms",
                    (self.spawn_min_interval - elapsed).as_millis()
                )));
            }
        }
        group.last_spawn_attempt = Some(Instant::now());

        let mut first_error: Option<String> = None;
        for instance in group.instances.iter_mut() {
            if instance.child.is_some() {
                continue;
            }
            match spawn_one(key, spec, instance.port) {
                Ok(child) => {
                    instance.child = Some(child);
                    instance.exit_code = None;
                }
                Err(e) if first_error.is_none() => first_error = Some(e),
                Err(_) => {}
            }
        }

        if let Some(e) = first_error {
            return Err(ProcessError::Spawn(e));
        }
        Ok(snapshot(group))
    }

    /// 現在の状態を返す(spawn はしない)。未起動の key でも `spec` に沿った
    /// `exited` 相当の一覧を返す。
    pub fn status(&self, key: &str, spec: &ProcessSpec) -> Vec<InstanceStatus> {
        let mut groups = self.lock();
        let group = groups.entry(key.to_string()).or_insert_with(|| Group {
            instances: Vec::new(),
            last_spawn_attempt: None,
        });
        reap(group);
        align_instances(group, spec);
        snapshot(group)
    }

    /// 当該サイドカーの全インスタンスを停止する。既に止まっていても成功。
    pub fn stop(&self, key: &str) {
        let mut groups = self.lock();
        if let Some(group) = groups.get_mut(key) {
            for instance in group.instances.iter_mut() {
                terminate(instance, self.shutdown_grace);
            }
        }
    }

    /// 全 key の全インスタンスを停止する(デーモン shutdown 用)。
    pub fn stop_all(&self) {
        let mut groups = self.lock();
        for group in groups.values_mut() {
            for instance in group.instances.iter_mut() {
                terminate(instance, self.shutdown_grace);
            }
        }
    }
}

impl Drop for ProcessDriver {
    /// 明示的な `stop_all` を呼び忘れた経路(テストや panic 巻き戻し)でも
    /// 孤児を残さないための最後の砦。通常経路は `stop_all` を明示的に呼ぶこと。
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// `spec.ports` に合わせてインスタンス列を作り直す。ポート構成が変わった
/// (replicas 変更など)場合は、生きているインスタンスを停止してから作り直す。
fn align_instances(group: &mut Group, spec: &ProcessSpec) {
    let same = group.instances.len() == spec.ports.len()
        && group
            .instances
            .iter()
            .zip(spec.ports.iter())
            .all(|(instance, port)| instance.port == *port);
    if same {
        return;
    }

    for instance in group.instances.iter_mut() {
        terminate(instance, Duration::from_millis(0));
    }
    group.instances = spec
        .ports
        .iter()
        .enumerate()
        .map(|(index, port)| Instance {
            index: index as u32,
            port: *port,
            child: None,
            exit_code: None,
        })
        .collect();
}

/// 終了済みの子を回収し、`exit_code` を記録する。
fn reap(group: &mut Group) {
    for instance in group.instances.iter_mut() {
        let Some(child) = instance.child.as_mut() else {
            continue;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                instance.exit_code = status.code();
                instance.child = None;
            }
            Ok(None) => {}
            Err(_) => {
                instance.child = None;
            }
        }
    }
}

fn snapshot(group: &Group) -> Vec<InstanceStatus> {
    group
        .instances
        .iter()
        .map(|instance| InstanceStatus {
            index: instance.index,
            port: instance.port,
            running: instance.child.is_some(),
            exit_code: instance.exit_code,
        })
        .collect()
}

fn spawn_one(key: &str, spec: &ProcessSpec, port: u16) -> Result<Child, String> {
    use std::os::unix::process::CommandExt;

    let args: Vec<String> = spec
        .args
        .iter()
        .map(|arg| arg.replace("{port}", &port.to_string()))
        .collect();

    let mut command = Command::new(&spec.command);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 新しいプロセスグループに置く。停止時に killpg するので、サイドカーが
    // 更に子を作っていても道連れにできる(孤児が残らない)。
    command.process_group(0);

    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", spec.command.display()))?;

    if let Some(stdout) = child.stdout.take() {
        forward_output(key.to_string(), "stdout", stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        forward_output(key.to_string(), "stderr", stderr);
    }

    Ok(child)
}

/// 子の出力を 1 行ずつホストのログへ流す。プラグインには渡さない。
fn forward_output<R: std::io::Read + Send + 'static>(key: String, stream: &'static str, reader: R) {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let buffered = std::io::BufReader::new(reader);
        for line in buffered.lines() {
            let Ok(line) = line else { break };
            tracing::info!(sidecar = %key, stream, "{line}");
        }
    });
}

/// SIGTERM をプロセスグループへ送り、`grace` 待って死ななければ SIGKILL。
fn terminate(instance: &mut Instance, grace: Duration) {
    let Some(child) = instance.child.as_mut() else {
        return;
    };
    let pid = child.id() as i32;

    // SAFETY: `pid` は自分が spawn した(まだ wait していない)子のもので、
    // `process_group(0)` により pid == pgid。負値にしてグループへ送る。
    unsafe {
        libc::killpg(pid, libc::SIGTERM);
    }

    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                instance.exit_code = status.code();
                instance.child = None;
                return;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    unsafe {
        libc::killpg(pid, libc::SIGKILL);
    }
    let _ = child.wait();
    instance.exit_code = None;
    instance.child = None;
}
```

- [ ] **Step 5: テストが通ることを確認する**

Run: `cargo test -p edlr-driver-process`
Expected: PASS

- [ ] **Step 6: レート制限のテストを追加する**

```rust
    #[test]
    fn respawn_within_min_interval_is_rate_limited() {
        let driver = ProcessDriver::new(Duration::from_millis(200), Duration::from_secs(1));
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "exit 3".to_string()],
            ports: vec![50100],
        };

        driver.ensure_started("p/quick", &spec).expect("first start");
        // 終了を待ってから再度 ensure_started する。
        std::thread::sleep(Duration::from_millis(200));

        let err = driver
            .ensure_started("p/quick", &spec)
            .expect_err("respawn within the min interval must be rejected");
        assert!(matches!(err, ProcessError::RateLimited(_)));
    }

    #[test]
    fn exited_process_reports_exit_code_and_not_running() {
        let driver = driver();
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "exit 7".to_string()],
            ports: vec![50101],
        };

        driver.ensure_started("p/exits", &spec).expect("start");
        std::thread::sleep(Duration::from_millis(200));

        let status = driver.status("p/exits", &spec);
        assert_eq!(status.len(), 1);
        assert!(!status[0].running);
        assert_eq!(status[0].exit_code, Some(7));
    }

    #[test]
    fn missing_executable_is_a_spawn_error() {
        let driver = driver();
        let spec = ProcessSpec {
            command: PathBuf::from("/nonexistent/edlr-test-binary"),
            args: vec![],
            ports: vec![50102],
        };

        let err = driver
            .ensure_started("p/missing", &spec)
            .expect_err("spawning a nonexistent binary must fail");
        assert!(matches!(err, ProcessError::Spawn(_)));
    }
```

- [ ] **Step 7: プロセスグループ kill のテストを追加する**

孫プロセスまで確実に死ぬことが、このドライバの一番重要な保証。孫が自分の PID を書くファイルを作らせ、停止後にその PID が生きていないことを確認する。

```rust
    #[test]
    fn stop_kills_grandchildren_via_process_group() {
        let tmp = std::env::temp_dir().join(format!("edlr-pgid-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);

        let driver = driver();
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".to_string(),
                format!("(sleep 60 & echo $! > {}); wait", tmp.display()),
            ],
            ports: vec![50103],
        };

        driver.ensure_started("p/tree", &spec).expect("start");

        // 孫が PID を書き出すまで待つ(最大 2 秒)。
        let mut grandchild_pid = None;
        for _ in 0..200 {
            if let Ok(content) = std::fs::read_to_string(&tmp) {
                if let Ok(pid) = content.trim().parse::<i32>() {
                    grandchild_pid = Some(pid);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let grandchild_pid = grandchild_pid.expect("grandchild should report its pid");

        driver.stop("p/tree");
        std::thread::sleep(Duration::from_millis(300));

        // signal 0 は「シグナルを送らず存在確認だけする」。0 が返る = まだ生きている。
        // SAFETY: 存在確認のみで、シグナルは送らない。
        let alive = unsafe { libc::kill(grandchild_pid, 0) } == 0;
        let _ = std::fs::remove_file(&tmp);
        assert!(
            !alive,
            "grandchild {grandchild_pid} survived stop(); process-group kill is not working"
        );
    }
```

- [ ] **Step 8: テストを実行する**

Run: `cargo test -p edlr-driver-process`
Expected: PASS(5 テスト)

- [ ] **Step 9: コミット**

```bash
git add Cargo.toml Cargo.lock drivers/process
git commit -m "feat(drivers): add process driver owning sidecar lifecycles"
```

---

### Task 2: manifest の `[[sidecar]]` パースと検証

**Files:**
- Modify: `core/src/plugin/manifest.rs`
- Test: `core/src/plugin/manifest.rs`(既存の `#[cfg(test)] mod tests` に追記)

**Interfaces:**
- Consumes: なし
- Produces:
  - `pub struct SidecarRequest { pub name: String, pub reason: String, pub args: Vec<String>, pub port: u16, pub scalable: bool }`(`serde::Deserialize + Serialize + Clone + PartialEq + Debug`)
  - `Manifest.sidecars: Vec<SidecarRequest>`
  - `Manifest::sidecar(&self, name: &str) -> Option<&SidecarRequest>`
  - `Manifest::sidecar_fingerprint(&self, name: &str) -> Option<String>`
  - `ManifestError::BadSidecar(String)`

- [ ] **Step 1: 失敗するテストを書く**

`core/src/plugin/manifest.rs` の `mod tests` に追記(既存テストが `write_manifest` 相当のヘルパを持つ場合はそれに合わせる。無ければ以下の `parse` ヘルパを足す):

```rust
    fn parse_sidecar_manifest(body: &str) -> Result<Manifest, ManifestError> {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("sc-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            format!(
                "id = \"sc-plugin\"\nname = \"SC\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n{body}"
            ),
        )
        .unwrap();
        load_manifest(&plugin_dir)
    }

    #[test]
    fn sidecar_block_is_parsed() {
        let manifest = parse_sidecar_manifest(
            r#"
[[sidecar]]
name = "tts"
reason = "音声合成エンジンをローカルで動かすため"
args = ["--port", "{port}"]
port = 50021
scalable = true
"#,
        )
        .expect("valid sidecar manifest should load");

        assert_eq!(manifest.sidecars.len(), 1);
        let sidecar = &manifest.sidecars[0];
        assert_eq!(sidecar.name, "tts");
        assert_eq!(sidecar.port, 50021);
        assert!(sidecar.scalable);
        assert_eq!(sidecar.args, vec!["--port".to_string(), "{port}".to_string()]);
    }

    #[test]
    fn scalable_defaults_to_false_and_args_default_to_empty() {
        let manifest = parse_sidecar_manifest(
            r#"
[[sidecar]]
name = "tts"
reason = "reason"
port = 50021
"#,
        )
        .expect("minimal sidecar manifest should load");

        assert!(!manifest.sidecars[0].scalable);
        assert!(manifest.sidecars[0].args.is_empty());
    }

    #[test]
    fn duplicate_sidecar_name_is_rejected() {
        let err = parse_sidecar_manifest(
            r#"
[[sidecar]]
name = "tts"
reason = "a"
port = 50021

[[sidecar]]
name = "tts"
reason = "b"
port = 50030
"#,
        )
        .expect_err("duplicate sidecar names must be rejected");
        assert!(matches!(err, ManifestError::BadSidecar(_)));
    }

    #[test]
    fn bad_sidecar_name_and_empty_reason_are_rejected() {
        assert!(matches!(
            parse_sidecar_manifest("[[sidecar]]\nname = \"TTS\"\nreason = \"a\"\nport = 1\n")
                .expect_err("uppercase name must be rejected"),
            ManifestError::BadSidecar(_)
        ));
        assert!(matches!(
            parse_sidecar_manifest("[[sidecar]]\nname = \"tts\"\nreason = \"  \"\nport = 1\n")
                .expect_err("blank reason must be rejected"),
            ManifestError::BadSidecar(_)
        ));
    }

    #[test]
    fn sidecar_fingerprint_is_stable_and_changes_with_the_request() {
        let manifest = parse_sidecar_manifest(
            "[[sidecar]]\nname = \"tts\"\nreason = \"a\"\nargs = [\"--port\", \"{port}\"]\nport = 50021\n",
        )
        .unwrap();
        let first = manifest.sidecar_fingerprint("tts").expect("fingerprint");
        assert_eq!(first, manifest.sidecar_fingerprint("tts").unwrap());
        assert_eq!(manifest.sidecar_fingerprint("nope"), None);

        let changed = parse_sidecar_manifest(
            "[[sidecar]]\nname = \"tts\"\nreason = \"a\"\nargs = [\"--port\", \"{port}\"]\nport = 50022\n",
        )
        .unwrap();
        assert_ne!(first, changed.sidecar_fingerprint("tts").unwrap());
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core sidecar`
Expected: FAIL(`sidecars` フィールドや `BadSidecar` が未定義でコンパイルエラー)

- [ ] **Step 3: 実装する**

`manifest.rs` に追加:

```rust
/// プラグインが要求するサイドカープロセス 1 件。
///
/// **実行ファイルのパス(`command`)はここに書けない** — 必ずユーザーが
/// UI で入力する。承認画面に出る内容と実際に走るプログラムを、ユーザー自身の
/// 明示的な指定によって一致させるため。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SidecarRequest {
    pub name: String,
    pub reason: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub port: u16,
    #[serde(default)]
    pub scalable: bool,
}
```

`Manifest` に:

```rust
    #[serde(default, rename = "sidecar")]
    pub sidecars: Vec<SidecarRequest>,
```

`impl Manifest` に:

```rust
    pub fn sidecar(&self, name: &str) -> Option<&SidecarRequest> {
        self.sidecars.iter().find(|s| s.name == name)
    }

    /// サイドカー 1 件の要求内容の安定フィンガープリント(grants の失効判定に使う)。
    ///
    /// `capabilities_fingerprint` と同じ長さ接頭辞エンコード + SHA-256。
    /// **ユーザーが入力する `command` は含めない** — パスの変更は再承認ではなく
    /// 「設定変更 → 停止 → 次の ensure-started で新パスを起動」として扱うため
    /// (設計書「付与(grants)」の節を参照)。
    pub fn sidecar_fingerprint(&self, name: &str) -> Option<String> {
        let sidecar = self.sidecar(name)?;

        let mut canonical = encode_field("sidecar");
        canonical.push_str(&encode_field(&sidecar.name));
        canonical.push_str(&encode_field(&sidecar.reason));
        canonical.push_str(&encode_field(&sidecar.args.len().to_string()));
        for arg in &sidecar.args {
            canonical.push_str(&encode_field(arg));
        }
        canonical.push_str(&encode_field(&sidecar.port.to_string()));
        canonical.push_str(&encode_field(if sidecar.scalable { "1" } else { "0" }));

        Some(sha256_hex(&canonical))
    }
```

`ManifestError` に `BadSidecar(String)` を追加し、`Display` に:

```rust
            ManifestError::BadSidecar(msg) => write!(f, "invalid sidecar request: {msg}"),
```

検証関数を追加:

```rust
/// `[[sidecar]]` を検証・正規化する。`reason` は `capabilities` と同じく
/// trim して不可視文字を拒否する(承認画面に出る文字列とフィンガープリントの
/// 元になる文字列を byte 単位で一致させるため)。
fn validate_sidecars(sidecars: &mut [SidecarRequest]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for sidecar in sidecars.iter_mut() {
        if !is_valid_id(&sidecar.name) {
            return Err(ManifestError::BadSidecar(format!(
                "sidecar name must match [a-z0-9-]+: {}",
                sidecar.name
            )));
        }
        if !seen.insert(sidecar.name.clone()) {
            return Err(ManifestError::BadSidecar(format!(
                "duplicate sidecar name: {}",
                sidecar.name
            )));
        }
        if sidecar.port == 0 {
            return Err(ManifestError::BadSidecar(
                "sidecar port must be 1..=65535".to_string(),
            ));
        }

        let trimmed = sidecar.reason.trim().to_string();
        if trimmed.is_empty() {
            return Err(ManifestError::BadSidecar(
                "sidecar requires a non-empty reason".to_string(),
            ));
        }
        reject_invisible_chars("reason", &trimmed).map_err(ManifestError::BadSidecar)?;
        sidecar.reason = trimmed;

        for arg in &sidecar.args {
            reject_invisible_chars("args", arg).map_err(ManifestError::BadSidecar)?;
        }
    }
    Ok(())
}
```

`load_manifest` の `validate_capabilities(...)` の直後に:

```rust
    validate_sidecars(&mut manifest.sidecars)?;
```

`core/src/plugin/mod.rs` の re-export に `SidecarRequest` を追加する。既存テストで `Manifest { ... }` をリテラル構築している箇所(`grants.rs` / `runner.rs` のテスト)には `sidecars: vec![]` を足してコンパイルを通す。

- [ ] **Step 4: テストを実行する**

Run: `cargo test -p edlr-core`
Expected: PASS(新規 5 テストを含め全て)

- [ ] **Step 5: コミット**

```bash
git add core/src/plugin/manifest.rs core/src/plugin/mod.rs core/src/plugin/grants.rs core/src/plugin/runner.rs
git commit -m "feat(plugin): parse and validate [[sidecar]] manifest blocks"
```

---

### Task 3: サイドカー設定ストアとポート採番

**Files:**
- Create: `core/src/plugin/sidecar.rs`
- Modify: `core/src/plugin/mod.rs`
- Test: `core/src/plugin/sidecar.rs`(同ファイル内 `mod tests`)

**Interfaces:**
- Consumes: `SidecarRequest`(Task 2)
- Produces:
  - `pub struct SidecarConfig { pub command: String, pub args: Vec<String>, pub port: u16, pub replicas: u16 }`(`Serialize + Deserialize + Clone + PartialEq + Debug`)
  - `SidecarConfig::from_request(&SidecarRequest) -> SidecarConfig`(`command` は空文字)
  - `pub fn assign_ports(config: &SidecarConfig) -> Vec<u16>`
  - `pub enum SidecarConfigError { NotScalable(String), MissingPortPlaceholder(String), PortOverflow(String), PortRangeOverlap(String), Io(std::io::Error), Serialize(serde_json::Error) }`(`Display + Error`)
  - `pub struct SidecarConfigStore`(`new(dir: PathBuf)`)
  - `SidecarConfigStore::effective(&self, manifest: &Manifest) -> BTreeMap<String, SidecarConfig>`
  - `SidecarConfigStore::update_and_effective(&self, manifest: &Manifest, name: &str, config: &SidecarConfig) -> Result<BTreeMap<String, SidecarConfig>, SidecarConfigError>`

- [ ] **Step 1: 失敗するテストを書く**

`core/src/plugin/sidecar.rs` に(実装は空のまま):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::SidecarRequest;

    fn manifest_with(sidecars: Vec<SidecarRequest>) -> Manifest {
        Manifest {
            id: "sc-plugin".into(),
            name: "SC".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars,
        }
    }

    fn request(name: &str, port: u16, scalable: bool) -> SidecarRequest {
        SidecarRequest {
            name: name.into(),
            reason: "reason".into(),
            args: vec!["--port".into(), "{port}".into()],
            port,
            scalable,
        }
    }

    #[test]
    fn effective_falls_back_to_manifest_defaults_with_empty_command() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SidecarConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("tts", 50021, true)]);

        let effective = store.effective(&manifest);
        let config = effective.get("tts").expect("tts config");
        assert_eq!(config.command, "");
        assert_eq!(config.port, 50021);
        assert_eq!(config.replicas, 1);
        assert_eq!(config.args, vec!["--port".to_string(), "{port}".to_string()]);
    }

    #[test]
    fn update_persists_and_assigns_sequential_ports() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        let store = SidecarConfigStore::new(dir.clone());
        let manifest = manifest_with(vec![request("tts", 50021, true)]);

        let updated = store
            .update_and_effective(
                &manifest,
                "tts",
                &SidecarConfig {
                    command: "/usr/bin/piper".into(),
                    args: vec!["--port".into(), "{port}".into()],
                    port: 50021,
                    replicas: 3,
                },
            )
            .expect("update should succeed");

        assert_eq!(updated["tts"].command, "/usr/bin/piper");
        assert_eq!(assign_ports(&updated["tts"]), vec![50021, 50022, 50023]);
        assert!(dir.join("sc-plugin.sidecars.json").is_file());

        // 再読込しても保持されている。
        let reread = SidecarConfigStore::new(dir).effective(&manifest);
        assert_eq!(reread["tts"].replicas, 3);
    }

    #[test]
    fn replicas_above_one_requires_scalable_and_a_port_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SidecarConfigStore::new(tmp.path().join("settings"));

        let not_scalable = manifest_with(vec![request("tts", 50021, false)]);
        let err = store
            .update_and_effective(
                &not_scalable,
                "tts",
                &SidecarConfig {
                    command: "/bin/true".into(),
                    args: vec!["--port".into(), "{port}".into()],
                    port: 50021,
                    replicas: 2,
                },
            )
            .expect_err("replicas > 1 on a non-scalable sidecar must be rejected");
        assert!(matches!(err, SidecarConfigError::NotScalable(_)));

        let scalable = manifest_with(vec![request("tts", 50021, true)]);
        let err = store
            .update_and_effective(
                &scalable,
                "tts",
                &SidecarConfig {
                    command: "/bin/true".into(),
                    args: vec!["--fixed-port".into(), "50021".into()],
                    port: 50021,
                    replicas: 2,
                },
            )
            .expect_err("replicas > 1 without {port} must be rejected");
        assert!(matches!(err, SidecarConfigError::MissingPortPlaceholder(_)));
    }

    #[test]
    fn overlapping_port_ranges_within_a_plugin_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SidecarConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("tts", 50021, true), request("tr", 50030, false)]);

        // tts が 50021..=50031 を占めると tr(50030)と重なる。
        let err = store
            .update_and_effective(
                &manifest,
                "tts",
                &SidecarConfig {
                    command: "/bin/true".into(),
                    args: vec!["--port".into(), "{port}".into()],
                    port: 50021,
                    replicas: 11,
                },
            )
            .expect_err("overlapping port ranges must be rejected");
        assert!(matches!(err, SidecarConfigError::PortRangeOverlap(_)));
    }

    #[test]
    fn port_range_overflowing_65535_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SidecarConfigStore::new(tmp.path().join("settings"));
        let manifest = manifest_with(vec![request("tts", 65535, true)]);

        let err = store
            .update_and_effective(
                &manifest,
                "tts",
                &SidecarConfig {
                    command: "/bin/true".into(),
                    args: vec!["--port".into(), "{port}".into()],
                    port: 65535,
                    replicas: 2,
                },
            )
            .expect_err("a port range past 65535 must be rejected");
        assert!(matches!(err, SidecarConfigError::PortOverflow(_)));
    }

    #[test]
    fn broken_json_on_disk_falls_back_to_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sc-plugin.sidecars.json"), "not json {{{").unwrap();

        let store = SidecarConfigStore::new(dir);
        let manifest = manifest_with(vec![request("tts", 50021, true)]);
        assert_eq!(store.effective(&manifest)["tts"].port, 50021);
    }
}
```

- [ ] **Step 2: テストが失敗することを確認する**

`core/src/plugin/mod.rs` に `pub mod sidecar;` と re-export を足したうえで、

Run: `cargo test -p edlr-core sidecar::tests`
Expected: FAIL(未実装のコンパイルエラー)

- [ ] **Step 3: 実装する**

`core/src/plugin/sidecar.rs`(テストモジュールの上):

```rust
//! サイドカーのユーザー設定(`command` / `args` / `port` / `replicas`)の
//! 永続化と検証、およびポート採番。
//!
//! 保存先は `<settings-dir>/<plugin-id>.sidecars.json`。通常の
//! `[[settings]]` とは別ファイルにしている: `SettingsStore::update` は
//! manifest の `[[settings]]` に無いキーをディスクから間引く実装なので、
//! 同じファイルに同居させると設定保存のたびにサイドカー設定が消えてしまう。

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::plugin::manifest::SidecarRequest;
use crate::plugin::Manifest;

/// サイドカー 1 件のユーザー設定。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SidecarConfig {
    /// 実行ファイルの絶対パス。空文字は「未設定」(承認も起動もできない)。
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub port: u16,
    #[serde(default = "one")]
    pub replicas: u16,
}

fn one() -> u16 {
    1
}

impl SidecarConfig {
    /// manifest の既定値から、`command` 未設定の初期設定を作る。
    pub fn from_request(request: &SidecarRequest) -> SidecarConfig {
        SidecarConfig {
            command: String::new(),
            args: request.args.clone(),
            port: request.port,
            replicas: 1,
        }
    }
}

/// `config` に対応する実ポート列(`port, port+1, …, port+replicas-1`)。
/// `replicas` が 0 のときは 1 台として扱う(UI/RPC 側の検証を通り抜けた
/// 値でも空の spec を作らないための下限)。
pub fn assign_ports(config: &SidecarConfig) -> Vec<u16> {
    let replicas = config.replicas.max(1);
    (0..replicas)
        .filter_map(|offset| config.port.checked_add(offset))
        .collect()
}

#[derive(Debug)]
pub enum SidecarConfigError {
    /// manifest にない `name` を指定した。
    UnknownSidecar(String),
    /// `scalable = false` のサイドカーに `replicas > 1` を指定した。
    NotScalable(String),
    /// `args` に `{port}` が無いまま `replicas > 1` を指定した。
    MissingPortPlaceholder(String),
    /// ポート採番が 65535 を超える。
    PortOverflow(String),
    /// 同一プラグイン内で他のサイドカーとポート範囲が重なる。
    PortRangeOverlap(String),
    Io(std::io::Error),
    Serialize(serde_json::Error),
}

impl fmt::Display for SidecarConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SidecarConfigError::UnknownSidecar(name) => write!(f, "unknown sidecar: {name}"),
            SidecarConfigError::NotScalable(name) => {
                write!(f, "sidecar {name} does not allow replicas > 1")
            }
            SidecarConfigError::MissingPortPlaceholder(name) => write!(
                f,
                "sidecar {name} needs {{port}} in args to run more than one replica"
            ),
            SidecarConfigError::PortOverflow(name) => {
                write!(f, "sidecar {name} port range exceeds 65535")
            }
            SidecarConfigError::PortRangeOverlap(name) => {
                write!(f, "sidecar {name} port range overlaps another sidecar")
            }
            SidecarConfigError::Io(e) => write!(f, "failed to write sidecar config: {e}"),
            SidecarConfigError::Serialize(e) => {
                write!(f, "failed to serialize sidecar config: {e}")
            }
        }
    }
}

impl std::error::Error for SidecarConfigError {}

/// `<settings-dir>/<plugin-id>.sidecars.json` を読み書きするストア。
/// `SettingsStore` と同じく内部 `Mutex<()>` で read-merge-write を直列化する。
pub struct SidecarConfigStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl SidecarConfigStore {
    pub fn new(dir: PathBuf) -> Self {
        SidecarConfigStore {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn path_for(&self, manifest: &Manifest) -> PathBuf {
        self.dir.join(format!("{}.sidecars.json", manifest.id))
    }

    /// manifest の既定値に保存済みの値をマージした設定一覧を返す。
    /// ファイルが無い・壊れている場合は既定値のみ(panic しない)。
    pub fn effective(&self, manifest: &Manifest) -> BTreeMap<String, SidecarConfig> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.effective_locked(manifest)
    }

    fn effective_locked(&self, manifest: &Manifest) -> BTreeMap<String, SidecarConfig> {
        let saved: BTreeMap<String, SidecarConfig> = fs::read_to_string(self.path_for(manifest))
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default();

        manifest
            .sidecars
            .iter()
            .map(|request| {
                let config = saved
                    .get(&request.name)
                    .cloned()
                    .unwrap_or_else(|| SidecarConfig::from_request(request));
                (request.name.clone(), config)
            })
            .collect()
    }

    /// 1 サイドカーの設定を検証して保存し、更新後の全設定を返す。
    /// 検証に失敗した場合は何も書き込まない。
    pub fn update_and_effective(
        &self,
        manifest: &Manifest,
        name: &str,
        config: &SidecarConfig,
    ) -> Result<BTreeMap<String, SidecarConfig>, SidecarConfigError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let request = manifest
            .sidecar(name)
            .ok_or_else(|| SidecarConfigError::UnknownSidecar(name.to_string()))?;

        let mut merged = self.effective_locked(manifest);
        validate(name, request, config)?;
        merged.insert(name.to_string(), config.clone());
        validate_no_overlap(&merged)?;

        fs::create_dir_all(&self.dir).map_err(SidecarConfigError::Io)?;
        let serialized =
            serde_json::to_string_pretty(&merged).map_err(SidecarConfigError::Serialize)?;
        let target = self.path_for(manifest);
        let tmp_path = self.dir.join(format!(
            "{}.sidecars.json.tmp.{}",
            manifest.id,
            std::process::id()
        ));
        fs::write(&tmp_path, serialized).map_err(SidecarConfigError::Io)?;
        fs::rename(&tmp_path, &target).map_err(SidecarConfigError::Io)?;

        Ok(merged)
    }
}

fn validate(
    name: &str,
    request: &SidecarRequest,
    config: &SidecarConfig,
) -> Result<(), SidecarConfigError> {
    if config.replicas > 1 {
        if !request.scalable {
            return Err(SidecarConfigError::NotScalable(name.to_string()));
        }
        if !config.args.iter().any(|arg| arg.contains("{port}")) {
            return Err(SidecarConfigError::MissingPortPlaceholder(name.to_string()));
        }
    }

    let replicas = config.replicas.max(1);
    if config
        .port
        .checked_add(replicas - 1)
        .is_none()
    {
        return Err(SidecarConfigError::PortOverflow(name.to_string()));
    }

    Ok(())
}

/// 同一プラグイン内でポート範囲が重ならないことを確認する。
fn validate_no_overlap(
    configs: &BTreeMap<String, SidecarConfig>,
) -> Result<(), SidecarConfigError> {
    let mut used: BTreeMap<u16, String> = BTreeMap::new();
    for (name, config) in configs {
        for port in assign_ports(config) {
            if let Some(other) = used.insert(port, name.clone()) {
                if &other != name {
                    return Err(SidecarConfigError::PortRangeOverlap(name.clone()));
                }
            }
        }
    }
    Ok(())
}
```

`core/src/plugin/mod.rs`:

```rust
pub mod sidecar;
pub use sidecar::{assign_ports, SidecarConfig, SidecarConfigError, SidecarConfigStore};
```

- [ ] **Step 4: テストを実行する**

Run: `cargo test -p edlr-core sidecar`
Expected: PASS(6 テスト)

- [ ] **Step 5: コミット**

```bash
git add core/src/plugin/sidecar.rs core/src/plugin/mod.rs
git commit -m "feat(plugin): add sidecar config store with port assignment"
```

---

### Task 4: サイドカー単位の grant

**Files:**
- Modify: `core/src/plugin/grants.rs`
- Test: `core/src/plugin/grants.rs`(既存 `mod tests` に追記)

**Interfaces:**
- Consumes: `Manifest::sidecar_fingerprint`(Task 2)
- Produces:
  - `GrantsStore::sidecar_state(&self, manifest: &Manifest, name: &str) -> GrantState`
  - `GrantsStore::set_sidecar(&self, manifest: &Manifest, name: &str, granted: bool) -> Result<GrantState, GrantsError>`
  - ディスク形式: 既存の `{granted, fingerprint}` に `sidecars: {"<name>": {"granted": bool, "fingerprint": "<hex>"}}` を追加(既存ファイルは `sidecars` 欠落として読める)

- [ ] **Step 1: 失敗するテストを書く**

```rust
    fn manifest_with_sidecar(port: u16) -> Manifest {
        Manifest {
            id: "sc-plugin".into(),
            name: "SC".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![crate::plugin::manifest::SidecarRequest {
                name: "tts".into(),
                reason: "reason".into(),
                args: vec!["--port".into(), "{port}".into()],
                port,
                scalable: true,
            }],
        }
    }

    #[test]
    fn sidecar_grant_defaults_to_ungranted_and_persists_per_name() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_sidecar(50021);

        assert_eq!(
            store.sidecar_state(&manifest, "tts"),
            GrantState { granted: false, stale: false }
        );

        let state = store
            .set_sidecar(&manifest, "tts", true)
            .expect("grant should succeed");
        assert_eq!(state, GrantState { granted: true, stale: false });
        assert_eq!(
            store.sidecar_state(&manifest, "tts"),
            GrantState { granted: true, stale: false }
        );
    }

    #[test]
    fn changed_sidecar_request_makes_the_grant_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_sidecar(50021);
        store.set_sidecar(&manifest, "tts", true).unwrap();

        let changed = manifest_with_sidecar(50099);
        assert_eq!(
            store.sidecar_state(&changed, "tts"),
            GrantState { granted: false, stale: true }
        );
    }

    #[test]
    fn sidecar_grant_and_http_grant_are_independent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let mut manifest = manifest_with_sidecar(50021);
        manifest.capabilities = vec![CapabilityRequest::Http {
            hosts: vec!["https://api.example.com".into()],
            reason: "fetch".into(),
        }];

        store.set_sidecar(&manifest, "tts", true).unwrap();
        assert!(store.sidecar_state(&manifest, "tts").granted);
        assert!(
            !store.state(&manifest).granted,
            "granting a sidecar must not grant the http capability"
        );

        store.set(&manifest, true).unwrap();
        assert!(store.state(&manifest).granted);
        assert!(
            store.sidecar_state(&manifest, "tts").granted,
            "granting http must not clobber the sidecar grant"
        );
    }

    #[test]
    fn unknown_sidecar_name_is_never_granted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GrantsStore::new(tmp.path().join("grants"));
        let manifest = manifest_with_sidecar(50021);

        assert_eq!(
            store.sidecar_state(&manifest, "nope"),
            GrantState { granted: false, stale: false }
        );
        let state = store
            .set_sidecar(&manifest, "nope", true)
            .expect("set on an unknown sidecar is a no-op, not an error");
        assert_eq!(state, GrantState { granted: false, stale: false });
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core grants`
Expected: FAIL(`sidecar_state` / `set_sidecar` が未定義)

- [ ] **Step 3: 実装する**

`SavedGrant` を拡張(既存ファイルとの互換のため `#[serde(default)]`):

```rust
#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct SavedGrant {
    #[serde(default)]
    granted: bool,
    #[serde(default)]
    fingerprint: String,
    /// サイドカー名 → その 1 件の承認状態。既存(サイドカー導入前)の
    /// grant ファイルにはこのキーが無いため `default` で空マップになる。
    #[serde(default)]
    sidecars: std::collections::BTreeMap<String, SavedSidecarGrant>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct SavedSidecarGrant {
    granted: bool,
    fingerprint: String,
}
```

`GrantsStore::set` は現在 `SavedGrant` を新規に組み立てて上書きしているので、**既存の `sidecars` を読み出してから書き戻す**ように直す(サイドカー承認を消さないため)。同様に `set_sidecar` は `granted`/`fingerprint` を保持する。共通の書き込みヘルパを足す:

```rust
    /// `SavedGrant` 全体を原子的に書き込む(呼び出し元が `self.lock` 保持済み)。
    fn write_saved(&self, manifest: &Manifest, saved: &SavedGrant) -> Result<(), GrantsError> {
        fs::create_dir_all(&self.dir).map_err(GrantsError::Io)?;
        let serialized = serde_json::to_string_pretty(saved).map_err(GrantsError::Serialize)?;
        let target = self.path_for(manifest);
        let tmp_path = self
            .dir
            .join(format!("{}.json.tmp.{}", manifest.id, std::process::id()));
        fs::write(&tmp_path, serialized).map_err(GrantsError::Io)?;
        fs::rename(&tmp_path, &target).map_err(GrantsError::Io)
    }

    /// サイドカー 1 件の承認状態。判定規則は `state()` と同じ
    /// (未保存 → 未承認 / fingerprint 不一致 → stale / 一致 → 保存値)。
    pub fn sidecar_state(&self, manifest: &Manifest, name: &str) -> GrantState {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.sidecar_state_locked(manifest, name)
    }

    fn sidecar_state_locked(&self, manifest: &Manifest, name: &str) -> GrantState {
        let Some(current) = manifest.sidecar_fingerprint(name) else {
            return GrantState { granted: false, stale: false };
        };
        let Some(saved) = self.read_saved(manifest) else {
            return GrantState { granted: false, stale: false };
        };
        let Some(entry) = saved.sidecars.get(name) else {
            return GrantState { granted: false, stale: false };
        };
        if entry.fingerprint != current {
            return GrantState { granted: false, stale: true };
        }
        GrantState { granted: entry.granted, stale: false }
    }

    /// サイドカー 1 件の承認/取消を保存する。manifest にない `name` は no-op。
    pub fn set_sidecar(
        &self,
        manifest: &Manifest,
        name: &str,
        granted: bool,
    ) -> Result<GrantState, GrantsError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let Some(current) = manifest.sidecar_fingerprint(name) else {
            return Ok(GrantState { granted: false, stale: false });
        };

        let mut saved = self.read_saved(manifest).unwrap_or_default();
        saved.sidecars.insert(
            name.to_string(),
            SavedSidecarGrant { granted, fingerprint: current },
        );
        self.write_saved(manifest, &saved)?;

        Ok(self.sidecar_state_locked(manifest, name))
    }
```

`set` 側は次のように書き換える(`sidecars` を保持):

```rust
        let mut saved = self.read_saved(manifest).unwrap_or_default();
        saved.granted = granted;
        saved.fingerprint = current_fingerprint;
        self.write_saved(manifest, &saved)?;
```

- [ ] **Step 4: テストを実行する**

Run: `cargo test -p edlr-core grants`
Expected: PASS(既存 9 テスト + 新規 4 テスト)

- [ ] **Step 5: コミット**

```bash
git add core/src/plugin/grants.rs
git commit -m "feat(plugin): add per-sidecar capability grants"
```

---

### Task 5: WIT `driver-process` とホスト実装

**Files:**
- Modify: `core/wit/plugin.wit`, `core/src/plugin/host.rs`, `core/Cargo.toml`
- Create: `core/src/plugin/sidecar_runtime.rs`
- Modify: `core/src/plugin/mod.rs`
- Test: `core/src/plugin/host.rs`(`mod tests`)、`core/src/plugin/sidecar_runtime.rs`(`mod tests`)

**Interfaces:**
- Consumes: `edlr_driver_process::{ProcessDriver, ProcessSpec, ProcessError, InstanceStatus}`(Task 1)、`SidecarConfig` / `assign_ports`(Task 3)
- Produces:
  - `sidecar_runtime::sidecars_json_string(entries: &[SidecarRuntimeEntry]) -> String`
  - `pub struct SidecarRuntimeEntry { pub name: String, pub granted: bool, pub command: String, pub args: Vec<String>, pub ports: Vec<u16> }`
  - `sidecar_runtime::parse_sidecars(raw: &str) -> BTreeMap<String, SidecarRuntimeEntry>`
  - `sidecar_runtime::implicit_http_hosts(entries: &[SidecarRuntimeEntry]) -> Vec<String>`(承認済みサイドカーの `http://127.0.0.1:<port>` 一覧)
  - `host::capabilities_json_string(hosts: &[String]) -> String`(**シグネチャ変更**: `granted` 引数を廃止し、実効許可ホストのみを受け取る)
  - `HostCtx::new(plugin_id, settings_json, capabilities_json, sidecars_json, http_driver, process_driver)`

- [ ] **Step 1: WIT を追加する**

`core/wit/plugin.wit` の `driver-http` の下に:

```wit
interface driver-process {
  enum instance-state { running, exited }

  record instance {
    index: u32,
    port: u16,
    state: instance-state,
    exit-code: option<s32>,
  }

  variant driver-error {
    permission-denied(string),
    not-configured(string),
    unknown-sidecar(string),
    rate-limited(string),
    spawn-failed(string),
  }

  ensure-started: func(name: string) -> result<list<instance>, driver-error>;
  stop: func(name: string) -> result<_, driver-error>;
  status: func(name: string) -> result<list<instance>, driver-error>;
}
```

`world plugin` に `import driver-process;` を追加する。`core/Cargo.toml` の依存に `edlr-driver-process = { path = "../drivers/process" }` を追加する。

- [ ] **Step 2: `sidecar_runtime` の失敗するテストを書く**

`core/src/plugin/sidecar_runtime.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, granted: bool, ports: Vec<u16>) -> SidecarRuntimeEntry {
        SidecarRuntimeEntry {
            name: name.into(),
            granted,
            command: "/usr/bin/piper".into(),
            args: vec!["--port".into(), "{port}".into()],
            ports,
        }
    }

    #[test]
    fn ungranted_entries_carry_no_command_or_ports() {
        let json = sidecars_json_string(&[entry("tts", false, vec![50021])]);
        let parsed = parse_sidecars(&json);
        let tts = parsed.get("tts").expect("tts entry survives serialization");
        assert!(!tts.granted);
        assert_eq!(tts.command, "");
        assert!(tts.ports.is_empty());
    }

    #[test]
    fn granted_entries_round_trip() {
        let json = sidecars_json_string(&[entry("tts", true, vec![50021, 50022])]);
        let parsed = parse_sidecars(&json);
        let tts = parsed.get("tts").unwrap();
        assert!(tts.granted);
        assert_eq!(tts.command, "/usr/bin/piper");
        assert_eq!(tts.ports, vec![50021, 50022]);
    }

    #[test]
    fn implicit_hosts_cover_granted_ports_only() {
        let hosts = implicit_http_hosts(&[
            entry("tts", true, vec![50021, 50022]),
            entry("tr", false, vec![50030]),
        ]);
        assert_eq!(
            hosts,
            vec![
                "http://127.0.0.1:50021".to_string(),
                "http://127.0.0.1:50022".to_string(),
            ]
        );
    }

    #[test]
    fn broken_json_parses_as_no_sidecars() {
        assert!(parse_sidecars("not json {{{").is_empty());
    }
}
```

- [ ] **Step 3: テストが失敗することを確認する**

Run: `cargo test -p edlr-core sidecar_runtime`
Expected: FAIL(未実装)

- [ ] **Step 4: `sidecar_runtime` を実装する**

```rust
//! `HostCtx` と `Registry` が共有する `sidecars_json` バッファの組み立てと解釈。
//!
//! `capabilities_json` と同じ流儀で、承認状態と実行に必要な値を 1 本の JSON
//! 文字列に載せる。プラグイン側からは参照も改変もできず、`Registry` が
//! 承認・設定変更のたびに上書きすることで、稼働中のプラグインへ再起動なしに
//! 反映される。
//!
//! **未承認のエントリには `command` も `ports` も載せない**。承認前は
//! 起動に必要な情報そのものがバッファに存在しないため、仮に将来 `granted`
//! を見ずに読む実装が生えても、未承認のサイドカーを起動できない。

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SidecarRuntimeEntry {
    pub name: String,
    pub granted: bool,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub ports: Vec<u16>,
}

pub fn sidecars_json_string(entries: &[SidecarRuntimeEntry]) -> String {
    let redacted: Vec<SidecarRuntimeEntry> = entries
        .iter()
        .map(|entry| {
            if entry.granted {
                entry.clone()
            } else {
                SidecarRuntimeEntry {
                    name: entry.name.clone(),
                    granted: false,
                    command: String::new(),
                    args: Vec::new(),
                    ports: Vec::new(),
                }
            }
        })
        .collect();
    serde_json::to_string(&redacted).unwrap_or_else(|_| "[]".to_string())
}

pub fn parse_sidecars(raw: &str) -> BTreeMap<String, SidecarRuntimeEntry> {
    let entries: Vec<SidecarRuntimeEntry> = serde_json::from_str(raw).unwrap_or_default();
    entries
        .into_iter()
        .map(|entry| (entry.name.clone(), entry))
        .collect()
}

/// 承認済みサイドカーの採番ポートに対する暗黙の HTTP 許可 origin 一覧。
pub fn implicit_http_hosts(entries: &[SidecarRuntimeEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| entry.granted)
        .flat_map(|entry| {
            entry
                .ports
                .iter()
                .map(|port| format!("http://127.0.0.1:{port}"))
        })
        .collect()
}
```

`core/src/plugin/mod.rs` に `pub mod sidecar_runtime;` を追加する。

- [ ] **Step 5: テストを実行する**

Run: `cargo test -p edlr-core sidecar_runtime`
Expected: PASS(4 テスト)

- [ ] **Step 6: `host.rs` の失敗するテストを書く**

既存の `mod tests` に追記し、同時に `capabilities_json_string` のシグネチャ変更に合わせて既存 2 テスト(`capabilities_json_string_omits_hosts_when_ungranted` / `..._includes_hosts_when_granted`)を次で置き換える:

```rust
    #[test]
    fn capabilities_json_string_carries_the_effective_hosts() {
        let json = capabilities_json_string(&["https://api.example.com".to_string()]);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["hosts"], serde_json::json!(["https://api.example.com"]));
    }

    #[test]
    fn empty_effective_hosts_means_nothing_is_permitted() {
        let mut ctx = ctx(&capabilities_json_string(&[]));
        let err = ctx
            .send(request("https://api.example.com/ping"))
            .expect_err("no effective hosts means every call is denied");
        assert!(matches!(err, WitDriverError::PermissionDenied(_)));
    }
```

サイドカー用のテスト:

```rust
    fn sidecar_ctx(sidecars_json: &str) -> HostCtx {
        HostCtx::new(
            "test-plugin".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(capabilities_json_string(&[]))),
            Arc::new(Mutex::new(sidecars_json.to_string())),
            test_http_driver(),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                Duration::from_millis(200),
                Duration::from_secs(1),
            )),
        )
    }

    fn runtime_entry(granted: bool, command: &str) -> crate::plugin::sidecar_runtime::SidecarRuntimeEntry {
        crate::plugin::sidecar_runtime::SidecarRuntimeEntry {
            name: "tts".to_string(),
            granted,
            command: command.to_string(),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            ports: vec![50201],
        }
    }

    #[test]
    fn ensure_started_without_grant_is_permission_denied() {
        use crate::plugin::sidecar_runtime::sidecars_json_string;
        let mut ctx = sidecar_ctx(&sidecars_json_string(&[runtime_entry(false, "/bin/sh")]));

        let err = ctx
            .ensure_started("tts".to_string())
            .expect_err("ungranted sidecar must not start");
        assert!(matches!(err, WitProcessError::PermissionDenied(_)));
    }

    #[test]
    fn unknown_sidecar_name_is_reported_as_such() {
        use crate::plugin::sidecar_runtime::sidecars_json_string;
        let mut ctx = sidecar_ctx(&sidecars_json_string(&[runtime_entry(true, "/bin/sh")]));

        let err = ctx
            .ensure_started("nope".to_string())
            .expect_err("unknown sidecar must be rejected");
        assert!(matches!(err, WitProcessError::UnknownSidecar(_)));
    }

    #[test]
    fn granted_but_unconfigured_command_is_not_configured() {
        use crate::plugin::sidecar_runtime::sidecars_json_string;
        let mut ctx = sidecar_ctx(&sidecars_json_string(&[runtime_entry(true, "")]));

        let err = ctx
            .ensure_started("tts".to_string())
            .expect_err("an empty command must be reported as not-configured");
        assert!(matches!(err, WitProcessError::NotConfigured(_)));
    }

    #[test]
    fn granted_and_configured_sidecar_starts_and_stops() {
        use crate::plugin::sidecar_runtime::sidecars_json_string;
        let mut ctx = sidecar_ctx(&sidecars_json_string(&[runtime_entry(true, "/bin/sh")]));

        let instances = ctx.ensure_started("tts".to_string()).expect("start");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].port, 50201);
        assert!(matches!(instances[0].state, WitInstanceState::Running));

        ctx.stop("tts".to_string()).expect("stop");
        let after = ctx.status("tts".to_string()).expect("status");
        assert!(matches!(after[0].state, WitInstanceState::Exited));
    }
```

- [ ] **Step 7: テストが失敗することを確認する**

Run: `cargo test -p edlr-core host`
Expected: FAIL(`HostCtx::new` の引数不足、`ensure_started` 未実装)

- [ ] **Step 8: `host.rs` を実装する**

生成バインディングの取り込みと再エクスポート:

```rust
use bindings::edlr::plugin::driver_process::{
    DriverError as WitProcessError, Host as DriverProcessHost, Instance as WitInstance,
    InstanceState as WitInstanceState,
};

pub use bindings::edlr::plugin::driver_process::{
    DriverError as WitSidecarError, Host as WitDriverProcessHost, Instance as WitSidecarInstance,
    InstanceState as WitSidecarInstanceState,
};
```

定数:

```rust
/// サイドカー停止時、SIGTERM から SIGKILL へ昇格するまでの猶予。
pub const SIDECAR_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// 同一サイドカーの spawn 試行の最短間隔。プラグインがループで
/// `ensure-started` を呼んでも spawn 嵐にならないための下限。
pub const SIDECAR_SPAWN_MIN_INTERVAL: Duration = Duration::from_secs(1);
```

`capabilities_json_string` を「実効許可ホストだけを載せる」形に変更する:

```rust
/// `capabilities_json` の形(`{"hosts": [...]}`)。ここに載るのは
/// **実効的に許可されたホストだけ**である:
/// - `[[capabilities]]` の hosts は、その capability が承認済みのときだけ
/// - 承認済みサイドカーの `http://127.0.0.1:<port>` は常に(暗黙許可)
///
/// 呼び出し側(`Registry`)が承認状態を解決してからこの関数に渡すため、
/// `driver-http.send` は「空なら全部拒否、そうでなければ allowlist 判定」
/// だけを見ればよい。サイドカーの暗黙許可は http capability の承認とは
/// 独立に効く(サイドカーだけ承認したプラグインも自分のサイドカーとは
/// 通信できる)。
pub fn capabilities_json_string(hosts: &[String]) -> String {
    serde_json::to_string(&serde_json::json!({ "hosts": hosts }))
        .unwrap_or_else(|_| r#"{"hosts":[]}"#.to_string())
}
```

`parse_capabilities` は `hosts` のみを返すよう単純化し、`DriverHttpHost::send` の先頭を次に変える:

```rust
        let hosts = parse_capability_hosts(&raw);
        if hosts.is_empty() {
            return Err(WitDriverError::PermissionDenied(
                "capability not granted".to_string(),
            ));
        }
        check_url(&hosts, &req.url).map_err(WitDriverError::PermissionDenied)?;
```

`HostCtx` にフィールドを足す:

```rust
    /// サイドカーの承認状態と実行に必要な値の共有バッファ。形は
    /// `sidecar_runtime::sidecars_json_string` を参照。`capabilities_json`
    /// と同じく `Registry` が承認・設定変更のたびに上書きする。
    pub sidecars_json: Arc<Mutex<String>>,
    /// サイドカープロセスを実際に所有するドライバ。`http_driver` と同様、
    /// `PluginHost` が 1 つだけ持ち、全プラグインインスタンスで共有する。
    /// プロセスは `<plugin-id>/<sidecar-name>` をキーに分離されるため、
    /// 共有していても他プラグインのプロセスには触れられない。
    process_driver: Arc<edlr_driver_process::ProcessDriver>,
```

`HostCtx::new` の引数を `(plugin_id, settings_json, capabilities_json, sidecars_json, http_driver, process_driver)` に拡張する。

`DriverProcessHost` 実装:

```rust
impl HostCtx {
    /// `sidecars_json` から当該サイドカーの実行仕様を解決する。
    ///
    /// 判定順は「manifest に存在するか」→「承認済みか」→「設定済みか」。
    /// `driver-http.send` と同じく、判定材料は全て `HostCtx` 側にあり、
    /// ゲストが渡すのはサイドカー名だけ。
    fn resolve_sidecar(
        &self,
        name: &str,
    ) -> Result<edlr_driver_process::ProcessSpec, WitProcessError> {
        let raw = self
            .sidecars_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let entries = crate::plugin::sidecar_runtime::parse_sidecars(&raw);

        let Some(entry) = entries.get(name) else {
            return Err(WitProcessError::UnknownSidecar(format!(
                "no such sidecar: {name}"
            )));
        };
        if !entry.granted {
            return Err(WitProcessError::PermissionDenied(format!(
                "sidecar not granted: {name}"
            )));
        }
        if entry.command.is_empty() {
            return Err(WitProcessError::NotConfigured(format!(
                "sidecar {name} has no executable configured"
            )));
        }

        Ok(edlr_driver_process::ProcessSpec {
            command: std::path::PathBuf::from(&entry.command),
            args: entry.args.clone(),
            ports: entry.ports.clone(),
        })
    }

    fn sidecar_key(&self, name: &str) -> String {
        format!("{}/{name}", self.plugin_id)
    }
}

fn to_wit_instances(statuses: Vec<edlr_driver_process::InstanceStatus>) -> Vec<WitInstance> {
    statuses
        .into_iter()
        .map(|status| WitInstance {
            index: status.index,
            port: status.port,
            state: if status.running {
                WitInstanceState::Running
            } else {
                WitInstanceState::Exited
            },
            exit_code: status.exit_code,
        })
        .collect()
}

impl DriverProcessHost for HostCtx {
    fn ensure_started(&mut self, name: String) -> Result<Vec<WitInstance>, WitProcessError> {
        let spec = self.resolve_sidecar(&name)?;
        let key = self.sidecar_key(&name);
        self.process_driver
            .ensure_started(&key, &spec)
            .map(to_wit_instances)
            .map_err(|e| match e {
                edlr_driver_process::ProcessError::RateLimited(msg) => {
                    WitProcessError::RateLimited(msg)
                }
                edlr_driver_process::ProcessError::Spawn(msg) => WitProcessError::SpawnFailed(msg),
            })
    }

    fn stop(&mut self, name: String) -> Result<(), WitProcessError> {
        // 停止は「承認済みで設定済み」まで解決できなくても許したいが、
        // 未知の名前は誤りとして返す(承認取消後に自分で止める経路のため、
        // permission-denied では停止できないと困る)。
        let key = self.sidecar_key(&name);
        let raw = self
            .sidecars_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if !crate::plugin::sidecar_runtime::parse_sidecars(&raw).contains_key(&name) {
            return Err(WitProcessError::UnknownSidecar(format!(
                "no such sidecar: {name}"
            )));
        }
        self.process_driver.stop(&key);
        Ok(())
    }

    fn status(&mut self, name: String) -> Result<Vec<WitInstance>, WitProcessError> {
        let spec = self.resolve_sidecar(&name)?;
        let key = self.sidecar_key(&name);
        Ok(to_wit_instances(self.process_driver.status(&key, &spec)))
    }
}
```

`PluginHost` に `process_driver: Arc<ProcessDriver>` を持たせ、`new()` で 1 つ生成し、`pub fn process_driver(&self) -> Arc<ProcessDriver>` を足す。`PluginHost::drop` で `self.process_driver.stop_all()` を呼ぶ(明示 shutdown 経路は Task 6)。

- [ ] **Step 9: テストを実行する**

Run: `cargo test -p edlr-core`
Expected: PASS(`runner.rs` / `registry.rs` の呼び出し側は次タスクで直すため、ここではコンパイルを通すための最小修正 —— `HostCtx::new` 呼び出しに `sidecars_json` と `process_driver` を渡し、`capabilities_json_string` の呼び出しを新シグネチャに合わせる —— を含めてよい)

- [ ] **Step 10: コミット**

```bash
git add core/wit/plugin.wit core/Cargo.toml Cargo.lock core/src/plugin/host.rs core/src/plugin/sidecar_runtime.rs core/src/plugin/mod.rs core/src/plugin/runner.rs core/src/plugin/registry.rs
git commit -m "feat(plugin): add driver-process WIT interface and host implementation"
```

---

### Task 6: Registry / runner への配線と shutdown

**Files:**
- Modify: `core/src/plugin/registry.rs`, `core/src/plugin/runner.rs`, `core/src/bin/edlr.rs`
- Create: `core/tests/driver_process_integration.rs`

**Interfaces:**
- Consumes: Task 3〜5 の全て
- Produces:
  - `PluginInfo.sidecars: Vec<SidecarInfo>`
  - `pub struct SidecarInfo { pub request: SidecarRequest, pub config: SidecarConfig, pub grant: GrantState, pub instances: Vec<InstanceStatus> }`
  - `Registry::sidecars(&self, id: &str) -> Result<Vec<SidecarInfo>, RegistryError>`
  - `Registry::set_sidecar_config(&self, id: &str, name: &str, config: &SidecarConfig) -> Result<Vec<SidecarInfo>, RegistryError>`
  - `Registry::set_sidecar_grant(&self, id: &str, name: &str, granted: bool) -> Result<Vec<SidecarInfo>, RegistryError>`
  - `Registry::control_sidecar(&self, id: &str, name: &str, action: SidecarAction) -> Result<Vec<SidecarInfo>, RegistryError>`
  - `pub enum SidecarAction { Start, Stop, Restart }`
  - `Registry::stop_all_sidecars(&self)`
  - `RegistryError::{SidecarConfig(SidecarConfigError), UnknownSidecar(String), Sidecar(String)}`
  - `start_plugins(plugins_dir, settings_store, sidecar_config_store, grants_store, router, host) -> Registry`(**引数追加**)

- [ ] **Step 1: 失敗する統合テストを書く**

`core/tests/driver_process_integration.rs`:

```rust
//! `Registry` 経由でのサイドカー設定・承認・制御と、暗黙の HTTP 許可、
//! shutdown での確実な停止を、実 wasm を介さずに検証する。
//! (wasm 側の呼び出し経路は `core/src/plugin/host.rs` の単体テストが担当。)

use std::time::Duration;

use edlr_core::plugin::{
    GrantsStore, PluginHost, Registry, SettingsStore, SidecarConfig, SidecarConfigStore,
};

mod support;

#[test]
fn granting_a_sidecar_adds_its_ports_to_the_http_allowlist() {
    let env = support::sidecar_env("tts", 50301, true);

    // 未承認では暗黙許可も無い。
    assert!(!support::effective_hosts(&env.registry, "sc-plugin")
        .iter()
        .any(|h| h.contains("50301")));

    env.registry
        .set_sidecar_config(
            "sc-plugin",
            "tts",
            &SidecarConfig {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                port: 50301,
                replicas: 2,
            },
        )
        .expect("config should save");
    env.registry
        .set_sidecar_grant("sc-plugin", "tts", true)
        .expect("grant should save");

    let hosts = support::effective_hosts(&env.registry, "sc-plugin");
    assert!(hosts.contains(&"http://127.0.0.1:50301".to_string()));
    assert!(hosts.contains(&"http://127.0.0.1:50302".to_string()));

    // 取消で暗黙許可も消える。
    env.registry
        .set_sidecar_grant("sc-plugin", "tts", false)
        .expect("revoke should save");
    assert!(!support::effective_hosts(&env.registry, "sc-plugin")
        .iter()
        .any(|h| h.contains("50301")));
}

#[test]
fn revoking_a_grant_stops_running_instances() {
    let env = support::sidecar_env("tts", 50311, false);
    env.registry
        .set_sidecar_config(
            "sc-plugin",
            "tts",
            &SidecarConfig {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                port: 50311,
                replicas: 1,
            },
        )
        .unwrap();
    env.registry.set_sidecar_grant("sc-plugin", "tts", true).unwrap();

    let started = env
        .registry
        .control_sidecar("sc-plugin", "tts", edlr_core::plugin::SidecarAction::Start)
        .expect("start");
    assert!(started[0].instances[0].running);

    let after = env
        .registry
        .set_sidecar_grant("sc-plugin", "tts", false)
        .expect("revoke");
    assert!(
        !after[0].instances[0].running,
        "revoking a grant must stop the running sidecar"
    );
}

#[test]
fn changing_the_config_stops_the_running_sidecar() {
    let env = support::sidecar_env("tts", 50321, true);
    let config = SidecarConfig {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), "sleep 30".into()],
        port: 50321,
        replicas: 1,
    };
    env.registry
        .set_sidecar_config("sc-plugin", "tts", &config)
        .unwrap();
    env.registry.set_sidecar_grant("sc-plugin", "tts", true).unwrap();
    env.registry
        .control_sidecar("sc-plugin", "tts", edlr_core::plugin::SidecarAction::Start)
        .unwrap();

    let updated = env
        .registry
        .set_sidecar_config(
            "sc-plugin",
            "tts",
            &SidecarConfig { port: 50325, ..config },
        )
        .expect("config change");
    assert!(
        !updated[0].instances[0].running,
        "changing the config must stop the running sidecar"
    );
}

#[test]
fn stop_all_sidecars_leaves_nothing_running() {
    let env = support::sidecar_env("tts", 50331, false);
    env.registry
        .set_sidecar_config(
            "sc-plugin",
            "tts",
            &SidecarConfig {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                port: 50331,
                replicas: 1,
            },
        )
        .unwrap();
    env.registry.set_sidecar_grant("sc-plugin", "tts", true).unwrap();
    env.registry
        .control_sidecar("sc-plugin", "tts", edlr_core::plugin::SidecarAction::Start)
        .unwrap();

    env.registry.stop_all_sidecars();
    std::thread::sleep(Duration::from_millis(200));

    let sidecars = env.registry.sidecars("sc-plugin").unwrap();
    assert!(sidecars[0].instances.iter().all(|i| !i.running));
}
```

`core/tests/support/mod.rs` にヘルパを置く(既存テストにプラグイン一式を組み立てるヘルパがあればそちらに合わせる):

```rust
//! サイドカー統合テスト用の足場。`plugins-dir` に manifest だけを持つ
//! プラグインを 1 件作り、`Registry` を組み立てる。wasm のロードには
//! 失敗する(= プラグインは `Disabled` になる)が、サイドカーの設定・承認・
//! 制御は `Registry` の API で完結するため、このテストには十分。

use std::path::PathBuf;
use std::sync::Arc;

use edlr_core::plugin::{
    GrantsStore, PluginHost, Registry, SettingsStore, SidecarConfigStore,
};
use edlr_core::router::Router;

pub struct Env {
    pub registry: Registry,
    _tmp: tempfile::TempDir,
}

pub fn sidecar_env(name: &str, port: u16, scalable: bool) -> Env {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugin_dir = plugins_dir.join("sc-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "id = \"sc-plugin\"\nname = \"SC\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n\
             [[sidecar]]\nname = \"{name}\"\nreason = \"test sidecar\"\n\
             args = [\"-c\", \"sleep 30\"]\nport = {port}\nscalable = {scalable}\n"
        ),
    )
    .unwrap();

    let router = Router::new();
    let registry = edlr_core::plugin::start_plugins(
        &plugins_dir,
        SettingsStore::new(tmp.path().join("settings")),
        SidecarConfigStore::new(tmp.path().join("settings")),
        GrantsStore::new(tmp.path().join("grants")),
        &router,
        PluginHost::new().expect("plugin host"),
    );

    Env { registry, _tmp: tmp }
}

/// 当該プラグインの `capabilities_json` が現在載せている実効許可ホスト。
pub fn effective_hosts(registry: &Registry, id: &str) -> Vec<String> {
    registry.effective_hosts(id).unwrap_or_default()
}
```

`Registry::effective_hosts(&self, id: &str) -> Result<Vec<String>, RegistryError>`(共有バッファをパースして返すテスト用アクセサ)も実装対象に含める。

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core --test driver_process_integration`
Expected: FAIL(`set_sidecar_config` などが未定義)

- [ ] **Step 3: `Registry` に実装する**

要点(既存 `set_capabilities` と同じ流儀 — `entries` ロックは共有ハンドル取得の間だけ、永続化とバッファ更新は `capabilities_lock` の下で不可分に):

```rust
    /// サイドカーの設定変更・承認変更のあとに必ず呼ぶ内部ヘルパ。
    ///
    /// 1. 実行中サイドカーを停止する(設定が変わった/承認が消えた以上、
    ///    走り続けてよい根拠が無い。次の `ensure-started` で新しい設定・
    ///    承認のもとに起動し直される)
    /// 2. `sidecars_json` を作り直す
    /// 3. `capabilities_json` を「http 承認済みなら manifest hosts」＋
    ///    「承認済みサイドカーの暗黙 127.0.0.1 ポート」で作り直す
    ///
    /// 2 と 3 を必ず同じ臨界区間で更新するのが重要で、片方だけ更新されると
    /// 「起動はできるが通信できない」「通信できるが承認は消えている」という
    /// 中途半端な状態が観測されうる。
    fn refresh_sidecar_runtime(&self, id: &str, stop_names: &[String]) -> Result<Vec<SidecarInfo>, RegistryError>
```

各公開メソッドは次の順で動く:

- `set_sidecar_config` — `SidecarConfigStore::update_and_effective`(検証込み)→ 当該サイドカーを停止 → `refresh_sidecar_runtime` → `sidecars()` を返す
- `set_sidecar_grant` — `GrantsStore::set_sidecar` → 承認が `false` になったなら停止 → `refresh_sidecar_runtime`
- `control_sidecar` — `Stop` は停止、`Start` は `ProcessDriver::ensure_started`、`Restart` は停止してから `ensure_started`。未承認・`command` 未設定は `RegistryError::Sidecar` で返す
- `sidecars` — manifest の `[[sidecar]]` 順に `SidecarInfo` を組み立てる。`instances` は `ProcessDriver::status`
- `stop_all_sidecars` — `ProcessDriver::stop_all`

`Registry` は `sidecar_config_store: Arc<SidecarConfigStore>` と `process_driver: Arc<ProcessDriver>` を新たに保持する。`PluginEntry` に `sidecars_json: Arc<Mutex<String>>` を追加する。

`PluginInfo` に `sidecars: Vec<SidecarInfo>` を足し、`list()` で埋める。

- [ ] **Step 4: `runner.rs` を配線する**

`start_plugins` に `sidecar_config_store: SidecarConfigStore` 引数を足し、`load_and_run_plugin` で:

```rust
    let sidecar_configs = sidecar_config_store.effective(manifest);
    let entries: Vec<SidecarRuntimeEntry> = manifest
        .sidecars
        .iter()
        .map(|request| {
            let config = sidecar_configs
                .get(&request.name)
                .cloned()
                .unwrap_or_else(|| SidecarConfig::from_request(request));
            let granted = grants_store.sidecar_state(manifest, &request.name).granted;
            SidecarRuntimeEntry {
                name: request.name.clone(),
                granted,
                command: config.command.clone(),
                args: config.args.clone(),
                ports: assign_ports(&config),
            }
        })
        .collect();
    let sidecars_json = Arc::new(Mutex::new(sidecars_json_string(&entries)));

    // http capability の承認済み hosts と、承認済みサイドカーの暗黙許可を合流させる。
    let mut hosts = if grant_state.granted { manifest.capability_hosts() } else { Vec::new() };
    hosts.extend(implicit_http_hosts(&entries));
    let capabilities_json = Arc::new(Mutex::new(capabilities_json_string(&hosts)));
```

**起動時にサイドカーを自動 spawn しないこと**(設計どおり、起動はプラグインの `ensure-started` かユーザー操作のみ)。

- [ ] **Step 5: `core/src/bin/edlr.rs` を配線する**

`SidecarConfigStore::new(settings_dir.clone())` を作って `start_plugins` に渡す。デーモン終了経路(既存の shutdown シグナル待ち箇所)で、`registry` が `Some` なら `registry.stop_all_sidecars()` を呼んでから抜ける。

- [ ] **Step 6: テストを実行する**

Run: `cargo test -p edlr-core`
Expected: PASS(統合テスト 4 本を含む)

- [ ] **Step 7: コミット**

```bash
git add core/src/plugin/registry.rs core/src/plugin/runner.rs core/src/bin/edlr.rs core/tests/driver_process_integration.rs core/tests/support
git commit -m "feat(plugin): wire sidecar config, grants, and shutdown into the registry"
```

---

### Task 7: RPC メソッド

**Files:**
- Modify: `core/src/server.rs`
- Test: `core/src/server.rs`(`mod tests`)、`core/tests/ws_rpc_integration.rs`

**Interfaces:**
- Consumes: `Registry::{sidecars, set_sidecar_config, set_sidecar_grant, control_sidecar}`(Task 6)
- Produces: RPC メソッド `plugins/get-sidecars` / `plugins/set-sidecar-config` / `plugins/set-sidecar-grant` / `plugins/sidecar-control`、および `plugins/list` の各要素の `sidecars` フィールド

- [ ] **Step 1: 失敗するテストを書く**

`core/tests/ws_rpc_integration.rs` に(既存テストのヘルパ — WS 接続と `call(method, params)` — を再利用する):

```rust
#[tokio::test]
async fn sidecar_rpc_round_trip() {
    // 既存テストと同じ流儀で、[[sidecar]] を持つプラグインを 1 件置いた
    // plugins-dir でサーバを起動する。
    let env = spawn_server_with_sidecar_plugin().await;

    let sidecars = env.call("plugins/get-sidecars", serde_json::json!({"plugin": "sc-plugin"})).await
        .expect("get-sidecars should succeed");
    let list = sidecars["sidecars"].as_array().expect("sidecars array");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "tts");
    assert_eq!(list[0]["granted"], serde_json::json!(false));
    assert_eq!(list[0]["config"]["command"], serde_json::json!(""));

    let updated = env.call(
        "plugins/set-sidecar-config",
        serde_json::json!({
            "plugin": "sc-plugin",
            "name": "tts",
            "config": {"command": "/bin/sh", "args": ["-c", "sleep 30"], "port": 50401, "replicas": 1}
        }),
    ).await.expect("set-sidecar-config should succeed");
    assert_eq!(updated["sidecars"][0]["config"]["command"], serde_json::json!("/bin/sh"));

    let granted = env.call(
        "plugins/set-sidecar-grant",
        serde_json::json!({"plugin": "sc-plugin", "name": "tts", "granted": true}),
    ).await.expect("set-sidecar-grant should succeed");
    assert_eq!(granted["sidecars"][0]["granted"], serde_json::json!(true));

    let started = env.call(
        "plugins/sidecar-control",
        serde_json::json!({"plugin": "sc-plugin", "name": "tts", "action": "start"}),
    ).await.expect("start should succeed");
    assert_eq!(started["sidecars"][0]["instances"][0]["state"], serde_json::json!("running"));

    let stopped = env.call(
        "plugins/sidecar-control",
        serde_json::json!({"plugin": "sc-plugin", "name": "tts", "action": "stop"}),
    ).await.expect("stop should succeed");
    assert_eq!(stopped["sidecars"][0]["instances"][0]["state"], serde_json::json!("exited"));
}

#[tokio::test]
async fn invalid_sidecar_config_is_an_rpc_error_and_changes_nothing() {
    let env = spawn_server_with_sidecar_plugin().await;

    let err = env.call(
        "plugins/set-sidecar-config",
        serde_json::json!({
            "plugin": "sc-plugin",
            "name": "tts",
            // scalable = false のサイドカーに replicas > 1
            "config": {"command": "/bin/sh", "args": ["-c", "sleep 30"], "port": 50411, "replicas": 4}
        }),
    ).await.expect_err("invalid config must be rejected");
    assert!(err.contains("replicas"));

    let sidecars = env.call("plugins/get-sidecars", serde_json::json!({"plugin": "sc-plugin"})).await.unwrap();
    assert_eq!(sidecars["sidecars"][0]["config"]["command"], serde_json::json!(""));
}

#[tokio::test]
async fn unknown_sidecar_name_is_an_rpc_error() {
    let env = spawn_server_with_sidecar_plugin().await;
    let err = env.call(
        "plugins/get-sidecars",
        serde_json::json!({"plugin": "nope"}),
    ).await.expect_err("unknown plugin must be rejected");
    assert!(err.contains("unknown plugin"));
}
```

`spawn_server_with_sidecar_plugin` は既存テストのサーバ起動ヘルパを、`[[sidecar]]`(`name = "tts"`, `port = 50401`, `scalable = false`)入りの manifest を置く形に拡張したもの。

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test -p edlr-core --test ws_rpc_integration`
Expected: FAIL(`unknown method: plugins/get-sidecars`)

- [ ] **Step 3: 実装する**

`handle_rpc` に 4 分岐を追加する:

```rust
        "plugins/get-sidecars" => {
            let plugin = param_str(params, "plugin")?;
            let sidecars = registry.sidecars(plugin).map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
        "plugins/set-sidecar-config" => {
            let plugin = param_str(params, "plugin")?;
            let name = param_str(params, "name")?;
            let config: crate::plugin::SidecarConfig = serde_json::from_value(
                params
                    .get("config")
                    .cloned()
                    .ok_or_else(|| "params.config must be an object".to_string())?,
            )
            .map_err(|e| format!("params.config is invalid: {e}"))?;
            let sidecars = registry
                .set_sidecar_config(plugin, name, &config)
                .map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
        "plugins/set-sidecar-grant" => {
            let plugin = param_str(params, "plugin")?;
            let name = param_str(params, "name")?;
            let granted = params
                .get("granted")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "params.granted must be a bool".to_string())?;
            let sidecars = registry
                .set_sidecar_grant(plugin, name, granted)
                .map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
        "plugins/sidecar-control" => {
            let plugin = param_str(params, "plugin")?;
            let name = param_str(params, "name")?;
            let action = match param_str(params, "action")? {
                "start" => crate::plugin::SidecarAction::Start,
                "stop" => crate::plugin::SidecarAction::Stop,
                "restart" => crate::plugin::SidecarAction::Restart,
                other => return Err(format!("unknown action: {other}")),
            };
            let sidecars = registry
                .control_sidecar(plugin, name, action)
                .map_err(|e| e.to_string())?;
            Ok(sidecars_result_json(&sidecars))
        }
```

共通ヘルパ:

```rust
fn param_str<'a>(params: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("params.{key} must be a string"))
}

/// `get-sidecars` / `set-sidecar-*` / `sidecar-control` の共通 result 形と、
/// `plugins/list` の各要素の `sidecars` フィールドに使う JSON。
fn sidecars_result_json(sidecars: &[crate::plugin::SidecarInfo]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = sidecars
        .iter()
        .map(|info| {
            serde_json::json!({
                "name": info.request.name,
                "reason": info.request.reason,
                "args": info.request.args,
                "port": info.request.port,
                "scalable": info.request.scalable,
                "granted": info.grant.granted,
                "staleGrant": info.grant.stale,
                "config": info.config,
                "instances": info.instances.iter().map(|instance| serde_json::json!({
                    "index": instance.index,
                    "port": instance.port,
                    "state": if instance.running { "running" } else { "exited" },
                    "exitCode": instance.exit_code,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({ "sidecars": items })
}
```

`plugins/list` の各要素に `"sidecars": sidecars_result_json(&info.sidecars)["sidecars"].clone()` を追加する。

- [ ] **Step 4: テストを実行する**

Run: `cargo test -p edlr-core`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add core/src/server.rs core/tests/ws_rpc_integration.rs
git commit -m "feat(server): add sidecar RPC methods"
```

---

### Task 8: UI(型・SidecarSection・Plugins 配線・実行ファイルピッカー・README)

**Files:**
- Create: `ui/frontend/src/components/SidecarSection.tsx`, `ui/frontend/src/components/SidecarSection.test.tsx`
- Modify: `ui/frontend/src/types/plugin.ts`, `ui/frontend/src/pages/Plugins.tsx`, `ui/frontend/src/pages/Plugins.test.tsx`, `ui/frontend/src/index.css`, `ui/src-tauri/src/main.rs`, `ui/frontend/src/lib/tauri.ts`, `README.md`

**Interfaces:**
- Consumes: Task 7 の RPC メソッド
- Produces: `SidecarSection` コンポーネント、`pick_executable` Tauri コマンド

- [ ] **Step 1: 型を足す**

`ui/frontend/src/types/plugin.ts`:

```ts
export interface SidecarConfig {
  command: string;
  args: string[];
  port: number;
  replicas: number;
}

export interface SidecarInstance {
  index: number;
  port: number;
  state: "running" | "exited";
  exitCode: number | null;
}

export interface Sidecar {
  name: string;
  reason: string;
  args: string[];
  port: number;
  scalable: boolean;
  granted: boolean;
  staleGrant: boolean;
  config: SidecarConfig;
  instances: SidecarInstance[];
}

export interface Sidecars {
  sidecars: Sidecar[];
}
```

`PluginInfo` に `sidecars: Sidecar[];` を追加する。

- [ ] **Step 2: 失敗するコンポーネントテストを書く**

`ui/frontend/src/components/SidecarSection.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import SidecarSection from "./SidecarSection";
import type { Sidecar } from "../types/plugin";

function sidecar(overrides: Partial<Sidecar> = {}): Sidecar {
  return {
    name: "tts",
    reason: "音声合成エンジンをローカルで動かすため",
    args: ["--port", "{port}"],
    port: 50021,
    scalable: true,
    granted: false,
    staleGrant: false,
    config: { command: "", args: ["--port", "{port}"], port: 50021, replicas: 1 },
    instances: [],
    ...overrides,
  };
}

const noop = async () => {};

describe("SidecarSection", () => {
  it("renders nothing when the plugin declares no sidecars", () => {
    const { container } = render(
      <SidecarSection sidecars={[]} onConfigChange={noop} onGrantChange={noop} onControl={noop} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("shows the reason and warns while ungranted", () => {
    render(
      <SidecarSection
        sidecars={[sidecar()]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.getByText(/音声合成エンジン/)).toBeInTheDocument();
    expect(screen.getByText(/未承認 — このプラグインはプロセスを起動できません/)).toBeInTheDocument();
  });

  it("disables the grant toggle until an executable path is set", () => {
    render(
      <SidecarSection
        sidecars={[sidecar()]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.getByRole("checkbox", { name: /このサイドカーを承認する/ })).toBeDisabled();
  });

  it("enables the grant toggle once a command is configured", async () => {
    const onGrantChange = vi.fn(async () => {});
    render(
      <SidecarSection
        sidecars={[sidecar({ config: { command: "/usr/bin/piper", args: [], port: 50021, replicas: 1 } })]}
        onConfigChange={noop}
        onGrantChange={onGrantChange}
        onControl={noop}
      />,
    );
    const toggle = screen.getByRole("checkbox", { name: /このサイドカーを承認する/ });
    expect(toggle).toBeEnabled();
    await userEvent.click(toggle);
    expect(onGrantChange).toHaveBeenCalledWith("tts", true);
  });

  it("warns that the sidecar runs outside the sandbox", () => {
    render(
      <SidecarSection
        sidecars={[sidecar()]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.getByText(/edlr のサンドボックスの外で動きます/)).toBeInTheDocument();
  });

  it("shows a stale-grant warning", () => {
    render(
      <SidecarSection
        sidecars={[sidecar({ staleGrant: true })]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.getByText(/要求が変わったため再承認が必要/)).toBeInTheDocument();
  });

  it("hides the replicas field for non-scalable sidecars", () => {
    render(
      <SidecarSection
        sidecars={[sidecar({ scalable: false })]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.queryByLabelText(/レプリカ数/)).not.toBeInTheDocument();
  });

  it("lists instances with port, state and exit code", () => {
    render(
      <SidecarSection
        sidecars={[
          sidecar({
            granted: true,
            instances: [
              { index: 0, port: 50021, state: "running", exitCode: null },
              { index: 1, port: 50022, state: "exited", exitCode: 1 },
            ],
          }),
        ]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.getByText(/50021/)).toBeInTheDocument();
    expect(screen.getByText(/終了コード 1/)).toBeInTheDocument();
  });

  it("sends start/stop/restart control actions", async () => {
    const onControl = vi.fn(async () => {});
    render(
      <SidecarSection
        sidecars={[sidecar({ granted: true, config: { command: "/usr/bin/piper", args: [], port: 50021, replicas: 1 } })]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={onControl}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "起動" }));
    expect(onControl).toHaveBeenCalledWith("tts", "start");
    await userEvent.click(screen.getByRole("button", { name: "停止" }));
    expect(onControl).toHaveBeenCalledWith("tts", "stop");
    await userEvent.click(screen.getByRole("button", { name: "再起動" }));
    expect(onControl).toHaveBeenCalledWith("tts", "restart");
  });

  it("surfaces an error from a rejected config save", async () => {
    const onConfigChange = vi.fn(async () => {
      throw new Error("sidecar tts does not allow replicas > 1");
    });
    render(
      <SidecarSection
        sidecars={[sidecar()]}
        onConfigChange={onConfigChange}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    await userEvent.type(screen.getByLabelText(/実行ファイル/), "/usr/bin/piper");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByText(/does not allow replicas/)).toBeInTheDocument();
  });
});
```

- [ ] **Step 3: テストが失敗することを確認する**

Run: `cd ui/frontend && mise exec -- pnpm test`
Expected: FAIL(`SidecarSection` が存在しない)

- [ ] **Step 4: `SidecarSection.tsx` を実装する**

要点(既存 `CapabilitySection.tsx` の流儀に合わせる):

- `sidecars.length === 0` なら `null` を返す
- サイドカーごとに `<fieldset>`: `reason` 表示 → 実行ファイルパス入力(`aria-label="実行ファイル"`、Tauri 環境なら「選択…」ボタンで `pick_executable`)→ 引数(スペース区切りではなく 1 行 1 引数の textarea にし、`\n` で分割)→ ポート → `scalable` なら「レプリカ数」→「保存」ボタン
- 承認チェックボックス(`aria-label="このサイドカーを承認する"`)は `config.command === ""` の間 `disabled`。`checked` は**サーバから返った `granted` のみ**で駆動する(`CapabilitySection` と同じ理由 — 楽観的更新をしない)
- 未承認なら「未承認 — このプラグインはプロセスを起動できません」、承認説明文として「承認するとこのプラグインはあなたが指定したプログラムを実行できます。そのプログラムは edlr のサンドボックスの外で動きます」を常時表示
- `staleGrant` なら「要求が変わったため再承認が必要です」
- インスタンス一覧: `#{index} :{port} 実行中 / 停止(終了コード N)`
- 起動 / 停止 / 再起動ボタン → `onControl(name, action)`
- 保存・承認・制御の各非同期呼び出しは try/catch し、`className="form-error"` にメッセージを出す

`ui/frontend/src/index.css` に `.sidecar-section` / `.sidecar-instances` などのスタイルを、既存 `.capability-*` に倣って足す。

- [ ] **Step 5: テストを実行する**

Run: `cd ui/frontend && mise exec -- pnpm test`
Expected: PASS(SidecarSection の 10 テスト)

- [ ] **Step 6: `Plugins.tsx` に配線し、テストを足す**

`Plugins.tsx`:

```tsx
  const handleSidecarConfig = (pluginId: string) => async (name: string, config: SidecarConfig) => {
    const client = clientRef.current;
    if (!client) throw new Error("RPC に接続されていません");
    const updated = await client.call<Sidecars>("plugins/set-sidecar-config", {
      plugin: pluginId,
      name,
      config,
    });
    if (!mountedRef.current) return;
    setPlugins((prev) =>
      prev.map((p) => (p.id === pluginId ? { ...p, sidecars: updated.sidecars } : p)),
    );
  };
```

`handleSidecarGrant`(`plugins/set-sidecar-grant`)と `handleSidecarControl`(`plugins/sidecar-control`)も同形で足し、`<SidecarSection sidecars={p.sidecars} ... />` を `CapabilitySection` の下に置く。

`Plugins.test.tsx` に、`plugins/list` のモック応答へ `sidecars` を含めたケースと、承認トグル操作が `plugins/set-sidecar-grant` を正しい params で呼ぶことを確認するテストを 2 本足す(既存のモック RPC ヘルパに合わせる)。

Run: `cd ui/frontend && mise exec -- pnpm test`
Expected: PASS

- [ ] **Step 7: `pick_executable` Tauri コマンドを足す**

`ui/src-tauri/src/main.rs`(`pick_journal_dir` の直後):

```rust
/// ネイティブのファイル選択ダイアログを開く(サイドカーの実行ファイル用)。
/// キャンセル時は None。
#[tauri::command]
async fn pick_executable(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_file(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten().map(|path| path.to_string())
}
```

`invoke_handler![...]` に `pick_executable` を追加する。

Run: `cd ui/src-tauri && cargo test`
Expected: PASS(既存テストが引き続き通る)

- [ ] **Step 8: README を更新する**

`README.md` の「capability(driver-http)」節の後ろに「サイドカープロセス(driver-process)」節を足す:

- `[[sidecar]]` の書式(`name` / `reason` / `args` / `port` / `scalable`)と、`command` はユーザーが UI で指定する旨
- `{port}` 展開と `replicas` によるポート採番(`port … port+replicas-1`)
- 承認するとそのポートへの `driver-http` アクセスが暗黙に許可されること
- 自動再起動はせず、`ensure-started` の最短間隔が 1 秒であること
- 停止はプロセスグループごと(SIGTERM → 3 秒 → SIGKILL)で、デーモン終了時に必ず止まること
- 設定の保存先が `<settings-dir>/<id>.sidecars.json`、承認が `<grants-dir>/<id>.json` であること

- [ ] **Step 9: 全テストを実行する**

```bash
cargo test
cd ui/frontend && mise exec -- pnpm test
cd ../src-tauri && cargo test
```
Expected: 全て PASS

- [ ] **Step 10: コミット**

```bash
git add ui README.md
git commit -m "feat(ui): add sidecar configuration and approval UI"
```

---

## 自己レビューメモ

- **設計書との差分は 1 点**: サイドカー設定の保存先を `<settings-dir>/<id>.json` の名前空間ではなく `<settings-dir>/<id>.sidecars.json` にした(理由は Global Constraints 参照)。実装時に設計書側も同じ内容へ直すこと
- **`capabilities_json` の形を変更**(`{"granted", "hosts"}` → `{"hosts"}`)。サイドカーの暗黙許可は http capability の承認とは独立に効く必要があるため。既存の `host.rs` の 2 テストは Task 5 Step 6 で置き換える
- 設計書の「テスト方針」の各項目は Task 1(Supervisor 単体・プロセスグループ kill)、Task 2(manifest・フィンガープリント)、Task 3(設定・ポート採番)、Task 4(grants)、Task 5(WIT ホスト実装・暗黙許可の組み立て)、Task 6(承認取消で停止・shutdown)、Task 7(RPC)、Task 8(UI)に対応する
- **実 wasm を使ったサイドカー統合テストは入れていない**。`driver-process` の判定は全て `HostCtx` 側にあり、wasm 経由でも同じ関数を通るため(既存 `driver_http_integration.rs` と同じ論拠)。実 wasm での経路確認が必要になったら、`examples/plugins/` にサイドカーを使うサンプルを足す別タスクとして扱う

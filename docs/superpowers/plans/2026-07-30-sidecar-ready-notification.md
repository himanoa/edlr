# sidecar-ready 通知 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ホストがドライバのサイドカーの port を監視し、初めて TCP 接続できた時点でドライバの `on-message` へ `from = "host", topic = "sidecar-ready"` の合成メッセージを届ける(仕様: `docs/superpowers/specs/2026-07-30-sidecar-ready-notification-design.md`)。

**Architecture:** `ProcessDriver`(drivers/process)が spawn ごとにバックグラウンド監視スレッドを立て、初回 TCP 接続時にコールバックを 1 回呼ぶ。core の `start_drivers` がそのコールバックを `Bus::notify_from_host`(新設。宣言不要トピックで `from = "host"` をドライバキューへ直接送る)に配線する。`from = "host"` のなりすましは plugin/driver 両 manifest の id 検証で塞ぐ。coeiroink ドライバ(別リポジトリ `edlr-himanoa-coeiroink`)は `sidecar-ready` を受けて speakers を再 publish する。

**Tech Stack:** Rust / std::net::TcpStream / std::sync::mpsc / wasmtime component host(既存構造は変更なし)

## Global Constraints

- サブエージェントを並列起動する前に必ず一度 `cargo fetch` を実行する(CLAUDE.md)。同一 worktree 内で cargo コマンドを並走させない。
- 予約 id は文字列 `"host"`。定数 `edlr_driver_channel::HOST_SENDER` として一元定義し、他の箇所はこれを参照する。
- 合成メッセージのトピック名は `"sidecar-ready"`、payload は `{"name": "<sidecar名>", "index": <n>, "port": <p>}`(JSON、UTF-8 バイト列)。
- 監視は約 200ms 間隔の TCP 接続ポーリング。タイムアウトなし。プロセス死亡が打ち切り条件。respawn ごとに監視を立て直す。
- 対象はドライバのサイドカーのみ(`DriverHost` の `ProcessDriver` にだけコールバックを設定する。`PluginHost` は別インスタンスを持つので触らない)。
- Task 5 は別リポジトリ `/mnt/game/caches/src/github.com/himanoa/edlr-himanoa-coeiroink` での作業。**このリポジトリには無関係の未コミット変更(`driver-core/src/route.rs`, `driver-core/src/settings.rs`, `driver-core/tests/driver_manifest.rs`, `driver/driver.toml`)が存在する。`git add -A` は絶対に使わず、自分が変更したファイルだけを明示的に add すること。**

---

### Task 1: `Bus::notify_from_host` と `HOST_SENDER` 定数(drivers/channel)

**Files:**
- Modify: `drivers/channel/src/lib.rs`

**Interfaces:**
- Produces: `pub const HOST_SENDER: &str = "host";`
- Produces: `pub fn Bus::notify_from_host(&self, driver_id: &str, topic: &str, payload: Vec<u8>) -> Result<(), BusError>`
  - `from = HOST_SENDER` の `Message` をドライバキューへ**ブロッキング送信**する(`SyncSender::send`)。監視スレッド専用の経路なので、キュー満杯時は待つ(ready 通知を取りこぼさないため。`publish` の try_send + QueueFull とは意図的に非対称)。
  - ホスト予約経路なので **トピック宣言チェックはしない**(`sidecar-ready` は manifest に宣言されない)。
  - 未知ドライバ → `UnknownDriver`、`available = false` → `DriverUnavailable`、受信側切断 → `DriverUnavailable`。
  - **バスのロックは sender の clone まで**。ブロッキング送信はロックを手放してから行う(ロック保持中に待つと全バス操作が巻き添えになる)。

- [ ] **Step 1: 失敗するテストを書く**

`drivers/channel/src/lib.rs` の `mod tests` に追加:

```rust
    #[test]
    fn notify_from_host_delivers_with_from_host_and_without_a_declared_topic() {
        let (bus, rx) = bus_with_driver(4);
        // "sidecar-ready" は topics() に宣言されていないが、ホスト予約経路は通る。
        bus.notify_from_host("ed-state", "sidecar-ready", b"{}".to_vec())
            .unwrap();
        let msg = rx.try_recv().expect("message queued");
        assert_eq!(msg.from, HOST_SENDER);
        assert_eq!(msg.topic, "sidecar-ready");
        assert_eq!(msg.payload, b"{}".to_vec());
    }

    #[test]
    fn notify_from_host_to_an_unknown_driver_is_rejected() {
        let bus = Bus::new();
        assert!(matches!(
            bus.notify_from_host("nope", "sidecar-ready", vec![]),
            Err(BusError::UnknownDriver(_))
        ));
    }

    #[test]
    fn notify_from_host_to_a_disabled_driver_is_rejected() {
        let (bus, _rx) = bus_with_driver(4);
        bus.disable_driver("ed-state");
        assert!(matches!(
            bus.notify_from_host("ed-state", "sidecar-ready", vec![]),
            Err(BusError::DriverUnavailable(_))
        ));
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-driver-channel notify_from_host`
Expected: コンパイルエラー(`notify_from_host` 未定義)

- [ ] **Step 3: 実装**

`BUS_MAX_PAYLOAD` の近くに定数を追加:

```rust
/// ホストが合成するメッセージの予約送信元 id。`sidecar-ready` などの
/// ホスト発通知はこの `from` で届く。なりすましを塞ぐため、core の
/// manifest 検証はこの id を持つプラグイン/ドライバを拒否する。
pub const HOST_SENDER: &str = "host";
```

`Bus` impl に追加(`publish` の直後あたり):

```rust
    /// ホスト発の合成メッセージ(`from = HOST_SENDER`)をドライバの
    /// キューへ届ける。監視スレッドなどホスト内部の専用スレッドから
    /// 呼ばれる前提で、`publish` と違い**ブロッキング送信**する
    /// (ready 通知は取りこぼすと空白期間が再発するため、キューが
    /// 満杯ならドライバが捌くまで待つ)。ホスト予約経路なので
    /// トピック宣言のチェックは行わない(`sidecar-ready` は manifest に
    /// 宣言されない)。送信そのものはバスのロックを手放してから行う。
    pub fn notify_from_host(
        &self,
        driver_id: &str,
        topic: &str,
        payload: Vec<u8>,
    ) -> Result<(), BusError> {
        let sender = {
            let state = self.lock_state();
            let slot = state
                .drivers
                .get(driver_id)
                .ok_or_else(|| BusError::UnknownDriver(driver_id.to_string()))?;
            if !slot.available {
                return Err(BusError::DriverUnavailable(driver_id.to_string()));
            }
            slot.sender.clone()
        };
        let message = Message {
            from: HOST_SENDER.to_string(),
            topic: topic.to_string(),
            payload,
        };
        sender
            .send(message)
            .map_err(|_| BusError::DriverUnavailable(driver_id.to_string()))
    }
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-driver-channel`
Expected: PASS(既存テスト含め全緑)

- [ ] **Step 5: Commit**

```bash
git add drivers/channel/src/lib.rs
git commit -m "feat(channel): ホスト発合成メッセージ用の Bus::notify_from_host を追加"
```

---

### Task 2: `ProcessDriver` の ready 監視(drivers/process)

**Files:**
- Modify: `drivers/process/src/lib.rs`

**Interfaces:**
- Produces: `pub struct ReadyEvent { pub key: String, pub index: u32, pub port: u16 }`
- Produces: `pub type ReadyCallback = std::sync::Arc<dyn Fn(ReadyEvent) + Send + Sync>;`
- Produces: `pub fn ProcessDriver::set_ready_callback(&self, callback: ReadyCallback)`
- 挙動: コールバック設定後に `ensure_started` が実際に spawn した各インスタンスについて監視スレッドを立てる。約 200ms 間隔で `127.0.0.1:<port>` へ TCP 接続を試み、初めて成功したら `ReadyEvent` を 1 回だけ呼んで終了。接続できる前にプロセスが死んだら(または stop で切り離されたら)何も呼ばず終了。respawn すれば新しい監視が立つ。コールバック未設定なら監視スレッドは一切立てない(プラグイン側の `ProcessDriver` は従来どおり)。

- [ ] **Step 1: 失敗するテストを書く**

`drivers/process/src/lib.rs` の `mod tests` に追加。まず共通ヘルパ:

```rust
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn ready_channel(
        driver: &ProcessDriver,
    ) -> std::sync::mpsc::Receiver<ReadyEvent> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<ReadyEvent>(8);
        driver.set_ready_callback(Arc::new(move |event| {
            let _ = tx.send(event);
        }));
        rx
    }
```

テスト 3 本:

```rust
    /// 「遅れて listen する」状況の再現: 子プロセス自体は sleep していて
    /// ポートを開かない。テスト側が後からそのポートで listen を開始する
    /// ことで、「起動直後は繋がらず、あとから繋がるようになる」を作る
    /// (監視は接続可能性しか見ないので、誰がポートを開いたかは問わない)。
    #[test]
    fn ready_is_notified_once_the_port_becomes_connectable() {
        let driver = driver();
        let port = free_port();
        let rx = ready_channel(&driver);

        driver
            .ensure_started("d/worker", &sleep_spec(vec![port]))
            .expect("start");

        // まだ誰も listen していないので通知は来ない。
        assert!(
            rx.recv_timeout(Duration::from_millis(600)).is_err(),
            "must not notify ready before the port accepts connections"
        );

        let _listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        let event = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready must be notified once the port opens");
        assert_eq!(event.key, "d/worker");
        assert_eq!(event.index, 0);
        assert_eq!(event.port, port);

        // 通知は 1 回だけ。
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "ready must be notified exactly once per spawn"
        );

        driver.stop("d/worker");
    }

    #[test]
    fn ready_is_not_notified_if_the_process_dies_before_the_port_opens() {
        let driver = driver();
        let port = free_port();
        let rx = ready_channel(&driver);
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "exit 0".to_string()],
            ports: vec![port],
        };

        driver.ensure_started("d/dead", &spec).expect("start");

        // 監視がプロセス死亡を観測するまで待ってから、あえてポートを開く。
        // 死亡後にポートが繋がるようになっても通知してはいけない。
        std::thread::sleep(Duration::from_millis(500));
        let _listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        assert!(
            rx.recv_timeout(Duration::from_secs(1)).is_err(),
            "a process that died before its port opened must not be reported ready"
        );
    }

    #[test]
    fn respawn_notifies_ready_again() {
        // spawn_min_interval = 0 の driver() を使う(即 respawn できる)。
        let driver = driver();
        let port = free_port();
        let rx = ready_channel(&driver);
        // 最初からポートが開いている状態にしておけば、spawn → 通知が
        // それぞれ即座に来る。
        let _listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();

        driver
            .ensure_started("d/re", &sleep_spec(vec![port]))
            .expect("first start");
        rx.recv_timeout(Duration::from_secs(2))
            .expect("first ready");

        driver.stop("d/re");
        driver
            .ensure_started("d/re", &sleep_spec(vec![port]))
            .expect("respawn");
        rx.recv_timeout(Duration::from_secs(2))
            .expect("respawn must be watched anew and notified again");

        driver.stop("d/re");
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-driver-process ready`
Expected: コンパイルエラー(`ReadyEvent` / `set_ready_callback` 未定義)

- [ ] **Step 3: 実装**

構造変更(いずれも `drivers/process/src/lib.rs`):

1. 型定義を `ProcessError` の近くに追加:

```rust
/// サイドカーの 1 インスタンスが「初めて TCP 接続できるようになった」ことを
/// 表すイベント。`key` は `ensure_started` に渡されたキー
/// (core 側では `<driver-id>/<sidecar-name>`)。
#[derive(Debug, Clone, PartialEq)]
pub struct ReadyEvent {
    pub key: String,
    pub index: u32,
    pub port: u16,
}

/// ready 通知の届け先。`set_ready_callback` で設定する。
pub type ReadyCallback = Arc<dyn Fn(ReadyEvent) + Send + Sync>;
```

2. `Instance` に世代を追加(respawn 後に古い監視スレッドが新インスタンスを
   誤認しないための識別子):

```rust
struct Instance {
    index: u32,
    port: u16,
    child: Option<Child>,
    exit_code: Option<i32>,
    terminating: bool,
    /// 何度目の spawn かを表す単調増加の識別子(0 = 一度も spawn していない)。
    /// 監視スレッドは自分の世代と一致するインスタンスだけを「生きている」と
    /// みなす。respawn で世代が進めば、古い監視スレッドは静かに終了する。
    generation: u64,
}
```

`new_instances` の生成箇所に `generation: 0,` を追加。

3. `ProcessDriver` のフィールド変更:
   - `groups: Mutex<HashMap<String, Group>>` を
     `groups: Arc<Mutex<HashMap<String, Group>>>` に変更
     (監視スレッドが `ProcessDriver` 本体より長生きしても安全に状態を見られる
     ようにするため。`lock()` ヘルパはそのまま使える)。
   - フィールド追加:

```rust
    /// spawn 世代の採番。`Instance::generation` を参照。
    next_generation: std::sync::atomic::AtomicU64,
    /// ready 監視の通知先。`None` の間は監視スレッドを一切立てない
    /// (プラグイン側の `ProcessDriver` は設定しないので従来どおり)。
    ready_callback: Mutex<Option<ReadyCallback>>,
```

   `ProcessDriver::new` の初期化に
   `groups: Arc::new(Mutex::new(HashMap::new()))`,
   `next_generation: std::sync::atomic::AtomicU64::new(1)`,
   `ready_callback: Mutex::new(None)` を追加。

4. setter を impl に追加:

```rust
    /// ready 監視の通知先を設定する。以後の `ensure_started` が spawn した
    /// インスタンスごとに監視スレッドが立つ。デーモン起動時に一度だけ
    /// 呼ばれる想定(core の `start_drivers`)。
    pub fn set_ready_callback(&self, callback: ReadyCallback) {
        *self
            .ready_callback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(callback);
    }
```

5. `ensure_started` の spawn ループを変更。spawn 成功時に世代を採番し、
   新規 spawn したインスタンスを記録する:

```rust
        let mut first_error: Option<String> = None;
        let mut spawned: Vec<(u32, u16, u64)> = Vec::new();
        for instance in group.instances.iter_mut() {
            if instance.child.is_some() || instance.terminating {
                continue;
            }
            match spawn_one(key, spec, instance.port) {
                Ok(child) => {
                    instance.child = Some(child);
                    instance.exit_code = None;
                    instance.generation = self
                        .next_generation
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    spawned.push((instance.index, instance.port, instance.generation));
                }
                Err(e) if first_error.is_none() => first_error = Some(e),
                Err(_) => {}
            }
        }

        // 一部の spawn が失敗して Err を返す場合でも、成功したインスタンスは
        // 生きて動き続けるので、それらの監視は立てる(エラー判定より先)。
        if !spawned.is_empty() {
            let callback = self
                .ready_callback
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if let Some(callback) = callback {
                for (index, port, generation) in spawned {
                    watch_ready(
                        Arc::clone(&self.groups),
                        key.to_string(),
                        index,
                        port,
                        generation,
                        Arc::clone(&callback),
                    );
                }
            }
        }

        if let Some(e) = first_error {
            return Err(ProcessError::Spawn(e));
        }
        Ok(snapshot(group))
```

6. 監視スレッド本体(モジュール直下の自由関数として追加):

```rust
/// spawn 直後のインスタンス 1 件の port を監視し、初めて TCP 接続できた
/// 時点で `callback` を 1 回呼んで終了する(設計書 sidecar-ready 参照)。
///
/// - 約 200ms 間隔のポーリング。タイムアウトは設けない(COEIROINK エンジン
///   のロードは分単位になりうる)。実質の打ち切り条件はプロセス死亡。
/// - 「死んだ」の判定は世代一致 + `child.is_some()`。`stop`/`stop_all` が
///   子を切り離した場合(`terminating`)も `child` が `None` になるので、
///   停止中のインスタンスに ready を報告することはない。
/// - `groups` を `Arc` で受けるので、`ProcessDriver` 本体が drop された後も
///   このスレッドは安全に状態を確認できる(次の確認で「いない」を見て終わる)。
fn watch_ready(
    groups: Arc<Mutex<HashMap<String, Group>>>,
    key: String,
    index: u32,
    port: u16,
    generation: u64,
    callback: ReadyCallback,
) {
    std::thread::spawn(move || loop {
        {
            let mut groups = groups.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let alive = groups
                .get_mut(&key)
                .map(|group| {
                    // 終了済みの子をここでも回収する。ensure_started/status が
                    // 呼ばれない限り reap されず、死んだ子が `child: Some` の
                    // まま残って監視が止まらないため。
                    reap(group);
                    group.instances.iter().any(|instance| {
                        instance.index == index
                            && instance.generation == generation
                            && instance.child.is_some()
                    })
                })
                .unwrap_or(false);
            if !alive {
                return;
            }
        }
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            callback(ReadyEvent { key, index, port });
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    });
}
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-driver-process`
Expected: PASS(既存テスト含め全緑。`groups` の `Arc` 化で既存コードが壊れて
いないこともここで確認される)

- [ ] **Step 5: Commit**

```bash
git add drivers/process/src/lib.rs
git commit -m "feat(process): spawn したインスタンスの port を監視して ready を通知する"
```

---

### Task 3: manifest 検証で id = "host" を予約(core)

**Files:**
- Modify: `core/src/plugin/manifest.rs`(`ManifestError` に variant 追加、`load_manifest` にチェック追加、テスト追加)
- Modify: `core/src/driver/manifest.rs`(`load_driver_manifest` にチェック追加、テスト追加)

**Interfaces:**
- Consumes: `edlr_driver_channel::HOST_SENDER`(Task 1)
- Produces: `ManifestError::ReservedId` variant。plugin/driver 両ローダが `id == "host"` を拒否する。

- [ ] **Step 1: 失敗するテストを書く**

`core/src/plugin/manifest.rs` のテストモジュールに追加(既存テストの組み立て
パターンに合わせる。プラグインの manifest は `manifest.toml`、ディレクトリ名 =
id が必須なので `host` ディレクトリを作る):

```rust
    #[test]
    fn rejects_the_reserved_plugin_id_host() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("host");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("plugin.wasm"), b"\0asm").unwrap();
        std::fs::write(
            sub.join("manifest.toml"),
            r#"
id = "host"
name = "Host Impersonator"
version = "0.1.0"
entry = "plugin.wasm"
"#,
        )
        .unwrap();
        let err = load_manifest(&sub)
            .expect_err("the id \"host\" is reserved for host-synthesized messages");
        assert!(matches!(err, ManifestError::ReservedId));
    }
```

(既存テストが `entry` のファイル名や書き込みヘルパを持っている場合はそれに
合わせて書き換えてよい。要点は「字種としては合法な `host` が弾かれ、エラーが
`ReservedId` であること」。)

`core/src/driver/manifest.rs` のテストモジュールに追加:

```rust
    #[test]
    fn rejects_the_reserved_driver_id_host() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("host");
        std::fs::create_dir(&sub).unwrap();
        write_entry(&sub);
        write(
            &sub,
            r#"
id = "host"
name = "Host Impersonator"
version = "0.1.0"
entry = "driver.wasm"
"#,
        );
        let err = load_driver_manifest(&sub)
            .expect_err("the id \"host\" is reserved for host-synthesized messages");
        assert!(matches!(err, ManifestError::ReservedId));
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-core reserved`
Expected: コンパイルエラー(`ReservedId` 未定義)

- [ ] **Step 3: 実装**

`core/src/plugin/manifest.rs`:

1. `ManifestError` に variant 追加(`BadId` の直後):

```rust
    /// `id` がホスト予約語(`edlr_driver_channel::HOST_SENDER` = "host")。
    /// `host` は字種 `[a-z0-9-]+` として合法だが、ホストが合成する
    /// `sidecar-ready` メッセージの `from` に使うため、なりすまし余地を
    /// 塞ぐ目的で予約する(設計書 sidecar-ready 参照)。
    ReservedId,
```

2. `Display` に追加:

```rust
            ManifestError::ReservedId => write!(
                f,
                "manifest id \"{}\" is reserved for host-synthesized messages",
                edlr_driver_channel::HOST_SENDER
            ),
```

3. `load_manifest` の `is_valid_id` チェック直後に追加:

```rust
    if manifest.id == edlr_driver_channel::HOST_SENDER {
        return Err(ManifestError::ReservedId);
    }
```

`core/src/driver/manifest.rs` の `load_driver_manifest` にも `is_valid_id`
チェック直後に同じ 3 行を追加する。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core manifest`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add core/src/plugin/manifest.rs core/src/driver/manifest.rs
git commit -m "feat(core): manifest 検証で予約 id \"host\" を拒否する"
```

---

### Task 4: driver runner での配線(core)

**Files:**
- Modify: `core/src/driver/runner.rs`

**Interfaces:**
- Consumes: `edlr_driver_process::{ReadyEvent, ReadyCallback}`(Task 2)、`Bus::notify_from_host`(Task 1)
- Produces: `fn forward_sidecar_ready(bus: &Bus, event: ReadyEvent)`(モジュール内自由関数)。`start_drivers` が `host.process_driver().set_ready_callback(...)` でこれを配線する。

- [ ] **Step 1: 失敗するテストを書く**

`core/src/driver/runner.rs` の `mod tests` に追加:

```rust
    #[test]
    fn forward_sidecar_ready_delivers_to_the_driver_queue_as_host() {
        let bus = Bus::new();
        let (tx, rx) = std_mpsc::sync_channel::<Message>(4);
        bus.register_driver("coeiroink", vec![], tx);

        forward_sidecar_ready(
            &bus,
            edlr_driver_process::ReadyEvent {
                key: "coeiroink/worker".to_string(),
                index: 0,
                port: 50021,
            },
        );

        let msg = rx.try_recv().expect("sidecar-ready must be queued");
        assert_eq!(msg.from, edlr_driver_channel::HOST_SENDER);
        assert_eq!(msg.topic, "sidecar-ready");
        let payload: serde_json::Value = serde_json::from_slice(&msg.payload).unwrap();
        assert_eq!(payload["name"], "worker");
        assert_eq!(payload["index"], 0);
        assert_eq!(payload["port"], 50021);
    }

    /// key が `<driver-id>/<sidecar-name>` の形でない(想定外の呼び出し元)
    /// 場合は、panic せず黙って捨てる。
    #[test]
    fn forward_sidecar_ready_drops_an_unrecognized_key() {
        let bus = Bus::new();
        let (tx, rx) = std_mpsc::sync_channel::<Message>(4);
        bus.register_driver("coeiroink", vec![], tx);

        forward_sidecar_ready(
            &bus,
            edlr_driver_process::ReadyEvent {
                key: "no-slash-here".to_string(),
                index: 0,
                port: 50021,
            },
        );

        assert!(rx.try_recv().is_err(), "nothing must be delivered");
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p edlr-core forward_sidecar_ready`
Expected: コンパイルエラー(`forward_sidecar_ready` 未定義)

- [ ] **Step 3: 実装**

`core/src/driver/runner.rs` に自由関数を追加(`load_and_run_driver` の手前
あたり):

```rust
/// `ProcessDriver` の ready 監視(spawn したサイドカーの port へ初めて TCP
/// 接続できた)を、当該ドライバの `on-message` キューへ
/// `from = "host", topic = "sidecar-ready"` の合成メッセージとして届ける
/// (設計書 sidecar-ready 参照)。
///
/// `event.key` は `DriverCtx::sidecar_key` が組み立てる
/// `<driver-id>/<sidecar-name>`。配送失敗(ドライバが Disabled 等)は warn
/// ログに出して捨てる — ready 通知は起動直後の空白期間を埋める最適化であり、
/// 既存の「speak 受信時に取り直す」経路が保険として残っているため。
fn forward_sidecar_ready(bus: &Bus, event: edlr_driver_process::ReadyEvent) {
    let Some((driver_id, name)) = event.key.split_once('/') else {
        tracing::warn!(key = %event.key, "sidecar-ready with an unrecognized key; dropping");
        return;
    };
    let payload = serde_json::json!({
        "name": name,
        "index": event.index,
        "port": event.port,
    });
    if let Err(e) = bus.notify_from_host(driver_id, "sidecar-ready", payload.to_string().into_bytes())
    {
        tracing::warn!(driver_id, sidecar = name, "failed to deliver sidecar-ready: {e}");
    }
}
```

`start_drivers` の `let host = Arc::new(host);` の直後に配線を追加:

```rust
    // spawn したサイドカーの port が初めて繋がった時点で、当該ドライバの
    // on-message へ sidecar-ready を届ける(設計書 sidecar-ready 参照)。
    // ドライバの init が最初の ensure-started を呼ぶより前に設定しておく
    // 必要があるため、走査ループより先に配線する。プラグイン側の
    // `PluginHost` は別の `ProcessDriver` を持つので影響しない。
    {
        let bus = bus.clone();
        host.process_driver()
            .set_ready_callback(Arc::new(move |event| forward_sidecar_ready(&bus, event)));
    }
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p edlr-core driver`
Expected: PASS

- [ ] **Step 5: ワークスペース全体の確認と Commit**

Run: `cargo test --workspace`
Expected: PASS(既知の flaky: devserver の ETXTBSY — issue
`cargo-test-workspace-devserver-etxtbsy-aenf`。落ちたら単体で再実行して確認)

```bash
git add core/src/driver/runner.rs
git commit -m "feat(core): sidecar-ready をドライバの on-message へ配送する"
```

---

### Task 5: coeiroink ドライバ側の変更(別リポジトリ)

**リポジトリ:** `/mnt/game/caches/src/github.com/himanoa/edlr-himanoa-coeiroink`
**注意:** このリポジトリには無関係の未コミット変更がある(Global Constraints 参照)。変更・add するのは下記のファイルだけにする。

**Files:**
- Create: `driver-core/src/sidecar_ready.rs`
- Modify: `driver-core/src/lib.rs`(`pub mod sidecar_ready;` を追加)
- Modify: `driver/src/lib.rs`(`on_message` に分岐追加、`SPEAKERS_PUBLISHED` リセット)

**Interfaces:**
- Produces: `pub fn sidecar_ready::parse(from: &str, topic: &str, payload: &[u8]) -> Option<SidecarReady>` / `pub struct SidecarReady { pub name: String, pub index: u32 }`
- wasm 側(`driver/src/lib.rs`)はホスト関数に依存するためユニットテスト不能。設計書の「index 0 で publish が走る / index != 0 では走らない」の判定ロジックは `parse` + `index == 0` ゲートとして driver-core 側でテストする。「ready 再通知で再 publish」はフラグを `store(false)` してから `publish_speakers()` を呼ぶ 2 行であり、wasm 側の目視レビュー対象。

- [ ] **Step 1: 失敗するテストを書く**

`driver-core/src/sidecar_ready.rs` を新規作成し、テストごと書く:

```rust
//! ホスト発の `sidecar-ready` 合成メッセージの解釈。
//!
//! edlr ホストは、spawn したサイドカーの port へ初めて TCP 接続できた時点で
//! `on-message(from = "host", topic = "sidecar-ready",
//! payload = {"name": ..., "index": ..., "port": ...})` を届ける
//! (edlr 側設計書 sidecar-ready 参照)。`from == "host"` はホスト予約で、
//! プラグインは名乗れない(manifest 検証で拒否される)。

/// 解釈済みの sidecar-ready 通知。`port` はここでは使わないので持たない
/// (宛先ポートは従来どおり `driver-process::status` から引く)。
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarReady {
    pub name: String,
    pub index: u32,
}

/// `on_message` の引数から sidecar-ready 通知を取り出す。
/// 該当しない(from/topic が違う、payload が壊れている)なら `None`。
pub fn parse(from: &str, topic: &str, payload: &[u8]) -> Option<SidecarReady> {
    if from != "host" || topic != "sidecar-ready" {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    Some(SidecarReady {
        name: value.get("name")?.as_str()?.to_string(),
        index: u32::try_from(value.get("index")?.as_u64()?).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_host_sidecar_ready_message() {
        let ready = parse(
            "host",
            "sidecar-ready",
            br#"{"name": "worker", "index": 0, "port": 51000}"#,
        )
        .expect("must parse");
        assert_eq!(ready.name, "worker");
        assert_eq!(ready.index, 0);
    }

    #[test]
    fn a_nonzero_index_is_parsed_as_is() {
        let ready = parse(
            "host",
            "sidecar-ready",
            br#"{"name": "worker", "index": 2, "port": 51002}"#,
        )
        .expect("must parse");
        assert_eq!(ready.index, 2);
    }

    #[test]
    fn a_non_host_sender_is_not_a_sidecar_ready() {
        // from はホスト予約。プラグイン名がたまたま入っていても解釈しない。
        assert_eq!(
            parse("some-plugin", "sidecar-ready", br#"{"name": "worker", "index": 0}"#),
            None
        );
    }

    #[test]
    fn another_topic_from_host_is_not_a_sidecar_ready() {
        assert_eq!(parse("host", "speak", br#"{"name": "worker", "index": 0}"#), None);
    }

    #[test]
    fn broken_payloads_are_ignored() {
        assert_eq!(parse("host", "sidecar-ready", b"not json"), None);
        assert_eq!(parse("host", "sidecar-ready", br#"{"index": 0}"#), None);
        assert_eq!(parse("host", "sidecar-ready", br#"{"name": "worker"}"#), None);
    }
}
```

`driver-core/src/lib.rs` に `pub mod sidecar_ready;` を追加する。
`driver-core/Cargo.toml` に `serde_json` が無ければ追加する(route.rs が
JSON を解釈しているので既にあるはず。確認のみ)。

- [ ] **Step 2: テストを実行して通ることを確認**

Run: `cargo test -p edlr-coeiroink-driver-core sidecar_ready`(リポジトリのパッケージ名は `driver-core/Cargo.toml` の `name` を確認して合わせる)
Expected: PASS(このタスクはモジュール新設なので、テストと実装を同時に書く)

- [ ] **Step 3: wasm ドライバの `on_message` に分岐を足す**

`driver/src/lib.rs`:

1. use 追加:

```rust
use edlr_coeiroink_driver_core::sidecar_ready;
```

2. `on_message` の冒頭(`if topic != TOPIC` より前)に追加:

```rust
        // ホスト発の起動完了通知。ワーカーが listen し始めた瞬間に speakers を
        // (再)publish する。話者は全ワーカー共通なので index 0 だけ見れば
        // よい。ワーカー再起動で話者構成が変わった場合も retain が追従する
        // よう、一発フラグをリセットしてから publish する。
        if let Some(ready) = sidecar_ready::parse(&from, &topic, &payload) {
            if ready.index == 0 {
                SPEAKERS_PUBLISHED.store(false, Ordering::Relaxed);
                publish_speakers();
            }
            return;
        }
```

3. `SPEAKERS_PUBLISHED` のドキュメントコメントを現実に合わせて更新:

```rust
/// 話者一覧を emit 済みか。
///
/// 成功したら次の sidecar-ready(ワーカーの再起動)までは取りに行かない。
/// ready 通知を受けたらリセットして再 publish する(ワーカー入れ替えで
/// 話者構成が変わった場合に retain を追従させるため)。失敗したら false の
/// ままにして、次の読み上げ依頼でもう一度試す(`ensure_started` と同じ
/// 遅延復帰)。
```

- [ ] **Step 4: ビルドとテストが通ることを確認**

Run(coeiroink リポジトリで): `cargo test --workspace` および wasm ドライバのビルド(`scripts/` か README のビルド手順に従う。最低限 `cargo check -p <driver パッケージ名> --target wasm32-wasip2`)
Expected: PASS / ビルド成功

- [ ] **Step 5: Commit(coeiroink リポジトリ)**

```bash
cd /mnt/game/caches/src/github.com/himanoa/edlr-himanoa-coeiroink
git add driver-core/src/sidecar_ready.rs driver-core/src/lib.rs driver/src/lib.rs
# driver-core/Cargo.toml を触った場合だけそれも add する。
# 無関係の未コミット変更(route.rs / settings.rs / driver_manifest.rs /
# driver.toml)は絶対に含めない。
git commit -m "feat(driver): sidecar-ready を受けて speakers を再 publish する"
```

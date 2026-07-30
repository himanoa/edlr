//! wasmtime component host for the `driver` world: loads driver components,
//! wires host-log / host-settings / driver-http / driver-process / driver-fs
//! imports, and implements `bus-host.emit` so drivers can publish back to
//! subscribers. Modeled on `crate::plugin::host` (same shape, different
//! `bindgen!` world and import set); see `crate::driver`'s module doc for why
//! the two are kept separate rather than unified.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "driver",
    });
}

use bindings::edlr::plugin::bus_host::Host as BusHostHost;
use bindings::edlr::plugin::bus_types::BusError as WitBusError;
use bindings::edlr::plugin::driver_fs::{
    DriverError as WitFsError, Entry as WitFsEntry, Host as DriverFsHost,
};
use bindings::edlr::plugin::driver_http::{
    DriverError as WitDriverError, Host as DriverHttpHost, Request as WitRequest,
    Response as WitResponse,
};
use bindings::edlr::plugin::driver_process::{
    DriverError as WitProcessError, Host as DriverProcessHost, Instance as WitInstance,
    InstanceState as WitInstanceState,
};
use bindings::edlr::plugin::host_log::{Host as HostLogHost, Level as WitLevel};
use bindings::edlr::plugin::host_settings::Host as HostSettingsHost;
use bindings::Driver as DriverBindings;

use crate::plugin::allowlist::check_url;

/// Interval between epoch ticks driven by the background ticker thread.
/// Mirrors `crate::plugin::host::EPOCH_TICK_INTERVAL`.
const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(100);

/// ドライバ向けの HTTP タイムアウト。プラグインの `HTTP_TIMEOUT`(1.5 秒)では
/// 音声合成のような数秒かかる呼び出しが完了しないため、ドライバ専用に長く取る。
///
/// epoch interruption は wasm の命令境界でしか作動せず、ブロッキングな HTTP
/// 呼び出し自体は打ち切れない。だから「HTTP タイムアウト < 呼び出し期限」の
/// 不変条件はドライバ側でも維持する必要がある(プラグイン側の `HTTP_TIMEOUT`
/// のドキュメント参照)。
pub const DRIVER_HTTP_TIMEOUT: Duration = Duration::from_secs(25);

const _: () = assert!(
    DRIVER_HTTP_TIMEOUT.as_millis() < DriverInstance::CALL_DEADLINE.as_millis(),
    "DRIVER_HTTP_TIMEOUT must stay strictly under DriverInstance::CALL_DEADLINE -- see DRIVER_HTTP_TIMEOUT's doc comment"
);

/// Maximum response body size, in bytes, a `driver-http.send` call will
/// return before failing with a `transport` error. Mirrors
/// `crate::plugin::host::HTTP_MAX_BODY`.
pub const HTTP_MAX_BODY: usize = 8 * 1024 * 1024;

/// `driver-fs` の 1 回の読み取り上限。`HTTP_MAX_BODY` と同値。ホスト側の
/// バッファを無制限にしないためのもので、扱えるファイルサイズの上限では
/// ない(超えるものは `stat` + `read-range` で分割して読む)。
pub const FS_READ_LIMIT: usize = HTTP_MAX_BODY;

/// `list` が返すエントリ数の上限。呼び出し期限(`CALL_DEADLINE`)を
/// 食い潰さないための保護。
pub const FS_LIST_LIMIT: usize = 10_000;

/// サイドカー停止時、SIGTERM から SIGKILL へ昇格するまでの猶予。
/// `crate::plugin::host::SIDECAR_SHUTDOWN_GRACE` と同じく
/// `edlr_config::SIDECAR_SHUTDOWN_GRACE_SECS` から取る。
pub const SIDECAR_SHUTDOWN_GRACE: Duration =
    Duration::from_secs(edlr_config::SIDECAR_SHUTDOWN_GRACE_SECS);

/// 同一サイドカーの spawn 試行の最短間隔。ドライバがループで
/// `ensure-started` を呼んでも spawn 嵐にならないための下限。
pub const SIDECAR_SPAWN_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum linear memory (bytes) a single driver instance may allocate.
/// Mirrors `crate::plugin::host`'s `PLUGIN_MEMORY_LIMIT`.
const DRIVER_MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Maximum number of component instances / tables a single driver `Store`
/// may create. Mirrors `crate::plugin::host`'s `PLUGIN_INSTANCE_LIMIT` /
/// `PLUGIN_TABLE_LIMIT`.
const DRIVER_INSTANCE_LIMIT: usize = 8;
const DRIVER_TABLE_LIMIT: usize = 8;

/// Per-driver-instance host state, exposed to the guest via the generated
/// `Host` traits.
pub struct DriverCtx {
    pub driver_id: String,
    /// Effective settings JSON string. Wrapped so the runner can swap the
    /// value in place between calls without reloading the driver.
    pub settings_json: Arc<Mutex<String>>,
    /// Capability grant state JSON string, shape `{"hosts": ["https://..."]}`
    /// -- the *effective* set of hosts this driver instance may reach via
    /// `driver-http.send`. See `crate::plugin::host::HostCtx::capabilities_json`
    /// for the full rationale; the same reasoning applies here verbatim,
    /// substituting "driver" for "plugin".
    pub capabilities_json: Arc<Mutex<String>>,
    /// サイドカーの承認状態と実行に必要な値の共有バッファ。形は
    /// `sidecar_runtime::sidecars_json_string` を参照。
    pub sidecars_json: Arc<Mutex<String>>,
    /// ファイルシステムルートの承認状態と実パスの共有バッファ。形は
    /// `fs_runtime::filesystem_json_string` を参照。
    pub filesystem_json: Arc<Mutex<String>>,
    /// プラグイン間バスの実体。全ドライバインスタンスで共有される
    /// (`http_driver`/`process_driver`/`fs_driver` と同様)。`bus-host.emit`
    /// はこの `bus` にドライバ自身の id を渡すだけの薄いラッパーで、
    /// トピックの未宣言/ペイロード超過/購読者への配送は全て `Bus::emit`
    /// (`edlr_driver_channel`)の責務。
    bus: edlr_driver_channel::Bus,
    /// The HTTP client used by `driver-http.send`. Shared (via `Arc`) across
    /// every driver instance the owning `DriverHost` loads.
    http_driver: Arc<edlr_driver_http::HttpDriver>,
    /// サイドカープロセスを実際に所有するドライバ。`http_driver` と同様、
    /// `DriverHost` が 1 つだけ持ち、全ドライバインスタンスで共有する。
    process_driver: Arc<edlr_driver_process::ProcessDriver>,
    /// 承認済みルート配下でのファイル操作を実際に行うドライバ。
    /// `http_driver` / `process_driver` と同様、`DriverHost` が 1 つだけ
    /// 持ち、全ドライバインスタンスで共有する。
    fs_driver: Arc<edlr_driver_fs::FsDriver>,
    /// WASI state. The `driver` world itself does not import WASI, but
    /// components built for the `wasm32-wasip2` target still import a
    /// baseline set of WASI interfaces (io, random, clocks, ...) from the
    /// Rust standard library / adapter, so the host must satisfy them.
    wasi_ctx: WasiCtx,
    wasi_table: ResourceTable,
    /// Resource limits (memory/instances/tables) enforced on this driver's
    /// `Store` via `Store::limiter`. See `DRIVER_MEMORY_LIMIT`.
    limits: StoreLimits,
}

impl DriverCtx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        driver_id: String,
        settings_json: Arc<Mutex<String>>,
        capabilities_json: Arc<Mutex<String>>,
        sidecars_json: Arc<Mutex<String>>,
        filesystem_json: Arc<Mutex<String>>,
        bus: edlr_driver_channel::Bus,
        http_driver: Arc<edlr_driver_http::HttpDriver>,
        process_driver: Arc<edlr_driver_process::ProcessDriver>,
        fs_driver: Arc<edlr_driver_fs::FsDriver>,
    ) -> DriverCtx {
        DriverCtx {
            driver_id,
            settings_json,
            capabilities_json,
            sidecars_json,
            filesystem_json,
            bus,
            http_driver,
            process_driver,
            fs_driver,
            // Deliberately empty sandbox default: no preopened directories,
            // no stdio, no network access, so a driver can only interact
            // with the host through the `driver` world's explicit imports.
            wasi_ctx: WasiCtxBuilder::new().build(),
            wasi_table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(DRIVER_MEMORY_LIMIT)
                .instances(DRIVER_INSTANCE_LIMIT)
                .tables(DRIVER_TABLE_LIMIT)
                .trap_on_grow_failure(true)
                .build(),
        }
    }
}

impl WasiView for DriverCtx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.wasi_table,
        }
    }
}

impl HostLogHost for DriverCtx {
    fn log(&mut self, level: WitLevel, message: String) {
        let driver_id = self.driver_id.as_str();
        match level {
            WitLevel::Debug => tracing::debug!(driver_id, "{message}"),
            WitLevel::Info => tracing::info!(driver_id, "{message}"),
            WitLevel::Warn => tracing::warn!(driver_id, "{message}"),
            WitLevel::Error => tracing::error!(driver_id, "{message}"),
        }
    }
}

impl HostSettingsHost for DriverCtx {
    fn get_all(&mut self) -> String {
        self.settings_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl DriverHttpHost for DriverCtx {
    /// See `crate::plugin::host::HostCtx::send` for the full rationale --
    /// identical logic, substituting `driver_id` for `plugin_id`.
    fn send(&mut self, req: WitRequest) -> Result<WitResponse, WitDriverError> {
        let raw = self
            .capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let hosts = crate::plugin::host::parse_capability_hosts(&raw);
        if hosts.is_empty() {
            return Err(WitDriverError::PermissionDenied(
                "capability not granted".to_string(),
            ));
        }
        check_url(&hosts, &req.url).map_err(WitDriverError::PermissionDenied)?;

        let driver_request = edlr_driver_http::HttpRequest {
            method: req.method,
            url: req.url,
            headers: req.headers,
            body: req.body,
        };

        self.http_driver
            .send(driver_request)
            .map(|response| WitResponse {
                status: response.status,
                headers: response.headers,
                body: response.body,
            })
            .map_err(|e| match e {
                edlr_driver_http::HttpError::InvalidRequest(msg) => {
                    WitDriverError::InvalidRequest(msg)
                }
                edlr_driver_http::HttpError::Transport(msg) => WitDriverError::Transport(msg),
            })
    }
}

/// `bus-error` を関数を持たない型だけの interface(`bus-types`)に切り出した
/// 都合で、`bindgen!` は関数のない `bus_types::Host` トレイトも生成する。
/// 中身は空(関数が無い)ので、空の impl でよい。
impl bindings::edlr::plugin::bus_types::Host for DriverCtx {}

impl BusHostHost for DriverCtx {
    /// `Bus::emit` に自分自身の id を渡すだけの薄いラッパー。ドライバは自分
    /// の id を引数として渡さない(プラグインが自分の id を渡さないのと
    /// 同じ理屈): 未宣言トピックの拒否・256 KiB のペイロード上限・retained
    /// の更新・購読者への配送は全て `Bus::emit` が行う。
    fn emit(&mut self, topic: String, payload: Vec<u8>) -> Result<(), WitBusError> {
        self.bus
            .emit(&self.driver_id, &topic, payload)
            .map_err(bus_error_to_wit)
    }
}

/// `edlr_driver_channel::BusError` を WIT の `bus-error` variant へ 1:1 で
/// 写像する。`crate::plugin::host` にも同名の関数があるが、2 つの
/// `bindgen!` 呼び出し(`world: "plugin"` と `world: "driver"`)は同じ WIT
/// variant に対してそれぞれ別の Rust 型を生成するため、共有できない
/// (`plugin::host::WitBusError` と `driver::host::WitBusError` は構造的に
/// 同一だが型としては別物)。ロジックは 5 行の 1:1 写像で、共有する価値の
/// あるほど複雑でもないため、素直に複製する。
fn bus_error_to_wit(error: edlr_driver_channel::BusError) -> WitBusError {
    use edlr_driver_channel::BusError;
    match error {
        BusError::UnknownDriver(m) => WitBusError::UnknownDriver(m),
        BusError::UnknownTopic(m) => WitBusError::UnknownTopic(m),
        BusError::DriverUnavailable(m) => WitBusError::DriverUnavailable(m),
        BusError::QueueFull(m) => WitBusError::QueueFull(m),
        BusError::TooLarge(m) => WitBusError::TooLarge(m),
    }
}

impl DriverCtx {
    /// `sidecars_json` から当該サイドカーの実行仕様を解決する。
    /// `crate::plugin::host::HostCtx::resolve_sidecar` と同じロジック。
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
        format!("{}/{name}", self.driver_id)
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

impl DriverProcessHost for DriverCtx {
    /// See `crate::plugin::host::HostCtx::ensure_started` for the residual
    /// TOCTOU rationale -- identical logic here.
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

    /// See `crate::plugin::host::HostCtx::stop`'s doc comment: uses the
    /// fire-and-forget `stop_detached` so a guest-facing `stop` call never
    /// blocks the driver's dedicated thread for the sidecar shutdown grace
    /// period.
    fn stop(&mut self, name: String) -> Result<(), WitProcessError> {
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
        self.process_driver.clone().stop_detached(&key);
        Ok(())
    }

    fn status(&mut self, name: String) -> Result<Vec<WitInstance>, WitProcessError> {
        let spec = self.resolve_sidecar(&name)?;
        let key = self.sidecar_key(&name);
        Ok(to_wit_instances(self.process_driver.status(&key, &spec)))
    }
}

impl DriverCtx {
    /// `filesystem_json` から当該ルートの実パスと mode を解決する。
    /// `crate::plugin::host::HostCtx::resolve_root` と同じロジック。
    fn resolve_root(&self, root: &str, need_write: bool) -> Result<std::path::PathBuf, WitFsError> {
        let raw = self
            .filesystem_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let entries = crate::plugin::fs_runtime::parse_filesystem(&raw);

        let Some(entry) = entries.get(root) else {
            return Err(WitFsError::UnknownRoot(format!("no such root: {root}")));
        };
        if !entry.granted {
            return Err(WitFsError::PermissionDenied(format!(
                "filesystem root not granted: {root}"
            )));
        }
        if entry.path.is_empty() {
            return Err(WitFsError::NotConfigured(format!(
                "root {root} has no directory configured"
            )));
        }
        if need_write && entry.mode != "read-write" {
            return Err(WitFsError::PermissionDenied(format!(
                "root {root} is read-only"
            )));
        }
        Ok(std::path::PathBuf::from(&entry.path))
    }
}

/// `edlr_driver_fs::FsError` を WIT の `driver-fs.driver-error` variant へ写像する。
fn to_wit_fs_error(e: edlr_driver_fs::FsError) -> WitFsError {
    match e {
        edlr_driver_fs::FsError::InvalidPath(m) => WitFsError::InvalidPath(m),
        edlr_driver_fs::FsError::NotFound(m) => WitFsError::NotFound(m),
        edlr_driver_fs::FsError::TooLarge(m) => WitFsError::TooLarge(m),
        edlr_driver_fs::FsError::Io(m) => WitFsError::Io(m),
    }
}

fn to_wit_fs_entry(entry: edlr_driver_fs::Entry) -> WitFsEntry {
    WitFsEntry {
        path: entry.path,
        size: entry.size,
        modified: entry.modified,
    }
}

impl DriverFsHost for DriverCtx {
    fn read(&mut self, root: String, path: String) -> Result<Vec<u8>, WitFsError> {
        let root_path = self.resolve_root(&root, false)?;
        self.fs_driver
            .read(&root_path, &path)
            .map_err(to_wit_fs_error)
    }

    fn read_range(
        &mut self,
        root: String,
        path: String,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, WitFsError> {
        let root_path = self.resolve_root(&root, false)?;
        self.fs_driver
            .read_range(&root_path, &path, offset, len)
            .map_err(to_wit_fs_error)
    }

    fn stat(&mut self, root: String, path: String) -> Result<WitFsEntry, WitFsError> {
        let root_path = self.resolve_root(&root, false)?;
        self.fs_driver
            .stat(&root_path, &path)
            .map(to_wit_fs_entry)
            .map_err(to_wit_fs_error)
    }

    fn list(&mut self, root: String, prefix: String) -> Result<Vec<WitFsEntry>, WitFsError> {
        let root_path = self.resolve_root(&root, false)?;
        self.fs_driver
            .list(&root_path, &prefix)
            .map(|entries| entries.into_iter().map(to_wit_fs_entry).collect())
            .map_err(to_wit_fs_error)
    }

    fn write(&mut self, root: String, path: String, bytes: Vec<u8>) -> Result<(), WitFsError> {
        let root_path = self.resolve_root(&root, true)?;
        self.fs_driver
            .write(&root_path, &path, &bytes)
            .map_err(to_wit_fs_error)
    }

    fn append(&mut self, root: String, path: String, bytes: Vec<u8>) -> Result<(), WitFsError> {
        let root_path = self.resolve_root(&root, true)?;
        self.fs_driver
            .append(&root_path, &path, &bytes)
            .map_err(to_wit_fs_error)
    }

    fn delete(&mut self, root: String, path: String) -> Result<(), WitFsError> {
        let root_path = self.resolve_root(&root, true)?;
        self.fs_driver
            .delete(&root_path, &path)
            .map_err(to_wit_fs_error)
    }
}

/// Owns the wasmtime `Engine` and a background thread that periodically
/// increments the engine's epoch counter, driving epoch-interruption-based
/// call deadlines for every driver instance loaded from this host.
pub struct DriverHost {
    engine: Engine,
    ticker_stop: Arc<AtomicBool>,
    /// The single `HttpDriver` shared by every driver instance this host
    /// loads. Built once here (not per driver, not per call).
    http_driver: Arc<edlr_driver_http::HttpDriver>,
    /// The single `ProcessDriver` shared by every driver instance this host
    /// loads, mirroring `http_driver` above. Sidecar processes are keyed by
    /// `<driver-id>/<sidecar-name>` (see `DriverCtx::sidecar_key`), so
    /// sharing this one driver across all drivers does not let one driver
    /// observe or touch another's sidecars.
    process_driver: Arc<edlr_driver_process::ProcessDriver>,
    /// The single `FsDriver` shared by every driver instance this host
    /// loads, mirroring `http_driver`/`process_driver` above. Root paths are
    /// resolved per call from each driver's own `filesystem_json`, so
    /// sharing this one driver across all drivers does not let one driver
    /// reach another's roots.
    fs_driver: Arc<edlr_driver_fs::FsDriver>,
}

impl DriverHost {
    pub fn new() -> anyhow::Result<DriverHost> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)
            .map_err(|e| anyhow::anyhow!("failed to create wasmtime engine: {e}"))?;

        let ticker_stop = Arc::new(AtomicBool::new(false));
        let ticker_engine = engine.clone();
        let ticker_stop_flag = ticker_stop.clone();
        thread::spawn(move || {
            while !ticker_stop_flag.load(Ordering::Relaxed) {
                thread::sleep(EPOCH_TICK_INTERVAL);
                ticker_engine.increment_epoch();
            }
        });

        let http_driver = Arc::new(
            edlr_driver_http::HttpDriver::new(DRIVER_HTTP_TIMEOUT, HTTP_MAX_BODY)
                .map_err(|e| anyhow::anyhow!("failed to build http driver: {e}"))?,
        );
        let process_driver = Arc::new(edlr_driver_process::ProcessDriver::new(
            SIDECAR_SHUTDOWN_GRACE,
            SIDECAR_SPAWN_MIN_INTERVAL,
        ));
        let fs_driver = Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT));

        Ok(DriverHost {
            engine,
            ticker_stop,
            http_driver,
            process_driver,
            fs_driver,
        })
    }

    /// Returns a clone of the shared `HttpDriver` `Arc` for wiring into a
    /// new driver's `DriverCtx`. Cloning an `Arc` is cheap; this does not
    /// build a new HTTP client.
    pub fn http_driver(&self) -> Arc<edlr_driver_http::HttpDriver> {
        self.http_driver.clone()
    }

    /// Returns a clone of the shared `ProcessDriver` `Arc` for wiring into a
    /// new driver's `DriverCtx`. Cloning an `Arc` is cheap; this does not
    /// spawn or otherwise touch any sidecar process.
    pub fn process_driver(&self) -> Arc<edlr_driver_process::ProcessDriver> {
        self.process_driver.clone()
    }

    /// Returns a clone of the shared `FsDriver` `Arc` for wiring into a new
    /// driver's `DriverCtx`. Cloning an `Arc` is cheap; this does not touch
    /// any file.
    pub fn fs_driver(&self) -> Arc<edlr_driver_fs::FsDriver> {
        self.fs_driver.clone()
    }

    pub fn load(&self, wasm_path: &Path, ctx: DriverCtx) -> anyhow::Result<DriverInstance> {
        let component = Component::from_file(&self.engine, wasm_path).map_err(|e| {
            anyhow::anyhow!("failed to load component at {}: {e}", wasm_path.display())
        })?;

        let mut linker = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| anyhow::anyhow!("failed to wire WASI imports into linker: {e}"))?;
        DriverBindings::add_to_linker::<_, HasSelf<_>>(&mut linker, |ctx| ctx)
            .map_err(|e| anyhow::anyhow!("failed to wire host imports into linker: {e}"))?;

        let mut store = Store::new(&self.engine, ctx);
        store.limiter(|ctx| &mut ctx.limits);
        // Ticks-beyond-current is set fresh before every call in
        // `DriverInstance::call`; this initial deadline just prevents
        // instantiation itself (which may run guest start code) from
        // hanging forever.
        store.set_epoch_deadline(deadline_ticks(DriverInstance::CALL_DEADLINE));

        let bindings = DriverBindings::instantiate(&mut store, &component, &linker)
            .map_err(|e| anyhow::anyhow!("failed to instantiate driver component: {e}"))?;

        Ok(DriverInstance { store, bindings })
    }
}

impl Drop for DriverHost {
    fn drop(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
        // 明示 shutdown 経路と同じく、`DriverHost` が消えるときにサイドカー
        // の孤児を残さない最後の砦として、無条件に全サイドカーを止める。
        self.process_driver.stop_all();
    }
}

/// Number of epoch ticks corresponding to `duration`, rounded up, with a
/// minimum of one tick so a zero-length deadline still traps promptly.
fn deadline_ticks(duration: Duration) -> u64 {
    let ticks = duration.as_nanos().div_ceil(EPOCH_TICK_INTERVAL.as_nanos());
    u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
}

/// A loaded, instantiated driver component together with its store.
pub struct DriverInstance {
    store: Store<DriverCtx>,
    bindings: DriverBindings,
}

impl DriverInstance {
    /// ドライバ 1 呼び出しの期限。ドライバは専用スレッドで動きイベント配信の
    /// ループを塞がないため、プラグインの 2 秒より長く取れる。代償として
    /// この間そのドライバのキューは詰まる(設計書「並行性」参照)。
    pub const CALL_DEADLINE: Duration = Duration::from_secs(edlr_config::DRIVER_CALL_DEADLINE_SECS);

    pub fn call_init(&mut self) -> anyhow::Result<()> {
        self.store
            .set_epoch_deadline(deadline_ticks(Self::CALL_DEADLINE));
        self.bindings
            .call_init(&mut self.store)
            .map_err(|e| anyhow::anyhow!("driver init() call failed or timed out: {e}"))
    }

    pub fn call_on_message(
        &mut self,
        from: &str,
        topic: &str,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        self.store
            .set_epoch_deadline(deadline_ticks(Self::CALL_DEADLINE));
        self.bindings
            .call_on_message(&mut self.store, from, topic, payload)
            .map_err(|e| anyhow::anyhow!("driver on-message() call failed or timed out: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_reaches_the_bus_and_updates_retained() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            }],
            tx,
        );
        let mut ctx = test_driver_ctx(bus.clone());

        ctx.emit("current-system".into(), b"Sol".to_vec()).unwrap();

        assert_eq!(
            bus.retained_for("ed-state", "current-system"),
            Some(b"Sol".to_vec())
        );
    }

    #[test]
    fn emit_to_an_undeclared_topic_is_rejected() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver("ed-state", vec![], tx);
        let mut ctx = test_driver_ctx(bus);

        assert!(matches!(
            ctx.emit("nope".into(), vec![]),
            Err(WitBusError::UnknownTopic(_))
        ));
    }

    #[test]
    fn oversized_emits_are_rejected() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(4);
        bus.register_driver(
            "ed-state",
            vec![edlr_driver_channel::TopicSpec {
                name: "current-system".into(),
                retain: true,
                description: String::new(),
            }],
            tx,
        );
        let mut ctx = test_driver_ctx(bus);
        let big = vec![0u8; edlr_driver_channel::BUS_MAX_PAYLOAD + 1];
        assert!(matches!(
            ctx.emit("current-system".into(), big),
            Err(WitBusError::TooLarge(_))
        ));
    }

    fn test_driver_ctx(bus: edlr_driver_channel::Bus) -> DriverCtx {
        DriverCtx::new(
            "ed-state".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(r#"{"hosts":[]}"#.to_string())),
            Arc::new(Mutex::new("[]".to_string())),
            Arc::new(Mutex::new("[]".to_string())),
            bus,
            Arc::new(
                edlr_driver_http::HttpDriver::new(DRIVER_HTTP_TIMEOUT, HTTP_MAX_BODY)
                    .expect("http driver builds"),
            ),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                SIDECAR_SHUTDOWN_GRACE,
                SIDECAR_SPAWN_MIN_INTERVAL,
            )),
            Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT)),
        )
    }

    /// `test_driver_ctx` に `sidecars_json`/`filesystem_json` を差し込めるようにした
    /// 別ヘルパー。既存の `test_driver_ctx` は emit 系テストの流儀を壊さないよう
    /// そのまま残し、resolve 系の錨テストはこちらを使う。
    fn test_driver_ctx_with(sidecars_json: &str, filesystem_json: &str) -> DriverCtx {
        DriverCtx::new(
            "ed-state".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(r#"{"hosts":[]}"#.to_string())),
            Arc::new(Mutex::new(sidecars_json.to_string())),
            Arc::new(Mutex::new(filesystem_json.to_string())),
            edlr_driver_channel::Bus::new(),
            Arc::new(
                edlr_driver_http::HttpDriver::new(DRIVER_HTTP_TIMEOUT, HTTP_MAX_BODY)
                    .expect("http driver builds"),
            ),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                SIDECAR_SHUTDOWN_GRACE,
                SIDECAR_SPAWN_MIN_INTERVAL,
            )),
            Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT)),
        )
    }

    fn sidecar_entry(
        granted: bool,
        command: &str,
    ) -> crate::plugin::sidecar_runtime::SidecarRuntimeEntry {
        crate::plugin::sidecar_runtime::SidecarRuntimeEntry {
            name: "tts".to_string(),
            granted,
            command: command.to_string(),
            args: vec![],
            ports: vec![],
        }
    }

    fn fs_entry(
        granted: bool,
        mode: &str,
        path: &str,
    ) -> crate::plugin::fs_runtime::FsRuntimeEntry {
        crate::plugin::fs_runtime::FsRuntimeEntry {
            name: "exports".to_string(),
            granted,
            mode: mode.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn ensure_started_without_grant_is_permission_denied() {
        use crate::plugin::sidecar_runtime::sidecars_json_string;
        let mut ctx = test_driver_ctx_with(
            &sidecars_json_string(&[sidecar_entry(false, "/bin/sh")]),
            "[]",
        );

        let err = ctx
            .ensure_started("tts".to_string())
            .expect_err("ungranted sidecar must not start");
        let WitProcessError::PermissionDenied(msg) = err else {
            panic!("expected PermissionDenied, got a different variant");
        };
        assert_eq!(msg, "sidecar not granted: tts");
    }

    #[test]
    fn ensure_started_unknown_sidecar_is_reported_as_such() {
        use crate::plugin::sidecar_runtime::sidecars_json_string;
        let mut ctx = test_driver_ctx_with(
            &sidecars_json_string(&[sidecar_entry(true, "/bin/sh")]),
            "[]",
        );

        let err = ctx
            .ensure_started("nope".to_string())
            .expect_err("unknown sidecar must be rejected");
        let WitProcessError::UnknownSidecar(msg) = err else {
            panic!("expected UnknownSidecar, got a different variant");
        };
        assert_eq!(msg, "no such sidecar: nope");
    }

    #[test]
    fn ensure_started_granted_but_unconfigured_command_is_not_configured() {
        use crate::plugin::sidecar_runtime::sidecars_json_string;
        let mut ctx = test_driver_ctx_with(&sidecars_json_string(&[sidecar_entry(true, "")]), "[]");

        let err = ctx
            .ensure_started("tts".to_string())
            .expect_err("empty command must be reported as not-configured");
        let WitProcessError::NotConfigured(msg) = err else {
            panic!("expected NotConfigured, got a different variant");
        };
        assert_eq!(msg, "sidecar tts has no executable configured");
    }

    #[test]
    fn read_unknown_root_is_reported_as_such() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = test_driver_ctx_with(
            "[]",
            &filesystem_json_string(&[fs_entry(true, "read-write", &dir.path().to_string_lossy())]),
        );

        let err = ctx
            .read("nope".to_string(), "a.txt".to_string())
            .expect_err("unknown root must be reported as such");
        let WitFsError::UnknownRoot(msg) = err else {
            panic!("expected UnknownRoot, got a different variant");
        };
        assert_eq!(msg, "no such root: nope");
    }

    #[test]
    fn write_under_read_mode_root_is_permission_denied() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = test_driver_ctx_with(
            "[]",
            &filesystem_json_string(&[fs_entry(true, "read", &dir.path().to_string_lossy())]),
        );

        let err = ctx
            .write("exports".to_string(), "a.txt".to_string(), vec![1])
            .expect_err("write under read mode must be denied");
        let WitFsError::PermissionDenied(msg) = err else {
            panic!("expected PermissionDenied, got a different variant");
        };
        assert_eq!(msg, "root exports is read-only");
    }

    #[test]
    fn send_with_no_effective_hosts_is_permission_denied() {
        let bus = edlr_driver_channel::Bus::new();
        let mut ctx = test_driver_ctx(bus);

        let req = WitRequest {
            method: "GET".to_string(),
            url: "https://api.example.com/ping".to_string(),
            headers: Vec::new(),
            body: None,
        };
        let err = ctx
            .send(req)
            .expect_err("no effective hosts means every call is denied");
        let WitDriverError::PermissionDenied(msg) = err else {
            panic!("expected PermissionDenied, got a different variant");
        };
        assert_eq!(msg, "capability not granted");
    }
}

//! wasmtime component host for the `driver` world: loads driver components,
//! wires host-log / host-settings / driver-http / driver-process / driver-fs
//! imports, and implements `bus-host.emit` so drivers can publish back to
//! subscribers. Modeled on `crate::host::plugin` (same shape, different
//! `bindgen!` world and import set); driver and plugin hosts are a symmetric
//! structure but a separate layer, so they are not forced to share code
//! beyond the grants/settings utilities they already share.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use super::drivers::SharedDrivers;
use super::engine::{deadline_ticks, EpochEngine};

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

use super::resolve::{
    check_http_permission, resolve_root, resolve_sidecar, RootResolveError, SidecarResolveError,
};

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
/// `crate::host::plugin::HTTP_MAX_BODY`.
pub const HTTP_MAX_BODY: usize = 8 * 1024 * 1024;

/// `driver-fs` の 1 回の読み取り上限。`HTTP_MAX_BODY` と同値。ホスト側の
/// バッファを無制限にしないためのもので、扱えるファイルサイズの上限では
/// ない(超えるものは `stat` + `read-range` で分割して読む)。
pub const FS_READ_LIMIT: usize = HTTP_MAX_BODY;

/// `list` が返すエントリ数の上限。呼び出し期限(`CALL_DEADLINE`)を
/// 食い潰さないための保護。
pub const FS_LIST_LIMIT: usize = 10_000;

/// サイドカー停止時、SIGTERM から SIGKILL へ昇格するまでの猶予。
/// `crate::host::plugin::SIDECAR_SHUTDOWN_GRACE` と同じく
/// `edlr_config::SIDECAR_SHUTDOWN_GRACE_SECS` から取る。
pub const SIDECAR_SHUTDOWN_GRACE: Duration =
    Duration::from_secs(edlr_config::SIDECAR_SHUTDOWN_GRACE_SECS);

/// 同一サイドカーの spawn 試行の最短間隔。ドライバがループで
/// `ensure-started` を呼んでも spawn 嵐にならないための下限。
pub const SIDECAR_SPAWN_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum linear memory (bytes) a single driver instance may allocate.
/// Mirrors `crate::host::plugin`'s `PLUGIN_MEMORY_LIMIT`.
const DRIVER_MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Maximum number of component instances / tables a single driver `Store`
/// may create. Mirrors `crate::host::plugin`'s `PLUGIN_INSTANCE_LIMIT` /
/// `PLUGIN_TABLE_LIMIT`.
const DRIVER_INSTANCE_LIMIT: usize = 8;
const DRIVER_TABLE_LIMIT: usize = 8;

/// Per-driver-instance host state, exposed to the guest via the generated
/// `Host` traits.
pub struct DriverCtx {
    /// このドライバの作業キューの送信側。`submit_send` が spawn したタスクが
    /// 完了通知(`DriverWork::JobComplete`)を push するのに使う。
    work_tx: crate::runner::plugin::queue::WorkSender<crate::runner::driver::DriverWork>,
    /// submit 系ジョブの共有状態(job id 採番・世代・in-flight 数)。
    /// プラグイン側と同じ型を共用する(`crate::host::plugin::PluginJobs`)。
    jobs: Arc<crate::host::plugin::PluginJobs>,
    pub driver_id: String,
    /// Effective settings JSON string. Wrapped so the runner can swap the
    /// value in place between calls without reloading the driver.
    pub settings_json: Arc<Mutex<String>>,
    /// Capability grant state JSON string, shape `{"hosts": ["https://..."]}`
    /// -- the *effective* set of hosts this driver instance may reach via
    /// `driver-http.send`. See `crate::host::plugin::HostCtx::capabilities_json`
    /// for the full rationale; the same reasoning applies here verbatim,
    /// substituting "driver" for "plugin".
    pub capabilities_json: Arc<Mutex<String>>,
    /// サイドカーの承認状態と実行に必要な値の共有バッファ。形は
    /// `crate::runtime::sidecar::sidecars_json_string` を参照。
    pub sidecars_json: Arc<Mutex<String>>,
    /// ファイルシステムルートの承認状態と実パスの共有バッファ。形は
    /// `crate::runtime::fs::filesystem_json_string` を参照。
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
        work_tx: crate::runner::plugin::queue::WorkSender<crate::runner::driver::DriverWork>,
        jobs: Arc<crate::host::plugin::PluginJobs>,
    ) -> DriverCtx {
        DriverCtx {
            work_tx,
            jobs,
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
    /// See `crate::host::plugin::HostCtx::send` for the full rationale --
    /// identical logic, substituting `driver_id` for `plugin_id`.
    fn send(&mut self, req: WitRequest) -> Result<WitResponse, WitDriverError> {
        let raw = self
            .capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let hosts = crate::host::plugin::parse_capability_hosts(&raw);
        check_http_permission(&hosts, &req.url).map_err(WitDriverError::PermissionDenied)?;

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

    /// 非同期 submit。プラグイン側
    /// (`crate::host::plugin::HostCtx::submit_send`)と同じセマンティクス:
    /// 許可判定と in-flight 上限(8)は同期で判定し、送信は tokio タスクへ
    /// spawn して即 `Ok(job-id)`。結果は spawn したタスクが
    /// `DriverWork::JobComplete` として作業キューへ push し、ドライバ専用
    /// スレッドが `on-job-complete` export で届ける。
    fn submit_send(
        &mut self,
        req: WitRequest,
        timeout_ms: Option<u32>,
    ) -> Result<u64, WitDriverError> {
        use crate::host::plugin::{job_result_json, submit_timeout, SUBMIT_IN_FLIGHT_LIMIT};

        let raw = self
            .capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let hosts = crate::host::plugin::parse_capability_hosts(&raw);
        check_http_permission(&hosts, &req.url).map_err(WitDriverError::PermissionDenied)?;

        if self.jobs.try_acquire_slot().is_err() {
            return Err(WitDriverError::Transport(format!(
                "submit queue full ({SUBMIT_IN_FLIGHT_LIMIT} jobs already in flight)"
            )));
        }

        let job_id = self.jobs.allocate_job_id();
        let generation = self.jobs.current_generation();
        let timeout = submit_timeout(timeout_ms);
        let driver_request = edlr_driver_http::HttpRequest {
            method: req.method,
            url: req.url,
            headers: req.headers,
            body: req.body,
        };

        let http_driver = self.http_driver.clone();
        let work_tx = self.work_tx.clone();
        let jobs = self.jobs.clone();
        self.http_driver.handle().spawn(async move {
            let result = http_driver.send_async(driver_request, Some(timeout)).await;
            jobs.release_slot();
            let _ = work_tx.push(crate::runner::driver::DriverWork::JobComplete {
                generation,
                job_id,
                result_json: job_result_json(result),
            });
        });

        Ok(job_id)
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
/// 写像する。`crate::host::plugin` にも同名の関数があるが、2 つの
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
    /// `sidecars_json` から当該サイドカーの実行仕様を解決する。判定自体は
    /// `resolve::resolve_sidecar`(`crate::host::plugin::HostCtx::resolve_sidecar`
    /// と共有)へ委譲し、ここでは variant を写像するだけ。
    fn resolve_sidecar(
        &self,
        name: &str,
    ) -> Result<edlr_driver_process::ProcessSpec, WitProcessError> {
        let raw = self
            .sidecars_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let entries = crate::runtime::sidecar::parse_sidecars(&raw);

        resolve_sidecar(&entries, name).map_err(|e| match e {
            SidecarResolveError::Unknown(m) => WitProcessError::UnknownSidecar(m),
            SidecarResolveError::NotGranted(m) => WitProcessError::PermissionDenied(m),
            SidecarResolveError::NotConfigured(m) => WitProcessError::NotConfigured(m),
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
    /// See `crate::host::plugin::HostCtx::ensure_started` for the residual
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

    /// See `crate::host::plugin::HostCtx::stop`'s doc comment: uses the
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
        if !crate::runtime::sidecar::parse_sidecars(&raw).contains_key(&name) {
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
    /// `filesystem_json` から当該ルートの実パスと mode を解決する。判定自体は
    /// `resolve::resolve_root`(`crate::host::plugin::HostCtx::resolve_root`
    /// と共有)へ委譲し、ここでは variant を写像するだけ。
    fn resolve_root(&self, root: &str, need_write: bool) -> Result<std::path::PathBuf, WitFsError> {
        let raw = self
            .filesystem_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let entries = crate::runtime::fs::parse_filesystem(&raw);

        resolve_root(&entries, root, need_write).map_err(|e| match e {
            RootResolveError::Unknown(m) => WitFsError::UnknownRoot(m),
            RootResolveError::NotGranted(m) => WitFsError::PermissionDenied(m),
            RootResolveError::NotConfigured(m) => WitFsError::NotConfigured(m),
            RootResolveError::ReadOnly(m) => WitFsError::PermissionDenied(m),
        })
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

/// Owns the wasmtime `Engine`/epoch ticker (`EpochEngine`) and the shared
/// http/process/fs drivers (`SharedDrivers`) for every driver instance
/// loaded from this host.
pub struct DriverHost {
    engine: EpochEngine,
    drivers: SharedDrivers,
}

impl DriverHost {
    /// `handle` はデーモンの tokio runtime のもの(`SharedDrivers::new` 参照)。
    pub fn new(handle: tokio::runtime::Handle) -> anyhow::Result<DriverHost> {
        let engine = EpochEngine::new()?;
        let drivers = SharedDrivers::new(DRIVER_HTTP_TIMEOUT, handle)?;

        Ok(DriverHost { engine, drivers })
    }

    /// Returns a clone of the shared `HttpDriver` `Arc` for wiring into a
    /// new driver's `DriverCtx`. Cloning an `Arc` is cheap; this does not
    /// build a new HTTP client.
    pub fn http_driver(&self) -> Arc<edlr_driver_http::HttpDriver> {
        self.drivers.http()
    }

    /// Returns a clone of the shared `ProcessDriver` `Arc` for wiring into a
    /// new driver's `DriverCtx`. Cloning an `Arc` is cheap; this does not
    /// spawn or otherwise touch any sidecar process.
    pub fn process_driver(&self) -> Arc<edlr_driver_process::ProcessDriver> {
        self.drivers.process()
    }

    /// Returns a clone of the shared `FsDriver` `Arc` for wiring into a new
    /// driver's `DriverCtx`. Cloning an `Arc` is cheap; this does not touch
    /// any file.
    pub fn fs_driver(&self) -> Arc<edlr_driver_fs::FsDriver> {
        self.drivers.fs()
    }

    pub fn load(&self, wasm_path: &Path, ctx: DriverCtx) -> anyhow::Result<DriverInstance> {
        let engine = self.engine.engine();
        let component = Component::from_file(engine, wasm_path).map_err(|e| {
            anyhow::anyhow!("failed to load component at {}: {e}", wasm_path.display())
        })?;

        let mut linker = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| anyhow::anyhow!("failed to wire WASI imports into linker: {e}"))?;
        DriverBindings::add_to_linker::<_, HasSelf<_>>(&mut linker, |ctx| ctx)
            .map_err(|e| anyhow::anyhow!("failed to wire host imports into linker: {e}"))?;

        let mut store = Store::new(engine, ctx);
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
        self.engine.stop_ticker();
        // 明示 shutdown 経路と同じく、`DriverHost` が消えるときにサイドカー
        // の孤児を残さない最後の砦として、無条件に全サイドカーを止める。
        self.drivers.process().stop_all();
    }
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

    /// `submit-send` 系ジョブの完了を届ける。呼ばれる時点で結果は揃って
    /// いるので、他の export と同じ同期呼び出し・同じ epoch deadline。
    pub fn call_on_job_complete(&mut self, job_id: u64, result_json: &str) -> anyhow::Result<()> {
        self.store
            .set_epoch_deadline(deadline_ticks(Self::CALL_DEADLINE));
        self.bindings
            .call_on_job_complete(&mut self.store, job_id, result_json)
            .map_err(|e| anyhow::anyhow!("driver on-job-complete() call failed or timed out: {e}"))
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

    /// submit 系を使わないテスト用のドライバ作業キュー送信側。受信側は
    /// `forget` で生かしたままにする(host/plugin.rs の `test_work_tx` と
    /// 同じ流儀)。
    fn test_work_tx() -> crate::runner::plugin::queue::WorkSender<crate::runner::driver::DriverWork>
    {
        let (tx, rx) = crate::runner::plugin::queue::channel_for(
            crate::runner::driver::admit_driver_work,
        );
        std::mem::forget(rx);
        tx
    }

    /// submit 系テスト用: 作業キューの受信側も返すビルダ。
    fn test_driver_ctx_with_queue(
        hosts_json: &str,
    ) -> (
        DriverCtx,
        crate::runner::plugin::queue::WorkReceiver<crate::runner::driver::DriverWork>,
    ) {
        let (work_tx, work_rx) = crate::runner::plugin::queue::channel_for(
            crate::runner::driver::admit_driver_work,
        );
        let ctx = DriverCtx::new(
            "ed-state".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(hosts_json.to_string())),
            Arc::new(Mutex::new("[]".to_string())),
            Arc::new(Mutex::new("[]".to_string())),
            edlr_driver_channel::Bus::new(),
            Arc::new(
                edlr_driver_http::HttpDriver::new(DRIVER_HTTP_TIMEOUT, HTTP_MAX_BODY, crate::host::drivers::test_handle())
                    .expect("http driver builds"),
            ),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                SIDECAR_SHUTDOWN_GRACE,
                SIDECAR_SPAWN_MIN_INTERVAL,
            )),
            Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT)),
            work_tx,
            crate::host::plugin::PluginJobs::new(),
        );
        (ctx, work_rx)
    }

    /// submit-send: 未承認ホストは同期の permission-denied で、ジョブは
    /// 始まらない(完了通知も来ない)。
    #[test]
    fn submit_send_without_a_grant_is_a_synchronous_permission_denied() {
        let (mut ctx, work_rx) = test_driver_ctx_with_queue(r#"{"hosts":[]}"#);
        let err = ctx
            .submit_send(
                WitRequest {
                    method: "GET".to_string(),
                    url: "https://example.com/x".to_string(),
                    headers: Vec::new(),
                    body: None,
                },
                None,
            )
            .expect_err("submit without a grant must be rejected synchronously");
        assert!(matches!(err, WitDriverError::PermissionDenied(_)));
        assert!(work_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err());
    }

    /// submit-send: 受付は成功し、transport 失敗が `DriverWork::JobComplete`
    /// の err 結果として非同期に届く(プラグイン側と同じセマンティクス)。
    #[test]
    fn submit_send_transport_failures_arrive_as_err_results() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);

        let (mut ctx, work_rx) = test_driver_ctx_with_queue(&format!(
            r#"{{"granted":true,"hosts":["http://{addr}"]}}"#
        ));
        let job_id = ctx
            .submit_send(
                WitRequest {
                    method: "GET".to_string(),
                    url: format!("http://{addr}/"),
                    headers: Vec::new(),
                    body: None,
                },
                None,
            )
            .expect("submission itself succeeds; the failure arrives asynchronously");
        assert_eq!(job_id, 1, "job ids start at 1");

        match work_rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(crate::runner::driver::DriverWork::JobComplete {
                generation,
                job_id: completed,
                result_json,
            }) => {
                assert_eq!(generation, 0);
                assert_eq!(completed, job_id);
                let value: serde_json::Value = serde_json::from_str(&result_json).unwrap();
                assert_eq!(value["err"]["kind"], "transport");
            }
            other => panic!("expected DriverWork::JobComplete, got {other:?}"),
        }
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
                edlr_driver_http::HttpDriver::new(DRIVER_HTTP_TIMEOUT, HTTP_MAX_BODY, crate::host::drivers::test_handle())
                    .expect("http driver builds"),
            ),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                SIDECAR_SHUTDOWN_GRACE,
                SIDECAR_SPAWN_MIN_INTERVAL,
            )),
            Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT)),
            test_work_tx(),
            crate::host::plugin::PluginJobs::new(),
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
                edlr_driver_http::HttpDriver::new(DRIVER_HTTP_TIMEOUT, HTTP_MAX_BODY, crate::host::drivers::test_handle())
                    .expect("http driver builds"),
            ),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                SIDECAR_SHUTDOWN_GRACE,
                SIDECAR_SPAWN_MIN_INTERVAL,
            )),
            Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT)),
            test_work_tx(),
            crate::host::plugin::PluginJobs::new(),
        )
    }

    fn sidecar_entry(granted: bool, command: &str) -> crate::runtime::sidecar::SidecarRuntimeEntry {
        crate::runtime::sidecar::SidecarRuntimeEntry {
            name: "tts".to_string(),
            granted,
            command: command.to_string(),
            args: vec![],
            ports: vec![],
        }
    }

    fn fs_entry(granted: bool, mode: &str, path: &str) -> crate::runtime::fs::FsRuntimeEntry {
        crate::runtime::fs::FsRuntimeEntry {
            name: "exports".to_string(),
            granted,
            mode: mode.to_string(),
            path: path.to_string(),
            target: "directory".to_string(),
        }
    }

    #[test]
    fn ensure_started_without_grant_is_permission_denied() {
        use crate::runtime::sidecar::sidecars_json_string;
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
        use crate::runtime::sidecar::sidecars_json_string;
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
        use crate::runtime::sidecar::sidecars_json_string;
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
        use crate::runtime::fs::filesystem_json_string;
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
        use crate::runtime::fs::filesystem_json_string;
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

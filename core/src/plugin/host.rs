//! wasmtime component host: loads plugin components, wires host-log /
//! host-settings imports, and enforces a per-call deadline via epoch
//! interruption so a runaway guest traps instead of hanging the kernel.

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
        world: "plugin",
    });
}

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
use bindings::{Event as WitEvent, Plugin as PluginBindings};

use crate::plugin::allowlist::check_url;

/// Public re-exports of the generated `driver-process` WIT types, for the
/// same reason `driver-http`'s equivalents are re-exported above: in-tree
/// consumers (unit tests here and, later, integration tests) call
/// `HostCtx::ensure_started`/`stop`/`status` directly rather than through a
/// full wasm round-trip.
pub use bindings::edlr::plugin::driver_process::{
    DriverError as WitSidecarError, Host as WitDriverProcessHost, Instance as WitSidecarInstance,
    InstanceState as WitSidecarInstanceState,
};

/// Public re-exports of the generated `driver-http` WIT types, under names
/// that don't collide with `edlr_driver_http`'s own (structurally identical
/// but distinct) `Http*` types. These exist so that in-tree consumers --
/// currently `core/tests/driver_http_integration.rs` -- can call
/// `HostCtx::send` (the WIT-facing entry point) directly, without a full
/// wasm round-trip, the same way this module's own unit tests do. The
/// generated `bindings` module otherwise stays private.
pub use bindings::edlr::plugin::driver_http::{
    DriverError as WitHttpError, Host as WitDriverHttpHost, Request as WitHttpRequest,
    Response as WitHttpResponse,
};

/// Public re-exports of the generated `driver-fs` WIT types, for the same
/// reason `driver-process`'s equivalents are re-exported above: in-tree
/// consumers (this module's own unit tests) call
/// `HostCtx::read`/`read_range`/`stat`/`list`/`write`/`append`/`delete`
/// directly rather than through a full wasm round-trip.
pub use bindings::edlr::plugin::driver_fs::{
    DriverError as WitDriverFsError, Entry as WitFsDriverEntry, Host as WitDriverFsHost,
};

/// Interval between epoch ticks driven by the background ticker thread.
const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Per-call timeout applied to every `driver-http.send` request (covers
/// connect through to the full response). Fixed for now; could become
/// configurable per-plugin later if a legitimate use case needs it.
///
/// Epoch interruption (see `PluginInstance::CALL_DEADLINE`) only fires at
/// wasm instruction boundaries, so it cannot preempt a blocking host call:
/// while a guest is inside `driver-http.send`, execution is in host (Rust)
/// code, not wasm, so no epoch check ever runs until the host call returns.
/// If `HTTP_TIMEOUT` were allowed to exceed `CALL_DEADLINE`, a single
/// `driver-http.send` call could occupy a plugin thread for far longer than
/// the documented per-call deadline -- against a host the plugin's own
/// author controls, this is trivial to trigger deliberately. Keeping
/// `HTTP_TIMEOUT` strictly less than `CALL_DEADLINE` (enforced by the const
/// assertion below) makes the HTTP driver's own timeout the binding
/// constraint for `driver-http.send` specifically, so it can never be the
/// reason a guest call runs longer than `CALL_DEADLINE` was meant to allow.
pub const HTTP_TIMEOUT: Duration = Duration::from_millis(1_500);

const _: () = assert!(
    HTTP_TIMEOUT.as_millis() < PluginInstance::CALL_DEADLINE.as_millis(),
    "HTTP_TIMEOUT must stay strictly under PluginInstance::CALL_DEADLINE -- see HTTP_TIMEOUT's doc comment"
);

/// Maximum response body size, in bytes, a `driver-http.send` call will
/// return before failing with a `transport` error. See
/// `edlr_driver_http::HttpDriver::send` for how this is enforced (a
/// streaming read capped at this limit, not a trusted `Content-Length`
/// header).
pub const HTTP_MAX_BODY: usize = 8 * 1024 * 1024;

/// `driver-fs` の 1 回の読み取り上限。`HTTP_MAX_BODY` と同値。ホスト側の
/// バッファを無制限にしないためのもので、扱えるファイルサイズの上限では
/// ない(超えるものは `stat` + `read-range` で分割して読む)。
pub const FS_READ_LIMIT: usize = HTTP_MAX_BODY;

/// `list` が返すエントリ数の上限。呼び出し期限(`CALL_DEADLINE`)を
/// 食い潰さないための保護。
pub const FS_LIST_LIMIT: usize = 10_000;

/// サイドカー停止時、SIGTERM から SIGKILL へ昇格するまでの猶予。
///
/// 値は `edlr_config::SIDECAR_SHUTDOWN_GRACE_SECS` から取る(ハードコードしない)。
/// `ui/src-tauri` の `daemon::STOP_GRACE` はこの値(× 想定インスタンス数上限)を
/// 超えることをコンパイル時アサーションで固定しているため、ここを直接
/// `Duration::from_secs(3)` のように書き直すと、その関係が経由する共有定数の
/// 意味が壊れる(2 つの crate の猶予が独立に変更されうる状態に戻ってしまう)。
pub const SIDECAR_SHUTDOWN_GRACE: Duration =
    Duration::from_secs(edlr_config::SIDECAR_SHUTDOWN_GRACE_SECS);

/// 同一サイドカーの spawn 試行の最短間隔。プラグインがループで
/// `ensure-started` を呼んでも spawn 嵐にならないための下限。
pub const SIDECAR_SPAWN_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum linear memory (bytes) a single plugin instance may allocate.
///
/// The epoch deadline (see `PluginInstance::CALL_DEADLINE`) bounds how long a
/// guest call may run *while executing wasm instructions* -- it says nothing
/// about how much memory a call may claim while doing so, nor does it bound
/// time spent blocked inside a host call such as `driver-http.send` (see
/// `HTTP_TIMEOUT`'s doc comment for that half of the story). Without a
/// memory cap a plugin can grow its linear memory without bound and OOM-kill
/// the whole daemon, defeating the isolation the plugin host is meant to
/// provide. 64 MiB is a generous ceiling for the kind of small,
/// single-purpose plugins this host targets (log formatters, simple
/// notifiers, ...); it is a fixed constant for now but can be made
/// configurable per-plugin later if a legitimate use case needs more.
const PLUGIN_MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Maximum number of component instances / tables a single plugin `Store`
/// may create. Plugins in this host are single-component and single-table by
/// construction; these caps are a cheap extra guard against a
/// pathological/malicious guest rather than a limit anyone should expect to
/// approach in normal use.
const PLUGIN_INSTANCE_LIMIT: usize = 8;
const PLUGIN_TABLE_LIMIT: usize = 8;

/// Per-plugin-instance host state, exposed to the guest via the generated
/// `Host` traits.
pub struct HostCtx {
    pub plugin_id: String,
    /// Effective settings JSON string. Wrapped so the runner can swap the
    /// value in place between calls without reloading the plugin.
    pub settings_json: Arc<Mutex<String>>,
    /// Capability grant state JSON string, shape `{"hosts": ["https://..."]}`
    /// -- the *effective* set of hosts this plugin instance may reach via
    /// `driver-http.send`. Built by the runner/`Registry` from `GrantsStore`,
    /// the manifest's requested hosts, and any approved sidecars' implicit
    /// `127.0.0.1` origins (see `sidecar_runtime::implicit_http_hosts`
    /// combined together), and readable/writable live via the same `Arc`
    /// the `Registry` holds -- so approving/revoking a capability takes
    /// effect on the very next `driver-http.send` call, no plugin restart
    /// required.
    ///
    /// This is the *only* source the `driver-http` host implementation
    /// consults to decide whether a call is permitted: the guest never
    /// passes its own id or grants as an argument, so a plugin cannot
    /// forge or observe another plugin's capability state, nor influence
    /// the decision through its inputs.
    pub capabilities_json: Arc<Mutex<String>>,
    /// サイドカーの承認状態と実行に必要な値の共有バッファ。形は
    /// `sidecar_runtime::sidecars_json_string` を参照。`capabilities_json`
    /// と同じく `Registry` が承認・設定変更のたびに上書きする。
    pub sidecars_json: Arc<Mutex<String>>,
    /// ファイルシステムルートの承認状態と実パスの共有バッファ。形は
    /// `fs_runtime::filesystem_json_string` を参照。`sidecars_json` と同じく
    /// `Registry` が承認・設定変更のたびに上書きする。
    pub filesystem_json: Arc<Mutex<String>>,
    /// The HTTP client used by `driver-http.send` once a call has passed
    /// the permission check above. Shared (via `Arc`) across every plugin
    /// instance the owning `PluginHost` loads -- see `PluginHost::new`,
    /// which builds exactly one `HttpDriver` for the whole host process, so
    /// construction cost (TLS setup, etc.) is paid once rather than per
    /// call or per plugin.
    http_driver: Arc<edlr_driver_http::HttpDriver>,
    /// サイドカープロセスを実際に所有するドライバ。`http_driver` と同様、
    /// `PluginHost` が 1 つだけ持ち、全プラグインインスタンスで共有する。
    /// プロセスは `<plugin-id>/<sidecar-name>` をキーに分離されるため、
    /// 共有していても他プラグインのプロセスには触れられない。
    process_driver: Arc<edlr_driver_process::ProcessDriver>,
    /// 承認済みルート配下でのファイル操作を実際に行うドライバ。
    /// `http_driver` / `process_driver` と同様、`PluginHost` が 1 つだけ
    /// 持ち、全プラグインインスタンスで共有する。ルートの実パスは
    /// `filesystem_json` から呼び出しごとに解決されるため、共有していても
    /// 他プラグインのルートには触れられない。
    fs_driver: Arc<edlr_driver_fs::FsDriver>,
    /// WASI state. The `plugin` world itself does not import WASI, but
    /// components built for the `wasm32-wasip2` target still import a
    /// baseline set of WASI interfaces (io, random, clocks, ...) from the
    /// Rust standard library / adapter, so the host must satisfy them.
    wasi_ctx: WasiCtx,
    wasi_table: ResourceTable,
    /// Resource limits (memory/instances/tables) enforced on this plugin's
    /// `Store` via `Store::limiter`. See `PLUGIN_MEMORY_LIMIT`.
    limits: StoreLimits,
}

impl HostCtx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plugin_id: String,
        settings_json: Arc<Mutex<String>>,
        capabilities_json: Arc<Mutex<String>>,
        sidecars_json: Arc<Mutex<String>>,
        filesystem_json: Arc<Mutex<String>>,
        http_driver: Arc<edlr_driver_http::HttpDriver>,
        process_driver: Arc<edlr_driver_process::ProcessDriver>,
        fs_driver: Arc<edlr_driver_fs::FsDriver>,
    ) -> HostCtx {
        HostCtx {
            plugin_id,
            settings_json,
            capabilities_json,
            sidecars_json,
            filesystem_json,
            http_driver,
            process_driver,
            fs_driver,
            // Deliberately empty sandbox default: no preopened directories,
            // no stdio, no network access, so a plugin can only interact
            // with the host through the `plugin` world's explicit imports.
            wasi_ctx: WasiCtxBuilder::new().build(),
            wasi_table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(PLUGIN_MEMORY_LIMIT)
                .instances(PLUGIN_INSTANCE_LIMIT)
                .tables(PLUGIN_TABLE_LIMIT)
                .trap_on_grow_failure(true)
                .build(),
        }
    }
}

impl WasiView for HostCtx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.wasi_table,
        }
    }
}

impl HostLogHost for HostCtx {
    fn log(&mut self, level: WitLevel, message: String) {
        let plugin_id = self.plugin_id.as_str();
        match level {
            WitLevel::Debug => tracing::debug!(plugin_id, "{message}"),
            WitLevel::Info => tracing::info!(plugin_id, "{message}"),
            WitLevel::Warn => tracing::warn!(plugin_id, "{message}"),
            WitLevel::Error => tracing::error!(plugin_id, "{message}"),
        }
    }
}

impl HostSettingsHost for HostCtx {
    fn get_all(&mut self) -> String {
        self.settings_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

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

/// `capabilities_json` から許可ホスト一覧だけを取り出す。シリアライズ形式は
/// `capabilities_json_string` を参照。共有バッファが万一不正な/想定外の
/// 形の JSON を持っていても(このホスト実装自身はそのような値を書かないが、
/// `Arc<Mutex<String>>` は防御的にパースする)「何も許可されていない」に
/// フォールバックする。
pub(crate) fn parse_capability_hosts(raw: &str) -> Vec<String> {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    value
        .get("hosts")
        .and_then(serde_json::Value::as_array)
        .map(|hosts| {
            hosts
                .iter()
                .filter_map(|h| h.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

impl DriverHttpHost for HostCtx {
    /// First decides *whether* the call is permitted, then -- only if so --
    /// performs it: not granted -> `permission-denied`; granted but the URL
    /// is outside the plugin's allowlisted hosts -> `permission-denied`
    /// with the allowlist rejection reason; otherwise the request is
    /// handed to `self.http_driver`, and its result is mapped onto the WIT
    /// types (`HttpError::InvalidRequest` -> `invalid-request`,
    /// `HttpError::Transport` -> `transport`).
    ///
    /// The permission decision is made *entirely* from
    /// `self.capabilities_json`, which is per-`HostCtx` (i.e. per plugin
    /// instance) and never derived from `req`. The guest supplies only the
    /// URL it wants to reach; it has no way to supply or influence its own
    /// grant state. Critically, the driver is never invoked until *after*
    /// this check succeeds -- a disallowed host is rejected without any
    /// network connection being attempted.
    fn send(&mut self, req: WitRequest) -> Result<WitResponse, WitDriverError> {
        let raw = self
            .capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let hosts = parse_capability_hosts(&raw);
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
    /// **既知の残存レース(意図的、`Registry::control_sidecar` のドキュメント
    /// コメント参照)**: `resolve_sidecar` は `sidecars_json` を読んだ
    /// あと、何のロックも保持せずに `process_driver.ensure_started` を呼ぶ。
    /// `Registry::control_sidecar`(ホスト起点/RPC 経路)は
    /// `sidecar_runtime_lock_for(id)` を取ってこの TOCTOU を閉じたが、
    /// ここ(ゲスト起点の経路)には同じロックを取らせていない: そうすると
    /// `refresh_sidecar_runtime`(承認取消・設定変更)が同じロックを持って
    /// `shutdown_grace`(既定 3 秒)まで `stop` を待つ間、この呼び出しをした
    /// プラグインの専用スレッドがブロックされ、`PluginInstance::
    /// CALL_DEADLINE`(2 秒)を超えて trap してしまう -- このブランチで
    /// 繰り返し守ってきた「ゲスト呼び出しをブロックさせない」制約に反する。
    /// 残るレース窓は「承認取消/設定変更が `sidecars_json` バッファに反映
    /// されてから、この呼び出しが次にそれを読むまで」の間だけで、次回の
    /// `ensure-started`/`status` 呼び出しでは新しい状態が見え、既に走って
    /// しまったインスタンスはホスト側(UI/RPC の `sidecar-control` や
    /// `set_sidecar_grant`/`set_sidecar_config` 経由の停止)から止められる。
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
        // permission-denied では停止できないと困る)。この非対称性
        // (`status`/`ensure_started` は `resolve_sidecar` を通るので承認
        // 取消直後は permission-denied になるが、`stop` は取消後も動く)は
        // 意図的: 承認を取り消された直後でも、プラグイン自身が起動した
        // サイドカーを自分で止められる経路は残しておきたい。
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
        // ゲスト呼び出し向けに fire-and-forget 版を使う: 同期版の `stop` は
        // SIGTERM 送信から `shutdown_grace`(既定 3 秒)経過後の SIGKILL まで
        // 呼び出しスレッドをブロックしうるが、これは
        // `PluginInstance::CALL_DEADLINE`(2 秒)を超え、かつエポック割り込み
        // はホスト呼び出し中は作動しないため trap もされない -- プラグイン
        // 専用スレッドが最大 3 秒詰まり、その間のイベントキューが滞留する。
        // `stop_detached` は「`Child` を detach し `terminating` を立てる」
        // ところまでを同期的に行ってから返るので、`status`/`ensure_started`
        // から見た効果(re-spawn 抑止・running=false の報告)は即座に効く。
        // 実際の kill/wait 待ちだけをバックグラウンドスレッドへ逃がす。
        // ホスト起点の停止(デーモン shutdown の `stop_all` や、将来
        // `Registry` が呼ぶ停止)は「戻った時点で確実に死んでいる」ことに
        // 依存するため、そちらは同期版のままにする。
        self.process_driver.clone().stop_detached(&key);
        Ok(())
    }

    fn status(&mut self, name: String) -> Result<Vec<WitInstance>, WitProcessError> {
        let spec = self.resolve_sidecar(&name)?;
        let key = self.sidecar_key(&name);
        Ok(to_wit_instances(self.process_driver.status(&key, &spec)))
    }
}

impl HostCtx {
    /// `filesystem_json` から当該ルートの実パスと mode を解決する。
    ///
    /// `driver-http` / `driver-process` と同じく、判定材料は全て `HostCtx`
    /// 側にあり、ゲストが渡すのはルート名と相対パスだけ。判定順は
    /// `resolve_sidecar` と揃える:「存在するか」→「承認済みか」→
    /// 「設定済みか」→(書き込み系なら)「mode が read-write か」。
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

impl DriverFsHost for HostCtx {
    /// `resolve_root` → `fs_driver` の対応メソッド呼び出し → `FsError` を
    /// WIT の variant へ写像するだけ。パス検証・原子的書き込み・サイズ上限
    /// はすべて `edlr_driver_fs::FsDriver` の責務で、ここでは持たない。
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
/// call deadlines for every plugin instance loaded from this host.
pub struct PluginHost {
    engine: Engine,
    ticker_stop: Arc<AtomicBool>,
    /// The single `HttpDriver` shared by every plugin instance this host
    /// loads. Built once here (not per plugin, not per call) since
    /// constructing a `reqwest::blocking::Client` sets up TLS/connection
    /// pool state that's meant to be reused.
    http_driver: Arc<edlr_driver_http::HttpDriver>,
    /// The single `ProcessDriver` shared by every plugin instance this host
    /// loads, mirroring `http_driver` above. Sidecar processes are keyed by
    /// `<plugin-id>/<sidecar-name>` (see `HostCtx::sidecar_key`), so sharing
    /// this one driver across all plugins does not let one plugin observe or
    /// touch another's sidecars.
    process_driver: Arc<edlr_driver_process::ProcessDriver>,
    /// The single `FsDriver` shared by every plugin instance this host
    /// loads, mirroring `http_driver`/`process_driver` above. Root paths
    /// are resolved per call from each plugin's own `filesystem_json`, so
    /// sharing this one driver across all plugins does not let one plugin
    /// reach another's roots.
    fs_driver: Arc<edlr_driver_fs::FsDriver>,
}

impl PluginHost {
    pub fn new() -> anyhow::Result<PluginHost> {
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
            edlr_driver_http::HttpDriver::new(HTTP_TIMEOUT, HTTP_MAX_BODY)
                .map_err(|e| anyhow::anyhow!("failed to build http driver: {e}"))?,
        );
        let process_driver = Arc::new(edlr_driver_process::ProcessDriver::new(
            SIDECAR_SHUTDOWN_GRACE,
            SIDECAR_SPAWN_MIN_INTERVAL,
        ));
        let fs_driver = Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT));

        Ok(PluginHost {
            engine,
            ticker_stop,
            http_driver,
            process_driver,
            fs_driver,
        })
    }

    /// Returns a clone of the shared `HttpDriver` `Arc` for wiring into a
    /// new plugin's `HostCtx`. Cloning an `Arc` is cheap; this does not
    /// build a new HTTP client.
    pub fn http_driver(&self) -> Arc<edlr_driver_http::HttpDriver> {
        self.http_driver.clone()
    }

    /// Returns a clone of the shared `ProcessDriver` `Arc` for wiring into a
    /// new plugin's `HostCtx`. Cloning an `Arc` is cheap; this does not spawn
    /// or otherwise touch any sidecar process.
    pub fn process_driver(&self) -> Arc<edlr_driver_process::ProcessDriver> {
        self.process_driver.clone()
    }

    /// Returns a clone of the shared `FsDriver` `Arc` for wiring into a new
    /// plugin's `HostCtx`. Cloning an `Arc` is cheap; this does not touch
    /// any file.
    pub fn fs_driver(&self) -> Arc<edlr_driver_fs::FsDriver> {
        self.fs_driver.clone()
    }

    pub fn load(&self, wasm_path: &Path, ctx: HostCtx) -> anyhow::Result<PluginInstance> {
        let component = Component::from_file(&self.engine, wasm_path).map_err(|e| {
            anyhow::anyhow!("failed to load component at {}: {e}", wasm_path.display())
        })?;

        let mut linker = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| anyhow::anyhow!("failed to wire WASI imports into linker: {e}"))?;
        PluginBindings::add_to_linker::<_, HasSelf<_>>(&mut linker, |ctx| ctx)
            .map_err(|e| anyhow::anyhow!("failed to wire host imports into linker: {e}"))?;

        let mut store = Store::new(&self.engine, ctx);
        store.limiter(|ctx| &mut ctx.limits);
        // Ticks-beyond-current is set fresh before every call in
        // `PluginInstance::call`; this initial deadline just prevents
        // instantiation itself (which may run guest start code) from
        // hanging forever.
        store.set_epoch_deadline(deadline_ticks(PluginInstance::CALL_DEADLINE));

        let bindings = PluginBindings::instantiate(&mut store, &component, &linker)
            .map_err(|e| anyhow::anyhow!("failed to instantiate plugin component: {e}"))?;

        Ok(PluginInstance { store, bindings })
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
        // 明示 shutdown 経路(daemon 終了時の一括停止など)は Task 6 で配線
        // する。ここでは「`PluginHost` が消えるときにサイドカーの孤児を
        // 残さない」という最後の砦として、無条件に全サイドカーを止める。
        self.process_driver.stop_all();
    }
}

/// Number of epoch ticks corresponding to `duration`, rounded up, with a
/// minimum of one tick so a zero-length deadline still traps promptly.
fn deadline_ticks(duration: Duration) -> u64 {
    let ticks = duration.as_nanos().div_ceil(EPOCH_TICK_INTERVAL.as_nanos());
    u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
}

/// A loaded, instantiated plugin component together with its store.
pub struct PluginInstance {
    store: Store<HostCtx>,
    bindings: PluginBindings,
}

impl PluginInstance {
    /// Maximum wall-clock time a single guest call may take before the host
    /// forcibly traps it via epoch interruption.
    pub const CALL_DEADLINE: Duration = Duration::from_secs(2);

    pub fn call_init(&mut self) -> anyhow::Result<()> {
        self.store
            .set_epoch_deadline(deadline_ticks(Self::CALL_DEADLINE));
        self.bindings
            .call_init(&mut self.store)
            .map_err(|e| anyhow::anyhow!("plugin init() call failed or timed out: {e}"))
    }

    pub fn call_on_event(
        &mut self,
        kind: &str,
        timestamp: Option<&str>,
        name: Option<&str>,
        payload_json: &str,
        replay: bool,
    ) -> anyhow::Result<()> {
        let event = WitEvent {
            kind: kind.to_string(),
            timestamp: timestamp.map(|s| s.to_string()),
            name: name.map(|s| s.to_string()),
            payload_json: payload_json.to_string(),
            replay,
        };
        self.store
            .set_epoch_deadline(deadline_ticks(Self::CALL_DEADLINE));
        self.bindings
            .call_on_event(&mut self.store, &event)
            .map_err(|e| anyhow::anyhow!("plugin on-event() call failed or timed out: {e}"))
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for `DriverHttpHost::send` and `DriverProcessHost::{
    //! ensure_started, stop, status}` called directly against `HostCtx`,
    //! without going through wasm at all. This is possible (and preferable
    //! to a full wasm round-trip for exercising the permission logic)
    //! because the whole point of the design is that the decision is made
    //! purely from `HostCtx`'s own `capabilities_json`/`sidecars_json`,
    //! never from anything the guest passes in as an argument -- so a
    //! host-side call with a hand-built request exercises exactly the same
    //! decision path a real guest call would.
    use super::*;

    fn test_http_driver() -> Arc<edlr_driver_http::HttpDriver> {
        Arc::new(
            edlr_driver_http::HttpDriver::new(HTTP_TIMEOUT, HTTP_MAX_BODY)
                .expect("build test http driver"),
        )
    }

    fn test_fs_driver() -> Arc<edlr_driver_fs::FsDriver> {
        Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT))
    }

    fn ctx(capabilities_json: &str) -> HostCtx {
        HostCtx::new(
            "test-plugin".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(capabilities_json.to_string())),
            Arc::new(Mutex::new("[]".to_string())),
            Arc::new(Mutex::new("[]".to_string())),
            test_http_driver(),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                SIDECAR_SHUTDOWN_GRACE,
                SIDECAR_SPAWN_MIN_INTERVAL,
            )),
            test_fs_driver(),
        )
    }

    fn request(url: &str) -> WitRequest {
        WitRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
        }
    }

    #[test]
    fn capabilities_json_string_carries_the_effective_hosts() {
        let json = capabilities_json_string(&["https://api.example.com".to_string()]);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["hosts"],
            serde_json::json!(["https://api.example.com"])
        );
    }

    #[test]
    fn empty_effective_hosts_means_nothing_is_permitted() {
        let mut ctx = ctx(&capabilities_json_string(&[]));
        let err = ctx
            .send(request("https://api.example.com/ping"))
            .expect_err("no effective hosts means every call is denied");
        assert!(matches!(err, WitDriverError::PermissionDenied(_)));
    }

    #[test]
    fn send_granted_but_disallowed_host_is_permission_denied() {
        let mut ctx = ctx(&capabilities_json_string(&[
            "https://api.example.com".to_string()
        ]));

        let err = ctx
            .send(request("https://evil.example.com/ping"))
            .expect_err("call to a non-allowlisted host should be rejected");

        assert!(matches!(err, WitDriverError::PermissionDenied(_)));
    }

    /// Once permission checks pass, `send` must actually dispatch to the
    /// driver rather than short-circuiting -- this is what task 3's
    /// `not-implemented` placeholder used to do, and this task replaces it
    /// with real networking. This test doesn't hit a live server (see
    /// `core/tests/driver_http_integration.rs` for the real end-to-end
    /// cases); it targets a bound-then-dropped local port, which is
    /// guaranteed to refuse the connection, to prove the call reaches the
    /// driver and comes back as a typed `transport` error rather than a
    /// `permission-denied` or a panic.
    #[test]
    fn send_granted_and_allowed_host_reaches_the_driver() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);
        let url = format!("http://{addr}/ping");

        let mut ctx = ctx(&capabilities_json_string(&[format!("http://{addr}")]));

        let err = ctx
            .send(request(&url))
            .expect_err("connection to a closed local port should fail");

        assert!(matches!(err, WitDriverError::Transport(_)));
    }

    fn sidecar_ctx(sidecars_json: &str) -> HostCtx {
        HostCtx::new(
            "test-plugin".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(capabilities_json_string(&[]))),
            Arc::new(Mutex::new(sidecars_json.to_string())),
            Arc::new(Mutex::new("[]".to_string())),
            test_http_driver(),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                Duration::from_millis(200),
                Duration::from_secs(1),
            )),
            test_fs_driver(),
        )
    }

    fn fs_ctx(filesystem_json: &str) -> HostCtx {
        HostCtx::new(
            "test-plugin".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(capabilities_json_string(&[]))),
            Arc::new(Mutex::new("[]".to_string())),
            Arc::new(Mutex::new(filesystem_json.to_string())),
            test_http_driver(),
            Arc::new(edlr_driver_process::ProcessDriver::new(
                Duration::from_millis(200),
                Duration::from_secs(1),
            )),
            test_fs_driver(),
        )
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
    fn fs_calls_without_grant_are_permission_denied() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = fs_ctx(&filesystem_json_string(&[fs_entry(
            false,
            "read-write",
            &dir.path().to_string_lossy(),
        )]));

        let err = ctx
            .read("exports".to_string(), "a.txt".to_string())
            .expect_err("ungranted root must be denied");
        assert!(matches!(err, WitFsError::PermissionDenied(_)));
    }

    #[test]
    fn unknown_root_is_reported_as_such() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = fs_ctx(&filesystem_json_string(&[fs_entry(
            true,
            "read-write",
            &dir.path().to_string_lossy(),
        )]));

        let err = ctx
            .read("nope".to_string(), "a.txt".to_string())
            .expect_err("unknown root");
        assert!(matches!(err, WitFsError::UnknownRoot(_)));
    }

    #[test]
    fn granted_but_unconfigured_root_is_not_configured() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let mut ctx = fs_ctx(&filesystem_json_string(&[fs_entry(true, "read-write", "")]));

        let err = ctx
            .read("exports".to_string(), "a.txt".to_string())
            .expect_err("no directory configured");
        assert!(matches!(err, WitFsError::NotConfigured(_)));
    }

    #[test]
    fn read_mode_rejects_every_mutating_call() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut ctx = fs_ctx(&filesystem_json_string(&[fs_entry(
            true,
            "read",
            &dir.path().to_string_lossy(),
        )]));

        assert!(ctx.read("exports".to_string(), "a.txt".to_string()).is_ok());
        assert!(matches!(
            ctx.write("exports".to_string(), "a.txt".to_string(), vec![1])
                .expect_err("write under read mode"),
            WitFsError::PermissionDenied(_)
        ));
        assert!(matches!(
            ctx.append("exports".to_string(), "a.txt".to_string(), vec![1])
                .expect_err("append under read mode"),
            WitFsError::PermissionDenied(_)
        ));
        assert!(matches!(
            ctx.delete("exports".to_string(), "a.txt".to_string())
                .expect_err("delete under read mode"),
            WitFsError::PermissionDenied(_)
        ));
    }

    #[test]
    fn granted_read_write_root_round_trips_and_still_refuses_escapes() {
        use crate::plugin::fs_runtime::filesystem_json_string;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = fs_ctx(&filesystem_json_string(&[fs_entry(
            true,
            "read-write",
            &dir.path().to_string_lossy(),
        )]));

        ctx.write("exports".to_string(), "a.txt".to_string(), b"hi".to_vec())
            .expect("write");
        assert_eq!(
            ctx.read("exports".to_string(), "a.txt".to_string())
                .unwrap(),
            b"hi".to_vec()
        );
        assert!(matches!(
            ctx.read("exports".to_string(), "../secret".to_string())
                .expect_err("escape attempt"),
            WitFsError::InvalidPath(_)
        ));
    }

    fn runtime_entry(
        granted: bool,
        command: &str,
    ) -> crate::plugin::sidecar_runtime::SidecarRuntimeEntry {
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

    /// Regression test for a review finding: `DriverProcessHost::stop` used
    /// to call `ProcessDriver::stop` (the synchronous version), which waits
    /// out `shutdown_grace` (SIGTERM -> grace -> SIGKILL) on the calling
    /// thread. Against a sidecar that ignores SIGTERM, that meant the
    /// guest's `stop` call -- and therefore the plugin's dedicated thread --
    /// blocked for the full grace period, which can exceed
    /// `PluginInstance::CALL_DEADLINE` and stalls that plugin's event queue
    /// in the meantime. `stop` must now return promptly regardless of how
    /// long the sidecar takes to actually die (see `stop`'s doc comment and
    /// `ProcessDriver::stop_detached`).
    #[test]
    fn stop_returns_promptly_even_when_the_sidecar_ignores_sigterm() {
        use crate::plugin::sidecar_runtime::{sidecars_json_string, SidecarRuntimeEntry};

        let sidecars_json = sidecars_json_string(&[SidecarRuntimeEntry {
            name: "tts".to_string(),
            granted: true,
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "trap '' TERM; sleep 30".to_string()],
            ports: vec![50202],
        }]);
        let mut ctx = HostCtx::new(
            "test-plugin".to_string(),
            Arc::new(Mutex::new("{}".to_string())),
            Arc::new(Mutex::new(capabilities_json_string(&[]))),
            Arc::new(Mutex::new(sidecars_json)),
            Arc::new(Mutex::new("[]".to_string())),
            test_http_driver(),
            // A grace period long enough that waiting it out synchronously
            // would trivially blow past both the assertion below and
            // `PluginInstance::CALL_DEADLINE`.
            Arc::new(edlr_driver_process::ProcessDriver::new(
                Duration::from_secs(2),
                Duration::from_secs(1),
            )),
            test_fs_driver(),
        );

        ctx.ensure_started("tts".to_string()).expect("start");

        let started = std::time::Instant::now();
        ctx.stop("tts".to_string()).expect("stop");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(500),
            "guest-facing stop() must not wait out the sidecar's shutdown \
             grace period; took {elapsed:?}"
        );
    }
}

//! 実行中プラグインの状態を保持する共有ビュー。`start_plugins` が構築し、以後は
//! カーネル内の複数箇所(将来の RPC を含む)から `Clone` して読める。

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use edlr_driver_channel::Bus;
use edlr_driver_process::ProcessDriver;

use crate::capability::grants::{GrantState, GrantsError, GrantsStore};
use crate::capability::request::CapabilityRequest;
use crate::host::plugin::PluginHost;
use crate::manifest::Manifest;
use crate::registry::bus::DiskBusService;
use crate::registry::driver::DriverRegistry;
use crate::registry::entries::{EntryTable, IdLocks};
use crate::registry::filesystem::DiskFilesystemService;
use crate::registry::grants::DiskGrantService;
use crate::registry::settings::DiskSettingsService;
use crate::registry::sidecar::DiskSidecarService;
use crate::registry::supervisor::ThreadSupervisor;
use crate::runner::plugin::queue::PluginWorkSender;
#[cfg(test)]
use crate::runner::plugin::PluginWork;
use crate::runtime::dropped::{DropCounters, DroppedCounts};
use crate::schedule::{Clock, ScheduleState, ScheduleView};
use crate::settings::filesystem::{FilesystemConfig, FilesystemConfigError, FilesystemConfigStore};
use crate::settings::sidecar::{SidecarConfig, SidecarConfigError, SidecarConfigStore};
use crate::settings::store::{split_secrets, SettingsError, SettingsStore};

/// プラグイン 1 件の現在の駆動状態。
#[derive(Debug, Clone, PartialEq)]
pub enum PluginState {
    Running,
    Disabled { reason: String },
}

/// レジストリに載る 1 プラグイン分のエントリ。
pub struct PluginEntry {
    pub manifest: Manifest,
    pub state: PluginState,
    /// `HostCtx` と共有される effective settings JSON。将来の RPC がここを
    /// 更新すると、次回以降の wasm 呼び出しに反映される。
    pub settings_json: Arc<Mutex<String>>,
    /// `HostCtx` と共有される capability 承認状態 JSON
    /// (`{"granted": bool, "hosts": [...]}`)。`Registry::set_capabilities`
    /// がここを更新すると、次回以降の `driver-http.send` 呼び出しに
    /// 再起動不要で反映される。
    pub capabilities_json: Arc<Mutex<String>>,
    /// `HostCtx` と共有されるサイドカー承認状態・実行仕様 JSON。形は
    /// `crate::runtime::sidecar::sidecars_json_string` を参照。
    /// `Registry::refresh_sidecar_runtime` がここを更新すると、次回以降の
    /// `driver-process.ensure-started` 呼び出しに再起動不要で反映される。
    pub sidecars_json: Arc<Mutex<String>>,
    /// `HostCtx` と共有されるファイルアクセス承認状態・実パス JSON。形は
    /// `crate::runtime::fs::filesystem_json_string` を参照。
    /// `Registry::refresh_filesystem_runtime` がここを更新すると、次回以降の
    /// `driver-fs.*` 呼び出しに再起動不要で反映される。未承認のルートは
    /// `path` を持たない(`crate::runtime::fs` のドキュメント参照)。
    pub filesystem_json: Arc<Mutex<String>>,
    /// `HostCtx` と共有されるバス承認状態・宣言済みトピック JSON。形は
    /// `crate::runtime::bus::bus_json_string` を参照。`filesystem_json` と同じく
    /// 起動時に `GrantsStore::bus_state` と manifest から組み立てられ、以後は
    /// 将来の `Registry` の bus 承認 API(Task 10)が更新する。
    pub bus_json: Arc<Mutex<String>>,
    /// `layout.kdl` / `layout.json` 由来の解決済みレイアウト。無ければ None
    /// (UI は平坦フォームで描画する)。ロード時に一度だけ解決する
    /// (`crate::layout::resolve` — settings の宣言は不変なので使い回せる)。
    pub layout: Option<crate::layout::Layout>,
}

/// `SidecarInfo` / `FilesystemInfo` / `BusInfo` / `DashboardInfo` /
/// `ScheduleInfo` は値型なので純粋モジュール `rpc::info` へ移設済み。ここでは
/// 旧パス(`crate::registry::plugin::SidecarInfo` 等)を温存するだけ
/// (registry(命令的)→ rpc(純粋)の re-export は依存方向として合法)。
pub use crate::rpc::info::{BusInfo, DashboardInfo, FilesystemInfo, ScheduleInfo, SidecarInfo};

/// RPC 応答用のプラグイン情報スナップショット。
pub struct PluginInfo {
    pub manifest: Manifest,
    pub state: PluginState,
    /// 設定値。**`secret` 型のキーは含まれない**(write-only なので RPC の
    /// 読み出し応答には載せない)。設定済みかどうかは `secrets_set` で分かる。
    pub values: serde_json::Map<String, serde_json::Value>,
    /// 空でない値が保存されている `secret` 型設定のキー(宣言順)。
    pub secrets_set: Vec<String>,
    pub capability_requests: Vec<CapabilityRequest>,
    pub grant_state: GrantState,
    pub sidecars: Vec<SidecarInfo>,
    pub filesystem: Vec<FilesystemInfo>,
    pub dashboard: Vec<DashboardInfo>,
    pub schedules: Vec<ScheduleInfo>,
    /// 作業キュー満杯で捨てた件数(デーモン起動時からの累計)。
    /// `plugin::dropped` のモジュールドキュメント参照。
    pub dropped: DroppedCounts,
    /// `PluginEntry::layout` のスナップショット。
    pub layout: Option<crate::layout::Layout>,
}

/// `control_sidecar` が指定できる操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarAction {
    Start,
    Stop,
    Restart,
}

/// `Registry` の値アクセス系メソッドが返しうるエラー。
#[derive(Debug)]
pub enum RegistryError {
    /// 指定された `id` のプラグインが登録されていない。
    UnknownPlugin(String),
    /// `SettingsStore::update` による検証・永続化エラー。
    Settings(SettingsError),
    /// `GrantsStore::set` による永続化エラー。
    Grants(GrantsError),
    /// `SidecarConfigStore::update_and_effective` による検証・永続化エラー。
    SidecarConfig(SidecarConfigError),
    /// 指定された `name` のサイドカーが manifest に無い。
    UnknownSidecar(String),
    /// サイドカー操作自体に関するエラー(未承認・未設定での `Start`/`Restart` など)。
    Sidecar(String),
    /// `FilesystemConfigStore::update_and_effective` による検証・永続化エラー。
    FilesystemConfig(FilesystemConfigError),
    /// 指定された `name` のファイルアクセスルートが manifest に無い。
    UnknownFilesystem(String),
    /// ファイルアクセス承認自体に関するエラー(未設定ディレクトリでの承認など)。
    Filesystem(String),
    /// 指定された `driver` のバス接続要求が manifest に無い。
    UnknownBus(String),
    /// 指定された `id` のダッシュボードウィジェットが manifest に無い。
    UnknownDashboard(String),
    /// 未承認のダッシュボードウィジェットのアセットが要求された。
    DashboardNotGranted(String),
    /// ダッシュボードアクションの宛先プラグインが動いていない
    /// (Disabled または未起動)。
    PluginNotRunning(String),
    /// ダッシュボードアクションを積む作業キューが満杯。
    ActionQueueFull(String),
    /// 指定された `id` のドライバが登録されていない。`UnknownPlugin` とは
    /// 別の variant にしてある: `crate::registry::driver::DriverRegistry` の
    /// サイドカー/ファイルアクセス系メソッド(`find_manifest_for_shared` /
    /// `refresh_sidecar_runtime` / `refresh_filesystem_runtime`)が返す未登録
    /// エラーは「プラグイン」ではなく「ドライバ」の話であり、
    /// `drivers/set-capabilities` など既存の `drivers/*` アーム
    /// (`DriverRegistryError::UnknownDriver` 経由)が既に "unknown driver:
    /// {id}" という文言を使っている。`UnknownPlugin` を使い回すと同じ失敗が
    /// アームによって違う文言になってしまう(レビュー指摘)ので、ここで
    /// 揃える。
    UnknownDriver(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::UnknownPlugin(id) => write!(f, "unknown plugin: {id}"),
            RegistryError::Settings(e) => write!(f, "{e}"),
            RegistryError::Grants(e) => write!(f, "{e}"),
            RegistryError::SidecarConfig(e) => write!(f, "{e}"),
            RegistryError::UnknownSidecar(name) => write!(f, "unknown sidecar: {name}"),
            RegistryError::Sidecar(msg) => write!(f, "{msg}"),
            RegistryError::FilesystemConfig(e) => write!(f, "{e}"),
            RegistryError::UnknownFilesystem(name) => write!(f, "unknown filesystem root: {name}"),
            RegistryError::Filesystem(msg) => write!(f, "{msg}"),
            RegistryError::UnknownBus(driver) => write!(f, "unknown bus connection: {driver}"),
            RegistryError::UnknownDashboard(id) => write!(f, "unknown dashboard widget: {id}"),
            RegistryError::DashboardNotGranted(id) => {
                write!(f, "dashboard widget not granted: {id}")
            }
            RegistryError::PluginNotRunning(id) => write!(f, "plugin is not running: {id}"),
            RegistryError::ActionQueueFull(id) => {
                write!(f, "plugin work queue is full, try again: {id}")
            }
            RegistryError::UnknownDriver(id) => write!(f, "unknown driver: {id}"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// 起動中プラグイン一覧の共有ビュー。
///
/// 内部で `PluginHost` の `Arc` も保持している。`PluginHost` はエポック割り込み
/// 用の ticker スレッドを持ち、それが動き続けている間だけ各プラグイン呼び出しの
/// デッドラインが有効になる。少なくとも 1 つの `Registry` クローンが生存して
/// いれば ticker は動き続けるので、`start_plugins` の呼び出し元は返ってきた
/// `Registry` を(プラグインを動かし続けたい間は)保持し続ける必要がある。
#[derive(Clone)]
pub struct Registry {
    entries: EntryTable<PluginEntry>,
    _host: Arc<PluginHost>,
    /// fs 群(`filesystem` / `set_filesystem_config` / `set_filesystem_grant`
    /// とその内部ヘルパー)の実体。`registry::filesystem::FilesystemService`
    /// のドキュメント参照(Phase 4 タスク4で移動)。
    filesystem_service: DiskFilesystemService<PluginEntry>,
    /// サイドカー群(`sidecars` / `set_sidecar_config` / `set_sidecar_grant` /
    /// `control_sidecar` / `stop_all_sidecars` とその内部ヘルパー)の実体。
    /// `registry::sidecar::SidecarService` のドキュメント参照(Phase 4
    /// タスク6で移動)。`capabilities_lock` の `Arc` はこのサービスと
    /// `grant_service` の両方へコンストラクタで同一のものを注入している
    /// (`Registry` 自身はもうこの `Arc` をフィールドとして保持しない --
    /// Phase 4 タスク7で `grant_service` へ委譲する側だけが使うようになった
    /// ため。`SidecarService`/`GrantService` のドキュメントコメント参照)。
    sidecar_service: DiskSidecarService<PluginEntry>,
    /// bus 群(`bus` / `bus_buffer` / `set_bus_grant` とその内部ヘルパー)の
    /// 実体。`registry::bus::BusService`(Phase 4 タスク5で抽出)。
    /// `bus_runtime_locks` と `BusInfo::resolved` 計算用の `DriverRegistry`
    /// クローンはこのサービスが持つ(元の `Registry` の
    /// `bus_runtime_locks`/`driver_registry` ドキュメント参照)。
    bus_service: DiskBusService,
    /// capability 承認群(`capabilities` / `set_capabilities` /
    /// `effective_hosts`)とダッシュボード群(`dashboard` /
    /// `set_dashboard_grant` / `dashboard_widgets_for_ui` /
    /// `dashboard_asset_path` / `events_of`)の実体。
    /// `registry::grants::GrantService`(Phase 4 タスク7で抽出)。
    /// `capabilities_lock` は同一の `Arc` を `sidecar_service` にも注入して
    /// いる(下のフィールドドキュメント参照)。
    grant_service: DiskGrantService<PluginEntry>,
    /// settings 群(`values` / `set_values`)の実体。
    /// `registry::settings::SettingsService`(Phase 4 タスク8で抽出)。
    settings_service: DiskSettingsService<PluginEntry>,
    /// プラグイン間バスの実体。`list()` が `options-from` を持つ select の候補を
    /// retain トピックから解決するために保持する
    /// (`crate::registry::select_options::resolve`)。`Bus` は `Clone` で内部を
    /// 共有するので、ドライバ側に配線したのと同じ実体を指す。
    bus: Bus,
    plugins_dir: PathBuf,
    /// プラグイン専用スレッドの登録・監督(停止・schedule view・drop counter
    /// の公開)。`registry::supervisor::ThreadSupervisor` のドキュメントコメント
    /// 参照(Phase 4 タスク3で `plugin_threads` / `bus_subscriber_shutdown` /
    /// `schedule_views` / `drop_counters` と対応メソッド本体をそちらへ移動)。
    supervisor: ThreadSupervisor,
}

impl Registry {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        host: Arc<PluginHost>,
        settings_store: Arc<SettingsStore>,
        grants_store: Arc<GrantsStore>,
        sidecar_config_store: Arc<SidecarConfigStore>,
        filesystem_config_store: Arc<FilesystemConfigStore>,
        process_driver: Arc<ProcessDriver>,
        driver_registry: DriverRegistry,
        bus: Bus,
        plugins_dir: PathBuf,
    ) -> Self {
        let entries = EntryTable::new();
        let filesystem_service = DiskFilesystemService::new(
            entries.clone(),
            grants_store.clone(),
            filesystem_config_store,
            IdLocks::new(),
        );
        let bus_service = DiskBusService::new(
            entries.clone(),
            grants_store.clone(),
            driver_registry,
            IdLocks::new(),
        );
        let capabilities_lock = Arc::new(Mutex::new(()));
        let sidecar_service = DiskSidecarService::new(
            entries.clone(),
            grants_store.clone(),
            sidecar_config_store,
            process_driver,
            capabilities_lock.clone(),
            IdLocks::new(),
        );
        let grant_service = DiskGrantService::new(
            entries.clone(),
            grants_store,
            capabilities_lock.clone(),
            plugins_dir.clone(),
        );
        let settings_service = DiskSettingsService::new(entries.clone(), settings_store);
        Registry {
            entries,
            _host: host,
            filesystem_service,
            sidecar_service,
            bus_service,
            grant_service,
            settings_service,
            bus,
            plugins_dir,
            supervisor: ThreadSupervisor::new(),
        }
    }

    /// `runner::start_plugins`(実際には `load_and_run_plugin`)が Running と
    /// して起動したプラグインの `work_tx` の複製と専用スレッドの
    /// `JoinHandle` を登録する。`shutdown_plugins` がこれを引いて
    /// `PluginWork::Stop` を送り、スレッドの終了を待つ(crate 内部専用)。
    ///
    /// Disabled で終わったプラグイン(`load` や `init` の失敗)はこれを
    /// 呼ばない -- そのスレッドは既に `ready_tx` へ結果を送って return 済み
    /// で `work_rx` を二度と読まないため、登録しても `Stop` が届かないだけ
    /// でなく、`shutdown_plugins` 側の join 待ちを無駄に長引かせる理由も無い
    /// (スレッド自体は既に終了しているので `is_finished()` は直ちに `true`
    /// になり実害は無いが、そもそも意味のある登録ではないため呼び出し元
    /// [`runner::load_and_run_plugin`] は Running のときだけ呼ぶ)。
    ///
    /// 実体は `registry::supervisor::ThreadSupervisor::register_thread`
    /// (Phase 4 タスク3で移動)。
    pub(crate) fn register_plugin_thread(
        &self,
        id: &str,
        work_tx: PluginWorkSender,
        handle: thread::JoinHandle<()>,
        stop_flag: Arc<AtomicBool>,
    ) {
        self.supervisor
            .register_thread(id, work_tx, handle, stop_flag);
    }

    /// プラグイン専用スレッドが、自分のスケジュール状態を公開する窓口を
    /// 登録する(`plugins/list` から読まれる)。スレッド自身がループへ入る
    /// 直前に呼ぶ(`runner::run_plugin_thread`)。
    ///
    /// スケジュールを 1 件も宣言していないプラグインは呼ばない。
    pub(crate) fn register_schedule_view(&self, id: &str, view: ScheduleView) {
        self.supervisor.register_schedule_view(id, view);
    }

    /// プラグインの取りこぼしカウンタを登録する(`plugins/list` から読まれる)。
    /// 購読タスクを起動する `runner::start_plugins` が呼ぶ。
    pub(crate) fn register_drop_counters(&self, id: &str, counters: Arc<DropCounters>) {
        self.supervisor.register_drop_counters(id, counters);
    }

    /// `id` のプラグインの取りこぼし件数。カウンタが未登録(Disabled で
    /// 購読タスクが起動していない)なら 0 件。
    fn dropped_counts(&self, id: &str) -> DroppedCounts {
        self.supervisor.dropped_counts(id)
    }

    /// Running な全プラグインへ `PluginWork::Stop` を送り、それぞれの専用
    /// スレッドの終了を待つ。デーモンの正常終了シーケンス専用
    /// (`core/src/bin/edlr.rs` を参照)。停止要求は全件へ先に送り、join は
    /// 1 つの共有デッドラインで待つ 2 段構造の詳細は
    /// `registry::supervisor::ThreadSupervisor::shutdown_all` のドキュメント
    /// コメント参照(Phase 4 タスク3で移動。2 段構造そのものは 1 行も変えて
    /// いない)。
    pub fn shutdown_plugins(&self) {
        self.supervisor.shutdown_all();
    }

    /// `crate::runner::plugin::spawn_bus_subscriber` が共有する shutdown
    /// フラグの `Arc` を返す。`runner.rs` がプラグインごとの購読タスクを
    /// 起動する際にこれを渡す(crate 内部専用: `pub(crate)`)。
    pub(crate) fn bus_subscriber_shutdown_flag(&self) -> Arc<AtomicBool> {
        self.supervisor.shutdown_flag()
    }

    pub(crate) fn push(&self, entry: PluginEntry) {
        self.entries.push(entry);
    }

    /// プラグインを走査した元ディレクトリ。
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    /// 現在登録されている全プラグインの `(manifest, state)` を返す。
    pub fn snapshot(&self) -> Vec<(Manifest, PluginState)> {
        self.entries.with_entries(|entries| {
            entries
                .iter()
                .map(|entry| (entry.manifest.clone(), entry.state.clone()))
                .collect()
        })
    }

    /// 現在登録されている全プラグインの `PluginInfo`(manifest・state・
    /// effective settings)を返す。RPC の一覧応答に使う。
    ///
    /// `entries` ロックは manifest/state のクローン取得のみに使い、ロックを
    /// 解放してから(ディスクを読む)`SettingsStore::effective` を呼ぶ。他の
    /// `Registry` 呼び出し(`set_disabled` など、プラグインスレッドから叩か
    /// れるものを含む)が settings のディスク I/O の間ブロックされないように
    /// するため。
    pub fn list(&self) -> Vec<PluginInfo> {
        let snapshot: Vec<(Manifest, PluginState, Option<crate::layout::Layout>)> =
            self.entries.with_entries(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        (
                            entry.manifest.clone(),
                            entry.state.clone(),
                            entry.layout.clone(),
                        )
                    })
                    .collect()
            });

        snapshot
            .into_iter()
            .map(|(mut manifest, state, layout)| {
                // `options-from` の select の候補を、いま retain されている値から
                // 解決して埋める(未解決なら `options` は None のまま)。
                crate::registry::select_options::resolve(&mut manifest.settings, &self.bus);
                // 秘密情報は RPC 応答から落とす(設定済みかどうかだけ返す)。
                let (values, secrets_set) =
                    split_secrets(&manifest, self.settings_service.effective_for(&manifest));
                let grant_state = self.grant_service.state_for(&manifest);
                let capability_requests = manifest.capabilities.clone();
                let sidecars = self.sidecar_service.build_sidecar_infos(&manifest);
                let filesystem = self.filesystem_service.build_filesystem_infos(&manifest);
                let dashboard = self.grant_service.build_dashboard_infos(&manifest);
                let schedules = self.build_schedule_infos(&manifest);
                let dropped = self.dropped_counts(&manifest.id);
                PluginInfo {
                    manifest,
                    state,
                    values,
                    secrets_set,
                    capability_requests,
                    grant_state,
                    sidecars,
                    filesystem,
                    dashboard,
                    schedules,
                    dropped,
                    layout,
                }
            })
            .collect()
    }

    /// `manifest.schedules` から `ScheduleInfo` の一覧を組み立てる(宣言順)。
    ///
    /// プラグインが Running なら、そのランナーループが `ScheduleView` へ
    /// 公開している**実際の**次回発火時刻を返す。まだ公開されていない
    /// (起動途中)か、プラグインが Disabled でスレッドが無い場合は、
    /// `ScheduleState` をこの場で作り直した推定値へフォールバックする
    /// (`ScheduleInfo` のドキュメントコメント参照)。
    fn build_schedule_infos(&self, manifest: &Manifest) -> Vec<ScheduleInfo> {
        let published = self.supervisor.published_schedule(&manifest.id);

        let clock = Clock::now();
        let state = ScheduleState::new(&manifest.schedules, clock);
        state
            .next_times(clock)
            .into_iter()
            .map(|(name, estimated)| {
                let spec = manifest
                    .schedules
                    .iter()
                    .find(|s| s.name == name)
                    .map(|s| s.spec.clone())
                    .expect("next_times names must come from manifest.schedules");
                ScheduleInfo {
                    next: published.get(name).copied().unwrap_or(estimated),
                    name: name.to_string(),
                    spec,
                }
            })
            .collect()
    }

    /// `id` の manifest が宣言する `events` フィルタ(`dashboard/list` が
    /// ウィジェットへのイベント転送範囲を UI に伝えるのに使う)。実体は
    /// `registry::grants::GrantService::events_of`(Phase 4 タスク7で移動)。
    pub fn events_of(&self, id: &str) -> Result<Vec<String>, RegistryError> {
        self.grant_service.events_of(id)
    }

    /// `id` のダッシュボードウィジェット一覧(UI 表示用)。実体は
    /// `registry::grants::GrantService::dashboard`(Phase 4 タスク7で移動)。
    pub fn dashboard(&self, id: &str) -> Result<Vec<DashboardInfo>, RegistryError> {
        self.grant_service.dashboard(id)
    }

    /// ダッシュボードウィジェット 1 件の承認/取消。実体は
    /// `registry::grants::GrantService::set_dashboard_grant`(Phase 4
    /// タスク7で移動)。
    pub fn set_dashboard_grant(
        &self,
        id: &str,
        widget: &str,
        granted: bool,
    ) -> Result<Vec<DashboardInfo>, RegistryError> {
        self.grant_service.set_dashboard_grant(id, widget, granted)
    }

    /// `dashboard/list` 用: 全プラグインの全ウィジェット
    /// (`(plugin_id, plugin_name, state, info)`)。grant の有無での絞り込みは
    /// 呼び出し側(server.rs)の責務。実体は
    /// `registry::grants::GrantService::dashboard_widgets_for_ui`(Phase 4
    /// タスク7で移動)。
    pub fn dashboard_widgets_for_ui(&self) -> Vec<(String, String, PluginState, DashboardInfo)> {
        self.grant_service.dashboard_widgets_for_ui()
    }

    /// ウィジェットアセットの実ファイルパスを解決する。grant 必須・entry の
    /// ディレクトリ外へのトラバーサルは拒否(`/plugin-ui/...` ハンドラの
    /// 心臓部。HTTP 層は薄く保ち、判定はここで単体テストする)。
    /// `rel_path` が空のときは entry ファイル自身を返す。実体は
    /// `registry::grants::GrantService::dashboard_asset_path`(Phase 4
    /// タスク7で移動)。
    pub fn dashboard_asset_path(
        &self,
        plugin: &str,
        widget: &str,
        rel_path: &str,
    ) -> Result<PathBuf, RegistryError> {
        self.grant_service
            .dashboard_asset_path(plugin, widget, rel_path)
    }

    /// ダッシュボードウィジェット発のアクションを、そのウィジェットが属する
    /// プラグインの `on-message(driver = "dashboard", topic = name)` へ届ける
    /// (`plugins/dashboard-action` RPC の実体)。
    ///
    /// grant 済みのウィジェットからのみ受け付ける(アセット配信と同じ判定)。
    /// 配送はプラグインの作業キューに積むだけで、実行の完了は待たない。
    pub fn dashboard_action(
        &self,
        plugin: &str,
        widget: &str,
        name: &str,
    ) -> Result<(), RegistryError> {
        self.grant_service.ensure_dashboard_granted(plugin, widget)?;
        self.supervisor
            .deliver_message(
                plugin,
                edlr_driver_channel::Delivery {
                    plugin_id: plugin.to_string(),
                    driver_id: edlr_driver_channel::DASHBOARD_SENDER.to_string(),
                    topic: name.to_string(),
                    payload: Vec::new(),
                },
            )
            .map_err(|e| match e {
                super::supervisor::DeliverError::NotRunning => {
                    RegistryError::PluginNotRunning(plugin.to_string())
                }
                super::supervisor::DeliverError::QueueFull => {
                    RegistryError::ActionQueueFull(plugin.to_string())
                }
            })
    }

    /// `id` のプラグインの capability 要求一覧と現在の承認状態を返す。実体は
    /// `registry::grants::GrantService::capabilities`(Phase 4 タスク7で移動)。
    pub fn capabilities(
        &self,
        id: &str,
    ) -> Result<(Vec<CapabilityRequest>, GrantState), RegistryError> {
        self.grant_service.capabilities(id)
    }

    /// `id` のプラグインの effective settings(`SettingsStore` 由来)を返す。
    /// 実体は `registry::settings::SettingsService::effective`(Phase 4
    /// タスク8で抽出)。秘密情報を読み出し応答から落とすのはここ(plugin
    /// wrapper)の役目 -- driver 側(`crate::registry::driver::DriverRegistry::values`)
    /// は落とさない(`split_secrets` のドキュメント参照)。
    pub fn values(
        &self,
        id: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, RegistryError> {
        let (manifest, effective) = self.settings_service.effective(id)?;
        let (visible, _secrets_set) = split_secrets(&manifest, effective);
        Ok(visible)
    }

    /// `id` のプラグインの settings を検証・永続化し、稼働中プラグインが参照
    /// する共有 `settings_json` も新しい effective 値で上書きする。実体は
    /// `registry::settings::SettingsService::update_and_effective`(Phase 4
    /// タスク8で抽出。ロック規律のドキュメントは移動先参照)。
    ///
    /// 検証(`SettingsStore::update`)に失敗した場合は何も変更されず、
    /// `RegistryError::Settings` を返す。
    ///
    /// プラグインへ渡すバッファ(`settings_json`)には秘密情報も含める --
    /// 渡す相手はそのプラグイン自身なので、ここで落としたら意味が無い
    /// (`SettingsService::update_and_effective` が書き込む値そのもの)。
    /// RPC の応答にだけ `split_secrets` で秘密情報を落とす。
    pub fn set_values(
        &self,
        id: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, RegistryError> {
        let (manifest, effective) = self.settings_service.update_and_effective(id, values)?;
        let (visible, _secrets_set) = split_secrets(&manifest, effective);
        Ok(visible)
    }

    /// `id` のプラグインの capability 承認/取消を `GrantsStore` に永続化し、
    /// 稼働中プラグインが参照する共有 `capabilities_json` も更新する。実体は
    /// `registry::grants::GrantService::set_capabilities`(Phase 4 タスク7で
    /// 移動)。ロック規律・"live な `sidecars_json` バッファを読み再計算はし
    /// ない"という不変条件のドキュメントは移動先のコメント参照。
    pub fn set_capabilities(&self, id: &str, granted: bool) -> Result<GrantState, RegistryError> {
        self.grant_service.set_capabilities(id, granted)
    }

    /// `id` のプラグインの `capabilities_json` 共有バッファが現在載せている
    /// 実効許可ホストを返す(テスト用アクセサ)。`driver-http.send` が実際に
    /// 参照するのと同じ値。実体は
    /// `registry::grants::GrantService::effective_hosts`(Phase 4 タスク7で
    /// 移動)。
    pub fn effective_hosts(&self, id: &str) -> Result<Vec<String>, RegistryError> {
        self.grant_service.effective_hosts(id)
    }

    /// `id` のプラグインの現在のサイドカー状態一覧(manifest の `[[sidecar]]`
    /// 宣言順)を返す。実体は `registry::sidecar::SidecarService::sidecars`
    /// (Phase 4 タスク6で移動)。
    pub fn sidecars(&self, id: &str) -> Result<Vec<SidecarInfo>, RegistryError> {
        self.sidecar_service.sidecars(id)
    }

    /// `id` のプラグインの現在のファイルアクセス状態一覧(manifest の
    /// `[[filesystem]]` 宣言順)を返す。実体は
    /// `registry::filesystem::FilesystemService::filesystem`(Phase 4
    /// タスク4で移動)。
    pub fn filesystem(&self, id: &str) -> Result<Vec<FilesystemInfo>, RegistryError> {
        self.filesystem_service.filesystem(id)
    }

    /// `id` のプラグインの `filesystem_json` 共有バッファの中身をそのまま
    /// 返す(テスト用アクセサ)。`driver-fs.*` が実際に参照するのと同じ
    /// 文字列(`crate::runtime::fs::filesystem_json_string` の出力そのもの)。実体は
    /// `registry::filesystem::FilesystemService::filesystem_buffer`。
    pub fn filesystem_buffer(&self, id: &str) -> Result<String, RegistryError> {
        self.filesystem_service.filesystem_buffer(id)
    }

    /// `id` のプラグインの `bus_json` 共有バッファの中身をそのまま返す
    /// (テスト用アクセサ)。実体は `registry::bus::BusService::bus_buffer`。
    pub fn bus_buffer(&self, id: &str) -> Result<String, RegistryError> {
        self.bus_service.bus_buffer(id)
    }

    /// `id` のプラグインの `name` ファイルアクセスルートの設定を検証・永続化
    /// し、稼働中プラグインが参照する `filesystem_json` を作り直してから
    /// 最新の `FilesystemInfo` 一覧を返す。実体は
    /// `registry::filesystem::FilesystemService::set_filesystem_config`。
    pub fn set_filesystem_config(
        &self,
        id: &str,
        name: &str,
        config: &FilesystemConfig,
    ) -> Result<Vec<FilesystemInfo>, RegistryError> {
        self.filesystem_service
            .set_filesystem_config(id, name, config)
    }

    /// `id` のプラグインの `name` ファイルアクセスルートの承認/取消を
    /// `GrantsStore` に永続化する。実体は
    /// `registry::filesystem::FilesystemService::set_filesystem_grant`。
    pub fn set_filesystem_grant(
        &self,
        id: &str,
        name: &str,
        granted: bool,
    ) -> Result<Vec<FilesystemInfo>, RegistryError> {
        self.filesystem_service
            .set_filesystem_grant(id, name, granted)
    }

    /// `id` のプラグインの現在のバス接続状態一覧(manifest の `[[bus]]`
    /// 宣言順)を返す。実体は `registry::bus::BusService::bus`。
    pub fn bus(&self, id: &str) -> Result<Vec<BusInfo>, RegistryError> {
        self.bus_service.bus(id)
    }

    /// `id` のプラグインの `driver` バス接続の承認/取消を `GrantsStore` に
    /// 永続化し、稼働中プラグインが参照する `bus_json` を作り直す。実体は
    /// `registry::bus::BusService::set_bus_grant`。
    pub fn set_bus_grant(
        &self,
        id: &str,
        driver: &str,
        granted: bool,
    ) -> Result<GrantState, RegistryError> {
        self.bus_service.set_bus_grant(id, driver, granted)
    }

    /// `id` のプラグインの `name` サイドカーの設定を検証・永続化し、稼働中の
    /// 実行を止めてから最新の `SidecarInfo` 一覧を返す。検証に失敗した場合は
    /// 何も変更されない。実体は
    /// `registry::sidecar::SidecarService::set_sidecar_config`(Phase 4
    /// タスク6で移動)。
    pub fn set_sidecar_config(
        &self,
        id: &str,
        name: &str,
        config: &SidecarConfig,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        self.sidecar_service.set_sidecar_config(id, name, config)
    }

    /// `id` のプラグインの `name` サイドカーの承認/取消を `GrantsStore` に
    /// 永続化する。実体は
    /// `registry::sidecar::SidecarService::set_sidecar_grant`(Phase 4
    /// タスク6で移動)。
    pub fn set_sidecar_grant(
        &self,
        id: &str,
        name: &str,
        granted: bool,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        self.sidecar_service.set_sidecar_grant(id, name, granted)
    }

    /// `id` のプラグインの `name` サイドカーを直接操作する(ユーザー操作
    /// 起点)。TOCTOU 対策・無効化されたプラグインへの拒否を含め、実体は
    /// `registry::sidecar::SidecarService::control_sidecar`(Phase 4
    /// タスク6で移動。ロック規律・挙動は一切変えていない)。
    pub fn control_sidecar(
        &self,
        id: &str,
        name: &str,
        action: SidecarAction,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        self.sidecar_service.control_sidecar(id, name, action)
    }

    /// 全プラグインの全サイドカーインスタンスを停止する(デーモン shutdown 用)。
    /// 実体は `registry::sidecar::SidecarService::stop_all`(Phase 4
    /// タスク6で移動)。
    pub fn stop_all_sidecars(&self) {
        self.sidecar_service.stop_all();
    }

    /// 全プラグインの `spawn_bus_subscriber` タスクへ shutdown を通知する
    /// (デーモン shutdown 用)。
    ///
    /// **これを `main()` が戻る(= `Runtime::drop` される)前に呼ばないと
    /// デーモンは正常終了できない。** `[[bus]] subscribe` を宣言するプラグ
    /// インが 1 つでもあれば、その `spawn_bus_subscriber` タスクは
    /// `tokio::task::spawn_blocking` の中でブロッキング受信をしており、送信側
    /// (`Bus::subscribe` に渡した `Sender<Delivery>`)はそのプラグインの購読
    /// エントリとして `Bus` の購読表に居座り続けるため、明示的に知らせない
    /// 限り自然には終了しない。`Runtime::drop` は実行中の `spawn_blocking`
    /// タスクの完了を待つため、これを呼ばずに `main` を抜けようとすると
    /// **プロセスが `Runtime::drop` の中で無期限にハングする**(実際に踏んだ
    /// Critical バグ。詳細は `crate::runner::plugin::
    /// BUS_SUBSCRIBER_SHUTDOWN_POLL_INTERVAL` のドキュメントコメント参照)。
    ///
    /// `stop_all_sidecars` と同じくデーモンの shutdown シーケンスの一部として
    /// 呼ぶことを想定している(`core/src/bin/edlr.rs` を参照)。フラグを立てる
    /// だけの軽い呼び出しなので、`stop_all_sidecars` のように `spawn_blocking`
    /// へ逃がす必要はない。
    pub fn shutdown_bus_subscribers(&self) {
        self.supervisor.shutdown_bus_subscribers();
    }

    /// `id` のプラグインが持つ settings JSON の共有ハンドルを返す。
    pub fn entry_settings(&self, id: &str) -> Option<Arc<Mutex<String>>> {
        self.entries.find(
            |entry| entry.manifest.id == id,
            |entry| entry.settings_json.clone(),
        )
    }

    /// `id` のプラグインを `Disabled { reason }` にし、そのプラグインが
    /// 持つ全サイドカーを停止する。存在しなければ何もしない。
    ///
    /// プラグインが無効化された時点で、そのサイドカーを止められる主体
    /// (プラグイン自身)はもう居なくなる -- `runner.rs` は `on_event` が
    /// trap したときここを呼んだ直後にプラグイン専用スレッドを終了させる
    /// ため、`HostCtx`(と、そこから辿れる状態)は drop されるが、
    /// サイドカープロセス自体はホスト(`ProcessDriver`)が引き続き所有して
    /// おり、明示的に止めない限り動き続けてしまう。設計書が挙げる
    /// 「プラグイン無効化 → そのプラグインの全サイドカーを停止」という
    /// ホストの保証を実装するのがこの停止処理(Important: 最終レビューで
    /// 見つかった取りこぼし)。
    ///
    /// `entries` ロックは `state` の書き換えと manifest のサイドカー名一覧の
    /// 取得だけに使い、手放してから `ProcessDriver::stop`(同期版。SIGTERM
    /// 無視の子がいれば `shutdown_grace` 秒ブロックしうる)を呼ぶ -- ここは
    /// プラグイン専用スレッド(`runner.rs`)から直接呼ばれる経路なので、
    /// 他プラグインの `entries` アクセスまで巻き添えでブロックしないように
    /// するため(このブランチで守ってきた「ロックを猶予期間まるごと保持
    /// しない」という制約と同じ理由)。ホスト起点の停止なので同期版
    /// `stop`(ゲスト向け `stop_detached` ではない)を使う: `set_disabled`
    /// を呼ぶ側(このプラグイン自身のスレッド)はここで終了するだけなので、
    /// `PluginInstance::CALL_DEADLINE` の制約は関係ない。
    pub fn set_disabled(&self, id: &str, reason: String) {
        let sidecar_names: Option<Vec<String>> = self.entries.find_mut(
            |entry| entry.manifest.id == id,
            |entry| {
                entry.state = PluginState::Disabled { reason };
                entry
                    .manifest
                    .sidecars
                    .iter()
                    .map(|s| s.name.clone())
                    .collect()
            },
        );

        let Some(names) = sidecar_names else {
            return;
        };

        // `entries` ロックを既に手放した後で `SidecarService::stop_named`
        // (内部で同じ id 別ロックを取る)を呼ぶ(ロック取得順序は他の箇所と
        // 同じ: `entries` → id 別ロック)。ここで id 別ロックを取ることで、
        // `control_sidecar` の `Start`/`Restart`(こちらも同じロックを取って
        // から `PluginState` を読む)と互いに排他になる -- 「状態を
        // `Disabled` にする」→「サイドカーを止める」の一連と、「(無効化前の)
        // 状態を読む」→「spawn する」が交差して、無効化の裏でサイドカーが
        // 起動されたまま残ることはない(`control_sidecar` のドキュメント
        // 参照)。
        self.sidecar_service.stop_named(id, &names);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::capability::request::BusRequest;
    use crate::event::Event;
    use crate::manifest::{ScheduleSpec, SettingField};
    use edlr_driver_process::ProcessSpec;
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::Duration;

    /// ドライバを 1 件もロードしていない `DriverRegistry`(存在しない
    /// ディレクトリを走査させることで空のまま作る -- `driver::runner` の
    /// 自テストにある `start_drivers_for_test` と同じ流儀)。
    fn empty_driver_registry(tmp: &std::path::Path) -> DriverRegistry {
        crate::runner::driver::start_drivers(
            &tmp.join("drivers"),
            SettingsStore::new(tmp.join("driver-settings")),
            SidecarConfigStore::new(tmp.join("driver-settings")),
            FilesystemConfigStore::new(tmp.join("driver-settings"), Vec::new()),
            GrantsStore::new_for_drivers(tmp.join("driver-grants")),
            edlr_driver_channel::Bus::new(),
            crate::host::driver::DriverHost::new(crate::host::drivers::test_handle())
                .expect("driver host should build"),
        )
    }

    fn empty_registry() -> Registry {
        let host = Arc::new(
            PluginHost::new(crate::host::drivers::test_handle()).expect("host should start"),
        );
        let tmp = tempfile::tempdir().unwrap();
        let settings_store = Arc::new(SettingsStore::new(tmp.path().join("settings")));
        let grants_store = Arc::new(GrantsStore::new(tmp.path().join("grants")));
        let sidecar_config_store = Arc::new(SidecarConfigStore::new(tmp.path().join("settings")));
        let filesystem_config_store = Arc::new(FilesystemConfigStore::new(
            tmp.path().join("settings"),
            Vec::new(),
        ));
        let process_driver = Arc::new(ProcessDriver::new(
            Duration::from_millis(200),
            Duration::from_millis(0),
        ));
        let driver_registry = empty_driver_registry(tmp.path());
        Registry::new(
            host,
            settings_store,
            grants_store,
            sidecar_config_store,
            filesystem_config_store,
            process_driver,
            driver_registry,
            edlr_driver_channel::Bus::new(),
            tmp.path().join("plugins"),
        )
    }

    #[test]
    fn values_for_unknown_plugin_returns_unknown_plugin_error() {
        let registry = empty_registry();

        let err = registry
            .values("does-not-exist")
            .expect_err("unknown id should be rejected");

        assert!(matches!(err, RegistryError::UnknownPlugin(id) if id == "does-not-exist"));
    }

    #[test]
    fn set_values_for_unknown_plugin_returns_unknown_plugin_error() {
        let registry = empty_registry();
        let mut values = serde_json::Map::new();
        values.insert("enabled".to_string(), serde_json::json!(false));

        let err = registry
            .set_values("does-not-exist", &values)
            .expect_err("unknown id should be rejected");

        assert!(matches!(err, RegistryError::UnknownPlugin(id) if id == "does-not-exist"));
    }

    #[test]
    fn set_capabilities_for_unknown_plugin_returns_unknown_plugin_error() {
        let registry = empty_registry();

        let err = registry
            .set_capabilities("does-not-exist", true)
            .expect_err("unknown id should be rejected");

        assert!(matches!(err, RegistryError::UnknownPlugin(id) if id == "does-not-exist"));
    }

    fn manifest_with_filesystem(id: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![crate::capability::request::FilesystemRequest {
                name: "exports".into(),
                reason: "reason".into(),
                mode: crate::capability::request::FilesystemMode::ReadWrite,
                target: Default::default(),
            }],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    fn manifest_with_bus(id: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![BusRequest {
                driver: "ed-state".into(),
                publish: vec!["ship-status".into()],
                subscribe: vec!["current-system".into()],
                reason: "r".into(),
            }],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    /// `options-from` の select を 1 件だけ持つプラグイン `speaky` を、渡された
    /// `Bus` 付きで載せた `Registry`。
    fn test_registry_with_dynamic_select(bus: edlr_driver_channel::Bus) -> Registry {
        let host = Arc::new(
            PluginHost::new(crate::host::drivers::test_handle()).expect("host should start"),
        );
        let tmp = tempfile::tempdir().unwrap();
        let settings_store = Arc::new(SettingsStore::new(tmp.path().join("settings")));
        let grants_store = Arc::new(GrantsStore::new(tmp.path().join("grants")));
        let sidecar_config_store = Arc::new(SidecarConfigStore::new(tmp.path().join("settings")));
        let filesystem_config_store = Arc::new(FilesystemConfigStore::new(
            tmp.path().join("settings"),
            Vec::new(),
        ));
        let process_driver = Arc::new(ProcessDriver::new(
            Duration::from_millis(200),
            Duration::from_millis(0),
        ));
        let registry = Registry::new(
            host,
            settings_store,
            grants_store,
            sidecar_config_store,
            filesystem_config_store,
            process_driver,
            empty_driver_registry(tmp.path()),
            bus,
            tmp.path().join("plugins"),
        );
        let mut manifest = manifest_with_bus("speaky");
        manifest.bus.clear();
        manifest.settings = vec![SettingField::Select {
            key: "speaker".into(),
            label: "話者".into(),
            default: String::new(),
            options: None,
            options_from: Some(crate::manifest::OptionsFrom {
                driver: "coeiroink".into(),
                topic: "speakers".into(),
            }),
        }];
        registry.push(PluginEntry {
            manifest,
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });
        registry
    }

    fn listed_options(registry: &Registry) -> Option<Vec<crate::manifest::SelectOption>> {
        let infos = registry.list();
        let SettingField::Select { options, .. } = &infos[0].manifest.settings[0] else {
            panic!("expected a select field");
        };
        options.clone()
    }

    #[test]
    fn list_fills_in_select_options_from_the_retained_value() {
        let bus = edlr_driver_channel::Bus::new();
        let (tx, _rx) = std_mpsc::sync_channel(4);
        bus.register_driver(
            "coeiroink",
            vec![edlr_driver_channel::TopicSpec {
                name: "speakers".into(),
                retain: true,
                description: String::new(),
            }],
            tx,
        );
        bus.emit("coeiroink", "speakers", br#"["Ame","Tsuina"]"#.to_vec())
            .expect("emit should succeed");

        let registry = test_registry_with_dynamic_select(bus);

        assert_eq!(
            listed_options(&registry),
            Some(vec!["Ame".into(), "Tsuina".into()])
        );
    }

    /// ドライバが居ない(未インストール・無効化済み)ときは `options` を
    /// `None` のままにする。UI はこれを見て「候補を取得できません」を出す。
    #[test]
    fn list_leaves_select_options_unresolved_without_a_driver() {
        let registry = test_registry_with_dynamic_select(edlr_driver_channel::Bus::new());

        assert_eq!(listed_options(&registry), None);
    }

    /// `[[bus]]` を 1 件持つプラグインだけを載せた `Registry`。
    /// `DriverRegistry` には何も登録しないので、`bus("translator")` の
    /// `resolved` は必ず `false` になる。
    pub(crate) fn test_registry_with_bus_request() -> Registry {
        let tmp = tempfile::tempdir().unwrap();
        test_registry_with_bus_request_using(empty_driver_registry(tmp.path()))
    }

    /// `test_registry_with_bus_request` と同じ `translator` プラグイン
    /// (`[[bus]] {driver="ed-state", publish=["ship-status"],
    /// subscribe=["current-system"]}`)を、呼び出し元が指定した
    /// `driver_registry` の上に載せた `Registry` を返す。
    ///
    /// `bus("translator")[0].resolved` は `Registry` 自身が保持する
    /// `driver_registry`(コンストラクタで焼き込まれる、他から差し替え不可)
    /// から計算されるので、resolved を true/false 両方でテストしたい呼び出し
    /// 元はこの関数に望みの `DriverRegistry` を渡す(`crate::server` の
    /// `plugins/list` テストがまさにこれ -- `ed-state` を持つ
    /// `DriverRegistry` を渡せば `resolved: true`、持たないものを渡せば
    /// `resolved: false` になる)。
    pub(crate) fn test_registry_with_bus_request_using(
        driver_registry: DriverRegistry,
    ) -> Registry {
        let host = Arc::new(
            PluginHost::new(crate::host::drivers::test_handle()).expect("host should start"),
        );
        let tmp = tempfile::tempdir().unwrap();
        let settings_store = Arc::new(SettingsStore::new(tmp.path().join("settings")));
        let grants_store = Arc::new(GrantsStore::new(tmp.path().join("grants")));
        let sidecar_config_store = Arc::new(SidecarConfigStore::new(tmp.path().join("settings")));
        let filesystem_config_store = Arc::new(FilesystemConfigStore::new(
            tmp.path().join("settings"),
            Vec::new(),
        ));
        let process_driver = Arc::new(ProcessDriver::new(
            Duration::from_millis(200),
            Duration::from_millis(0),
        ));
        let registry = Registry::new(
            host,
            settings_store,
            grants_store,
            sidecar_config_store,
            filesystem_config_store,
            process_driver,
            driver_registry,
            edlr_driver_channel::Bus::new(),
            tmp.path().join("plugins"),
        );
        registry.push(PluginEntry {
            manifest: manifest_with_bus("translator"),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });
        registry
    }

    /// `[[dashboard]]` を 1 件持つプラグイン `widgety`(widget id "status"、
    /// entry "ui/index.html")だけを載せた `Registry`。plugins_dir は
    /// 返り値の `TempDir` 配下(`<tmp>/plugins`)なので、entry ファイルの
    /// 有無を呼び出し元が操作できる(fixture が `TempDir` を drop すると
    /// ディレクトリごと消えるため、所有権ごと返す)。
    pub(crate) fn test_registry_with_dashboard() -> (Registry, tempfile::TempDir) {
        let host = Arc::new(
            PluginHost::new(crate::host::drivers::test_handle()).expect("host should start"),
        );
        let tmp = tempfile::tempdir().unwrap();
        let settings_store = Arc::new(SettingsStore::new(tmp.path().join("settings")));
        let grants_store = Arc::new(GrantsStore::new(tmp.path().join("grants")));
        let sidecar_config_store = Arc::new(SidecarConfigStore::new(tmp.path().join("settings")));
        let filesystem_config_store = Arc::new(FilesystemConfigStore::new(
            tmp.path().join("settings"),
            Vec::new(),
        ));
        let process_driver = Arc::new(ProcessDriver::new(
            Duration::from_millis(200),
            Duration::from_millis(0),
        ));
        let driver_registry = empty_driver_registry(tmp.path());
        let registry = Registry::new(
            host,
            settings_store,
            grants_store,
            sidecar_config_store,
            filesystem_config_store,
            process_driver,
            driver_registry,
            edlr_driver_channel::Bus::new(),
            tmp.path().join("plugins"),
        );
        registry.push(PluginEntry {
            manifest: manifest_with_dashboard("widgety"),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });
        (registry, tmp)
    }

    fn manifest_with_dashboard(id: &str) -> Manifest {
        use crate::capability::request::{DashboardWidget, WidgetSize};
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec!["FSDJump".into()],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![DashboardWidget {
                id: "status".into(),
                title: "Status".into(),
                entry: "ui/index.html".into(),
                size: WidgetSize::Small,
            }],
            schedules: vec![],
        }
    }

    /// `[[schedule]]` を 2 件(interval・cron)持つプラグイン `scheduler-plugin`。
    fn manifest_with_schedule(id: &str) -> Manifest {
        use crate::manifest::ScheduleRequest;
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![
                ScheduleRequest {
                    name: "flush".into(),
                    spec: ScheduleSpec::IntervalSeconds(60),
                    catch_up: false,
                },
                ScheduleRequest {
                    name: "daily".into(),
                    spec: ScheduleSpec::Cron("0 9 * * *".to_string()),
                    catch_up: false,
                },
            ],
        }
    }

    /// `[[schedule]]` を 2 件持つプラグインだけを載せた `Registry`。
    pub(crate) fn test_registry_with_schedule() -> Registry {
        let registry = empty_registry();
        registry.push(PluginEntry {
            manifest: manifest_with_schedule("scheduler-plugin"),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });
        registry
    }

    /// `secret` 型設定を 1 件だけ持つプラグインを載せた `Registry`。
    /// `settings_store` は実ディレクトリを使うので、`set_values` で書いた値が
    /// `list()`/`values()` に効く。
    fn test_registry_with_secret() -> (Registry, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let host = Arc::new(
            PluginHost::new(crate::host::drivers::test_handle()).expect("host should start"),
        );
        let settings_store = Arc::new(SettingsStore::new(tmp.path().join("settings")));
        let grants_store = Arc::new(GrantsStore::new(tmp.path().join("grants")));
        let sidecar_config_store = Arc::new(SidecarConfigStore::new(tmp.path().join("settings")));
        let filesystem_config_store = Arc::new(FilesystemConfigStore::new(
            tmp.path().join("settings"),
            Vec::new(),
        ));
        let process_driver = host.process_driver();
        let registry = Registry::new(
            host,
            settings_store,
            grants_store,
            sidecar_config_store,
            filesystem_config_store,
            process_driver,
            empty_driver_registry(tmp.path()),
            edlr_driver_channel::Bus::new(),
            tmp.path().join("plugins"),
        );

        let mut manifest = plain_manifest("secret-plugin");
        manifest.settings = vec![
            SettingField::String {
                key: "endpoint".into(),
                label: "Endpoint".into(),
                default: "https://example.test".into(),
            },
            SettingField::Secret {
                key: "api-key".into(),
                label: "API Key".into(),
            },
        ];
        registry.push(PluginEntry {
            manifest,
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });
        (registry, tmp)
    }

    /// 保存した秘密情報が、どの読み出し経路からも返らないこと
    /// (`list` / `values` / `set_values` の応答)。一方でプラグインへ渡す
    /// `settings_json` バッファには入っていること。
    #[test]
    fn secret_values_never_appear_in_read_responses() {
        let (registry, _tmp) = test_registry_with_secret();

        let mut values = serde_json::Map::new();
        values.insert("api-key".into(), serde_json::json!("sk-live-123"));
        let returned = registry
            .set_values("secret-plugin", &values)
            .expect("storing a secret should succeed");

        assert!(
            !returned.contains_key("api-key"),
            "set_values must not echo the secret back"
        );

        let fetched = registry.values("secret-plugin").expect("values");
        assert!(
            !fetched.contains_key("api-key"),
            "plugins/get-settings must not return the secret"
        );
        // 秘密でない設定は普通に返る。
        assert_eq!(
            fetched.get("endpoint"),
            Some(&serde_json::json!("https://example.test"))
        );

        let infos = registry.list();
        assert!(
            !infos[0].values.contains_key("api-key"),
            "plugins/list must not return the secret"
        );
        assert_eq!(
            infos[0].secrets_set,
            vec!["api-key".to_string()],
            "but it must say the secret is configured"
        );

        // プラグイン自身は受け取れること -- 渡す相手はこのプラグイン。
        let settings_json = registry
            .entries
            .with_entries(|entries| entries[0].settings_json.clone());
        let buffer = settings_json.lock().unwrap().clone();
        assert!(
            buffer.contains("sk-live-123"),
            "the guest-facing settings buffer must still carry the secret, got {buffer}"
        );
    }

    #[test]
    fn an_unset_secret_is_not_reported_as_configured() {
        let (registry, _tmp) = test_registry_with_secret();
        let infos = registry.list();
        assert!(infos[0].secrets_set.is_empty());
        assert!(!infos[0].values.contains_key("api-key"));
    }

    #[test]
    fn list_reports_schedules_with_name_spec_and_next() {
        let registry = test_registry_with_schedule();
        let infos = registry.list();
        assert_eq!(infos.len(), 1);
        let schedules = &infos[0].schedules;
        assert_eq!(schedules.len(), 2);

        assert_eq!(schedules[0].name, "flush");
        assert_eq!(schedules[0].spec, ScheduleSpec::IntervalSeconds(60));
        assert!(schedules[0].next > chrono::Local::now());

        assert_eq!(schedules[1].name, "daily");
        assert_eq!(
            schedules[1].spec,
            ScheduleSpec::Cron("0 9 * * *".to_string())
        );
        assert!(schedules[1].next > chrono::Local::now());
    }

    /// ランナーループが公開した実際の発火時刻が、その場の推定値ではなく
    /// そのまま `plugins/list` に載ること。これが無かった頃、interval の
    /// `next` は RPC のたびに「now + interval」へ作り直され、スレッドの
    /// 実際の発火時点と無関係だった。
    #[test]
    fn list_reports_the_next_fire_published_by_the_runner_thread() {
        let registry = test_registry_with_schedule();

        // スレッドが「flush は 3 秒後に発火予定」と公開している状況を作る。
        // 推定値(now + 60s)とは明確に違う値にしておく。
        let published_flush = chrono::Local::now() + chrono::Duration::seconds(3);
        let view = crate::schedule::ScheduleView::default();
        view.set_for_test(vec![("flush".to_string(), published_flush)]);
        registry.register_schedule_view("scheduler-plugin", view);

        let infos = registry.list();
        let schedules = &infos[0].schedules;

        assert_eq!(schedules[0].name, "flush");
        assert_eq!(
            schedules[0].next, published_flush,
            "公開済みの発火時刻をそのまま返すこと"
        );

        // 公開されていない `daily` は推定値へフォールバックする。
        assert_eq!(schedules[1].name, "daily");
        assert!(schedules[1].next > chrono::Local::now());
    }

    /// スレッドがまだ何も公開していない(起動途中 / Disabled)場合は、
    /// 従来どおりその場の推定値を返す。
    #[test]
    fn list_falls_back_to_the_estimate_when_nothing_is_published() {
        let registry = test_registry_with_schedule();
        registry
            .register_schedule_view("scheduler-plugin", crate::schedule::ScheduleView::default());

        let infos = registry.list();
        assert_eq!(infos[0].schedules.len(), 2);
        assert!(infos[0].schedules[0].next > chrono::Local::now());
    }

    #[test]
    fn list_reports_empty_schedules_array_when_none_declared() {
        let registry = empty_registry();
        registry.push(PluginEntry {
            manifest: manifest_with_dashboard("no-schedule-plugin"),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });

        let infos = registry.list();
        assert_eq!(infos.len(), 1);
        assert!(infos[0].schedules.is_empty());
    }

    #[test]
    fn dashboard_reports_resolved_only_when_entry_file_exists() {
        let (registry, tmp) = test_registry_with_dashboard();
        let plugins_dir = tmp.path().join("plugins");
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
        let (registry, _tmp) = test_registry_with_dashboard();

        let infos = registry
            .set_dashboard_grant("widgety", "status", true)
            .unwrap();
        assert!(infos[0].grant.granted);
        let infos = registry
            .set_dashboard_grant("widgety", "status", false)
            .unwrap();
        assert!(!infos[0].grant.granted);
        let err = registry
            .set_dashboard_grant("widgety", "nope", true)
            .unwrap_err();
        assert!(matches!(err, RegistryError::UnknownDashboard(w) if w == "nope"));
    }

    #[test]
    fn dashboard_widgets_for_ui_lists_every_declared_widget() {
        let (registry, _tmp) = test_registry_with_dashboard();
        let widgets = registry.dashboard_widgets_for_ui();
        assert_eq!(widgets.len(), 1);
        let (plugin_id, plugin_name, _state, info) = &widgets[0];
        assert_eq!(plugin_id, "widgety");
        assert_eq!(plugin_name, "widgety");
        assert_eq!(info.request.id, "status");
        assert!(!info.grant.granted);
    }

    #[test]
    fn dashboard_asset_path_requires_grant_and_rejects_traversal() {
        let (registry, tmp) = test_registry_with_dashboard();
        let ui_dir = tmp.path().join("plugins").join("widgety").join("ui");
        std::fs::create_dir_all(&ui_dir).unwrap();
        std::fs::write(ui_dir.join("index.html"), "<html></html>").unwrap();
        std::fs::write(ui_dir.join("app.js"), "//").unwrap();

        // 未 grant → エラー
        let err = registry
            .dashboard_asset_path("widgety", "status", "index.html")
            .unwrap_err();
        assert!(matches!(err, RegistryError::DashboardNotGranted(_)));

        registry
            .set_dashboard_grant("widgety", "status", true)
            .unwrap();
        // 正常系: entry ディレクトリ配下のファイル
        let path = registry
            .dashboard_asset_path("widgety", "status", "app.js")
            .unwrap();
        assert!(path.ends_with("widgety/ui/app.js"));
        // 空パスは entry ファイル自身
        let path = registry
            .dashboard_asset_path("widgety", "status", "")
            .unwrap();
        assert!(path.ends_with("widgety/ui/index.html"));
        // トラバーサルは拒否
        assert!(registry
            .dashboard_asset_path("widgety", "status", "../manifest.toml")
            .is_err());
        assert!(registry
            .dashboard_asset_path("widgety", "status", "a/../../manifest.toml")
            .is_err());
        assert!(registry
            .dashboard_asset_path("widgety", "status", "/etc/passwd")
            .is_err());
        // 未知 widget / plugin も拒否
        assert!(registry
            .dashboard_asset_path("widgety", "nope", "index.html")
            .is_err());
        assert!(registry
            .dashboard_asset_path("nope", "status", "index.html")
            .is_err());
    }

    /// `plugins/dashboard-action` の実体の検証: grant 必須、スレッド未登録なら
    /// `PluginNotRunning`、登録済みなら `on-message(driver = "dashboard")` の
    /// 形(`PluginWork::Message`)で作業キューへ届く。
    #[test]
    fn dashboard_action_requires_grant_and_delivers_to_the_work_queue() {
        let (registry, _tmp) = test_registry_with_dashboard();

        let err = registry
            .dashboard_action("widgety", "status", "resync")
            .unwrap_err();
        assert!(matches!(err, RegistryError::DashboardNotGranted(_)));

        registry
            .set_dashboard_grant("widgety", "status", true)
            .unwrap();

        let err = registry
            .dashboard_action("widgety", "status", "resync")
            .unwrap_err();
        assert!(matches!(err, RegistryError::PluginNotRunning(id) if id == "widgety"));

        let (work_tx, work_rx) = crate::runner::plugin::queue::channel();
        registry.register_plugin_thread(
            "widgety",
            work_tx,
            thread::spawn(|| {}),
            Arc::new(AtomicBool::new(false)),
        );
        registry
            .dashboard_action("widgety", "status", "resync")
            .unwrap();
        match work_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            PluginWork::Message(delivery) => {
                assert_eq!(delivery.plugin_id, "widgety");
                assert_eq!(delivery.driver_id, "dashboard");
                assert_eq!(delivery.topic, "resync");
                assert!(delivery.payload.is_empty());
            }
            _ => panic!("expected PluginWork::Message"),
        }
    }

    #[test]
    fn a_bus_request_for_a_missing_driver_is_reported_as_unresolved() {
        // DriverRegistry が空のとき、BusInfo.resolved は false になる。
        let registry = test_registry_with_bus_request();
        let info = registry.bus("translator").unwrap();
        assert_eq!(info.len(), 1);
        assert!(!info[0].resolved);
    }

    /// 上のテストの裏付け: 同じ manifest でも、要求したトピックを両方とも
    /// 揃えたドライバがインストールされていれば `resolved` は `true` になる。
    /// 「ドライバが無ければ false」を「ドライバがあれば必ず true になりうる」
    /// と対で示すことで、`resolved` の計算自体(`manifest_of`/`topic` の
    /// 呼び出し)が効いていることを確認する -- さもないと前のテストは
    /// 「常に false を返す」実装でも通ってしまう。
    #[test]
    fn a_bus_request_whose_driver_and_topics_are_all_installed_is_resolved() {
        let tmp = tempfile::tempdir().unwrap();
        let driver_registry = empty_driver_registry(tmp.path());
        driver_registry.push(crate::registry::driver::DriverEntry {
            manifest: crate::manifest::driver::DriverManifest {
                id: "ed-state".into(),
                name: "ED State".into(),
                version: "0.1.0".into(),
                description: String::new(),
                entry: "driver.wasm".into(),
                topics: vec![
                    edlr_driver_channel::TopicSpec {
                        name: "ship-status".into(),
                        retain: false,
                        description: String::new(),
                    },
                    edlr_driver_channel::TopicSpec {
                        name: "current-system".into(),
                        retain: true,
                        description: String::new(),
                    },
                ],
                settings: vec![],
                capabilities: vec![],
                sidecars: vec![],
                filesystem: vec![],
            },
            state: crate::registry::driver::DriverState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });

        let registry = test_registry_with_bus_request_using(driver_registry);

        let info = registry.bus("translator").unwrap();
        assert_eq!(info.len(), 1);
        assert!(
            info[0].resolved,
            "driver is installed and both requested topics are declared, so this must resolve"
        );
    }

    /// `set_bus_grant` は `GrantsStore` への永続化と `bus_json` バッファの
    /// 作り直しを両方行う。承認・取消の両方を同じプラグイン・同じ manifest
    /// で確認し、`bus_json` に載る `publish`/`subscribe` の有無が承認状態と
    /// 一致することを見る(`crate::runtime::bus` の「未承認は topics を落とす」
    /// 契約どおりであることの確認)。
    #[test]
    fn set_bus_grant_persists_and_updates_the_shared_buffer_both_ways() {
        let registry = test_registry_with_bus_request();

        let granted = registry
            .set_bus_grant("translator", "ed-state", true)
            .expect("granting a declared bus connection should succeed");
        assert!(granted.granted);
        let parsed = crate::runtime::bus::parse_bus(&registry.bus_buffer("translator").unwrap());
        let entry = parsed.get("ed-state").expect("entry present after grant");
        assert!(entry.granted);
        assert_eq!(entry.subscribe, vec!["current-system".to_string()]);

        let revoked = registry
            .set_bus_grant("translator", "ed-state", false)
            .expect("revoking should succeed");
        assert!(!revoked.granted);
        let parsed = crate::runtime::bus::parse_bus(&registry.bus_buffer("translator").unwrap());
        let entry = parsed
            .get("ed-state")
            .expect("entry still present after revoke");
        assert!(!entry.granted);
        assert!(
            entry.subscribe.is_empty(),
            "a revoked bus connection must not keep its topics in the shared buffer"
        );
    }

    #[test]
    fn set_bus_grant_for_an_undeclared_driver_returns_unknown_bus_error() {
        let registry = test_registry_with_bus_request();
        let err = registry
            .set_bus_grant("translator", "not-declared", true)
            .expect_err("undeclared bus connection must be rejected");
        assert!(matches!(err, RegistryError::UnknownBus(driver) if driver == "not-declared"));
    }

    /// 承認取消は、同じプラグインのサイドカー停止の裏で待たされてはならない。
    ///
    /// `refresh_sidecar_runtime` は同期 `ProcessDriver::stop` を臨界区間に
    /// 含み、SIGTERM を無視するサイドカー × インスタンス数だけブロックしうる。
    /// ファイルアクセスがその id 別ロックを共有していると、その間
    /// `filesystem_json` は古い `granted:true` と path を保持したままになり、
    /// ディスク上は取り消された承認で読み書きが続く(fail-open)。
    /// ここではサイドカー側の臨界区間をロックを掴んだまま模し、その間でも
    /// ファイルアクセスの取消が共有バッファへ速やかに反映されることを見る。
    #[test]
    fn revoking_filesystem_access_is_not_blocked_by_a_sidecar_stop_in_progress() {
        let host = Arc::new(
            PluginHost::new(crate::host::drivers::test_handle()).expect("host should start"),
        );
        let tmp = tempfile::tempdir().unwrap();
        let settings_store = Arc::new(SettingsStore::new(tmp.path().join("settings")));
        let grants_store = Arc::new(GrantsStore::new(tmp.path().join("grants")));
        let sidecar_config_store = Arc::new(SidecarConfigStore::new(tmp.path().join("settings")));
        let filesystem_config_store = Arc::new(FilesystemConfigStore::new(
            tmp.path().join("settings"),
            Vec::new(),
        ));
        let process_driver = Arc::new(ProcessDriver::new(
            Duration::from_millis(200),
            Duration::from_millis(0),
        ));
        let registry = Registry::new(
            host,
            settings_store,
            grants_store,
            sidecar_config_store,
            filesystem_config_store,
            process_driver,
            empty_driver_registry(tmp.path()),
            edlr_driver_channel::Bus::new(),
            tmp.path().join("plugins"),
        );

        let manifest = manifest_with_filesystem("fs-plugin");
        let filesystem_json = Arc::new(Mutex::new("[]".to_string()));
        registry.push(PluginEntry {
            manifest,
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: filesystem_json.clone(),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });

        let root = tmp.path().join("exports");
        std::fs::create_dir(&root).unwrap();
        registry
            .set_filesystem_config(
                "fs-plugin",
                "exports",
                &FilesystemConfig {
                    path: root.to_string_lossy().to_string(),
                },
            )
            .expect("configuring the directory should succeed");
        registry
            .set_filesystem_grant("fs-plugin", "exports", true)
            .expect("granting should succeed");
        let granted =
            crate::runtime::fs::parse_filesystem(&filesystem_json.lock().unwrap().clone());
        assert!(granted["exports"].granted);
        assert!(!granted["exports"].path.is_empty());

        // サイドカー停止(`ProcessDriver::stop`)がまだ終わっていない状態を、
        // その臨界区間で使われる id 別ロックを掴んだまま模す。
        let sidecar_lock = registry
            .sidecar_service
            .sidecar_runtime_lock_for("fs-plugin");
        let sidecar_guard = sidecar_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let (tx, rx) = std::sync::mpsc::channel();
        let revoker = {
            let registry = registry.clone();
            thread::spawn(move || {
                let _ = tx.send(registry.set_filesystem_grant("fs-plugin", "exports", false));
            })
        };

        let result = rx
            .recv_timeout(Duration::from_secs(3))
            .expect("revoking filesystem access must not wait for the sidecar critical section");
        result.expect("revoking should succeed");
        revoker.join().expect("revoker thread should not panic");

        let revoked =
            crate::runtime::fs::parse_filesystem(&filesystem_json.lock().unwrap().clone());
        assert!(!revoked["exports"].granted);
        assert_eq!(
            revoked["exports"].path, "",
            "a revoked root must not keep its path in the shared buffer"
        );

        drop(sidecar_guard);
    }

    fn manifest_with_sidecar(id: &str, port: u16) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![crate::capability::request::SidecarRequest {
                name: "tts".into(),
                reason: "reason".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                port,
                scalable: false,
            }],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    /// Regression test for a review finding: disabling a plugin used to only
    /// flip `PluginEntry::state` to `Disabled`, leaving any sidecar it had
    /// started running with nobody left able to stop it (the plugin's own
    /// thread has already exited by the time `set_disabled` is reached from
    /// `runner.rs`). `set_disabled` must now also stop every sidecar the
    /// disabled plugin's manifest declares.
    #[test]
    fn set_disabled_stops_all_sidecars_of_that_plugin() {
        let registry = empty_registry();
        let manifest = manifest_with_sidecar("sc-plugin", 50900);
        registry.push(PluginEntry {
            manifest: manifest.clone(),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });

        let key = crate::registry::sidecar::sidecar_key("sc-plugin", "tts");
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "sleep 30".into()],
            ports: vec![50900],
        };
        registry
            .sidecar_service
            .process_driver()
            .ensure_started(&key, &spec)
            .expect("start sidecar directly via the driver, bypassing wasm entirely");
        assert!(
            registry
                .sidecar_service
                .process_driver()
                .status(&key, &spec)[0]
                .running,
            "sidecar should be running before disabling the plugin"
        );

        registry.set_disabled("sc-plugin", "on-event trapped".to_string());

        assert!(
            !registry
                .sidecar_service
                .process_driver()
                .status(&key, &spec)[0]
                .running,
            "set_disabled must stop the disabled plugin's sidecars"
        );
        let snapshot = registry.snapshot();
        assert!(matches!(
            snapshot
                .iter()
                .find(|(m, _)| m.id == "sc-plugin")
                .map(|(_, s)| s.clone()),
            Some(PluginState::Disabled { .. })
        ));
    }

    /// Regression test for a re-review finding: `control_sidecar` used to
    /// ignore `PluginState` entirely, so a `plugins/sidecar-control` `start`
    /// arriving after (or racing with) `set_disabled` could bring a disabled
    /// plugin's sidecar back to life even though nothing running could ever
    /// stop it again -- the "disabled implies its sidecars stay stopped"
    /// invariant `set_disabled` establishes didn't persist. `Start`/`Restart`
    /// must now be rejected once the plugin is `Disabled`, while `Stop` must
    /// remain unconditionally available (a stop action must never be blocked
    /// by the plugin's own disabled state).
    #[test]
    fn control_sidecar_rejects_start_and_restart_once_the_plugin_is_disabled_but_allows_stop() {
        let registry = empty_registry();
        let manifest = manifest_with_sidecar("sc-plugin", 50930);
        registry.push(PluginEntry {
            manifest: manifest.clone(),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });
        registry
            .sidecar_service
            .grants_store()
            .set_sidecar(&manifest, "tts", true)
            .expect("grant should persist");
        registry
            .sidecar_service
            .sidecar_config_store()
            .update_and_effective(
                &manifest,
                "tts",
                &SidecarConfig {
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    port: 50930,
                    replicas: 1,
                },
            )
            .expect("config should persist");

        registry.set_disabled("sc-plugin", "on-event trapped".to_string());

        let start_result = registry.control_sidecar("sc-plugin", "tts", SidecarAction::Start);
        match start_result {
            Err(RegistryError::Sidecar(_)) => {}
            _ => panic!(
                "Start on a disabled plugin's sidecar must be rejected as RegistryError::Sidecar"
            ),
        }
        let restart_result = registry.control_sidecar("sc-plugin", "tts", SidecarAction::Restart);
        match restart_result {
            Err(RegistryError::Sidecar(_)) => {}
            _ => panic!(
                "Restart on a disabled plugin's sidecar must be rejected as RegistryError::Sidecar"
            ),
        }

        let key = crate::registry::sidecar::sidecar_key("sc-plugin", "tts");
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            ports: vec![50930],
        };
        assert!(
            !registry
                .sidecar_service
                .process_driver()
                .status(&key, &spec)[0]
                .running,
            "rejected Start/Restart must not have spawned anything"
        );

        // Stop must remain available regardless of the disabled state.
        registry
            .control_sidecar("sc-plugin", "tts", SidecarAction::Stop)
            .expect("Stop must never be blocked by the plugin's disabled state");
    }

    /// Regression test for the TOCTOU the security review flagged in
    /// `control_sidecar`: `Start`/`Restart` used to read the sidecar's grant
    /// and config, then spawn, with no lock held across the two steps --
    /// so a concurrent `set_sidecar_grant(false)` could land in between,
    /// and `control_sidecar` would spawn (or leave running) an instance
    /// using the grant it read *before* the revocation, even though the
    /// on-disk grant is now revoked. `control_sidecar`'s fix takes the same
    /// per-plugin `sidecar_runtime_lock_for(id)` that `set_sidecar_grant`
    /// (via `refresh_sidecar_runtime`) takes, so the two calls can no longer
    /// interleave.
    ///
    /// Like `concurrent_set_capabilities_keeps_shared_buffer_consistent_with_disk`
    /// above, this hammers the race from many threads and checks the
    /// invariant that must hold once every thread has finished: if the
    /// on-disk grant ends up revoked, no instance may be left running. It's
    /// a standing regression guard under real concurrent load rather than a
    /// guaranteed reproduction of the pre-fix bug (the race window without
    /// the fix is narrow and timing-dependent, same caveat as that test).
    #[test]
    fn concurrent_control_sidecar_start_and_grant_revoke_never_leaves_an_ungranted_instance_running(
    ) {
        let registry = empty_registry();
        let manifest = manifest_with_sidecar("sc-plugin", 50910);
        registry.push(PluginEntry {
            manifest: manifest.clone(),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });

        registry
            .sidecar_service
            .grants_store()
            .set_sidecar(&manifest, "tts", true)
            .expect("grant should persist");
        registry
            .sidecar_service
            .sidecar_config_store()
            .update_and_effective(
                &manifest,
                "tts",
                &SidecarConfig {
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    port: 50910,
                    replicas: 1,
                },
            )
            .expect("config should persist");

        const THREADS: usize = 8;
        const ITERATIONS: usize = 20;
        let barrier = Arc::new(std::sync::Barrier::new(THREADS + 1));

        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let registry = registry.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..ITERATIONS {
                    let _ = registry.control_sidecar("sc-plugin", "tts", SidecarAction::Start);
                }
            }));
        }
        let revoker = {
            let registry = registry.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                // Revoke partway through the hammering, then leave the
                // grant revoked (last write wins on the grants file, and
                // this is this test's only writer of the grant).
                std::thread::sleep(Duration::from_millis(5));
                registry
                    .set_sidecar_grant("sc-plugin", "tts", false)
                    .expect("revoke should succeed");
            })
        };

        for handle in handles {
            handle.join().expect("worker thread should not panic");
        }
        revoker.join().expect("revoker thread should not panic");

        let disk_granted = registry
            .sidecar_service
            .grants_store()
            .sidecar_state(&manifest, "tts")
            .granted;
        let key = crate::registry::sidecar::sidecar_key("sc-plugin", "tts");
        let running = registry
            .sidecar_service
            .process_driver()
            .status(
                &key,
                &ProcessSpec {
                    command: PathBuf::from("/bin/sh"),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    ports: vec![50910],
                },
            )
            .iter()
            .any(|i| i.running);

        assert!(
            disk_granted || !running,
            "sidecar is running (running={running}) while the on-disk grant is \
             revoked (granted={disk_granted}) -- a Start must have used a grant \
             value read before the concurrent revoke took effect"
        );

        registry.sidecar_service.process_driver().stop(&key);
    }

    /// Regression test for a review finding: `set_sidecar_grant` used to
    /// persist `granted == true` unconditionally, without checking whether
    /// the sidecar's `command` was ever configured. That let a caller grant
    /// a sidecar before the user had seen (or set) what executable it would
    /// run -- the UI enforces this client-side (the checkbox is `disabled`
    /// while `command` is empty), but that's not a substitute for enforcing
    /// it in the store: a direct RPC call bypasses the UI entirely.
    #[test]
    fn set_sidecar_grant_rejects_granting_without_a_configured_command() {
        let registry = empty_registry();
        let manifest = manifest_with_sidecar("sc-plugin", 50920);
        registry.push(PluginEntry {
            manifest: manifest.clone(),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });

        let result = registry.set_sidecar_grant("sc-plugin", "tts", true);
        match result {
            Err(RegistryError::Sidecar(_)) => {}
            _ => panic!(
                "granting with no command configured must be rejected as RegistryError::Sidecar"
            ),
        }
        assert!(
            !registry
                .sidecar_service
                .grants_store()
                .sidecar_state(&manifest, "tts")
                .granted,
            "a rejected grant must not be persisted"
        );

        // Revoking (granted == false) must always be allowed, even with no
        // command configured -- otherwise a caller couldn't back out of a
        // stale/invalid state.
        registry
            .set_sidecar_grant("sc-plugin", "tts", false)
            .expect("revoke must always succeed");

        // Once a command is configured, granting succeeds.
        registry
            .sidecar_service
            .sidecar_config_store()
            .update_and_effective(
                &manifest,
                "tts",
                &SidecarConfig {
                    command: "/bin/true".to_string(),
                    args: vec![],
                    port: 50920,
                    replicas: 1,
                },
            )
            .expect("config should persist");
        registry
            .set_sidecar_grant("sc-plugin", "tts", true)
            .expect("granting with a configured command must succeed");
        assert!(
            registry
                .sidecar_service
                .grants_store()
                .sidecar_state(&manifest, "tts")
                .granted
        );
    }

    fn manifest_with_http_capability(id: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![crate::capability::request::CapabilityRequest::Http {
                hosts: vec!["https://api.example.com".to_string()],
                reason: "test".into(),
            }],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    #[test]
    fn set_capabilities_persists_grant_and_updates_shared_capabilities_json() {
        let registry = empty_registry();
        let manifest = manifest_with_http_capability("cap-plugin");
        let capabilities_json = Arc::new(Mutex::new(
            crate::host::plugin::capabilities_json_string(&[]),
        ));
        registry.push(PluginEntry {
            manifest: manifest.clone(),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: capabilities_json.clone(),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });

        let state = registry
            .set_capabilities("cap-plugin", true)
            .expect("set_capabilities should succeed");

        assert!(state.granted);
        assert!(!state.stale);

        let stored = capabilities_json.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            parsed["hosts"],
            serde_json::json!(["https://api.example.com"])
        );

        // Revoking updates the shared buffer again, live, without touching
        // the registered entry itself.
        let state = registry
            .set_capabilities("cap-plugin", false)
            .expect("revoke should succeed");
        assert!(!state.granted);
        let stored = capabilities_json.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(parsed["hosts"], serde_json::json!([]));
    }

    /// Regression test for a race the security review flagged: without
    /// serializing "persist to `GrantsStore`" + "overwrite the shared
    /// `capabilities_json` buffer" as one unit, two concurrent
    /// `set_capabilities` calls could interleave so that the on-disk grant
    /// and the live buffer end up disagreeing (see `set_capabilities`'s doc
    /// comment for the exact interleaving). This hammers the same plugin
    /// from many threads at once and asserts the buffer always agrees with
    /// what `GrantsStore` reports from disk once every thread has finished.
    ///
    /// This test is inherently about scheduling: with organic OS scheduling
    /// alone the race window here is narrow enough that this plain
    /// N-threads-hammering-the-same-plugin form did not reliably fail on the
    /// pre-fix code in local runs (see the task report for how the race was
    /// actually forced open and confirmed, using a temporary artificial
    /// stall). Kept here anyway as a standing regression guard under real
    /// concurrent load; it is deterministic (always passes) with the
    /// `capabilities_lock` fix in place, since the fix removes the race
    /// window entirely (single critical section) rather than narrowing it,
    /// so no amount of scheduling luck can reintroduce a disagreement.
    #[test]
    fn concurrent_set_capabilities_keeps_shared_buffer_consistent_with_disk() {
        let host = Arc::new(
            PluginHost::new(crate::host::drivers::test_handle()).expect("host should start"),
        );
        let tmp = tempfile::tempdir().unwrap();
        let settings_store = Arc::new(SettingsStore::new(tmp.path().join("settings")));
        let grants_store = Arc::new(GrantsStore::new(tmp.path().join("grants")));
        let sidecar_config_store = Arc::new(SidecarConfigStore::new(tmp.path().join("settings")));
        let filesystem_config_store = Arc::new(FilesystemConfigStore::new(
            tmp.path().join("settings"),
            Vec::new(),
        ));
        let process_driver = Arc::new(ProcessDriver::new(
            Duration::from_millis(200),
            Duration::from_millis(0),
        ));
        let registry = Registry::new(
            host,
            settings_store,
            grants_store.clone(),
            sidecar_config_store,
            filesystem_config_store,
            process_driver,
            empty_driver_registry(tmp.path()),
            edlr_driver_channel::Bus::new(),
            tmp.path().join("plugins"),
        );

        let manifest = manifest_with_http_capability("cap-plugin");
        let capabilities_json = Arc::new(Mutex::new(
            crate::host::plugin::capabilities_json_string(&[]),
        ));
        registry.push(PluginEntry {
            manifest: manifest.clone(),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: capabilities_json.clone(),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });

        const THREADS: usize = 16;
        const ITERATIONS: usize = 30;
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));

        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let registry = registry.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    for i in 0..ITERATIONS {
                        let granted = (t + i) % 2 == 0;
                        let _ = registry.set_capabilities("cap-plugin", granted);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("worker thread should not panic");
        }

        let disk_granted = grants_store.state(&manifest).granted;
        // `capabilities_json` no longer carries an explicit `granted` flag
        // (see `capabilities_json_string`'s doc comment): "granted" is now
        // inferred from whether the effective `hosts` list is non-empty,
        // which holds for this manifest since it always requests a
        // non-empty host list when granted.
        let buffer_granted: bool = {
            let stored = capabilities_json.lock().unwrap().clone();
            let parsed: serde_json::Value = serde_json::from_str(&stored).unwrap();
            !parsed["hosts"]
                .as_array()
                .map(|hosts| hosts.is_empty())
                .unwrap_or(true)
        };

        assert_eq!(
            disk_granted, buffer_granted,
            "shared capabilities_json buffer (granted={buffer_granted}) disagrees \
             with GrantsStore's on-disk state (granted={disk_granted}) after \
             concurrent set_capabilities calls"
        );
    }

    /// Registry::list が entry の layout をそのまま PluginInfo へ載せることの
    /// 固定(Task 5)。
    #[test]
    fn list_carries_layout_through() {
        let registry = empty_registry();
        let layout = crate::layout::Layout {
            sections: vec![crate::layout::Section {
                title: "基本".into(),
                description: None,
                children: vec![crate::layout::Node::Field {
                    field: "voice".into(),
                }],
            }],
        };
        let mut manifest = plain_manifest("layout-plugin");
        manifest.settings = vec![SettingField::String {
            key: "voice".into(),
            label: "Voice".into(),
            default: String::new(),
        }];
        registry.push(PluginEntry {
            manifest,
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: Some(layout.clone()),
        });

        let infos = registry.list();

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].layout, Some(layout));
    }

    /// Task 5: `shutdown_plugins` のための最小限の manifest(id 以外はどの
    /// テストでも共通)。
    fn plain_manifest(id: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    /// `id` の `Running` エントリを(`register_plugin_thread` は呼ばずに)
    /// `registry` へ載せる。`shutdown_plugins` のテストは別途
    /// `register_plugin_thread` を呼んでスレッドを登録する。
    fn push_running_entry(registry: &Registry, id: &str) {
        registry.push(PluginEntry {
            manifest: plain_manifest(id),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });
    }

    /// `shutdown_plugins` が Running かつ登録済みのプラグインへ
    /// `PluginWork::Stop` を送り、そのスレッドの終了を実際に待つことの検証。
    /// wasm の実体は無く、`register_plugin_thread` に渡すのは `Stop` を
    /// 受け取ったらフラグを立てて終了するだけの素のスレッド
    /// (`runner::run_plugin_thread` の `LoopAction::Stop` 分岐の骨格だけを
    /// 模したもの)。
    #[test]
    fn shutdown_plugins_sends_stop_and_waits_for_the_registered_thread_to_exit() {
        let registry = empty_registry();
        push_running_entry(&registry, "stoppable");

        let (work_tx, work_rx) = crate::runner::plugin::queue::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let handle = {
            let stopped = stopped.clone();
            thread::spawn(move || {
                if let Ok(PluginWork::Stop) = work_rx.recv_timeout(Duration::from_secs(5)) {
                    stopped.store(true, Ordering::SeqCst);
                }
            })
        };
        registry.register_plugin_thread(
            "stoppable",
            work_tx,
            handle,
            Arc::new(AtomicBool::new(false)),
        );

        registry.shutdown_plugins();

        assert!(
            stopped.load(Ordering::SeqCst),
            "shutdown_plugins must send PluginWork::Stop to a registered Running \
             plugin's thread and wait for it to exit"
        );
    }

    /// **`Stop` がワークキューを追い越すことの検証**。キューが満杯で
    /// `try_send(Stop)` が失敗しても、ランナーループはキューを読む前に
    /// `stop_flag` を見るので on-stop へ到達できる。
    ///
    /// かつては `Stop` がキュー経由だけだったため、詰まったプラグインは
    /// 先行ワークを全部消化するまで on-stop に辿り着けず、5 秒しか待たない
    /// `shutdown_plugins` から見ると flush は事実上スキップされていた。
    #[test]
    fn shutdown_plugins_stop_flag_overtakes_a_full_work_queue() {
        let registry = empty_registry();
        push_running_entry(&registry, "full-queue");

        let (work_tx, work_rx) = crate::runner::plugin::queue::channel();
        // キューを埋めて `push(Stop)` が `Dropped` になるようにする。
        for _ in 0..crate::runner::plugin::PLUGIN_WORK_QUEUE_CAPACITY {
            work_tx
                .push(PluginWork::Event(Arc::new(Event::Status {
                    raw: serde_json::json!({}),
                })))
                .expect("filling the queue up to capacity should succeed");
        }

        let stop_flag = Arc::new(AtomicBool::new(false));
        let flushed = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop_flag = stop_flag.clone();
            let flushed = flushed.clone();
            // `run_plugin_thread` のループの骨格: 毎周期、キューを読む**前**に
            // stop フラグを確認する。
            thread::spawn(move || loop {
                if stop_flag.load(Ordering::SeqCst) {
                    flushed.store(true, Ordering::SeqCst);
                    return;
                }
                let _ = work_rx.recv_timeout(Duration::from_millis(10));
            })
        };
        registry.register_plugin_thread("full-queue", work_tx, handle, stop_flag);

        registry.shutdown_plugins();

        assert!(
            flushed.load(Ordering::SeqCst),
            "a plugin with a full work queue must still reach on-stop, because the \
             stop flag is checked before the queue is read"
        );
    }

    /// `Disabled`(init 失敗などで Running にならなかった)プラグインは
    /// `register_plugin_thread` を呼ばれない(`runner::load_and_run_plugin`
    /// が Running のときだけ呼ぶ設計)ため、`shutdown_plugins` にとっては
    /// 最初から存在しないのと同じになる。ここではその前提のもとで
    /// `shutdown_plugins` が(送るべき相手がいなくても)パニックしないことを
    /// 確認する。
    #[test]
    fn shutdown_plugins_is_a_no_op_for_disabled_plugins_that_were_never_registered() {
        let registry = empty_registry();
        registry.push(PluginEntry {
            manifest: plain_manifest("disabled-plugin"),
            state: PluginState::Disabled {
                reason: "init failed".to_string(),
            },
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::host::plugin::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
            layout: None,
        });

        registry.shutdown_plugins();
    }
}

//! 実行中プラグインの状態を保持する共有ビュー。`start_plugins` が構築し、以後は
//! カーネル内の複数箇所(将来の RPC を含む)から `Clone` して読める。

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use edlr_driver_process::{InstanceStatus, ProcessDriver, ProcessSpec};

use crate::driver::registry::DriverRegistry;
use crate::plugin::bus_runtime::{bus_json_string, BusRuntimeEntry};
use crate::plugin::filesystem::{FilesystemConfig, FilesystemConfigError, FilesystemConfigStore};
use crate::plugin::dropped::{DropCounters, DroppedCounts};
use crate::plugin::fs_runtime::{filesystem_json_string, FsRuntimeEntry};
use crate::plugin::grants::{GrantState, GrantsError, GrantsStore};
use crate::plugin::host::{capabilities_json_string, parse_capability_hosts, PluginHost};
use crate::plugin::runner::PluginWork;
use crate::plugin::schedule::{Clock, ScheduleState, ScheduleView};
use crate::plugin::settings::{split_secrets, SettingsStore};
use crate::plugin::sidecar::{assign_ports, SidecarConfig, SidecarConfigError, SidecarConfigStore};
use crate::plugin::sidecar_runtime::{
    implicit_http_hosts, sidecars_json_string, SidecarRuntimeEntry,
};
use crate::plugin::{
    BusRequest, CapabilityRequest, DashboardWidget, FilesystemRequest, Manifest, ScheduleSpec,
    SettingsError, SidecarRequest,
};

/// `Registry::shutdown_plugins` が 1 プラグインの `JoinHandle` を待つ上限。
/// `edlr_config::PLUGIN_ON_STOP_GRACE_SECS` のドキュメントコメント参照
/// (= `PluginInstance::CALL_DEADLINE` + 余裕)。
const PLUGIN_STOP_JOIN_TIMEOUT: Duration =
    Duration::from_secs(edlr_config::PLUGIN_ON_STOP_GRACE_SECS);

/// `shutdown_plugins` が `JoinHandle::is_finished()` をポーリングする間隔。
/// std に `join_timeout` が無いための代替手段(タスクブリーフ参照)。値自体に
/// 強い意味は無く、`PLUGIN_STOP_JOIN_TIMEOUT` に対して十分細かく、かつ
/// 無駄なビジーポーリングにならない程度、という程度の選択。
const PLUGIN_STOP_JOIN_POLL_INTERVAL: Duration = Duration::from_millis(20);

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
    /// `sidecar_runtime::sidecars_json_string` を参照。
    /// `Registry::refresh_sidecar_runtime` がここを更新すると、次回以降の
    /// `driver-process.ensure-started` 呼び出しに再起動不要で反映される。
    pub sidecars_json: Arc<Mutex<String>>,
    /// `HostCtx` と共有されるファイルアクセス承認状態・実パス JSON。形は
    /// `fs_runtime::filesystem_json_string` を参照。
    /// `Registry::refresh_filesystem_runtime` がここを更新すると、次回以降の
    /// `driver-fs.*` 呼び出しに再起動不要で反映される。未承認のルートは
    /// `path` を持たない(`fs_runtime` のドキュメント参照)。
    pub filesystem_json: Arc<Mutex<String>>,
    /// `HostCtx` と共有されるバス承認状態・宣言済みトピック JSON。形は
    /// `bus_runtime::bus_json_string` を参照。`filesystem_json` と同じく
    /// 起動時に `GrantsStore::bus_state` と manifest から組み立てられ、以後は
    /// 将来の `Registry` の bus 承認 API(Task 10)が更新する。
    pub bus_json: Arc<Mutex<String>>,
}

/// サイドカー 1 件分の現在状態(`Registry::sidecars` / `PluginInfo::sidecars` 用)。
pub struct SidecarInfo {
    pub request: SidecarRequest,
    pub config: SidecarConfig,
    pub grant: GrantState,
    pub instances: Vec<InstanceStatus>,
}

/// ファイルアクセスのルート 1 件分の現在状態
/// (`Registry::filesystem` / `PluginInfo::filesystem` 用)。
#[derive(Debug)]
pub struct FilesystemInfo {
    pub request: FilesystemRequest,
    pub config: FilesystemConfig,
    pub grant: GrantState,
}

/// バス接続先 1 件分の現在状態(`Registry::bus` / `PluginInfo::bus` 用)。
///
/// `resolved` は「宣言している接続先が実在するか」を表す: 接続先ドライバが
/// インストールされていない、または宣言したトピック(`publish`/`subscribe`
/// のいずれか)がそのドライバの `driver.toml` に無い場合に `false` になる。
/// **承認(`grant`)とは独立**: 未承認でも接続先自体は解決していることが
/// あり得るし、逆に解決していない接続先を承認すること自体は妨げない
/// (ドライバが後から入れば、既に承認済みの状態のまま解決される)。
#[derive(Debug)]
pub struct BusInfo {
    pub request: BusRequest,
    pub grant: GrantState,
    pub resolved: bool,
}

/// ダッシュボードウィジェット 1 件の RPC 応答用スナップショット
/// (`BusInfo` と同じ流儀)。`resolved` は entry ファイルが plugins_dir 内に
/// 実在するかどうか。承認とは独立(未解決でも承認自体は妨げない -- entry を
/// 後から置けば、承認済みのまま解決される)。
#[derive(Debug)]
pub struct DashboardInfo {
    pub request: DashboardWidget,
    pub grant: GrantState,
    pub resolved: bool,
}

/// スケジュール 1 件の RPC 応答用スナップショット(`PluginInfo::schedules` 用)。
///
/// `next` はプラグインスレッドが実際に予定している発火時刻。
///
/// 真の発火スケジュール(`ScheduleState`)はプラグイン専用スレッドが所有して
/// おり、`take_due` が状態を進める可変操作である以上、そのままスレッドを
/// またいで共有はできない。代わりにランナーループが自分の状態を更新する
/// たびに `ScheduleView` へ壁時計へ変換済みのスナップショットを書き込み、
/// `plugins/list` はそれを読む(`Registry::schedule_views`)。
///
/// プラグインがまだ公開していない(起動途中)か、Disabled でスレッドが
/// 存在しない場合だけ、`ScheduleState` をその場で作り直した**推定値**へ
/// フォールバックする。
#[derive(Debug, Clone)]
pub struct ScheduleInfo {
    pub name: String,
    pub spec: ScheduleSpec,
    pub next: chrono::DateTime<chrono::Local>,
}

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
    /// 指定された `id` のドライバが登録されていない。`UnknownPlugin` とは
    /// 別の variant にしてある: `crate::driver::registry::DriverRegistry` の
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
    entries: Arc<Mutex<Vec<PluginEntry>>>,
    _host: Arc<PluginHost>,
    settings_store: Arc<SettingsStore>,
    grants_store: Arc<GrantsStore>,
    sidecar_config_store: Arc<SidecarConfigStore>,
    filesystem_config_store: Arc<FilesystemConfigStore>,
    /// サイドカープロセスを実際に所有するドライバ。`PluginHost` が全プラグ
    /// インで共有している 1 インスタンスをそのまま指す(`HostCtx` に配線
    /// されているものと同じ `Arc`)。`Registry` はここに直接
    /// `ensure_started`/`status`/`stop`/`stop_all` を発行することで、
    /// wasm 呼び出しを経由せずホスト起点でサイドカーを操作できる。
    process_driver: Arc<ProcessDriver>,
    /// Serializes the whole "persist to `GrantsStore` + overwrite the shared
    /// `capabilities_json` buffer" sequence in `set_capabilities`, so the two
    /// writes for a given call always land together and in order relative to
    /// any other concurrent `set_capabilities` call. See `set_capabilities`
    /// for why this is needed (disk and the live buffer could otherwise
    /// disagree under concurrent callers, e.g. two RPC clients toggling the
    /// same plugin's capability at once).
    capabilities_lock: Arc<Mutex<()>>,
    /// `set_sidecar_config` / `set_sidecar_grant` が呼ぶ
    /// `refresh_sidecar_runtime`(停止 → `sidecars_json` の作り直し →
    /// `capabilities_json` の作り直し)の臨界区間を **プラグイン ID ごとに**
    /// 直列化するロックのマップ。id → 専用 `Arc<Mutex<()>>` を引く。
    ///
    /// なぜプラグイン単位にしたか: `refresh_sidecar_runtime` は同期版
    /// `ProcessDriver::stop` を臨界区間の中で呼ぶため、SIGTERM を無視する
    /// サイドカーが 1 つあれば `shutdown_grace`(既定 3 秒)× 停止対象の
    /// インスタンス数ブロックしうる。単一のグローバルロックだと、あるプラグ
    /// インのサイドカー停止待ちの間、無関係な別プラグインの
    /// `set_sidecar_config`/`set_sidecar_grant` まで足止めされてしまう。
    /// `capabilities_lock`(こちらは全プラグイン共有のグローバルロックの
    /// まま)が守っているのはインメモリのバッファ差し替えだけで済むのとは
    /// リスクの質が違うため、こちらだけプラグイン単位に分けてある。
    ///
    /// マップ自体を保護する `Mutex`(`sidecar_runtime_lock_for` を参照)は
    /// 「id からロックの `Arc` を引く/無ければ作る」間だけ保持し、返した
    /// `Arc<Mutex<()>>` の実際の臨界区間(プロセス停止・ディスク読み書き)の
    /// 間は保持しない。したがってマップの Mutex 自体がボトルネックになる
    /// ことはない(id ごとの `Arc<Mutex<()>>` を作る/引くだけの一瞬)。
    ///
    /// ロック取得順序は一方向を保っている: `entries` は「manifest と共有
    /// ハンドルのクローンを取る」瞬間だけ握ってすぐ手放し(この時点でマップの
    /// Mutex も id 別ロックもまだ取っていない)、その後
    /// `sidecar_runtime_lock_for(id)` でマップから id 別ロックを引いて
    /// (マップの Mutex は取得直後に手放す)、最後にその id 別ロックを取る。
    /// id 別ロックの臨界区間の中で `capabilities_lock` を(id をまたいで共有
    /// される `capabilities_json` バッファのため)追加で取ることがある
    /// (`refresh_sidecar_runtime` を参照)が、逆方向(`capabilities_lock` を
    /// 保持したまま id 別ロックやマップの Mutex を取る)は無い。
    /// `set_capabilities` は `capabilities_lock` だけを取り、id 別ロックにも
    /// マップの Mutex にも触れない。したがって「entries → マップの Mutex →
    /// id 別ロック → capabilities_lock」の一方向のみが成立し、循環しないので
    /// デッドロックしない。
    sidecar_runtime_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// `set_filesystem_config` / `set_filesystem_grant` が呼ぶ
    /// `refresh_filesystem_runtime`(`filesystem_json` の作り直し)の臨界
    /// 区間を **プラグイン ID ごとに** 直列化するロックのマップ。
    ///
    /// **サイドカーとは別のマップにしてある。** かつては
    /// `sidecar_runtime_locks` に相乗りしており、その根拠は「ファイルアクセス
    /// 側の臨界区間はディスク読み書きだけで短いから相乗りしてよい」だったが、
    /// 問題は逆向きだった: 長いのは**サイドカー側**で、`ProcessDriver::stop`
    /// が SIGTERM を無視するサイドカー × インスタンス数だけブロックする間、
    /// 同じプラグインのファイルアクセス承認取消がロック待ちで足止めされる。
    /// その間 `filesystem_json` は古い `granted:true` と path を保持したまま
    /// なので、ディスク上は取り消された承認でプラグインが読み書きを続けられて
    /// しまう(fail-open。設計書の「承認・取消は即座に反映される」に反する)。
    ///
    /// 両者が扱う資源は独立している(プロセス起動 vs. ディレクトリアクセス)
    /// ので、分けてもロック順序の一方向性は壊れない: どちらの臨界区間からも
    /// もう一方のマップ・id 別ロックを取ることは無く、
    /// `refresh_filesystem_runtime` は `capabilities_json` に触れないため
    /// `capabilities_lock` も取らない。
    filesystem_runtime_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// `set_bus_grant` が呼ぶ `refresh_bus_runtime`(`bus_json` の作り直し)の
    /// 臨界区間を **プラグイン ID ごとに** 直列化するロックのマップ。
    ///
    /// `filesystem_runtime_locks` と同じ理由で別マップにしてある: バス承認は
    /// 止めるべきプロセスを持たないので臨界区間自体は短いが、サイドカー側の
    /// `ProcessDriver::stop`(SIGTERM 無視の子がいれば `shutdown_grace` 秒
    /// ブロックしうる)と同じマップを共有すると、その停止待ちの間バス承認の
    /// 取消がロック待ちで足止めされ、その間 `bus_json` が古い `granted:true`
    /// を保持し続けてしまう(fail-open)。資源が独立している(バス承認 vs.
    /// プロセス起動/ディレクトリアクセス)以上、分けてもロック順序の一方向性は
    /// 壊れない。
    bus_runtime_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// バス接続先の解決状況(`BusInfo::resolved`)を答えるために保持する。
    /// `DriverRegistry` は `Clone` で安価に持ち回れる(`DriverRegistry` 自身の
    /// ドキュメント参照)。
    driver_registry: DriverRegistry,
    plugins_dir: PathBuf,
    /// `crate::plugin::runner::spawn_bus_subscriber` の各インスタンスが共有
    /// する shutdown フラグ。`shutdown_bus_subscribers` が立て、各購読タスクは
    /// `BUS_SUBSCRIBER_SHUTDOWN_POLL_INTERVAL` ごとにこれを見に行く。詳しい
    /// 経緯は `shutdown_bus_subscribers` のドキュメントコメント参照。
    bus_subscriber_shutdown: Arc<AtomicBool>,
    /// Running として起動した各プラグインの `work_tx`(の複製)と専用
    /// スレッドの `JoinHandle` を id で引けるようにしたマップ
    /// (`shutdown_plugins` 用)。`register_plugin_thread` が
    /// `runner::start_plugins` から呼ばれて登録する。Disabled で終わった
    /// プラグイン(init 失敗など)は登録しない -- そのスレッドは既に
    /// return 済みで `work_rx` を読んでいないため、`Stop` を送っても意味が
    /// 無い(`register_plugin_thread` のドキュメント参照)。
    plugin_threads: Arc<Mutex<HashMap<String, PluginThreadHandle>>>,
    /// 各プラグインのランナーループが公開する「実際の次回発火時刻」を id で
    /// 引けるようにしたマップ(`plugins/list` 用)。`register_schedule_view` が
    /// プラグイン専用スレッド自身から呼ばれて登録する。
    ///
    /// これが無かった頃、`build_schedule_infos` は RPC のたびに
    /// `ScheduleState` を作り直していたため、interval の `next` は常に
    /// 「now + interval」になり、スレッドの実際の発火時点と無関係だった
    /// (UI のカウントダウンが意味を持たなかった)。
    schedule_views: Arc<Mutex<HashMap<String, ScheduleView>>>,
    /// 各プラグインの「作業キュー満杯で捨てた件数」を id で引けるようにした
    /// マップ(`plugins/list` 用)。`register_drop_counters` が
    /// `runner::start_plugins` から呼ばれて登録する。
    ///
    /// 取りこぼしを黙って捨てるのは設計判断(遅いだけのプラグインを殺さない)
    /// だが、何がどれだけ失われているか分からないままではキュー容量の
    /// チューニングが当て推量になる。`plugin::dropped` のモジュールドキュメント
    /// を参照。
    drop_counters: Arc<Mutex<HashMap<String, Arc<DropCounters>>>>,
}

/// `Registry::plugin_threads` の 1 エントリ。`shutdown_plugins` 専用。
struct PluginThreadHandle {
    work_tx: std_mpsc::SyncSender<PluginWork>,
    handle: thread::JoinHandle<()>,
    /// `Stop` のアウトオブバンド経路。ランナーループが毎周期(ワークキューを
    /// 読む前に)確認するので、有界キューに積まれた先行ワークを追い越せる。
    /// `work_tx` への `PluginWork::Stop` 送信は、待ちに入っているスレッドを
    /// 起こすためのもの(`shutdown_plugins` 参照)。
    stop_flag: Arc<AtomicBool>,
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
        plugins_dir: PathBuf,
    ) -> Self {
        Registry {
            entries: Arc::new(Mutex::new(Vec::new())),
            _host: host,
            settings_store,
            grants_store,
            sidecar_config_store,
            filesystem_config_store,
            process_driver,
            capabilities_lock: Arc::new(Mutex::new(())),
            sidecar_runtime_locks: Arc::new(Mutex::new(HashMap::new())),
            filesystem_runtime_locks: Arc::new(Mutex::new(HashMap::new())),
            bus_runtime_locks: Arc::new(Mutex::new(HashMap::new())),
            driver_registry,
            plugins_dir,
            bus_subscriber_shutdown: Arc::new(AtomicBool::new(false)),
            plugin_threads: Arc::new(Mutex::new(HashMap::new())),
            schedule_views: Arc::new(Mutex::new(HashMap::new())),
            drop_counters: Arc::new(Mutex::new(HashMap::new())),
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
    pub(crate) fn register_plugin_thread(
        &self,
        id: &str,
        work_tx: std_mpsc::SyncSender<PluginWork>,
        handle: thread::JoinHandle<()>,
        stop_flag: Arc<AtomicBool>,
    ) {
        self.plugin_threads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                id.to_string(),
                PluginThreadHandle {
                    work_tx,
                    handle,
                    stop_flag,
                },
            );
    }

    /// プラグイン専用スレッドが、自分のスケジュール状態を公開する窓口を
    /// 登録する(`plugins/list` から読まれる)。スレッド自身がループへ入る
    /// 直前に呼ぶ(`runner::run_plugin_thread`)。
    ///
    /// スケジュールを 1 件も宣言していないプラグインは呼ばない。
    pub(crate) fn register_schedule_view(&self, id: &str, view: ScheduleView) {
        self.schedule_views
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_string(), view);
    }

    /// プラグインの取りこぼしカウンタを登録する(`plugins/list` から読まれる)。
    /// 購読タスクを起動する `runner::start_plugins` が呼ぶ。
    pub(crate) fn register_drop_counters(&self, id: &str, counters: Arc<DropCounters>) {
        self.drop_counters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_string(), counters);
    }

    /// `id` のプラグインの取りこぼし件数。カウンタが未登録(Disabled で
    /// 購読タスクが起動していない)なら 0 件。
    fn dropped_counts(&self, id: &str) -> DroppedCounts {
        self.drop_counters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .map(|counters| counters.snapshot())
            .unwrap_or_default()
    }

    /// Running な全プラグインへ `PluginWork::Stop` を送り、それぞれの専用
    /// スレッドの終了を(1 件あたり `PLUGIN_STOP_JOIN_TIMEOUT` を上限に)
    /// 待つ。デーモンの正常終了シーケンス専用(`core/src/bin/edlr.rs` を
    /// 参照)。
    ///
    /// **`shutdown_bus_subscribers` より前に呼ぶこと**: on-stop の中で
    /// プラグインがまだバス経由の publish を行いたいかもしれない
    /// (`HostCtx::check_bus` はこの時点でまだ `shutdown_bus_subscribers` が
    /// 立てるフラグを見ないので、bus 自体はまだ生きている)。先に
    /// バス購読側を止めてしまう理由が無い以上、後始末の順序として自然な方
    /// (プラグインに最後の一仕事をさせてから、購読タスクを畳む)にしてある。
    ///
    /// **`Stop` はワークキューを追い越す**: 停止の合図は 2 経路ある。
    ///
    /// 1. `PluginThreadHandle::stop_flag`(主経路)-- ランナーループが毎周期、
    ///    ワークキューを読む**前**に確認する。これにより、有界 64 スロットの
    ///    キューに積まれた先行ワークを `Stop` が追い越せる
    /// 2. `work_tx` への `PluginWork::Stop`(補助)-- キューが空で
    ///    `recv_timeout` に入っているスレッドを直ちに起こすためだけのもの。
    ///    満杯なら `try_send` は失敗するが、その場合スレッドは待ちに入って
    ///    おらず、次の周回でフラグを見るので問題ない
    ///
    /// かつては 2 の経路しか無かったため、プラグインスレッドは先行する全ワーク
    /// を消化するまで `call_on_stop` へ到達できず(最悪 63 件 x
    /// `CALL_DEADLINE` 2 秒 ≒ 126 秒)、`PLUGIN_STOP_JOIN_TIMEOUT` しか待たない
    /// この関数から見ると on-stop の flush は事実上スキップされていた。
    ///
    /// 送信側が既に切断されている(スレッドが trap などで既に終了済み)場合も
    /// 何もしない -- いずれの場合も後続の join は行う(トラップ済みなら即座に
    /// `is_finished()` が `true` になるだけ)。
    ///
    /// **停止要求は全件へ先に送り、join は 1 つの共有デッドラインで待つ**:
    /// プラグインの on-stop は互いに独立なので、直列に待つ理由が無い。まず
    /// 全プラグインへ停止を伝えてから、全スレッドの `is_finished()` を
    /// `PLUGIN_STOP_JOIN_POLL_INTERVAL` 間隔でまとめてポーリングする。
    /// これにより最悪ケースが `N × PLUGIN_STOP_JOIN_TIMEOUT` から
    /// `1 × PLUGIN_STOP_JOIN_TIMEOUT` に縮む(Tauri 側の
    /// `daemon::STOP_GRACE` はこの最悪ケースを見積もって決まっているので、
    /// あちらの定数も一桁下げられる)。
    ///
    /// 標準ライブラリの `JoinHandle` には `join_timeout` が無いためポーリング
    /// で代替している。デッドラインを過ぎても終わっていないスレッドは諦める
    /// (warn ログを出して join しない -- プロセス自体はこの直後に終了するので
    /// 待ちきれなかったスレッドはプロセス終了と共に消える。ハングしたプラグイン
    /// のために shutdown シーケンス全体を止めるべきではない)。
    pub fn shutdown_plugins(&self) {
        let handles: Vec<(String, PluginThreadHandle)> = self
            .plugin_threads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .collect();

        // 第 1 段: 全プラグインへ停止を伝える。ここでは一切待たない。
        for (_, thread_handle) in &handles {
            // 主経路: フラグを先に立てる。ランナーループは次の周回で、
            // キューを読む前にこれを見て on-stop へ進む。
            thread_handle.stop_flag.store(true, Ordering::SeqCst);

            // 補助経路: `recv_timeout` で待ちに入っているスレッドを起こす。
            match thread_handle.work_tx.try_send(PluginWork::Stop) {
                Ok(()) => {}
                Err(std_mpsc::TrySendError::Full(_)) => {
                    // キューが満杯 = スレッドは待ちに入っていないので、
                    // 起こす必要が無い。次の周回でフラグを見る。
                }
                Err(std_mpsc::TrySendError::Disconnected(_)) => {
                    // プラグインスレッドは既に(trap などで)終了済み。
                }
            }
        }

        // 第 2 段: 1 つの共有デッドラインで、全スレッドの終了を並行に待つ。
        let deadline = Instant::now() + PLUGIN_STOP_JOIN_TIMEOUT;
        while handles.iter().any(|(_, h)| !h.handle.is_finished()) && Instant::now() < deadline {
            thread::sleep(PLUGIN_STOP_JOIN_POLL_INTERVAL);
        }

        for (id, thread_handle) in handles {
            if thread_handle.handle.is_finished() {
                let _ = thread_handle.handle.join();
            } else {
                tracing::warn!(
                    plugin_id = %id,
                    "plugin thread did not exit within {PLUGIN_STOP_JOIN_TIMEOUT:?} of the \
                     stop signal; abandoning join (the process is exiting regardless)"
                );
            }
        }
    }

    /// `crate::plugin::runner::spawn_bus_subscriber` が共有する shutdown
    /// フラグの `Arc` を返す。`runner.rs` がプラグインごとの購読タスクを
    /// 起動する際にこれを渡す(crate 内部専用: `pub(crate)`)。
    pub(crate) fn bus_subscriber_shutdown_flag(&self) -> Arc<AtomicBool> {
        self.bus_subscriber_shutdown.clone()
    }

    pub(crate) fn push(&self, entry: PluginEntry) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(entry);
    }

    /// プラグインを走査した元ディレクトリ。
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    /// 現在登録されている全プラグインの `(manifest, state)` を返す。
    pub fn snapshot(&self) -> Vec<(Manifest, PluginState)> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|entry| (entry.manifest.clone(), entry.state.clone()))
            .collect()
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
        let snapshot: Vec<(Manifest, PluginState)> = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|entry| (entry.manifest.clone(), entry.state.clone()))
            .collect();

        snapshot
            .into_iter()
            .map(|(manifest, state)| {
                // 秘密情報は RPC 応答から落とす(設定済みかどうかだけ返す)。
                let (values, secrets_set) =
                    split_secrets(&manifest, self.settings_store.effective(&manifest));
                let grant_state = self.grants_store.state(&manifest);
                let capability_requests = manifest.capabilities.clone();
                let sidecars = self.build_sidecar_infos(&manifest);
                let filesystem = self.build_filesystem_infos(&manifest);
                let dashboard = self.build_dashboard_infos(&manifest);
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
                }
            })
            .collect()
    }

    /// `<plugin-id>/<sidecar-name>` の形で `ProcessDriver` のキーを組み立てる。
    /// `HostCtx::sidecar_key`(`core/src/plugin/host.rs`)と同じ規則。
    fn sidecar_key(plugin_id: &str, name: &str) -> String {
        format!("{plugin_id}/{name}")
    }

    /// `manifest.sidecars` の宣言順に `SidecarInfo` を組み立てる。設定
    /// (`SidecarConfigStore`)・承認(`GrantsStore`)はディスクを読むが、
    /// `ProcessDriver::status` は読み取り専用(プロセスを起動も停止もしない)。
    fn build_sidecar_infos(&self, manifest: &Manifest) -> Vec<SidecarInfo> {
        let configs = self.sidecar_config_store.effective(manifest);
        manifest
            .sidecars
            .iter()
            .map(|request| {
                let config = configs
                    .get(&request.name)
                    .cloned()
                    .unwrap_or_else(|| SidecarConfig::from_request(request));
                let grant = self.grants_store.sidecar_state(manifest, &request.name);
                self.sidecar_info_and_entry(manifest, request, config, grant)
                    .0
            })
            .collect()
    }

    /// `request` 1 件分の(既に取得済みの)設定・承認状態から、`SidecarInfo`
    /// と(`sidecars_json` バッファ用の)`SidecarRuntimeEntry` を両方組み立
    /// てる。`ProcessDriver::status` の呼び出しを両者で 1 回だけ共有する
    /// (`config`/`grant` の取得元は呼び出し側に委ねているので、ここではもう
    /// ディスクを読まない -- `refresh_sidecar_runtime` が前半で読んだ値を
    /// そのまま渡し、末尾で読み直さずに済ませるための分離)。
    fn sidecar_info_and_entry(
        &self,
        manifest: &Manifest,
        request: &SidecarRequest,
        config: SidecarConfig,
        grant: GrantState,
    ) -> (SidecarInfo, SidecarRuntimeEntry) {
        let ports = assign_ports(&config);
        let spec = ProcessSpec {
            command: PathBuf::from(&config.command),
            args: config.args.clone(),
            ports: ports.clone(),
        };
        let key = Self::sidecar_key(&manifest.id, &request.name);
        let instances = self.process_driver.status(&key, &spec);

        let runtime_entry = SidecarRuntimeEntry {
            name: request.name.clone(),
            granted: grant.granted,
            command: config.command.clone(),
            args: config.args.clone(),
            ports,
        };
        let info = SidecarInfo {
            request: request.clone(),
            config,
            grant,
            instances,
        };
        (info, runtime_entry)
    }

    /// `manifest.filesystem` の宣言順に `FilesystemInfo` を組み立てる。
    /// 設定(`FilesystemConfigStore`)・承認(`GrantsStore`)はディスクを読む
    /// (`build_sidecar_infos` と同じ流儀。こちらはプロセス状態が無いので
    /// `ProcessDriver::status` に相当する読み取りは無い)。
    fn build_filesystem_infos(&self, manifest: &Manifest) -> Vec<FilesystemInfo> {
        let configs = self.filesystem_config_store.effective(manifest);
        manifest
            .filesystem
            .iter()
            .map(|request| {
                let config =
                    configs
                        .get(&request.name)
                        .cloned()
                        .unwrap_or_else(|| FilesystemConfig {
                            path: String::new(),
                        });
                let grant = self.grants_store.filesystem_state(manifest, &request.name);
                FilesystemInfo {
                    request: request.clone(),
                    config,
                    grant,
                }
            })
            .collect()
    }

    /// `manifest.bus` の宣言順に `BusInfo` を組み立てる。承認
    /// (`GrantsStore::bus_state`)はディスクを読む。`resolved` は
    /// `driver_registry.manifest_of` が返す `DriverManifest`(インストール済み
    /// ドライバの現在の宣言)と、この要求が挙げる `publish`/`subscribe` の
    /// 全トピックとを突き合わせて決める -- ドライバ自体が無ければ即
    /// `false`、ドライバはあってもトピックが 1 つでも無ければ `false`。
    fn build_bus_infos(&self, manifest: &Manifest) -> Vec<BusInfo> {
        manifest
            .bus
            .iter()
            .map(|request| {
                let grant = self.grants_store.bus_state(manifest, &request.driver);
                let resolved = match self.driver_registry.manifest_of(&request.driver) {
                    Some(driver_manifest) => request
                        .publish
                        .iter()
                        .chain(request.subscribe.iter())
                        .all(|topic| driver_manifest.topic(topic).is_some()),
                    None => false,
                };
                BusInfo {
                    request: request.clone(),
                    grant,
                    resolved,
                }
            })
            .collect()
    }

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
                DashboardInfo {
                    request: widget.clone(),
                    grant,
                    resolved,
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
        let published: HashMap<String, chrono::DateTime<chrono::Local>> = self
            .schedule_views
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&manifest.id)
            .map(|view| view.snapshot().into_iter().collect())
            .unwrap_or_default();

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
    /// ウィジェットへのイベント転送範囲を UI に伝えるのに使う)。
    pub fn events_of(&self, id: &str) -> Result<Vec<String>, RegistryError> {
        Ok(self.find_manifest(id)?.events.clone())
    }

    /// `id` のダッシュボードウィジェット一覧(UI 表示用)。
    pub fn dashboard(&self, id: &str) -> Result<Vec<DashboardInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        Ok(self.build_dashboard_infos(&manifest))
    }

    /// ダッシュボードウィジェット 1 件の承認/取消。`set_bus_grant` と同じ
    /// 流儀で、更新後のウィジェット一覧全体を返す(UI が 1 往復で
    /// リスト全体を更新できるように)。
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

    /// `dashboard/list` 用: 全プラグインの全ウィジェット
    /// (`(plugin_id, plugin_name, state, info)`)。grant の有無での絞り込みは
    /// 呼び出し側(server.rs)の責務。
    pub fn dashboard_widgets_for_ui(&self) -> Vec<(String, String, PluginState, DashboardInfo)> {
        let snapshot: Vec<(Manifest, PluginState)> = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|entry| (entry.manifest.clone(), entry.state.clone()))
            .collect();

        snapshot
            .into_iter()
            .flat_map(|(manifest, state)| {
                self.build_dashboard_infos(&manifest)
                    .into_iter()
                    .map(|info| {
                        (
                            manifest.id.clone(),
                            manifest.name.clone(),
                            state.clone(),
                            info,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// ウィジェットアセットの実ファイルパスを解決する。grant 必須・entry の
    /// ディレクトリ外へのトラバーサルは拒否(`/plugin-ui/...` ハンドラの
    /// 心臓部。HTTP 層は薄く保ち、判定はここで単体テストする)。
    /// `rel_path` が空のときは entry ファイル自身を返す。
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
        if rel_path.is_empty() {
            return Ok(entry);
        }
        let base = entry
            .parent()
            .ok_or_else(|| RegistryError::UnknownDashboard(widget.to_string()))?;
        let rel = Path::new(rel_path);
        if rel
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(RegistryError::UnknownDashboard(widget.to_string()));
        }
        Ok(base.join(rel))
    }

    /// `id` 用のサイドカー実行時ロック(`sidecar_runtime_locks`)を引く。
    /// 無ければ作る。マップ自体を保護する `Mutex` は、id からロックの `Arc`
    /// を引く/挿入する間だけ保持し、返した `Arc<Mutex<()>>` の実際の臨界
    /// 区間(呼び出し側が別途 `.lock()` する)の間は保持しない。
    fn sidecar_runtime_lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        Self::lock_for(&self.sidecar_runtime_locks, id)
    }

    /// `id` 用のファイルアクセス実行時ロック(`filesystem_runtime_locks`)を
    /// 引く。サイドカー側とは**別のマップ**であることが要点
    /// (`filesystem_runtime_locks` のドキュメント参照)。
    fn filesystem_runtime_lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        Self::lock_for(&self.filesystem_runtime_locks, id)
    }

    /// `id` 用のバス実行時ロック(`bus_runtime_locks`)を引く。サイドカー・
    /// ファイルアクセスとは**別のマップ**であることが要点
    /// (`bus_runtime_locks` のドキュメント参照)。
    fn bus_runtime_lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        Self::lock_for(&self.bus_runtime_locks, id)
    }

    fn lock_for(map: &Mutex<HashMap<String, Arc<Mutex<()>>>>, id: &str) -> Arc<Mutex<()>> {
        let mut locks = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        locks
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// `id` のプラグインの capability 要求一覧と現在の承認状態を返す。
    ///
    /// `values`/`set_values` と同様、`entries` ロックは manifest のクローン
    /// 取得のみに使い、ロックを解放してから `GrantsStore::state`(ディスク
    /// 読み取り)を呼ぶ。
    pub fn capabilities(
        &self,
        id: &str,
    ) -> Result<(Vec<CapabilityRequest>, GrantState), RegistryError> {
        let manifest = self.find_manifest(id)?;
        let grant_state = self.grants_store.state(&manifest);
        Ok((manifest.capabilities.clone(), grant_state))
    }

    /// `id` のプラグインの effective settings(`SettingsStore` 由来)を返す。
    ///
    /// `list()` と同様、`entries` ロックは manifest のクローン取得のみに使い、
    /// ロックを解放してから `SettingsStore::effective` を呼ぶ。
    pub fn values(
        &self,
        id: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        // 秘密情報は読み出し応答に載せない(`split_secrets` 参照)。
        let (visible, _secrets_set) =
            split_secrets(&manifest, self.settings_store.effective(&manifest));
        Ok(visible)
    }

    /// `id` のプラグインの settings を検証・永続化し、稼働中プラグインが参照
    /// する共有 `settings_json` も新しい effective 値で上書きする。
    ///
    /// 検証(`SettingsStore::update`)に失敗した場合は何も変更されず、
    /// `RegistryError::Settings` を返す。
    ///
    /// `entries` ロックは manifest と `settings_json` の共有ハンドル
    /// (`Arc<Mutex<String>>`)を取得する間だけ保持し、
    /// `SettingsStore::update_and_effective` によるファイル I/O はロックを
    /// 解放した後に行う。書き込み先の `settings_json` は `Arc` のクローンな
    /// ので、`entries` ロックを再取得せずに書き込んでも実行中プラグインが
    /// 参照しているのと同じセルを更新できる。`update_and_effective` は
    /// `SettingsStore` 内部ロックの下で書き込みと直後の読み出しをまとめて
    /// 行うため、他スレッドの並行 `set_values` が割り込んでここでの
    /// `effective` が「自分が書いた値」とずれることはない。
    pub fn set_values(
        &self,
        id: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, RegistryError> {
        let (manifest, settings_json) = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;
            (entry.manifest.clone(), entry.settings_json.clone())
        };

        let effective = self
            .settings_store
            .update_and_effective(&manifest, values)
            .map_err(RegistryError::Settings)?;

        // プラグインへ渡すバッファには秘密情報も含める -- 渡す相手は
        // そのプラグイン自身なので、ここで落としたら意味が無い。
        let settings_json_string =
            serde_json::to_string(&serde_json::Value::Object(effective.clone()))
                .unwrap_or_else(|_| "{}".to_string());
        *settings_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings_json_string;

        // RPC の応答には載せない。
        let (visible, _secrets_set) = split_secrets(&manifest, effective);
        Ok(visible)
    }

    /// `id` のプラグインの capability 承認/取消を `GrantsStore` に永続化し、
    /// 稼働中プラグインが参照する共有 `capabilities_json` も更新する。
    ///
    /// `entries` ロックは manifest と `capabilities_json` の共有ハンドルを
    /// 取得する間だけ保持し、`GrantsStore::set` のファイル I/O はロックを
    /// 解放した後に行う(`set_values` と同じ流儀。`entries` ロックをファイル
    /// I/O の間保持すると、他プラグインの settings/capabilities 操作や
    /// `set_disabled` までブロックしてしまうため)。
    ///
    /// 一方で、「ディスクへの永続化」と「共有バッファへの反映」の 2 ステップは
    /// 同じ呼び出しの中で不可分に行う必要がある。呼び出しごとに
    /// `capabilities_lock` を取り、`GrantsStore::set` の呼び出しから
    /// `capabilities_json` バッファへの書き込みまでを 1 つの臨界区間として
    /// 保持する。これが無いと、2 つの同時呼び出し(例: 2 つの RPC クライアントが
    /// 同じプラグインを同時に許可/取消)が
    /// `A.set(true) → B.set(false) → B が buffer に false を書く →
    /// A が buffer に true を書く` のように交互実行され、ディスク上は
    /// 取消済みなのに稼働中プラグインのバッファは許可済みのまま、という
    /// fail-open な不整合が起こりうる(このロックはそれぞれの呼び出しの
    /// 「永続化 + バッファ反映」を丸ごと直列化することで、ディスクとバッファの
    /// 最終状態が必ず「最後にこの臨界区間を抜けた呼び出し」の結果で一致する
    /// ことを保証する)。
    ///
    /// `GrantsStore` 自身も内部に別の `Mutex<()>` を持つが、それは
    /// `GrantsStore::set` 単体(ファイル書き込みとその直後の読み出し)の
    /// アトミック性のためのものであり、バッファ書き込みまでは面倒を見ない。
    /// そのため `capabilities_lock` は `GrantsStore` の内部ロックとは別に
    /// `Registry` 側で持つ(`GrantsStore` に `capabilities_json` の形を
    /// 知らせたくない、という関心の分離の意味もある)。2 つのロックの取得
    /// 順序は常に `capabilities_lock` → (`GrantsStore` 内部ロック) の一方向
    /// のみなのでデッドロックの心配もない。
    ///
    /// `capabilities_json` は `refresh_sidecar_runtime`(サイドカーの設定
    /// 変更・承認変更のたびに、承認済みサイドカーの暗黙 127.0.0.1 許可を
    /// 織り込んで書き直す)とも書き込み先を共有している。そちらも同じ
    /// `capabilities_lock` を取ってから書くので、「http capability の
    /// 承認/取消」と「サイドカーの設定/承認変更」が同時に起きても、
    /// このバッファへの 2 つの書き込みが交互実行で食い違うことはない
    /// (`refresh_sidecar_runtime` のドキュメントコメント参照。ロック順序は
    /// 常に「id 別ロック(プラグイン単位)→ `capabilities_lock`」の一方向
    /// のみで、この関数は `capabilities_lock` だけを取り id 別ロックにも
    /// そのマップにも触れないため、両者を合わせてもデッドロックしない)。
    /// この関数自身は
    /// サイドカーの設定/承認を変更しないので、現在の `sidecars_json` バッファ
    /// をそのまま読み(再計算はしない)、そこから暗黙許可ホストを合流させる。
    pub fn set_capabilities(&self, id: &str, granted: bool) -> Result<GrantState, RegistryError> {
        let (manifest, capabilities_json, sidecars_json) = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;
            (
                entry.manifest.clone(),
                entry.capabilities_json.clone(),
                entry.sidecars_json.clone(),
            )
        };

        let _capabilities_guard = self
            .capabilities_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let state = self
            .grants_store
            .set(&manifest, granted)
            .map_err(RegistryError::Grants)?;

        let mut effective_hosts = if state.granted {
            manifest.capability_hosts()
        } else {
            Vec::new()
        };
        let sidecar_entries: Vec<SidecarRuntimeEntry> =
            crate::plugin::sidecar_runtime::parse_sidecars(
                &sidecars_json
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )
            .into_values()
            .collect();
        effective_hosts.extend(implicit_http_hosts(&sidecar_entries));

        let capabilities_json_string = capabilities_json_string(&effective_hosts);
        *capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = capabilities_json_string;

        Ok(state)
    }

    /// `id` のプラグインが現在 `Disabled` かどうか。未登録の id は `false`
    /// (`control_sidecar` はこの手前で既に `find_manifest` を通しているので、
    /// 未登録を別扱いする必要はない)。
    fn is_disabled(&self, id: &str) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|entry| entry.manifest.id == id)
            .is_some_and(|entry| matches!(entry.state, PluginState::Disabled { .. }))
    }

    /// `id` のプラグインの manifest クローンを返す(`entries` ロック保持は
    /// このルックアップの間だけ)。
    fn find_manifest(&self, id: &str) -> Result<Manifest, RegistryError> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|entry| entry.manifest.id == id)
            .map(|entry| entry.manifest.clone())
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))
    }

    /// `id` のプラグインの `capabilities_json` 共有バッファが現在載せている
    /// 実効許可ホストを返す(テスト用アクセサ)。`driver-http.send` が実際に
    /// 参照するのと同じ値。
    pub fn effective_hosts(&self, id: &str) -> Result<Vec<String>, RegistryError> {
        let capabilities_json = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .map(|entry| entry.capabilities_json.clone())
                .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?
        };
        let raw = capabilities_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Ok(parse_capability_hosts(&raw))
    }

    /// `id` のプラグインの現在のサイドカー状態一覧(manifest の `[[sidecar]]`
    /// 宣言順)を返す。
    pub fn sidecars(&self, id: &str) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        Ok(self.build_sidecar_infos(&manifest))
    }

    /// `id` のプラグインの現在のファイルアクセス状態一覧(manifest の
    /// `[[filesystem]]` 宣言順)を返す。
    pub fn filesystem(&self, id: &str) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        Ok(self.build_filesystem_infos(&manifest))
    }

    /// `id` のプラグインの `filesystem_json` 共有バッファの中身をそのまま
    /// 返す(テスト用アクセサ)。`driver-fs.*` が実際に参照するのと同じ
    /// 文字列(`fs_runtime::filesystem_json_string` の出力そのもの)。
    pub fn filesystem_buffer(&self, id: &str) -> Result<String, RegistryError> {
        let filesystem_json = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .map(|entry| entry.filesystem_json.clone())
                .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?
        };
        let buffer = filesystem_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Ok(buffer)
    }

    /// `id` のプラグインの `bus_json` 共有バッファの中身をそのまま返す
    /// (テスト用アクセサ)。`HostCtx::check_bus` /
    /// `runner::spawn_bus_subscriber` が実際に参照するのと同じ文字列
    /// (`bus_runtime::bus_json_string` の出力そのもの)。
    pub fn bus_buffer(&self, id: &str) -> Result<String, RegistryError> {
        let bus_json = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .map(|entry| entry.bus_json.clone())
                .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?
        };
        let buffer = bus_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Ok(buffer)
    }

    /// ファイルアクセスの設定変更・承認変更のあとに必ず呼ぶ内部ヘルパー。
    ///
    /// サイドカーと違い、ファイルアクセスには「止めるべきプロセス」が無い
    /// ので、`refresh_sidecar_runtime` の手順 1(`ProcessDriver::stop`)に
    /// 相当する処理は無い。`FilesystemConfigStore::effective` /
    /// `GrantsStore::filesystem_state` の現在値から `filesystem_json` を
    /// 作り直すだけ(未承認のルートは `path` を持たない -- `fs_runtime` の
    /// ドキュメント参照)。それでも「永続化(呼び出し元で完了済み)」と
    /// 「バッファへの反映」を同じ id 別ロックの臨界区間に収めるのは
    /// `refresh_sidecar_runtime` と同じ理由: 2 つの同時呼び出し(例えば
    /// 同じルートの設定変更と承認取消を 2 つの RPC クライアントがほぼ同時に
    /// 行う)がディスクと共有バッファを食い違わせないようにするため。
    ///
    /// ロックは `filesystem_runtime_lock_for(id)` -- **サイドカーとは別の
    /// マップ** から引く(`filesystem_runtime_locks` のドキュメント参照)。
    /// 共有していた頃は、同じプラグインのサイドカー停止
    /// (`ProcessDriver::stop`)が終わるまで承認取消がロック待ちになり、その
    /// 間 `filesystem_json` が古い `granted:true` と path を持ち続けていた
    /// (最大で `shutdown_grace` × インスタンス数の fail-open)。
    /// 取得順序は既存の一方向(`entries` → マップの Mutex → id 別ロック)の
    /// ままで、この関数は `capabilities_lock` を取らない(ファイルアクセスの
    /// 承認は `capabilities_json` に影響しない)ため、`set_capabilities`/
    /// `refresh_sidecar_runtime` との間に新たな循環は生じない。
    fn refresh_filesystem_runtime(&self, id: &str) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let (manifest, filesystem_json) = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;
            (entry.manifest.clone(), entry.filesystem_json.clone())
        };

        let runtime_lock = self.filesystem_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let infos = self.build_filesystem_infos(&manifest);
        let runtime_entries: Vec<FsRuntimeEntry> = infos
            .iter()
            .map(|info| FsRuntimeEntry {
                name: info.request.name.clone(),
                granted: info.grant.granted,
                mode: info.request.mode.as_str().to_string(),
                path: info.config.path.clone(),
            })
            .collect();
        *filesystem_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            filesystem_json_string(&runtime_entries);

        Ok(infos)
    }

    /// `id` のプラグインの `name` ファイルアクセスルートの設定を検証・永続化
    /// し(`FilesystemConfigStore::update_and_effective`)、稼働中プラグイン
    /// が参照する `filesystem_json` を作り直してから最新の `FilesystemInfo`
    /// 一覧を返す。検証に失敗した場合は何も変更されない。
    ///
    /// **承認は維持する**: パス自体は `Manifest::filesystem_fingerprint` に
    /// 含まれない(fingerprint が変わるのは `name`/`reason`/`mode` のみ)ので、
    /// ディレクトリを変更しても `GrantsStore` 上の承認は stale にならず、
    /// 生きたまま新しいパスに追従する(ブリーフの
    /// `changing_the_directory_takes_effect_without_reapproval` が検証する
    /// 挙動)。
    pub fn set_filesystem_config(
        &self,
        id: &str,
        name: &str,
        config: &FilesystemConfig,
    ) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        self.filesystem_config_store
            .update_and_effective(&manifest, name, config)
            .map_err(RegistryError::FilesystemConfig)?;
        self.refresh_filesystem_runtime(id)
    }

    /// `id` のプラグインの `name` ファイルアクセスルートの承認/取消を
    /// `GrantsStore` に永続化する。
    ///
    /// `granted == true` のとき、ディレクトリが未設定(空文字)のルートは
    /// 拒否する(`RegistryError::Filesystem`)。UI 側は未設定の間チェック
    /// ボックスを `disabled` にしているはずだが、それは UI 上の防御に過ぎ
    /// ない -- RPC を直接叩けばこの検証を経由せずに「ユーザーがどこへの
    /// アクセスかを一度も選んでいない」状態のルートを承認できてしまう。
    /// `set_sidecar_grant` の `command` 未設定チェックと同じ理由・同じ
    /// 場所(ストア/`Registry` 側)で強制し、UI・RPC 層では二重実装しない。
    /// 取消は逆に常に許す -- ディレクトリを消した状態でも稼働中の承認を
    /// 取り消せなければ fail-open になってしまうため。
    pub fn set_filesystem_grant(
        &self,
        id: &str,
        name: &str,
        granted: bool,
    ) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        if manifest.filesystem_root(name).is_none() {
            return Err(RegistryError::UnknownFilesystem(name.to_string()));
        }

        if granted {
            let configs = self.filesystem_config_store.effective(&manifest);
            let path_configured = configs
                .get(name)
                .map(|config| !config.path.is_empty())
                .unwrap_or(false);
            if !path_configured {
                return Err(RegistryError::Filesystem(format!(
                    "filesystem root {name} has no directory configured; cannot grant"
                )));
            }
        }

        self.grants_store
            .set_filesystem(&manifest, name, granted)
            .map_err(RegistryError::Grants)?;

        self.refresh_filesystem_runtime(id)
    }

    /// `id` のプラグインの現在のバス接続状態一覧(manifest の `[[bus]]`
    /// 宣言順)を返す。`BusInfo::resolved` は `DriverRegistry` の現在の登録
    /// 状況(ドライバの有無・トピックの有無)から都度計算する(承認と違い
    /// ディスクに永続化しない -- ドライバの実体そのものが真実源)。
    pub fn bus(&self, id: &str) -> Result<Vec<BusInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        Ok(self.build_bus_infos(&manifest))
    }

    /// バス承認変更のあとに必ず呼ぶ内部ヘルパー。ファイルアクセスと同じく
    /// 「止めるべきプロセス」が無いので、`GrantsStore::bus_state` /
    /// `DriverRegistry::manifest_of` の現在値から `bus_json` を作り直す
    /// だけ。未承認の接続先は `publish`/`subscribe` を持たない
    /// (`bus_runtime` のドキュメント参照)。
    ///
    /// ロックは `bus_runtime_lock_for(id)` -- サイドカー・ファイルアクセス
    /// とは別のマップ(`bus_runtime_locks` のドキュメント参照)から引く。
    fn refresh_bus_runtime(&self, id: &str) -> Result<Vec<BusInfo>, RegistryError> {
        let (manifest, bus_json) = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;
            (entry.manifest.clone(), entry.bus_json.clone())
        };

        let runtime_lock = self.bus_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let infos = self.build_bus_infos(&manifest);
        let runtime_entries: Vec<BusRuntimeEntry> = infos
            .iter()
            .map(|info| BusRuntimeEntry {
                driver: info.request.driver.clone(),
                granted: info.grant.granted,
                publish: info.request.publish.clone(),
                subscribe: info.request.subscribe.clone(),
            })
            .collect();
        *bus_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = bus_json_string(&runtime_entries);

        Ok(infos)
    }

    /// `id` のプラグインの `driver` バス接続の承認/取消を `GrantsStore` に
    /// 永続化し、稼働中プラグインが参照する `bus_json` を作り直す
    /// (`set_filesystem_grant` と同じ形)。
    ///
    /// **配信は取消を即座に反映する**: `bus_json` の書き換えだけで済むのは、
    /// `runner.rs` の配信転送タスク(`spawn_bus_subscriber`)が配信のたびに
    /// この同じ `bus_json` を読み直して承認を再確認するため。ここでは
    /// 購読の解除(`Bus::subscribe` の取り消し)自体は行わない -- 取り消して
    /// も転送タスク側で捨てられるだけなので、購読表を触る必要が無い。
    pub fn set_bus_grant(
        &self,
        id: &str,
        driver: &str,
        granted: bool,
    ) -> Result<GrantState, RegistryError> {
        let manifest = self.find_manifest(id)?;
        if manifest.bus_request(driver).is_none() {
            return Err(RegistryError::UnknownBus(driver.to_string()));
        }

        let state = self
            .grants_store
            .set_bus(&manifest, driver, granted)
            .map_err(RegistryError::Grants)?;

        self.refresh_bus_runtime(id)?;

        Ok(state)
    }

    /// サイドカーの設定変更・承認変更のあとに必ず呼ぶ内部ヘルパー。
    ///
    /// 1. `stop_names` に挙げられたサイドカーを(同期版 `ProcessDriver::stop`
    ///    で)停止する。設定が変わった/承認が消えた以上、走り続けてよい根拠が
    ///    無い -- 次の `ensure-started`(プラグインの明示操作かユーザー操作)
    ///    で新しい設定・承認のもとに起動し直される。
    /// 2. `sidecars_json` を(`SidecarConfigStore`/`GrantsStore` の現在値から)
    ///    作り直す。
    /// 3. `capabilities_json` を「http capability が承認済みなら manifest
    ///    hosts」+「承認済みサイドカーの暗黙 127.0.0.1 ポート」で作り直す。
    ///
    /// 2 と 3 を同じ臨界区間で更新するのが重要で、片方だけ更新されると
    /// 「起動はできるが通信できない(2 だけ進んだ)」「通信できるが承認は消えて
    /// いる(3 だけ進んだ)」という中途半端な状態が観測されうる。そのため
    /// 呼び出しごとに `sidecar_runtime_lock_for(id)` で **`id` 専用の**
    /// ロックを取り、停止からバッファ書き込みまでを 1 つの臨界区間として
    /// 保持する(`set_capabilities` の `capabilities_lock` と同じ理由: 2 つの
    /// 同時呼び出し -- 例えば同じサイドカーの設定変更と承認取消を 2 つの RPC
    /// クライアントがほぼ同時に行う -- がディスクと共有バッファを食い違わせ
    /// ないようにするため)。
    ///
    /// このロックは(`capabilities_lock` と違って)**プラグイン ID ごと**に
    /// 分かれている(`sidecar_runtime_locks` のドキュメントコメント参照)。
    /// 手順 1 の `ProcessDriver::stop` は SIGTERM 無視の子がいれば
    /// `shutdown_grace` 秒ブロックしうる同期呼び出しであり、これを全プラグ
    /// イン共有のロックの中で行うと、無関係な別プラグインのサイドカー操作
    /// まで同じだけ足止めしてしまうため。
    ///
    /// `capabilities_json` への書き込みは、`set_capabilities` とも共有する
    /// 書き込み先であるため、内側で追加に `capabilities_lock` を取ってから
    /// 行う(`set_capabilities` のドキュメントコメント参照)。ロック順序は
    /// 常に「id 別ロック → `capabilities_lock`」の一方向のみで、
    /// `set_capabilities` は `capabilities_lock` だけを取り id 別ロックにも
    /// そのマップにも触れないため、循環せずデッドロックしない。
    ///
    /// 手順 2 の材料(`SidecarConfigStore::effective` / `GrantsStore::
    /// sidecar_state`)はこの関数の中で 1 回だけディスクから読み、
    /// `sidecar_info_and_entry` で `SidecarInfo`(戻り値用)と
    /// `SidecarRuntimeEntry`(`sidecars_json` 用)を同時に組み立てる。
    /// 末尾で改めて `self.sidecars(id)` を呼んで読み直すことはしない --
    /// 以前の実装はそうしており、id 別ロックの臨界区間をディスク I/O 分
    /// 余計に伸ばしていた。
    fn refresh_sidecar_runtime(
        &self,
        id: &str,
        stop_names: &[String],
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let (manifest, sidecars_json, capabilities_json) = {
            let guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = guard
                .iter()
                .find(|entry| entry.manifest.id == id)
                .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;
            (
                entry.manifest.clone(),
                entry.sidecars_json.clone(),
                entry.capabilities_json.clone(),
            )
        };

        let runtime_lock = self.sidecar_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        for name in stop_names {
            let key = Self::sidecar_key(&manifest.id, name);
            self.process_driver.stop(&key);
        }

        let sidecar_configs = self.sidecar_config_store.effective(&manifest);
        let mut infos = Vec::with_capacity(manifest.sidecars.len());
        let mut runtime_entries = Vec::with_capacity(manifest.sidecars.len());
        for request in &manifest.sidecars {
            let config = sidecar_configs
                .get(&request.name)
                .cloned()
                .unwrap_or_else(|| SidecarConfig::from_request(request));
            let grant = self.grants_store.sidecar_state(&manifest, &request.name);
            let (info, runtime_entry) =
                self.sidecar_info_and_entry(&manifest, request, config, grant);
            infos.push(info);
            runtime_entries.push(runtime_entry);
        }
        *sidecars_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            sidecars_json_string(&runtime_entries);

        {
            let _capabilities_guard = self
                .capabilities_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let http_granted = self.grants_store.state(&manifest).granted;
            let mut hosts = if http_granted {
                manifest.capability_hosts()
            } else {
                Vec::new()
            };
            hosts.extend(implicit_http_hosts(&runtime_entries));
            *capabilities_json
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                capabilities_json_string(&hosts);
        }

        Ok(infos)
    }

    /// `id` のプラグインの `name` サイドカーの設定を検証・永続化し、稼働中の
    /// 実行を止めてから(`refresh_sidecar_runtime`)、最新の `SidecarInfo` 一覧
    /// を返す。検証(`SidecarConfigStore::update_and_effective`)に失敗した
    /// 場合は何も変更されない。
    pub fn set_sidecar_config(
        &self,
        id: &str,
        name: &str,
        config: &SidecarConfig,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        self.sidecar_config_store
            .update_and_effective(&manifest, name, config)
            .map_err(RegistryError::SidecarConfig)?;
        let stop_names = vec![name.to_string()];
        self.refresh_sidecar_runtime(id, &stop_names)
    }

    /// `id` のプラグインの `name` サイドカーの承認/取消を `GrantsStore` に
    /// 永続化する。取消(`granted == false`)のときは稼働中の実行を止める
    /// (走り続けてよい根拠が承認と共に無くなるため)。
    ///
    /// `granted == true` のとき、`command` が未設定(空文字)のサイドカーは
    /// 拒否する(`RegistryError::Sidecar`)。UI 側(`SidecarSection.tsx`)は
    /// `command` 未設定の間チェックボックスを `disabled` にしているが、それは
    /// UI 上の防御に過ぎない -- RPC を直接叩けば(あるいは将来別の UI 経路が
    /// 増えれば)この検証を経由せずに「ユーザーが何が走るか一度も見ていない」
    /// 状態のサイドカーを承認できてしまう。設計書・docs/capabilities.md が明言する
    /// 「`command` 未設定では承認できない」という不変条件はここ(ストア/
    /// `Registry` 側)で強制し、UI・RPC 層では二重実装しない(Important:
    /// 最終レビューで見つかった取りこぼし)。取消は逆に常に許す --
    /// `command` を消した状態でも稼働中の承認を取り消せなければ fail-open に
    /// なってしまうため。
    pub fn set_sidecar_grant(
        &self,
        id: &str,
        name: &str,
        granted: bool,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        if manifest.sidecar(name).is_none() {
            return Err(RegistryError::UnknownSidecar(name.to_string()));
        }

        if granted {
            let configs = self.sidecar_config_store.effective(&manifest);
            let command_configured = configs
                .get(name)
                .map(|config| !config.command.is_empty())
                .unwrap_or(false);
            if !command_configured {
                return Err(RegistryError::Sidecar(format!(
                    "sidecar {name} has no executable configured; cannot grant"
                )));
            }
        }

        self.grants_store
            .set_sidecar(&manifest, name, granted)
            .map_err(RegistryError::Grants)?;

        let stop_names: Vec<String> = if granted {
            Vec::new()
        } else {
            vec![name.to_string()]
        };
        self.refresh_sidecar_runtime(id, &stop_names)
    }

    /// `id` のプラグインの `name` サイドカーを直接操作する(ユーザー操作
    /// 起点)。`Stop` は同期版 `ProcessDriver::stop`、`Start` は
    /// `ProcessDriver::ensure_started`、`Restart` は停止してから
    /// `ensure_started`。`Start`/`Restart` は未承認・`command` 未設定を
    /// `RegistryError::Sidecar` として拒否する(`refresh_sidecar_runtime` の
    /// ような JSON バッファの作り直しは、設定・承認自体は変わらないので行わない)。
    ///
    /// **TOCTOU 対策**: `Start`/`Restart` は `sidecar_runtime_lock_for(id)`
    /// (`refresh_sidecar_runtime` -- `set_sidecar_config`/`set_sidecar_grant`
    /// が呼ぶ -- と共有する、プラグイン単位のロック)を、承認・設定を読む
    /// 前から `ensure_started` を呼び終えるまで保持する。これが無いと、
    /// 「grant を読む」→「(ここで承認取消が割り込む)」→「その grant を
    /// 信じて spawn する」という窓ができ、取消の裏で起動してしまう
    /// (Important: 最終レビューで見つかった取りこぼし)。ロックを取ることで、
    /// この関数と `refresh_sidecar_runtime` は互いに排他になり、
    /// 「読んだ承認状態」と「実際に spawn する」の間に他の書き込みが
    /// 挟まらないことを保証する。ロック取得順序は既存のドキュメント
    /// (`sidecar_runtime_locks` を参照)のとおり一方向のまま
    /// (`entries` → マップの Mutex → id 別ロック)であり、この関数は
    /// `capabilities_lock` にも `sidecar_runtime_locks` のマップ自体にも
    /// 触れないので新たな循環は生じない。
    ///
    /// この対策はホスト起点(RPC/UI)の経路のみをカバーする。ゲスト
    /// (wasm)からの `ensure-started` は `HostCtx::ensure_started`
    /// (`host.rs`)経由で、こちらとは別に `sidecars_json` 共有バッファを
    /// 直接読むだけなので同じレースが理論上は残る -- が、それは意図的:
    /// ゲスト呼び出しにこの id 別ロックを取らせると、`refresh_sidecar_runtime`
    /// が同じロックを持って `shutdown_grace`(既定 3 秒)まで `stop` を待つ
    /// 間、`ensure-started` を呼んだプラグインのスレッドがブロックされ、
    /// `PluginInstance::CALL_DEADLINE`(2 秒)を超えて trap してしまう。
    /// ゲスト経路のレース窓は「取消が `sidecars_json` バッファに反映されて
    /// から `HostCtx::resolve_sidecar` が次に読むまで」の間だけで、次回の
    /// `ensure-started`/`status` 呼び出しでは新しい(取消済みの)状態が
    /// 見え、既に走っているインスタンスは UI から `stop` できる。
    ///
    /// **無効化されたプラグインは `Start`/`Restart` できない**(再レビューで
    /// 見つかった Minor な取りこぼし): `set_disabled` はそのプラグインの
    /// 全サイドカーを停止するが、`control_sidecar` が `PluginState` を見て
    /// いなかったため、無効化後(あるいは並行中)に `sidecar-control` の
    /// `start` が来ると、もう生きているプラグインスレッドが無いサイドカーが
    /// 再び起動してしまっていた。この関数は `sidecar_runtime_lock_for(id)`
    /// を取った**後**に現在の `PluginState` を読み、`Disabled` なら拒否する
    /// -- `set_disabled` 自身もサイドカー停止の前に同じ id 別ロックを取る
    /// ように直したので(`set_disabled` のドキュメント参照)、「状態を
    /// `Disabled` にする」→「サイドカーを止める」という一連の操作と、この
    /// 関数の「状態を読む」→「spawn する」は互いに排他になり、無効化の裏で
    /// 起動してしまう窓は無い。`Stop` はプラグインの状態に関わらず常に許す
    /// (止める操作を無効化状態で塞いではいけない)。
    pub fn control_sidecar(
        &self,
        id: &str,
        name: &str,
        action: SidecarAction,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        let request = manifest
            .sidecar(name)
            .ok_or_else(|| RegistryError::UnknownSidecar(name.to_string()))?;
        let key = Self::sidecar_key(&manifest.id, name);

        match action {
            SidecarAction::Stop => {
                self.process_driver.stop(&key);
            }
            SidecarAction::Start | SidecarAction::Restart => {
                let runtime_lock = self.sidecar_runtime_lock_for(id);
                let _runtime_guard = runtime_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());

                // ロック保持中に読む: `set_disabled` も同じ id 別ロックを
                // 取ってからサイドカーを止めるので、ここで見る状態は
                // 「無効化処理が確定済みならその後の値」であることが保証
                // される(無効化される前の "running" を信じて spawn する
                // ことはない)。
                if self.is_disabled(id) {
                    return Err(RegistryError::Sidecar(format!("plugin {id} is disabled")));
                }

                if action == SidecarAction::Restart {
                    self.process_driver.stop(&key);
                }

                // ロック保持中に読み直す: `refresh_sidecar_runtime` は同じ
                // ロックを取ってから承認取消/設定変更を確定するので、ここで
                // 読む値は「取消/変更が確定済みならその後の値」であることが
                // 保証される(取消が確定する前の古い値を信じて spawn する
                // ことはない)。
                let grant = self.grants_store.sidecar_state(&manifest, name);
                if !grant.granted {
                    return Err(RegistryError::Sidecar(format!(
                        "sidecar {name} is not granted"
                    )));
                }

                let configs = self.sidecar_config_store.effective(&manifest);
                let config = configs
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| SidecarConfig::from_request(request));
                if config.command.is_empty() {
                    return Err(RegistryError::Sidecar(format!(
                        "sidecar {name} has no executable configured"
                    )));
                }

                let spec = ProcessSpec {
                    command: PathBuf::from(&config.command),
                    args: config.args.clone(),
                    ports: assign_ports(&config),
                };
                self.process_driver
                    .ensure_started(&key, &spec)
                    .map_err(|e| RegistryError::Sidecar(e.to_string()))?;
            }
        }

        self.sidecars(id)
    }

    /// 全プラグインの全サイドカーインスタンスを停止する(デーモン shutdown 用)。
    ///
    /// `ProcessDriver::stop_all` をそのまま呼ぶ。`ProcessDriver` 自身も
    /// `Drop` で `stop_all` を最後の砦として呼ぶが(`PluginHost::drop` 経由)、
    /// こちらは `Registry`/`PluginHost` がまだ生きている shutdown シーケンスの
    /// 一部として明示的に呼ぶための入口であり、2 つ目の `stop_all` 呼び出し元
    /// になる。`stop_all` はどのキーについても「戻った時点で実際に死んでいる」
    /// ことを保証し(`stop_detached` 経由で detach 済みの kill も
    /// `pending_detached` 経由で待つ)、この保証はこの関数がゲスト側の
    /// `stop_detached` 呼び出しと同時に走っても成り立つ(`ProcessDriver::
    /// stop_detached` のドキュメントコメント参照: `stop_detached` は
    /// `groups` ロックを、バックグラウンドスレッドを立てて `pending_detached`
    /// に登録し終えるまで保持し続けるので、`stop_all` が「まだ `terminating`
    /// にもなっておらず `pending_detached` にも登録されていない」中間状態を
    /// 観測することはない)。
    pub fn stop_all_sidecars(&self) {
        self.process_driver.stop_all();
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
    /// Critical バグ。詳細は `crate::plugin::runner::
    /// BUS_SUBSCRIBER_SHUTDOWN_POLL_INTERVAL` のドキュメントコメント参照)。
    ///
    /// `stop_all_sidecars` と同じくデーモンの shutdown シーケンスの一部として
    /// 呼ぶことを想定している(`core/src/bin/edlr.rs` を参照)。フラグを立てる
    /// だけの軽い呼び出しなので、`stop_all_sidecars` のように `spawn_blocking`
    /// へ逃がす必要はない。
    pub fn shutdown_bus_subscribers(&self) {
        self.bus_subscriber_shutdown
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// `id` のプラグインが持つ settings JSON の共有ハンドルを返す。
    pub fn entry_settings(&self, id: &str) -> Option<Arc<Mutex<String>>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|entry| entry.manifest.id == id)
            .map(|entry| entry.settings_json.clone())
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
        let sidecar_names: Option<Vec<String>> = {
            let mut guard = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard
                .iter_mut()
                .find(|entry| entry.manifest.id == id)
                .map(|entry| {
                    entry.state = PluginState::Disabled { reason };
                    entry
                        .manifest
                        .sidecars
                        .iter()
                        .map(|s| s.name.clone())
                        .collect()
                })
        };

        let Some(names) = sidecar_names else {
            return;
        };

        // `entries` ロックを既に手放した後で `sidecar_runtime_lock_for(id)`
        // を取る(ロック取得順序は他の箇所と同じ: `entries` → id 別ロック)。
        // ここで id 別ロックを取ることで、`control_sidecar` の `Start`/
        // `Restart`(こちらも同じロックを取ってから `PluginState` を読む)と
        // 互いに排他になる -- 「状態を `Disabled` にする」→「サイドカーを
        // 止める」の一連と、「(無効化前の)状態を読む」→「spawn する」が
        // 交差して、無効化の裏でサイドカーが起動されたまま残ることはない
        // (`control_sidecar` のドキュメント参照)。
        let runtime_lock = self.sidecar_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for name in names {
            let key = Self::sidecar_key(id, &name);
            self.process_driver.stop(&key);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::event::Event;
    use crate::plugin::SettingField;
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::Duration;

    /// ドライバを 1 件もロードしていない `DriverRegistry`(存在しない
    /// ディレクトリを走査させることで空のまま作る -- `driver::runner` の
    /// 自テストにある `start_drivers_for_test` と同じ流儀)。
    fn empty_driver_registry(tmp: &std::path::Path) -> DriverRegistry {
        crate::driver::start_drivers(
            &tmp.join("drivers"),
            SettingsStore::new(tmp.join("driver-settings")),
            SidecarConfigStore::new(tmp.join("driver-settings")),
            FilesystemConfigStore::new(tmp.join("driver-settings"), Vec::new()),
            GrantsStore::new_for_drivers(tmp.join("driver-grants")),
            edlr_driver_channel::Bus::new(),
            crate::driver::host::DriverHost::new().expect("driver host should build"),
        )
    }

    fn empty_registry() -> Registry {
        let host = Arc::new(PluginHost::new().expect("host should start"));
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
            filesystem: vec![crate::plugin::manifest::FilesystemRequest {
                name: "exports".into(),
                reason: "reason".into(),
                mode: crate::plugin::manifest::FilesystemMode::ReadWrite,
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
    pub(crate) fn test_registry_with_bus_request_using(driver_registry: DriverRegistry) -> Registry {
        let host = Arc::new(PluginHost::new().expect("host should start"));
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
            tmp.path().join("plugins"),
        );
        registry.push(PluginEntry {
            manifest: manifest_with_bus("translator"),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
        });
        registry
    }

    /// `[[dashboard]]` を 1 件持つプラグイン `widgety`(widget id "status"、
    /// entry "ui/index.html")だけを載せた `Registry`。plugins_dir は
    /// 返り値の `TempDir` 配下(`<tmp>/plugins`)なので、entry ファイルの
    /// 有無を呼び出し元が操作できる(fixture が `TempDir` を drop すると
    /// ディレクトリごと消えるため、所有権ごと返す)。
    pub(crate) fn test_registry_with_dashboard() -> (Registry, tempfile::TempDir) {
        let host = Arc::new(PluginHost::new().expect("host should start"));
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
            tmp.path().join("plugins"),
        );
        registry.push(PluginEntry {
            manifest: manifest_with_dashboard("widgety"),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
        });
        (registry, tmp)
    }

    fn manifest_with_dashboard(id: &str) -> Manifest {
        use crate::plugin::manifest::{DashboardWidget, WidgetSize};
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
        use crate::plugin::manifest::ScheduleRequest;
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
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
        });
        registry
    }

    /// `secret` 型設定を 1 件だけ持つプラグインを載せた `Registry`。
    /// `settings_store` は実ディレクトリを使うので、`set_values` で書いた値が
    /// `list()`/`values()` に効く。
    fn test_registry_with_secret() -> (Registry, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let host = Arc::new(PluginHost::new().expect("host should start"));
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
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
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
        let guard = registry
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let buffer = guard[0].settings_json.lock().unwrap().clone();
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
        let view = crate::plugin::schedule::ScheduleView::default();
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
        registry.register_schedule_view(
            "scheduler-plugin",
            crate::plugin::schedule::ScheduleView::default(),
        );

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
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
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
        driver_registry.push(crate::driver::registry::DriverEntry {
            manifest: crate::driver::manifest::DriverManifest {
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
            state: crate::driver::registry::DriverState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
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
    /// 一致することを見る(`bus_runtime` の「未承認は topics を落とす」
    /// 契約どおりであることの確認)。
    #[test]
    fn set_bus_grant_persists_and_updates_the_shared_buffer_both_ways() {
        let registry = test_registry_with_bus_request();

        let granted = registry
            .set_bus_grant("translator", "ed-state", true)
            .expect("granting a declared bus connection should succeed");
        assert!(granted.granted);
        let parsed = crate::plugin::bus_runtime::parse_bus(&registry.bus_buffer("translator").unwrap());
        let entry = parsed.get("ed-state").expect("entry present after grant");
        assert!(entry.granted);
        assert_eq!(entry.subscribe, vec!["current-system".to_string()]);

        let revoked = registry
            .set_bus_grant("translator", "ed-state", false)
            .expect("revoking should succeed");
        assert!(!revoked.granted);
        let parsed = crate::plugin::bus_runtime::parse_bus(&registry.bus_buffer("translator").unwrap());
        let entry = parsed.get("ed-state").expect("entry still present after revoke");
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
        let host = Arc::new(PluginHost::new().expect("host should start"));
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
            tmp.path().join("plugins"),
        );

        let manifest = manifest_with_filesystem("fs-plugin");
        let filesystem_json = Arc::new(Mutex::new("[]".to_string()));
        registry.push(PluginEntry {
            manifest,
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: filesystem_json.clone(),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
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
            crate::plugin::fs_runtime::parse_filesystem(&filesystem_json.lock().unwrap().clone());
        assert!(granted["exports"].granted);
        assert!(!granted["exports"].path.is_empty());

        // サイドカー停止(`ProcessDriver::stop`)がまだ終わっていない状態を、
        // その臨界区間で使われる id 別ロックを掴んだまま模す。
        let sidecar_lock = registry.sidecar_runtime_lock_for("fs-plugin");
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
            crate::plugin::fs_runtime::parse_filesystem(&filesystem_json.lock().unwrap().clone());
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
            sidecars: vec![crate::plugin::manifest::SidecarRequest {
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
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
        });

        let key = Registry::sidecar_key("sc-plugin", "tts");
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "sleep 30".into()],
            ports: vec![50900],
        };
        registry
            .process_driver
            .ensure_started(&key, &spec)
            .expect("start sidecar directly via the driver, bypassing wasm entirely");
        assert!(
            registry.process_driver.status(&key, &spec)[0].running,
            "sidecar should be running before disabling the plugin"
        );

        registry.set_disabled("sc-plugin", "on-event trapped".to_string());

        assert!(
            !registry.process_driver.status(&key, &spec)[0].running,
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
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
        });
        registry
            .grants_store
            .set_sidecar(&manifest, "tts", true)
            .expect("grant should persist");
        registry
            .sidecar_config_store
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

        let key = Registry::sidecar_key("sc-plugin", "tts");
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            ports: vec![50930],
        };
        assert!(
            !registry.process_driver.status(&key, &spec)[0].running,
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
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
        });

        registry
            .grants_store
            .set_sidecar(&manifest, "tts", true)
            .expect("grant should persist");
        registry
            .sidecar_config_store
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
            .grants_store
            .sidecar_state(&manifest, "tts")
            .granted;
        let key = Registry::sidecar_key("sc-plugin", "tts");
        let running = registry
            .process_driver
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

        registry.process_driver.stop(&key);
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
            capabilities_json: Arc::new(Mutex::new(crate::plugin::host::capabilities_json_string(
                &[],
            ))),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
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
                .grants_store
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
            .sidecar_config_store
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
                .grants_store
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
            capabilities: vec![crate::plugin::CapabilityRequest::Http {
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
            crate::plugin::host::capabilities_json_string(&[]),
        ));
        registry.push(PluginEntry {
            manifest: manifest.clone(),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: capabilities_json.clone(),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
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
        let host = Arc::new(PluginHost::new().expect("host should start"));
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
            tmp.path().join("plugins"),
        );

        let manifest = manifest_with_http_capability("cap-plugin");
        let capabilities_json = Arc::new(Mutex::new(
            crate::plugin::host::capabilities_json_string(&[]),
        ));
        registry.push(PluginEntry {
            manifest: manifest.clone(),
            state: PluginState::Running,
            settings_json: Arc::new(Mutex::new("{}".to_string())),
            capabilities_json: capabilities_json.clone(),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
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
            capabilities_json: Arc::new(Mutex::new(
                crate::plugin::host::capabilities_json_string(&[]),
            )),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
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

        let (work_tx, work_rx) = std_mpsc::sync_channel::<PluginWork>(4);
        let stopped = Arc::new(AtomicBool::new(false));
        let handle = {
            let stopped = stopped.clone();
            thread::spawn(move || {
                if let Ok(PluginWork::Stop) = work_rx.recv() {
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

        let (work_tx, work_rx) = std_mpsc::sync_channel::<PluginWork>(1);
        // キューを埋めて `try_send(Stop)` が `Full` になるようにする。
        work_tx
            .try_send(PluginWork::Event(Arc::new(Event::Status {
                raw: serde_json::json!({}),
            })))
            .expect("capacity-1 channel should accept the first send");

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
            capabilities_json: Arc::new(Mutex::new(
                crate::plugin::host::capabilities_json_string(&[]),
            )),
            sidecars_json: Arc::new(Mutex::new("[]".to_string())),
            filesystem_json: Arc::new(Mutex::new("[]".to_string())),
            bus_json: Arc::new(Mutex::new("[]".to_string())),
        });

        registry.shutdown_plugins();
    }
}

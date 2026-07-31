//! RPC 応答が使う「現在状態のスナップショット」値型群。
//!
//! ここに置く型はすべて値イン値アウト(フィールドは純粋または外部 crate の
//! 値型のみ)。`PluginInfo` / `DriverInfo` / `PluginState` は registry の語彙
//! なので `registry::plugin` に残す。

use edlr_driver_process::InstanceStatus;

use crate::capability::grants::GrantState;
use crate::capability::request::{BusRequest, DashboardWidget, FilesystemRequest, SidecarRequest};
use crate::manifest::ScheduleSpec;
use crate::settings::filesystem::FilesystemConfig;
use crate::settings::sidecar::SidecarConfig;

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

//! 実行中プラグインの状態を保持する共有ビュー。`start_plugins` が構築し、以後は
//! カーネル内の複数箇所(将来の RPC を含む)から `Clone` して読める。

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use edlr_driver_process::{InstanceStatus, ProcessDriver, ProcessSpec};

use crate::plugin::grants::{GrantState, GrantsError, GrantsStore};
use crate::plugin::host::{capabilities_json_string, parse_capability_hosts, PluginHost};
use crate::plugin::settings::SettingsStore;
use crate::plugin::sidecar::{assign_ports, SidecarConfig, SidecarConfigError, SidecarConfigStore};
use crate::plugin::sidecar_runtime::{implicit_http_hosts, sidecars_json_string, SidecarRuntimeEntry};
use crate::plugin::{CapabilityRequest, Manifest, SettingsError, SidecarRequest};

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
}

/// サイドカー 1 件分の現在状態(`Registry::sidecars` / `PluginInfo::sidecars` 用)。
pub struct SidecarInfo {
    pub request: SidecarRequest,
    pub config: SidecarConfig,
    pub grant: GrantState,
    pub instances: Vec<InstanceStatus>,
}

/// RPC 応答用のプラグイン情報スナップショット。
pub struct PluginInfo {
    pub manifest: Manifest,
    pub state: PluginState,
    pub values: serde_json::Map<String, serde_json::Value>,
    pub capability_requests: Vec<CapabilityRequest>,
    pub grant_state: GrantState,
    pub sidecars: Vec<SidecarInfo>,
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
    /// `refresh_sidecar_runtime` 全体(停止 → `sidecars_json` の作り直し →
    /// `capabilities_json` の作り直し)を直列化するロック。`capabilities_lock`
    /// と同じ理由(2 つの同時呼び出しがディスクと共有バッファを食い違わせない
    /// ため)に加え、`sidecars_json` と `capabilities_json` の 2 バッファを
    /// 同じ臨界区間で更新するためにも要る。`set_capabilities` の
    /// `capabilities_lock` とは別ロックにしてある(片方は http capability
    /// 専用、もう片方はサイドカー設定/承認専用の操作系列で、互いをブロック
    /// する理由が無いため)。
    ///
    /// ロック取得順序: `set_capabilities` と同じ流儀で、`entries` は
    /// 「manifest と共有ハンドルのクローンを取る」瞬間だけ握ってすぐ手放し、
    /// それを終えてから `sidecar_runtime_lock` を取る(`entries` → 解放 →
    /// `sidecar_runtime_lock` の順で、両者を同時に保持する区間は無い)。
    /// `sidecar_runtime_lock` の臨界区間の中で `entries` を再度取る箇所
    /// (`sidecars()` の呼び出し経由)があるが、その時点で
    /// `sidecar_runtime_lock` を保持したまま新たに `entries` を取るだけで、
    /// 逆方向(`entries` を保持したまま新たに `sidecar_runtime_lock` を
    /// 取る)は無いので循環せず、デッドロックしない。
    sidecar_runtime_lock: Arc<Mutex<()>>,
    plugins_dir: PathBuf,
}

impl Registry {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        host: Arc<PluginHost>,
        settings_store: Arc<SettingsStore>,
        grants_store: Arc<GrantsStore>,
        sidecar_config_store: Arc<SidecarConfigStore>,
        process_driver: Arc<ProcessDriver>,
        plugins_dir: PathBuf,
    ) -> Self {
        Registry {
            entries: Arc::new(Mutex::new(Vec::new())),
            _host: host,
            settings_store,
            grants_store,
            sidecar_config_store,
            process_driver,
            capabilities_lock: Arc::new(Mutex::new(())),
            sidecar_runtime_lock: Arc::new(Mutex::new(())),
            plugins_dir,
        }
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
                let values = self.settings_store.effective(&manifest);
                let grant_state = self.grants_store.state(&manifest);
                let capability_requests = manifest.capabilities.clone();
                let sidecars = self.build_sidecar_infos(&manifest);
                PluginInfo {
                    manifest,
                    state,
                    values,
                    capability_requests,
                    grant_state,
                    sidecars,
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
                let spec = ProcessSpec {
                    command: PathBuf::from(&config.command),
                    args: config.args.clone(),
                    ports: assign_ports(&config),
                };
                let key = Self::sidecar_key(&manifest.id, &request.name);
                let instances = self.process_driver.status(&key, &spec);
                SidecarInfo {
                    request: request.clone(),
                    config,
                    grant,
                    instances,
                }
            })
            .collect()
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
        Ok(self.settings_store.effective(&manifest))
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

        let settings_json_string =
            serde_json::to_string(&serde_json::Value::Object(effective.clone()))
                .unwrap_or_else(|_| "{}".to_string());
        *settings_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings_json_string;

        Ok(effective)
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
    /// 常に `sidecar_runtime_lock` → `capabilities_lock` の一方向のみで、
    /// この関数は `capabilities_lock` だけを取り `sidecar_runtime_lock` には
    /// 触れないため、両者を合わせてもデッドロックしない)。この関数自身は
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
        let sidecar_entries: Vec<SidecarRuntimeEntry> = crate::plugin::sidecar_runtime::parse_sidecars(
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
    /// 呼び出しごとに `sidecar_runtime_lock` を取り、停止からバッファ書き込み
    /// までを 1 つの臨界区間として保持する(`set_capabilities` の
    /// `capabilities_lock` と同じ理由: 2 つの同時呼び出し -- 例えば同じ
    /// サイドカーの設定変更と承認取消を 2 つの RPC クライアントがほぼ同時に
    /// 行う -- がディスクと共有バッファを食い違わせないようにするため)。
    ///
    /// `capabilities_json` への書き込みは、`set_capabilities` とも共有する
    /// 書き込み先であるため、内側で追加に `capabilities_lock` を取ってから
    /// 行う(`set_capabilities` のドキュメントコメント参照)。ロック順序は
    /// 常に `sidecar_runtime_lock` → `capabilities_lock` の一方向のみで、
    /// `set_capabilities` は `capabilities_lock` だけを取り
    /// `sidecar_runtime_lock` には触れないため、循環せずデッドロックしない。
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

        let _runtime_guard = self
            .sidecar_runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        for name in stop_names {
            let key = Self::sidecar_key(&manifest.id, name);
            self.process_driver.stop(&key);
        }

        let sidecar_configs = self.sidecar_config_store.effective(&manifest);
        let entries: Vec<SidecarRuntimeEntry> = manifest
            .sidecars
            .iter()
            .map(|request| {
                let config = sidecar_configs
                    .get(&request.name)
                    .cloned()
                    .unwrap_or_else(|| SidecarConfig::from_request(request));
                let granted = self
                    .grants_store
                    .sidecar_state(&manifest, &request.name)
                    .granted;
                SidecarRuntimeEntry {
                    name: request.name.clone(),
                    granted,
                    command: config.command.clone(),
                    args: config.args.clone(),
                    ports: assign_ports(&config),
                }
            })
            .collect();
        *sidecars_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = sidecars_json_string(&entries);

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
            hosts.extend(implicit_http_hosts(&entries));
            *capabilities_json
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = capabilities_json_string(&hosts);
        }

        self.sidecars(id)
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
                if action == SidecarAction::Restart {
                    self.process_driver.stop(&key);
                }

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

    /// `id` のプラグインが持つ settings JSON の共有ハンドルを返す。
    pub fn entry_settings(&self, id: &str) -> Option<Arc<Mutex<String>>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|entry| entry.manifest.id == id)
            .map(|entry| entry.settings_json.clone())
    }

    /// `id` のプラグインを `Disabled { reason }` にする。存在しなければ何もしない。
    pub fn set_disabled(&self, id: &str, reason: String) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = guard.iter_mut().find(|entry| entry.manifest.id == id) {
            entry.state = PluginState::Disabled { reason };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    fn empty_registry() -> Registry {
        let host = Arc::new(PluginHost::new().expect("host should start"));
        let tmp = tempfile::tempdir().unwrap();
        let settings_store = Arc::new(SettingsStore::new(tmp.path().join("settings")));
        let grants_store = Arc::new(GrantsStore::new(tmp.path().join("grants")));
        let sidecar_config_store = Arc::new(SidecarConfigStore::new(tmp.path().join("settings")));
        let process_driver = Arc::new(ProcessDriver::new(
            Duration::from_millis(200),
            Duration::from_millis(0),
        ));
        Registry::new(
            host,
            settings_store,
            grants_store,
            sidecar_config_store,
            process_driver,
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
        let process_driver = Arc::new(ProcessDriver::new(
            Duration::from_millis(200),
            Duration::from_millis(0),
        ));
        let registry = Registry::new(
            host,
            settings_store,
            grants_store.clone(),
            sidecar_config_store,
            process_driver,
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
}

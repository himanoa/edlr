//! サイドカープロセスの承認・起動停止状態管理。
//!
//! Phase 4 タスク6の move-only コミットで `crate::registry::plugin::Registry`
//! から plugin 専用として抽出したあと、このコミットで `RegistrySubject` を
//! 使って driver 側(`crate::registry::driver::DriverRegistry`)も同じ
//! `SidecarService` に載せた。分析(`docs/superpowers/specs/2026-07-31-phase4-registry-analysis.md`
//! §3)のとおり、この2つの実装はエラー文字列を含めて byte 同一(差は
//! 「未登録 id のエラー variant」「設定/承認ストア向けの `Manifest` 射影」
//! 「disabled メッセージの主語("plugin"/"driver")」だけ)だったので、その
//! 3点だけを `RegistrySubject`/`SidecarEntry` 越しにパラメータ化すれば良い。
//!
//! `P: registry::ProcessControl` はサイドカープロセス制御(Phase 0 で定義した
//! trait)の**初の consumer**。ディスク実装は
//! `edlr_driver_process::ProcessDriver`(`DiskSidecarService` alias が固定する)。
//!
//! `capabilities_lock` はコンストラクタ注入の共有 `Arc<Mutex<()>>`。plugin 側
//! `Registry` と driver 側 `DriverRegistry` は、それぞれ自分自身が
//! `set_capabilities` で使うのと同一の `Arc` を、自分の `SidecarService`
//! インスタンスにも渡す(plugin と driver で `Arc` を共有するわけではない --
//! 両者は別々の `capabilities_json` バッファ・別々のロックを持つ)。
//! `refresh_sidecar_runtime` の手順3(`capabilities_json` 書き換え)は、その
//! 同じレジストリの `set_capabilities` と書き込み先を共有しているため、同じ
//! ロックでなければ両者の書き込みが交互実行で食い違いうる。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use edlr_driver_process::ProcessSpec;

use crate::capability::grants::{GrantState, GrantsStore};
use crate::capability::request::SidecarRequest;
use crate::capability::GrantStorage;
use crate::host::plugin::capabilities_json_string;
use crate::manifest::Manifest;
use crate::registry::entries::{EntryTable, IdLocks};
use crate::registry::plugin::{PluginEntry, RegistryError, SidecarAction, SidecarInfo};
use crate::registry::subject::RegistrySubject;
use crate::registry::ProcessControl;
use crate::runtime::sidecar::{implicit_http_hosts, sidecars_json_string, SidecarRuntimeEntry};
use crate::settings::sidecar::{assign_ports, SidecarConfig, SidecarConfigStore};

/// `<id>/<sidecar-name>` の形で `ProcessControl` のキーを組み立てる。
/// `HostCtx::sidecar_key`(`core/src/host/plugin.rs`)と同じ規則。
pub(crate) fn sidecar_key(id: &str, name: &str) -> String {
    format!("{id}/{name}")
}

/// `control_sidecar` の `Start`/`Restart` 前提判定(値イン値アウト)。
/// 呼び出し側はこの結果に応じてエラー文字列を組み立てるだけにする(文字列
/// 自体は判定結果からは変えず、呼び出し側の `format!` が元の実装と byte
/// 同一になるようにしている -- `is_disabled` の主語("plugin"/"driver")は
/// `E::Subject::subject_noun()` に、`name` は呼び出し側の変数にしか無いため)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidecarStartDecision {
    /// プラグイン/ドライバ自体が無効化されている。
    Disabled,
    /// サイドカーが未承認。
    NotGranted,
    /// 実行ファイルが未設定。
    NoExecutable,
    /// 起動してよい。
    Proceed,
}

pub(crate) fn sidecar_start_decision(
    disabled: bool,
    granted: bool,
    command_configured: bool,
) -> SidecarStartDecision {
    if disabled {
        SidecarStartDecision::Disabled
    } else if !granted {
        SidecarStartDecision::NotGranted
    } else if !command_configured {
        SidecarStartDecision::NoExecutable
    } else {
        SidecarStartDecision::Proceed
    }
}

/// `set_sidecar_grant` の承認判定(値イン値アウト)。取消
/// (`granted == false`)は常に許す。承認(`granted == true`)は実行ファイルが
/// 設定済みのときだけ許す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantSidecarDecision {
    NoExecutable,
    Proceed,
}

pub(crate) fn grant_sidecar_decision(
    granted: bool,
    command_configured: bool,
) -> GrantSidecarDecision {
    if granted && !command_configured {
        GrantSidecarDecision::NoExecutable
    } else {
        GrantSidecarDecision::Proceed
    }
}

/// `set_sidecar_grant` が `refresh_sidecar_runtime` に渡す停止対象名の決定
/// (値イン値アウト)。取消(`granted == false`)のときだけ、そのサイドカー
/// 自身を停止対象にする(実際に止めるのは `refresh_sidecar_runtime` /
/// `stop_named` の役目で、ここは「何を止めるか」の判断だけを持つ)。
pub(crate) fn sidecar_grant_stop_names(name: &str, granted: bool) -> Vec<String> {
    if granted {
        Vec::new()
    } else {
        vec![name.to_string()]
    }
}

/// 実効許可ホストのマージ判定(値イン値アウト)。`granted` が偽なら明示許可
/// ホストは無視し、稼働中サイドカーの暗黙 127.0.0.1 許可だけを残す。
/// `refresh_sidecar_runtime`(このファイル)と `GrantService::set_capabilities`
/// (`registry/grants.rs`)の両方が同じ形の判断をしていたため、こちらに
/// 1本化して `grants.rs` から import する。
pub(crate) fn merged_effective_hosts(
    granted: bool,
    base_hosts: Vec<String>,
    sidecar_entries: &[SidecarRuntimeEntry],
) -> Vec<String> {
    let base = if granted { base_hosts } else { Vec::new() };
    base.into_iter()
        .chain(implicit_http_hosts(sidecar_entries))
        .collect()
}

/// `EntryTable<E>` の要素 `E` がサイドカー群に対して持つべき最小限の面。
///
/// `PluginEntry`/`DriverEntry` はどちらも `manifest` フィールドの型が違う
/// (`Manifest` / `DriverManifest`)ので、フィールドへ直接アクセスする代わりに
/// この trait 越しに引く(`registry::filesystem::FilesystemEntry` と同じ
/// パターン)。`is_disabled` だけ `FilesystemEntry` に無い面: `control_sidecar`
/// が id 別ロックの臨界区間の中で「現在このエントリが Disabled かどうか」を
/// 読む必要があるが、`PluginState`/`DriverState` の型はサービス側に見せたく
/// ないため、判定済みの `bool` だけを返させる。
pub(crate) trait SidecarEntry {
    type Subject: RegistrySubject;

    fn manifest(&self) -> &Self::Subject;
    fn sidecars_json(&self) -> &Arc<Mutex<String>>;
    fn capabilities_json(&self) -> &Arc<Mutex<String>>;
    /// このエントリが現在 `Disabled` かどうか。
    fn is_disabled(&self) -> bool;
}

impl SidecarEntry for PluginEntry {
    type Subject = crate::manifest::Manifest;

    fn manifest(&self) -> &crate::manifest::Manifest {
        &self.manifest
    }

    fn sidecars_json(&self) -> &Arc<Mutex<String>> {
        &self.sidecars_json
    }

    fn capabilities_json(&self) -> &Arc<Mutex<String>> {
        &self.capabilities_json
    }

    fn is_disabled(&self) -> bool {
        matches!(
            self.state,
            crate::registry::plugin::PluginState::Disabled { .. }
        )
    }
}

impl SidecarEntry for crate::registry::driver::DriverEntry {
    type Subject = crate::manifest::driver::DriverManifest;

    fn manifest(&self) -> &crate::manifest::driver::DriverManifest {
        &self.manifest
    }

    fn sidecars_json(&self) -> &Arc<Mutex<String>> {
        &self.sidecars_json
    }

    fn capabilities_json(&self) -> &Arc<Mutex<String>> {
        &self.capabilities_json
    }

    fn is_disabled(&self) -> bool {
        matches!(
            self.state,
            crate::registry::driver::DriverState::Disabled { .. }
        )
    }
}

/// サイドカー群(`sidecars` / `set_sidecar_config` / `set_sidecar_grant` /
/// `control_sidecar` / `stop_all` / `stop_named` とその内部ヘルパー)を束ねる
/// サービス。
///
/// `G: GrantStorage` はディスク実装(`GrantsStore`)を挿すためのジェネリクス。
/// `P: ProcessControl` はサイドカープロセス制御(ディスク実装は
/// `edlr_driver_process::ProcessDriver`)。`E: SidecarEntry` は plugin/driver
/// どちらの `EntryTable` 要素も受け付けるためのジェネリクス。公開面
/// (`DiskSidecarService`)は具象 `GrantsStore`/`ProcessDriver` を固定するが、
/// `E` はエントリ型そのものなので隠さず残す(`FilesystemService` と同じ
/// 流儀)。
pub(crate) struct SidecarService<G: GrantStorage, P: ProcessControl, E: SidecarEntry> {
    entries: EntryTable<E>,
    grants_store: Arc<G>,
    sidecar_config_store: Arc<SidecarConfigStore>,
    process_driver: Arc<P>,
    capabilities_lock: Arc<Mutex<()>>,
    sidecar_runtime_locks: IdLocks,
}

/// 手書き `Clone`: `derive(Clone)` は `G`/`P`/`E` 自体に `Clone` を要求して
/// しまうが、実際に clone が要るのは `Arc`/`EntryTable`/`IdLocks`(いずれも
/// 中身の型に関わらず `Clone`)だけなので、要らない境界を足さないよう手で書く
/// (`FilesystemService` と同じ理由)。
impl<G: GrantStorage, P: ProcessControl, E: SidecarEntry> Clone for SidecarService<G, P, E> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            grants_store: self.grants_store.clone(),
            sidecar_config_store: self.sidecar_config_store.clone(),
            process_driver: self.process_driver.clone(),
            capabilities_lock: self.capabilities_lock.clone(),
            sidecar_runtime_locks: self.sidecar_runtime_locks.clone(),
        }
    }
}

/// ディスク実装(`GrantsStore`/`ProcessDriver`)を挿した公開面。plugin/driver
/// どちらの `EntryTable` 要素かは呼び出し側が `E` で指定する。
pub(crate) type DiskSidecarService<E> =
    SidecarService<GrantsStore, edlr_driver_process::ProcessDriver, E>;

impl<G: GrantStorage, P: ProcessControl, E: SidecarEntry> SidecarService<G, P, E> {
    pub(crate) fn new(
        entries: EntryTable<E>,
        grants_store: Arc<G>,
        sidecar_config_store: Arc<SidecarConfigStore>,
        process_driver: Arc<P>,
        capabilities_lock: Arc<Mutex<()>>,
        sidecar_runtime_locks: IdLocks,
    ) -> Self {
        Self {
            entries,
            grants_store,
            sidecar_config_store,
            process_driver,
            capabilities_lock,
            sidecar_runtime_locks,
        }
    }

    /// `id` の manifest クローンを返す(`entries` ロック保持はこの
    /// ルックアップの間だけ)。未登録 id のエラーは `E::Subject::unknown_error`
    /// に委ねる(`UnknownPlugin` vs `UnknownDriver`)。
    fn find_manifest(&self, id: &str) -> Result<E::Subject, RegistryError> {
        self.entries
            .find(
                |entry| entry.manifest().id() == id,
                |entry| entry.manifest().clone(),
            )
            .ok_or_else(|| E::Subject::unknown_error(id))
    }

    /// `subject.sidecars()` の宣言順に `SidecarInfo` を組み立てる。設定
    /// (`SidecarConfigStore`)・承認(`GrantsStore`)はディスクを読むが、
    /// `ProcessControl::status` は読み取り専用(プロセスを起動も停止もしない)。
    pub(crate) fn build_sidecar_infos(&self, subject: &E::Subject) -> Vec<SidecarInfo> {
        let settings_manifest = subject.as_settings_manifest();
        let configs = self.sidecar_config_store.effective(&settings_manifest);
        subject
            .sidecars()
            .iter()
            .map(|request| {
                let config = configs
                    .get(&request.name)
                    .cloned()
                    .unwrap_or_else(|| SidecarConfig::from_request(request));
                let grant = self
                    .grants_store
                    .sidecar_state(&settings_manifest, &request.name);
                self.sidecar_info_and_entry(subject, request, config, grant)
                    .0
            })
            .collect()
    }

    /// `request` 1 件分の(既に取得済みの)設定・承認状態から、`SidecarInfo`
    /// と(`sidecars_json` バッファ用の)`SidecarRuntimeEntry` を両方組み立
    /// てる。`ProcessControl::status` の呼び出しを両者で 1 回だけ共有する
    /// (`config`/`grant` の取得元は呼び出し側に委ねているので、ここではもう
    /// ディスクを読まない -- `refresh_sidecar_runtime` が前半で読んだ値を
    /// そのまま渡し、末尾で読み直さずに済ませるための分離)。
    fn sidecar_info_and_entry(
        &self,
        subject: &E::Subject,
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
        let key = sidecar_key(subject.id(), &request.name);
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

    /// `id` 用のサイドカー実行時ロック(`sidecar_runtime_locks`)を引く。
    /// 無ければ作る。マップ自体を保護する `Mutex` は、id からロックの `Arc`
    /// を引く/挿入する間だけ保持し、返した `Arc<Mutex<()>>` の実際の臨界
    /// 区間(呼び出し側が別途 `.lock()` する)の間は保持しない。
    ///
    /// `pub(crate)`: サイドカー停止待ちがどれだけロックを保持するかを検証
    /// するテストが `crate::registry::plugin::tests` /
    /// `crate::registry::driver::tests` から直接このロックを取るため。
    pub(crate) fn sidecar_runtime_lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        self.sidecar_runtime_locks.lock_for(id)
    }

    /// テスト用アクセサ: サイドカーを wasm 呼び出しを経由せずホスト側から
    /// 直接操作する統合テストのため、内部の `ProcessControl` をそのまま返す。
    #[cfg(test)]
    pub(crate) fn process_driver(&self) -> &Arc<P> {
        &self.process_driver
    }

    /// テスト用アクセサ: `set_sidecar_config` の RPC 経路を経由せず、直接
    /// サイドカー設定を仕込む統合テストのため、内部の `SidecarConfigStore`
    /// をそのまま返す。
    #[cfg(test)]
    pub(crate) fn sidecar_config_store(&self) -> &Arc<SidecarConfigStore> {
        &self.sidecar_config_store
    }

    /// `id` の現在のサイドカー状態一覧(manifest の `[[sidecar]]` 宣言順)を
    /// 返す。
    pub(crate) fn sidecars(&self, id: &str) -> Result<Vec<SidecarInfo>, RegistryError> {
        let subject = self.find_manifest(id)?;
        Ok(self.build_sidecar_infos(&subject))
    }

    /// `id` が現在 `Disabled` かどうか。未登録の id は `false`
    /// (`control_sidecar` はこの手前で既に `find_manifest` を通しているので、
    /// 未登録を別扱いする必要はない)。
    fn is_disabled(&self, id: &str) -> bool {
        self.entries
            .find(
                |entry| entry.manifest().id() == id,
                |entry| entry.is_disabled(),
            )
            .unwrap_or(false)
    }

    /// サイドカーの設定変更・承認変更のあとに必ず呼ぶ内部ヘルパー。
    ///
    /// 手順は3段: 1) `stop_names` を `ProcessControl::stop` で停止、
    /// 2) `SidecarConfigStore`/`GrantsStore` の現在値から `sidecars_json` を
    /// 作り直す、3) `capabilities_lock` を取り、承認済みサイドカーの暗黙
    /// 127.0.0.1 許可を織り込んで `capabilities_json` も作り直す。
    ///
    /// `id` 専用のロック(`sidecar_runtime_lock_for`)を、手順 1〜3 すべてを
    /// 覆う 1 つの臨界区間として保持する(2 つの同時呼び出しがディスクと
    /// 共有バッファを食い違わせないようにするため)。ロック取得順序は常に
    /// 「id 別ロック → `capabilities_lock`」の一方向のみ。
    fn refresh_sidecar_runtime(
        &self,
        id: &str,
        stop_names: &[String],
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let (subject, sidecars_json, capabilities_json) = self
            .entries
            .find(
                |entry| entry.manifest().id() == id,
                |entry| {
                    (
                        entry.manifest().clone(),
                        entry.sidecars_json().clone(),
                        entry.capabilities_json().clone(),
                    )
                },
            )
            .ok_or_else(|| E::Subject::unknown_error(id))?;

        let runtime_lock = self.sidecar_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        for name in stop_names {
            let key = sidecar_key(subject.id(), name);
            self.process_driver.stop(&key);
        }

        let settings_manifest = subject.as_settings_manifest();
        let sidecar_configs = self.sidecar_config_store.effective(&settings_manifest);
        let mut infos = Vec::with_capacity(subject.sidecars().len());
        let mut runtime_entries = Vec::with_capacity(subject.sidecars().len());
        for request in subject.sidecars() {
            let config = sidecar_configs
                .get(&request.name)
                .cloned()
                .unwrap_or_else(|| SidecarConfig::from_request(request));
            let grant = self
                .grants_store
                .sidecar_state(&settings_manifest, &request.name);
            let (info, runtime_entry) =
                self.sidecar_info_and_entry(&subject, request, config, grant);
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
            let http_granted = self.grants_store.state(&settings_manifest).granted;
            let hosts = merged_effective_hosts(
                http_granted,
                settings_manifest.capability_hosts(),
                &runtime_entries,
            );
            *capabilities_json
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                capabilities_json_string(&hosts);
        }

        Ok(infos)
    }

    /// `id` の `name` サイドカーの設定を検証・永続化し、稼働中の実行を止めて
    /// から(`refresh_sidecar_runtime`)、最新の `SidecarInfo` 一覧を返す。
    /// 検証(`SidecarConfigStore::update_and_effective`)に失敗した場合は
    /// 何も変更されない。
    pub(crate) fn set_sidecar_config(
        &self,
        id: &str,
        name: &str,
        config: &SidecarConfig,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let subject = self.find_manifest(id)?;
        let settings_manifest = subject.as_settings_manifest();
        self.sidecar_config_store
            .update_and_effective(&settings_manifest, name, config)
            .map_err(RegistryError::SidecarConfig)?;
        let stop_names = vec![name.to_string()];
        self.refresh_sidecar_runtime(id, &stop_names)
    }

    /// `id` の `name` サイドカーの承認/取消を `GrantsStore` に永続化する。
    /// 取消(`granted == false`)のときは稼働中の実行を止める(走り続けてよい
    /// 根拠が承認と共に無くなるため)。
    ///
    /// `granted == true` のとき、`command` が未設定(空文字)のサイドカーは
    /// 拒否する(`RegistryError::Sidecar`)。`command` 未設定では承認できない
    /// という不変条件はここ(ストア/サービス側)で強制し、UI・RPC 層では
    /// 二重実装しない。取消は逆に常に許す。
    pub(crate) fn set_sidecar_grant(
        &self,
        id: &str,
        name: &str,
        granted: bool,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let subject = self.find_manifest(id)?;
        let settings_manifest = subject.as_settings_manifest();
        if settings_manifest.sidecar(name).is_none() {
            return Err(RegistryError::UnknownSidecar(name.to_string()));
        }

        // `granted == false` のときは実行ファイル設定を読む必要が無い(取消は
        // 常に許すため)。元の実装と同じく、その場合はディスクを読まない。
        let command_configured = granted
            && self
                .sidecar_config_store
                .effective(&settings_manifest)
                .get(name)
                .map(|config| !config.command.is_empty())
                .unwrap_or(false);
        if grant_sidecar_decision(granted, command_configured) == GrantSidecarDecision::NoExecutable
        {
            return Err(RegistryError::Sidecar(format!(
                "sidecar {name} has no executable configured; cannot grant"
            )));
        }

        self.grants_store
            .set_sidecar(&settings_manifest, name, granted)
            .map_err(RegistryError::Grants)?;

        let stop_names = sidecar_grant_stop_names(name, granted);
        self.refresh_sidecar_runtime(id, &stop_names)
    }

    /// `id` の `name` サイドカーを直接操作する(ユーザー操作起点)。`Stop` は
    /// 同期版 `ProcessControl::stop`、`Start` は `ProcessControl::ensure_started`、
    /// `Restart` は停止してから `ensure_started`。
    ///
    /// **TOCTOU 対策**: `Start`/`Restart` は `sidecar_runtime_lock_for(id)`
    /// (`refresh_sidecar_runtime` -- `set_sidecar_config`/`set_sidecar_grant`
    /// が呼ぶ -- と共有する、id 単位のロック)を、承認・設定を読む前から
    /// `ensure_started` を呼び終えるまで保持する。これが無いと、「grant を
    /// 読む」→「(ここで承認取消が割り込む)」→「その grant を信じて spawn
    /// する」という窓ができ、取消の裏で起動してしまう。
    ///
    /// **無効化された id は `Start`/`Restart` できない**: `stop_named` は
    /// そのプラグイン/ドライバの全サイドカーを停止するが、この関数が状態を
    /// 見ていないと、無効化後(あるいは並行中)に `start` が来ると、もう
    /// 生きている本体が無いサイドカーが再び起動してしまう。この関数は
    /// `sidecar_runtime_lock_for(id)` を取った**後**に `is_disabled(id)` を
    /// 読み、`true` なら拒否する -- 無効化処理(facade の `set_disabled`)
    /// 自身もサイドカー停止の前に同じ id 別ロックを取るので、「状態を
    /// `Disabled` にする」→「サイドカーを止める」という一連の操作と、この
    /// 関数の「状態を読む」→「spawn する」は互いに排他になり、無効化の裏で
    /// 起動してしまう窓は無い。`Stop` は状態に関わらず常に許す。
    ///
    /// disabled メッセージの主語("plugin {id} is disabled" / "driver {id}
    /// is disabled")は `E::Subject::subject_noun()` で分岐する(文字列は
    /// 元の実装と byte 同一)。
    pub(crate) fn control_sidecar(
        &self,
        id: &str,
        name: &str,
        action: SidecarAction,
    ) -> Result<Vec<SidecarInfo>, RegistryError> {
        let subject = self.find_manifest(id)?;
        let settings_manifest = subject.as_settings_manifest();
        let request = settings_manifest
            .sidecar(name)
            .ok_or_else(|| RegistryError::UnknownSidecar(name.to_string()))?;
        let key = sidecar_key(subject.id(), name);

        match action {
            SidecarAction::Stop => {
                self.process_driver.stop(&key);
            }
            SidecarAction::Start | SidecarAction::Restart => {
                self.start_or_restart_sidecar(id, name, &key, action, &settings_manifest, request)?;
            }
        }

        self.sidecars(id)
    }

    /// `control_sidecar` の `Start`/`Restart` 実行部(判断は
    /// `sidecar_start_decision` に委ね、ここは手順の羅列だけを持つ)。
    fn start_or_restart_sidecar(
        &self,
        id: &str,
        name: &str,
        key: &str,
        action: SidecarAction,
        settings_manifest: &Manifest,
        request: &SidecarRequest,
    ) -> Result<(), RegistryError> {
        let runtime_lock = self.sidecar_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // ロック保持中に読む: 無効化処理も同じ id 別ロックを取ってから
        // サイドカーを止めるので、ここで見る状態は「無効化処理が確定済み
        // ならその後の値」であることが保証される。
        let disabled = self.is_disabled(id);

        if action == SidecarAction::Restart && !disabled {
            self.process_driver.stop(key);
        }

        // ロック保持中に読み直す: `refresh_sidecar_runtime` は同じロックを
        // 取ってから承認取消/設定変更を確定するので、ここで読む値は
        // 「取消/変更が確定済みならその後の値」であることが保証される。
        let grant = self.grants_store.sidecar_state(settings_manifest, name);
        let configs = self.sidecar_config_store.effective(settings_manifest);
        let config = configs
            .get(name)
            .cloned()
            .unwrap_or_else(|| SidecarConfig::from_request(request));

        match sidecar_start_decision(disabled, grant.granted, !config.command.is_empty()) {
            SidecarStartDecision::Disabled => {
                return Err(RegistryError::Sidecar(format!(
                    "{} {id} is disabled",
                    E::Subject::subject_noun()
                )));
            }
            SidecarStartDecision::NotGranted => {
                return Err(RegistryError::Sidecar(format!(
                    "sidecar {name} is not granted"
                )));
            }
            SidecarStartDecision::NoExecutable => {
                return Err(RegistryError::Sidecar(format!(
                    "sidecar {name} has no executable configured"
                )));
            }
            SidecarStartDecision::Proceed => {}
        }

        let spec = ProcessSpec {
            command: PathBuf::from(&config.command),
            args: config.args.clone(),
            ports: assign_ports(&config),
        };
        self.process_driver
            .ensure_started(key, &spec)
            .map_err(|e| RegistryError::Sidecar(e.to_string()))?;
        Ok(())
    }

    /// 全プラグイン/ドライバの全サイドカーインスタンスを停止する(デーモン
    /// shutdown 用)。`ProcessControl::stop_all` をそのまま呼ぶ。
    pub(crate) fn stop_all(&self) {
        self.process_driver.stop_all();
    }

    /// `id` の `names` サイドカーを、`id` 専用の実行時ロックを取った上で
    /// 停止する。facade の `set_disabled` 用(`sidecars_json`/
    /// `capabilities_json` バッファの書き換えは行わない -- 元の
    /// `set_disabled` もサイドカーを止めるだけでバッファは触っていなかった)。
    ///
    /// 同じ id 専用ロックを `control_sidecar`(`Start`/`Restart`)や
    /// `refresh_sidecar_runtime` と共有するので、無効化に伴う停止とこれらの
    /// 操作は互いに排他になる(`control_sidecar` のドキュメントコメント
    /// 参照)。
    pub(crate) fn stop_named(&self, id: &str, names: &[String]) {
        let runtime_lock = self.sidecar_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for name in names {
            let key = sidecar_key(id, name);
            self.process_driver.stop(&key);
        }
    }
}

/// `capability::GrantStorage` / `registry::ProcessControl` の手書きモック。
/// `testing.md` の方針どおり mockall 等は使わず、ここに手書きする
/// (`InMemoryGrantStorage` は `registry::grants` のテストからも再利用する)。
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::capability::grants::GrantsError;
    use edlr_driver_process::{InstanceStatus, ProcessError};
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    /// `GrantStorage` のインメモリ実装。キーは
    /// `"<kind>:<manifest-id>[:<sub>]"` の合成文字列。未設定のキーは
    /// `GrantsStore` がファイル無しのときと同じ既定値
    /// (`granted: false, stale: false`)を返す。
    #[derive(Default)]
    pub(crate) struct InMemoryGrantStorage {
        states: StdMutex<HashMap<String, GrantState>>,
    }

    impl InMemoryGrantStorage {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        /// テストのセットアップ用: `set_sidecar` を経由せず、`manifest_id` の
        /// サイドカー `name` の承認状態を直接仕込む。
        pub(crate) fn seed_sidecar(&self, manifest_id: &str, name: &str, state: GrantState) {
            self.states
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(Self::sidecar_key(manifest_id, name), state);
        }

        fn base_key(manifest_id: &str) -> String {
            format!("base:{manifest_id}")
        }

        fn sidecar_key(manifest_id: &str, name: &str) -> String {
            format!("sidecar:{manifest_id}:{name}")
        }

        fn get_or_default(&self, key: &str) -> GrantState {
            self.states
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(key)
                .cloned()
                .unwrap_or(GrantState {
                    granted: false,
                    stale: false,
                })
        }

        fn set_key(&self, key: String, granted: bool) -> Result<GrantState, GrantsError> {
            let state = GrantState {
                granted,
                stale: false,
            };
            self.states
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(key, state.clone());
            Ok(state)
        }
    }

    impl GrantStorage for InMemoryGrantStorage {
        fn state(&self, manifest: &Manifest) -> GrantState {
            self.get_or_default(&Self::base_key(&manifest.id))
        }
        fn set(&self, manifest: &Manifest, granted: bool) -> Result<GrantState, GrantsError> {
            self.set_key(Self::base_key(&manifest.id), granted)
        }
        fn sidecar_state(&self, manifest: &Manifest, name: &str) -> GrantState {
            self.get_or_default(&Self::sidecar_key(&manifest.id, name))
        }
        fn set_sidecar(
            &self,
            manifest: &Manifest,
            name: &str,
            granted: bool,
        ) -> Result<GrantState, GrantsError> {
            self.set_key(Self::sidecar_key(&manifest.id, name), granted)
        }
        fn filesystem_state(&self, manifest: &Manifest, name: &str) -> GrantState {
            self.get_or_default(&format!("filesystem:{}:{name}", manifest.id))
        }
        fn set_filesystem(
            &self,
            manifest: &Manifest,
            name: &str,
            granted: bool,
        ) -> Result<GrantState, GrantsError> {
            self.set_key(format!("filesystem:{}:{name}", manifest.id), granted)
        }
        fn bus_state(&self, manifest: &Manifest, driver: &str) -> GrantState {
            self.get_or_default(&format!("bus:{}:{driver}", manifest.id))
        }
        fn set_bus(
            &self,
            manifest: &Manifest,
            driver: &str,
            granted: bool,
        ) -> Result<GrantState, GrantsError> {
            self.set_key(format!("bus:{}:{driver}", manifest.id), granted)
        }
        fn dashboard_state(&self, manifest: &Manifest, widget: &str) -> GrantState {
            self.get_or_default(&format!("dashboard:{}:{widget}", manifest.id))
        }
        fn set_dashboard(
            &self,
            manifest: &Manifest,
            widget: &str,
            granted: bool,
        ) -> Result<GrantState, GrantsError> {
            self.set_key(format!("dashboard:{}:{widget}", manifest.id), granted)
        }
    }

    /// `ProcessControl` の手書きモック: 呼び出し記録(`key` の列)+
    /// `key` ごとに設定した固定応答(`InstanceStatus` 一覧)を返す。
    #[derive(Default)]
    pub(crate) struct FakeProcessControl {
        responses: StdMutex<HashMap<String, Vec<InstanceStatus>>>,
        pub(crate) ensure_started_calls: StdMutex<Vec<String>>,
        pub(crate) status_calls: StdMutex<Vec<String>>,
        pub(crate) stop_calls: StdMutex<Vec<String>>,
    }

    impl FakeProcessControl {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        /// `key` に対する `status`/`ensure_started` の固定応答を設定する。
        pub(crate) fn set_response(&self, key: &str, instances: Vec<InstanceStatus>) {
            self.responses
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(key.to_string(), instances);
        }
    }

    impl ProcessControl for FakeProcessControl {
        fn ensure_started(
            &self,
            key: &str,
            _spec: &ProcessSpec,
        ) -> Result<Vec<InstanceStatus>, ProcessError> {
            self.ensure_started_calls
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(key.to_string());
            Ok(self
                .responses
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(key)
                .cloned()
                .unwrap_or_default())
        }
        fn status(&self, key: &str, _spec: &ProcessSpec) -> Vec<InstanceStatus> {
            self.status_calls
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(key.to_string());
            self.responses
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(key)
                .cloned()
                .unwrap_or_default()
        }
        fn stop(&self, key: &str) {
            self.stop_calls
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(key.to_string());
        }
        fn stop_all(&self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{FakeProcessControl, InMemoryGrantStorage};
    use super::*;
    use crate::capability::request::SidecarRequest as ManifestSidecarRequest;
    use crate::registry::plugin::{PluginEntry, PluginState};
    use edlr_driver_process::InstanceStatus;
    use std::sync::{Arc, Mutex};

    // --- 純関数の値イン値アウトテスト ---

    #[test]
    fn sidecar_start_decision_disabled_wins_over_other_checks() {
        // disabled が true なら granted/command_configured の値によらず
        // Disabled が最優先される。
        assert_eq!(
            sidecar_start_decision(true, true, true),
            SidecarStartDecision::Disabled
        );
        assert_eq!(
            sidecar_start_decision(true, false, false),
            SidecarStartDecision::Disabled
        );
    }

    #[test]
    fn sidecar_start_decision_not_granted_when_enabled() {
        assert_eq!(
            sidecar_start_decision(false, false, true),
            SidecarStartDecision::NotGranted
        );
    }

    #[test]
    fn sidecar_start_decision_no_executable_when_granted_but_unconfigured() {
        assert_eq!(
            sidecar_start_decision(false, true, false),
            SidecarStartDecision::NoExecutable
        );
    }

    #[test]
    fn sidecar_start_decision_proceeds_when_all_clear() {
        assert_eq!(
            sidecar_start_decision(false, true, true),
            SidecarStartDecision::Proceed
        );
    }

    #[test]
    fn grant_sidecar_decision_rejects_grant_without_executable() {
        assert_eq!(
            grant_sidecar_decision(true, false),
            GrantSidecarDecision::NoExecutable
        );
    }

    #[test]
    fn grant_sidecar_decision_allows_revoke_without_executable() {
        // 取消(granted=false)は実行ファイル未設定でも常に許す。
        assert_eq!(
            grant_sidecar_decision(false, false),
            GrantSidecarDecision::Proceed
        );
    }

    #[test]
    fn grant_sidecar_decision_allows_grant_with_executable() {
        assert_eq!(
            grant_sidecar_decision(true, true),
            GrantSidecarDecision::Proceed
        );
    }

    #[test]
    fn sidecar_grant_stop_names_is_empty_when_granting() {
        assert_eq!(sidecar_grant_stop_names("s1", true), Vec::<String>::new());
    }

    #[test]
    fn sidecar_grant_stop_names_contains_name_when_revoking() {
        assert_eq!(
            sidecar_grant_stop_names("s1", false),
            vec!["s1".to_string()]
        );
    }

    #[test]
    fn merged_effective_hosts_keeps_explicit_and_implicit_when_granted() {
        let sidecars = vec![SidecarRuntimeEntry {
            name: "s1".into(),
            granted: true,
            command: "/bin/s1".into(),
            args: vec![],
            ports: vec![9000],
        }];
        let hosts = merged_effective_hosts(true, vec!["example.com".into()], &sidecars);
        assert_eq!(
            hosts,
            vec![
                "example.com".to_string(),
                "http://127.0.0.1:9000".to_string()
            ]
        );
    }

    #[test]
    fn merged_effective_hosts_drops_explicit_but_keeps_implicit_when_not_granted() {
        let sidecars = vec![SidecarRuntimeEntry {
            name: "s1".into(),
            granted: true,
            command: "/bin/s1".into(),
            args: vec![],
            ports: vec![9000],
        }];
        let hosts = merged_effective_hosts(false, vec!["example.com".into()], &sidecars);
        assert_eq!(hosts, vec!["http://127.0.0.1:9000".to_string()]);
    }

    // --- モック越しの service フローテスト ---

    fn manifest_with_sidecar(id: &str, sidecar_name: &str, port: u16) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![],
            sidecars: vec![ManifestSidecarRequest {
                name: sidecar_name.to_string(),
                reason: "reason".into(),
                port,
                args: vec![],
                scalable: false,
            }],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    fn plugin_entry(manifest: Manifest, state: PluginState) -> PluginEntry {
        PluginEntry {
            manifest,
            state,
            settings_json: Arc::new(Mutex::new(String::new())),
            capabilities_json: Arc::new(Mutex::new(String::new())),
            sidecars_json: Arc::new(Mutex::new(String::new())),
            filesystem_json: Arc::new(Mutex::new(String::new())),
            bus_json: Arc::new(Mutex::new(String::new())),
        }
    }

    /// テスト用サービス組み立て: 実ディスク(`SidecarConfigStore`)は空の
    /// ディレクトリを指すだけ(`effective` はファイルが無ければ既定値のみを
    /// 返すので、これ自体はディスク I/O を伴わない -- `tempdir` は使わない)。
    fn test_service(
        entry: PluginEntry,
    ) -> (
        SidecarService<InMemoryGrantStorage, FakeProcessControl, PluginEntry>,
        Arc<InMemoryGrantStorage>,
        Arc<FakeProcessControl>,
    ) {
        let entries = EntryTable::new();
        entries.push(entry);
        let grants_store = Arc::new(InMemoryGrantStorage::new());
        let process_driver = Arc::new(FakeProcessControl::new());
        let sidecar_config_store = Arc::new(SidecarConfigStore::new(PathBuf::from(
            "/nonexistent/edlr-task10-test-sidecar-config",
        )));
        let service = SidecarService::new(
            entries,
            grants_store.clone(),
            sidecar_config_store,
            process_driver.clone(),
            Arc::new(Mutex::new(())),
            IdLocks::new(),
        );
        (service, grants_store, process_driver)
    }

    #[test]
    fn set_sidecar_grant_revokes_and_stops_the_running_instance() {
        let manifest = manifest_with_sidecar("p1", "s1", 9100);
        let (service, grants_store, process_driver) =
            test_service(plugin_entry(manifest, PluginState::Running));
        grants_store.seed_sidecar(
            "p1",
            "s1",
            GrantState {
                granted: true,
                stale: false,
            },
        );
        // 停止後もプロセスは(即座には exit しない)固定応答として観測できる:
        // `refresh_sidecar_runtime` が `process_driver.status` を呼んで
        // `SidecarInfo::instances` に反映することを検証する。
        process_driver.set_response(
            "p1/s1",
            vec![InstanceStatus {
                index: 0,
                port: 9100,
                running: false,
                exit_code: Some(0),
            }],
        );

        let infos = service
            .set_sidecar_grant("p1", "s1", false)
            .expect("revoke should succeed");

        assert!(!infos[0].grant.granted);
        assert_eq!(infos[0].instances[0].exit_code, Some(0));
        assert_eq!(
            process_driver.stop_calls.lock().unwrap().as_slice(),
            ["p1/s1"]
        );
    }

    #[test]
    fn set_sidecar_grant_rejects_granting_without_executable_configured() {
        let manifest = manifest_with_sidecar("p1", "s1", 9100);
        let (service, _grants_store, _process_driver) =
            test_service(plugin_entry(manifest, PluginState::Running));

        let Err(err) = service.set_sidecar_grant("p1", "s1", true) else {
            panic!("granting without a configured executable must be rejected");
        };

        assert!(matches!(
            err,
            RegistryError::Sidecar(msg) if msg == "sidecar s1 has no executable configured; cannot grant"
        ));
    }

    #[test]
    fn control_sidecar_start_rejects_disabled_plugin() {
        let manifest = manifest_with_sidecar("p1", "s1", 9100);
        let (service, _grants_store, _process_driver) = test_service(plugin_entry(
            manifest,
            PluginState::Disabled {
                reason: "trapped".into(),
            },
        ));

        let Err(err) = service.control_sidecar("p1", "s1", SidecarAction::Start) else {
            panic!("disabled plugin must reject Start");
        };

        assert!(matches!(
            err,
            RegistryError::Sidecar(msg) if msg == "plugin p1 is disabled"
        ));
    }

    #[test]
    fn control_sidecar_start_rejects_when_not_granted() {
        let manifest = manifest_with_sidecar("p1", "s1", 9100);
        let (service, _grants_store, _process_driver) =
            test_service(plugin_entry(manifest, PluginState::Running));

        let Err(err) = service.control_sidecar("p1", "s1", SidecarAction::Start) else {
            panic!("ungranted sidecar must reject Start");
        };

        assert!(matches!(
            err,
            RegistryError::Sidecar(msg) if msg == "sidecar s1 is not granted"
        ));
    }

    #[test]
    fn control_sidecar_stop_is_allowed_even_when_disabled() {
        let manifest = manifest_with_sidecar("p1", "s1", 9100);
        let (service, _grants_store, process_driver) = test_service(plugin_entry(
            manifest,
            PluginState::Disabled {
                reason: "trapped".into(),
            },
        ));

        let infos = service
            .control_sidecar("p1", "s1", SidecarAction::Stop)
            .expect("Stop is always allowed regardless of disabled state");

        assert_eq!(infos.len(), 1);
        assert_eq!(
            process_driver.stop_calls.lock().unwrap().as_slice(),
            ["p1/s1"]
        );
    }
}

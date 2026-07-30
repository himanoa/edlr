//! サイドカープロセスの承認・起動停止状態管理。
//!
//! Phase 4 タスク6の move-only コミットで `crate::plugin::registry::Registry`
//! から plugin 専用として抽出したあと、このコミットで `RegistrySubject` を
//! 使って driver 側(`crate::driver::registry::DriverRegistry`)も同じ
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

use crate::capability::GrantStorage;
use crate::plugin::grants::{GrantState, GrantsStore};
use crate::plugin::host::capabilities_json_string;
use crate::plugin::registry::{PluginEntry, RegistryError, SidecarAction, SidecarInfo};
use crate::plugin::sidecar::{assign_ports, SidecarConfig, SidecarConfigStore};
use crate::plugin::sidecar_runtime::{
    implicit_http_hosts, sidecars_json_string, SidecarRuntimeEntry,
};
use crate::plugin::SidecarRequest;
use crate::registry::entries::{EntryTable, IdLocks};
use crate::registry::subject::RegistrySubject;
use crate::registry::ProcessControl;

/// `<id>/<sidecar-name>` の形で `ProcessControl` のキーを組み立てる。
/// `HostCtx::sidecar_key`(`core/src/plugin/host.rs`)と同じ規則。
pub(crate) fn sidecar_key(id: &str, name: &str) -> String {
    format!("{id}/{name}")
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
    type Subject = crate::plugin::Manifest;

    fn manifest(&self) -> &crate::plugin::Manifest {
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
            crate::plugin::registry::PluginState::Disabled { .. }
        )
    }
}

impl SidecarEntry for crate::driver::registry::DriverEntry {
    type Subject = crate::driver::manifest::DriverManifest;

    fn manifest(&self) -> &crate::driver::manifest::DriverManifest {
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
            crate::driver::registry::DriverState::Disabled { .. }
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
    #[allow(clippy::too_many_arguments)]
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
    /// するテストが `crate::plugin::registry::tests` /
    /// `crate::driver::registry::tests` から直接このロックを取るため。
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
            let mut hosts = if http_granted {
                settings_manifest.capability_hosts()
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

        if granted {
            let configs = self.sidecar_config_store.effective(&settings_manifest);
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
            .set_sidecar(&settings_manifest, name, granted)
            .map_err(RegistryError::Grants)?;

        let stop_names: Vec<String> = if granted {
            Vec::new()
        } else {
            vec![name.to_string()]
        };
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
                let runtime_lock = self.sidecar_runtime_lock_for(id);
                let _runtime_guard = runtime_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());

                // ロック保持中に読む: 無効化処理も同じ id 別ロックを取って
                // からサイドカーを止めるので、ここで見る状態は「無効化処理
                // が確定済みならその後の値」であることが保証される。
                if self.is_disabled(id) {
                    return Err(RegistryError::Sidecar(format!(
                        "{} {id} is disabled",
                        E::Subject::subject_noun()
                    )));
                }

                if action == SidecarAction::Restart {
                    self.process_driver.stop(&key);
                }

                // ロック保持中に読み直す: `refresh_sidecar_runtime` は同じ
                // ロックを取ってから承認取消/設定変更を確定するので、ここで
                // 読む値は「取消/変更が確定済みならその後の値」であることが
                // 保証される。
                let grant = self.grants_store.sidecar_state(&settings_manifest, name);
                if !grant.granted {
                    return Err(RegistryError::Sidecar(format!(
                        "sidecar {name} is not granted"
                    )));
                }

                let configs = self.sidecar_config_store.effective(&settings_manifest);
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

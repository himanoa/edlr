//! ファイルアクセス承認(`[[filesystem]]`)の状態管理。
//!
//! Phase 4 タスク4の move-only コミットで `crate::registry::plugin::Registry`
//! から plugin 専用として抽出したあと、このコミットで `RegistrySubject` を
//! 使って driver 側(`crate::registry::driver::DriverRegistry`)も同じ
//! `FilesystemService` に載せた。分析(`docs/superpowers/specs/2026-07-31-phase4-registry-analysis.md`
//! §3)のとおり、この2つの実装はエラー文字列を含めて byte 同一(差は
//! 「未登録 id のエラー variant」と「設定/承認ストア向けの `Manifest` 射影」
//! だけ)だったので、その2点だけを `RegistrySubject`/`FilesystemEntry` 越しに
//! パラメータ化すれば良い。

use std::sync::{Arc, Mutex};

use crate::capability::grants::GrantsStore;
use crate::capability::GrantStorage;
use crate::registry::entries::{EntryTable, IdLocks};
use crate::registry::plugin::{FilesystemInfo, PluginEntry, RegistryError};
use crate::registry::subject::RegistrySubject;
use crate::runtime::fs::{filesystem_json_string, FsRuntimeEntry};
use crate::settings::filesystem::{FilesystemConfig, FilesystemConfigStore};

/// `EntryTable<E>` の要素 `E` が fs 群に対して持つべき最小限の面。
///
/// `PluginEntry`/`DriverEntry` はどちらも `manifest` フィールドの型が違う
/// (`Manifest` / `DriverManifest`)ので、フィールドへ直接アクセスする代わりに
/// この trait 越しに引く。`Subject` が `RegistrySubject` を実装することで
/// `FilesystemService` は manifest の具体型を知らずに済む。
pub(crate) trait FilesystemEntry {
    type Subject: RegistrySubject;

    fn manifest(&self) -> &Self::Subject;
    fn filesystem_json(&self) -> &Arc<Mutex<String>>;
}

impl FilesystemEntry for PluginEntry {
    type Subject = crate::manifest::Manifest;

    fn manifest(&self) -> &crate::manifest::Manifest {
        &self.manifest
    }

    fn filesystem_json(&self) -> &Arc<Mutex<String>> {
        &self.filesystem_json
    }
}

impl FilesystemEntry for crate::registry::driver::DriverEntry {
    type Subject = crate::manifest::driver::DriverManifest;

    fn manifest(&self) -> &crate::manifest::driver::DriverManifest {
        &self.manifest
    }

    fn filesystem_json(&self) -> &Arc<Mutex<String>> {
        &self.filesystem_json
    }
}

/// fs 群(`filesystem` / `filesystem_buffer` / `set_filesystem_config` /
/// `set_filesystem_grant` とその内部ヘルパー)を束ねるサービス。
///
/// `G: GrantStorage` はディスク実装(`GrantsStore`)を挿すためのジェネリクス
/// (Phase 0 の `capability::GrantStorage` の最初の consumer -- `trait-di.md`
/// 参照)。`E: FilesystemEntry` は plugin/driver どちらの `EntryTable` 要素も
/// 受け付けるためのジェネリクス。公開面(`DiskFilesystemService`)は具象
/// `GrantsStore` を固定するが、`E` はエントリ型そのものなので隠さず残す。
///
/// 元の `Registry`/`DriverRegistry` から抽出した各フィールドの役割はそのまま:
/// `entries` は manifest クローン取得のみに使い、ディスク I/O やロック待ちに
/// 入る前に手放す(`EntryTable` のドキュメント参照)。`filesystem_runtime_locks`
/// はプラグイン/ドライバ ID ごとに `refresh_filesystem_runtime` の臨界区間を
/// 直列化する(サイドカーの `ProcessDriver::stop` に巻き込まれて fail-open に
/// ならないよう、意図的にサイドカー側とは別マップ -- 元の `Registry` の
/// `filesystem_runtime_locks` ドキュメント参照)。
pub(crate) struct FilesystemService<G: GrantStorage, E: FilesystemEntry> {
    entries: EntryTable<E>,
    grants_store: Arc<G>,
    filesystem_config_store: Arc<FilesystemConfigStore>,
    filesystem_runtime_locks: IdLocks,
}

/// 手書き `Clone`: `derive(Clone)` は `G`/`E` 自体に `Clone` を要求してしまうが、
/// 実際に clone が要るのは `Arc`/`EntryTable`/`IdLocks`(いずれも中身の型に
/// 関わらず `Clone`)だけなので、要らない境界を足さないよう手で書く。
impl<G: GrantStorage, E: FilesystemEntry> Clone for FilesystemService<G, E> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            grants_store: self.grants_store.clone(),
            filesystem_config_store: self.filesystem_config_store.clone(),
            filesystem_runtime_locks: self.filesystem_runtime_locks.clone(),
        }
    }
}

/// ディスク実装(`GrantsStore`)を挿した公開面。plugin/driver どちらの
/// `EntryTable` 要素かは呼び出し側が `E` で指定する(`trait-di.md` の
/// 「内部は generics、公開面は alias」に対応)。
pub(crate) type DiskFilesystemService<E> = FilesystemService<GrantsStore, E>;

impl<G: GrantStorage, E: FilesystemEntry> FilesystemService<G, E> {
    pub(crate) fn new(
        entries: EntryTable<E>,
        grants_store: Arc<G>,
        filesystem_config_store: Arc<FilesystemConfigStore>,
        filesystem_runtime_locks: IdLocks,
    ) -> Self {
        Self {
            entries,
            grants_store,
            filesystem_config_store,
            filesystem_runtime_locks,
        }
    }

    /// `id` の manifest クローンを返す(`entries` ロック保持はこの
    /// ルックアップの間だけ)。`Registry::find_manifest` /
    /// `DriverRegistry::find_manifest_for_shared` と同じ流儀。未登録 id の
    /// エラーは `E::Subject::unknown_registry_error` に委ねる(`UnknownPlugin` vs
    /// `UnknownDriver`)。
    fn find_manifest(&self, id: &str) -> Result<E::Subject, RegistryError> {
        self.entries
            .find(
                |entry| entry.manifest().id() == id,
                |entry| entry.manifest().clone(),
            )
            .ok_or_else(|| E::Subject::unknown_registry_error(id))
    }

    /// `subject.filesystem()` の宣言順に `FilesystemInfo` を組み立てる。
    /// 設定(`FilesystemConfigStore`)・承認(`GrantsStore`)はディスクを読む
    /// (`build_sidecar_infos` と同じ流儀。こちらはプロセス状態が無いので
    /// `ProcessDriver::status` に相当する読み取りは無い)。設定/承認ストアは
    /// `crate::manifest::Manifest` しか受け付けないため、`as_settings_manifest`
    /// で一度だけ射影する(plugin は clone、driver は
    /// `DriverManifest::as_settings_manifest` と同じ変換)。
    pub(crate) fn build_filesystem_infos(&self, subject: &E::Subject) -> Vec<FilesystemInfo> {
        let settings_manifest = subject.as_settings_manifest();
        let configs = self.filesystem_config_store.effective(&settings_manifest);
        subject
            .filesystem()
            .iter()
            .map(|request| {
                let config =
                    configs
                        .get(&request.name)
                        .cloned()
                        .unwrap_or_else(|| FilesystemConfig {
                            path: String::new(),
                        });
                let grant = self
                    .grants_store
                    .filesystem_state(&settings_manifest, &request.name);
                FilesystemInfo {
                    request: request.clone(),
                    config,
                    grant,
                }
            })
            .collect()
    }

    /// `id` 用のファイルアクセス実行時ロック(`filesystem_runtime_locks`)を
    /// 引く。サイドカー側とは**別のマップ**であることが要点
    /// (`filesystem_runtime_locks` のドキュメント参照)。
    fn filesystem_runtime_lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        self.filesystem_runtime_locks.lock_for(id)
    }

    /// `id` の現在のファイルアクセス状態一覧(manifest の `[[filesystem]]`
    /// 宣言順)を返す。
    pub(crate) fn filesystem(&self, id: &str) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let subject = self.find_manifest(id)?;
        Ok(self.build_filesystem_infos(&subject))
    }

    /// `id` の `filesystem_json` 共有バッファの中身をそのまま返す
    /// (テスト用アクセサ)。`driver-fs.*` が実際に参照するのと同じ文字列
    /// (`crate::runtime::fs::filesystem_json_string` の出力そのもの)。
    pub(crate) fn filesystem_buffer(&self, id: &str) -> Result<String, RegistryError> {
        let filesystem_json = self
            .entries
            .find(
                |entry| entry.manifest().id() == id,
                |entry| entry.filesystem_json().clone(),
            )
            .ok_or_else(|| E::Subject::unknown_registry_error(id))?;
        let buffer = filesystem_json
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
    /// 作り直すだけ(未承認のルートは `path` を持たない -- `crate::runtime::fs` の
    /// ドキュメント参照)。それでも「永続化(呼び出し元で完了済み)」と
    /// 「バッファへの反映」を同じ id 別ロックの臨界区間に収めるのは
    /// `refresh_sidecar_runtime` と同じ理由: 2 つの同時呼び出し(例えば
    /// 同じルートの設定変更と承認取消を 2 つの RPC クライアントがほぼ同時に
    /// 行う)がディスクと共有バッファを食い違わせないようにするため。
    ///
    /// ロックは `filesystem_runtime_lock_for(id)` -- **サイドカーとは別の
    /// マップ** から引く(`filesystem_runtime_locks` のドキュメント参照)。
    /// 共有していた頃は、同じプラグイン/ドライバのサイドカー停止
    /// (`ProcessDriver::stop`)が終わるまで承認取消がロック待ちになり、その
    /// 間 `filesystem_json` が古い `granted:true` と path を持ち続けていた
    /// (最大で `shutdown_grace` × インスタンス数の fail-open)。
    /// 取得順序は既存の一方向(`entries` → マップの Mutex → id 別ロック)の
    /// ままで、この関数は `capabilities_lock` を取らない(ファイルアクセスの
    /// 承認は `capabilities_json` に影響しない)ため、`set_capabilities`/
    /// `refresh_sidecar_runtime` との間に新たな循環は生じない。
    fn refresh_filesystem_runtime(&self, id: &str) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let (subject, filesystem_json) = self
            .entries
            .find(
                |entry| entry.manifest().id() == id,
                |entry| (entry.manifest().clone(), entry.filesystem_json().clone()),
            )
            .ok_or_else(|| E::Subject::unknown_registry_error(id))?;

        let runtime_lock = self.filesystem_runtime_lock_for(id);
        let _runtime_guard = runtime_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let infos = self.build_filesystem_infos(&subject);
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

    /// `id` の `name` ファイルアクセスルートの設定を検証・永続化し
    /// (`FilesystemConfigStore::update_and_effective`)、稼働中プラグイン/
    /// ドライバが参照する `filesystem_json` を作り直してから最新の
    /// `FilesystemInfo` 一覧を返す。検証に失敗した場合は何も変更されない。
    ///
    /// **承認は維持する**: パス自体は `Manifest::filesystem_fingerprint` に
    /// 含まれない(fingerprint が変わるのは `name`/`reason`/`mode` のみ)ので、
    /// ディレクトリを変更しても `GrantsStore` 上の承認は stale にならず、
    /// 生きたまま新しいパスに追従する(ブリーフの
    /// `changing_the_directory_takes_effect_without_reapproval` が検証する
    /// 挙動)。
    pub(crate) fn set_filesystem_config(
        &self,
        id: &str,
        name: &str,
        config: &FilesystemConfig,
    ) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let subject = self.find_manifest(id)?;
        let settings_manifest = subject.as_settings_manifest();
        self.filesystem_config_store
            .update_and_effective(&settings_manifest, name, config)
            .map_err(RegistryError::FilesystemConfig)?;
        self.refresh_filesystem_runtime(id)
    }

    /// `id` の `name` ファイルアクセスルートの承認/取消を `GrantsStore` に
    /// 永続化する。
    ///
    /// `granted == true` のとき、ディレクトリが未設定(空文字)のルートは
    /// 拒否する(`RegistryError::Filesystem`)。UI 側は未設定の間チェック
    /// ボックスを `disabled` にしているはずだが、それは UI 上の防御に過ぎ
    /// ない -- RPC を直接叩けばこの検証を経由せずに「ユーザーがどこへの
    /// アクセスかを一度も選んでいない」状態のルートを承認できてしまう。
    /// `set_sidecar_grant` の `command` 未設定チェックと同じ理由・同じ
    /// 場所(ストア/サービス側)で強制し、UI・RPC 層では二重実装しない。
    /// 取消は逆に常に許す -- ディレクトリを消した状態でも稼働中の承認を
    /// 取り消せなければ fail-open になってしまうため。
    pub(crate) fn set_filesystem_grant(
        &self,
        id: &str,
        name: &str,
        granted: bool,
    ) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let subject = self.find_manifest(id)?;
        let settings_manifest = subject.as_settings_manifest();
        if settings_manifest.filesystem_root(name).is_none() {
            return Err(RegistryError::UnknownFilesystem(name.to_string()));
        }

        if granted {
            let configs = self.filesystem_config_store.effective(&settings_manifest);
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
            .set_filesystem(&settings_manifest, name, granted)
            .map_err(RegistryError::Grants)?;

        self.refresh_filesystem_runtime(id)
    }
}

//! ファイルアクセス承認(`[[filesystem]]`)の状態管理。
//!
//! `crate::plugin::registry::Registry` から fs 群のメソッド本体をそのまま
//! 移した(Phase 4 タスク4、move-only)。この時点ではまだ plugin 専用
//! (`GrantsStore` / `PluginEntry` 具象)で、driver 側への一般化は次のコミットで
//! `RegistrySubject` を導入してから行う。

use std::sync::{Arc, Mutex};

use crate::plugin::filesystem::{FilesystemConfig, FilesystemConfigStore};
use crate::plugin::fs_runtime::{filesystem_json_string, FsRuntimeEntry};
use crate::plugin::grants::GrantsStore;
use crate::plugin::registry::{FilesystemInfo, PluginEntry, RegistryError};
use crate::plugin::Manifest;
use crate::registry::entries::{EntryTable, IdLocks};

/// fs 群(`filesystem` / `filesystem_buffer` / `set_filesystem_config` /
/// `set_filesystem_grant` とその内部ヘルパー)を束ねるサービス。
///
/// 元の `Registry` から抽出した各フィールドの役割はそのまま:
/// `entries` は manifest クローン取得のみに使い、ディスク I/O やロック待ちに
/// 入る前に手放す(`EntryTable` のドキュメント参照)。`filesystem_runtime_locks`
/// はプラグイン ID ごとに `refresh_filesystem_runtime` の臨界区間を直列化する
/// (サイドカーの `ProcessDriver::stop` に巻き込まれて fail-open にならない
/// よう、意図的にサイドカー側とは別マップ -- 元の `Registry` の
/// `filesystem_runtime_locks` ドキュメント参照)。
#[derive(Clone)]
pub(crate) struct FilesystemService {
    entries: EntryTable<PluginEntry>,
    grants_store: Arc<GrantsStore>,
    filesystem_config_store: Arc<FilesystemConfigStore>,
    filesystem_runtime_locks: IdLocks,
}

impl FilesystemService {
    pub(crate) fn new(
        entries: EntryTable<PluginEntry>,
        grants_store: Arc<GrantsStore>,
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

    /// `id` のプラグインの manifest クローンを返す(`entries` ロック保持は
    /// このルックアップの間だけ)。`Registry::find_manifest` と同じ流儀
    /// (サイドカー/capabilities など他の関心事も同じ形の私有ヘルパーを持つ)。
    fn find_manifest(&self, id: &str) -> Result<Manifest, RegistryError> {
        self.entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| entry.manifest.clone(),
            )
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))
    }

    /// `manifest.filesystem` の宣言順に `FilesystemInfo` を組み立てる。
    /// 設定(`FilesystemConfigStore`)・承認(`GrantsStore`)はディスクを読む
    /// (`build_sidecar_infos` と同じ流儀。こちらはプロセス状態が無いので
    /// `ProcessDriver::status` に相当する読み取りは無い)。
    pub(crate) fn build_filesystem_infos(&self, manifest: &Manifest) -> Vec<FilesystemInfo> {
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

    /// `id` 用のファイルアクセス実行時ロック(`filesystem_runtime_locks`)を
    /// 引く。サイドカー側とは**別のマップ**であることが要点
    /// (`filesystem_runtime_locks` のドキュメント参照)。
    fn filesystem_runtime_lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        self.filesystem_runtime_locks.lock_for(id)
    }

    /// `id` のプラグインの現在のファイルアクセス状態一覧(manifest の
    /// `[[filesystem]]` 宣言順)を返す。
    pub(crate) fn filesystem(&self, id: &str) -> Result<Vec<FilesystemInfo>, RegistryError> {
        let manifest = self.find_manifest(id)?;
        Ok(self.build_filesystem_infos(&manifest))
    }

    /// `id` のプラグインの `filesystem_json` 共有バッファの中身をそのまま
    /// 返す(テスト用アクセサ)。`driver-fs.*` が実際に参照するのと同じ
    /// 文字列(`fs_runtime::filesystem_json_string` の出力そのもの)。
    pub(crate) fn filesystem_buffer(&self, id: &str) -> Result<String, RegistryError> {
        let filesystem_json = self
            .entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| entry.filesystem_json.clone(),
            )
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;
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
        let (manifest, filesystem_json) = self
            .entries
            .find(
                |entry| entry.manifest.id == id,
                |entry| (entry.manifest.clone(), entry.filesystem_json.clone()),
            )
            .ok_or_else(|| RegistryError::UnknownPlugin(id.to_string()))?;

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
    pub(crate) fn set_filesystem_config(
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
    pub(crate) fn set_filesystem_grant(
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
}

//! 実行中プラグインの状態を保持する共有ビュー。`start_plugins` が構築し、以後は
//! カーネル内の複数箇所(将来の RPC を含む)から `Clone` して読める。

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::plugin::host::PluginHost;
use crate::plugin::settings::SettingsStore;
use crate::plugin::{Manifest, SettingsError};

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
}

/// RPC 応答用のプラグイン情報スナップショット。
pub struct PluginInfo {
    pub manifest: Manifest,
    pub state: PluginState,
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// `Registry` の値アクセス系メソッドが返しうるエラー。
#[derive(Debug)]
pub enum RegistryError {
    /// 指定された `id` のプラグインが登録されていない。
    UnknownPlugin(String),
    /// `SettingsStore::update` による検証・永続化エラー。
    Settings(SettingsError),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::UnknownPlugin(id) => write!(f, "unknown plugin: {id}"),
            RegistryError::Settings(e) => write!(f, "{e}"),
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
    plugins_dir: PathBuf,
}

impl Registry {
    pub(crate) fn new(
        host: Arc<PluginHost>,
        settings_store: Arc<SettingsStore>,
        plugins_dir: PathBuf,
    ) -> Self {
        Registry {
            entries: Arc::new(Mutex::new(Vec::new())),
            _host: host,
            settings_store,
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
                PluginInfo {
                    manifest,
                    state,
                    values,
                }
            })
            .collect()
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

    fn empty_registry() -> Registry {
        let host = Arc::new(PluginHost::new().expect("host should start"));
        let tmp = tempfile::tempdir().unwrap();
        let settings_store = Arc::new(SettingsStore::new(tmp.path().join("settings")));
        Registry::new(host, settings_store, tmp.path().join("plugins"))
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
}

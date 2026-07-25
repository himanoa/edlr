//! 実行中プラグインの状態を保持する共有ビュー。`start_plugins` が構築し、以後は
//! カーネル内の複数箇所(将来の RPC を含む)から `Clone` して読める。

use std::sync::{Arc, Mutex};

use crate::plugin::host::PluginHost;
use crate::plugin::Manifest;

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
}

impl Registry {
    pub(crate) fn new(host: Arc<PluginHost>) -> Self {
        Registry {
            entries: Arc::new(Mutex::new(Vec::new())),
            _host: host,
        }
    }

    pub(crate) fn push(&self, entry: PluginEntry) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(entry);
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

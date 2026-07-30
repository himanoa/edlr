//! `crate::plugin::registry::Registry` と `crate::driver::registry::DriverRegistry`
//! が共通で持っていた「エントリ一覧 + per-id ロックマップ」の配線を抽出した
//! もの(Phase 4 タスク2、move-only)。
//!
//! 抽出元の2箇所(`plugin/registry.rs` の `entries: Arc<Mutex<Vec<PluginEntry>>>`
//! と `driver/registry.rs` の `entries: Arc<Mutex<Vec<DriverEntry>>>`)は、
//! ロック保持区間の規律が完全に同一だった: `entries` のロックは
//! 「manifest や共有バッファハンドルのクローンを取る」間だけ保持し、
//! ディスク I/O やプロセス制御など時間のかかる処理へ入る前に手放す
//! (`crate::plugin::registry::Registry` の型ドキュメントコメント、
//! `sidecar_runtime_locks` 付近の記述を参照)。`EntryTable<E>` の各メソッドは
//! その規律をそのまま体現する: クロージャを呼んでいる間だけロックを握り、
//! 返り値はクロージャの外へ出る前に(`T` として)取り出し済みになる。
//!
//! `IdLocks` は `sidecar_runtime_locks` / `filesystem_runtime_locks` /
//! `bus_runtime_locks`(plugin 側)や `sidecar_runtime_locks` /
//! `filesystem_runtime_locks`(driver 側)として個別に持っていた
//! `Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>` のマップ操作を抽出した
//! もの。**マップを1つに統合するものではない**: 3本(plugin)/2本(driver)の
//! 別々の `IdLocks` インスタンスをそれぞれのフィールドとして持つ
//! (フィールドを分けている理由は fail-open 対策の意図的な設計であり、
//! `crate::plugin::registry::Registry` の `filesystem_runtime_locks` /
//! `bus_runtime_locks` のドキュメントコメントを参照)。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// エントリ一覧 `Vec<E>` を `Arc<Mutex<_>>` で共有するための薄いラッパー。
///
/// `PluginEntry` / `DriverEntry` のどちらでも使える(現行コードの `entries`
/// 直接操作から逆算したメソッド集合のみを持つ -- 過不足なく)。
pub(crate) struct EntryTable<E> {
    entries: Arc<Mutex<Vec<E>>>,
}

impl<E> EntryTable<E> {
    pub(crate) fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 末尾に1件追加する(`Registry::push` / `DriverRegistry::push`)。
    pub(crate) fn push(&self, entry: E) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(entry);
    }

    /// 全エントリのスナップショットに対してクロージャを適用する
    /// (`snapshot` / `list` / `dashboard_widgets_for_ui` 用)。ロックは
    /// クロージャの実行中だけ保持する。
    pub(crate) fn with_entries<T>(&self, f: impl FnOnce(&[E]) -> T) -> T {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&guard)
    }

    /// `pred` に最初にマッチしたエントリへ `f` を適用する
    /// (`find_manifest` / `is_disabled` / `set_values` / `set_capabilities` /
    /// `refresh_*_runtime` / `effective_hosts` / `*_buffer` / `entry_settings`
    /// / `manifest_of` 用)。ロックは `pred` と `f` の実行中だけ保持する
    /// (見つけて即クローンし、ディスク I/O やプロセス制御に入る前に手放す、
    /// という既存の規律をそのまま保つ)。
    pub(crate) fn find<T>(&self, pred: impl Fn(&E) -> bool, f: impl FnOnce(&E) -> T) -> Option<T> {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.iter().find(|entry| pred(entry)).map(f)
    }

    /// `pred` に最初にマッチしたエントリを可変で取り、`f` を適用する
    /// (`set_disabled` 用: 状態の書き換えと、書き換え後の値からの読み出しを
    /// 同じ臨界区間で行う)。
    pub(crate) fn find_mut<T>(
        &self,
        pred: impl Fn(&E) -> bool,
        f: impl FnOnce(&mut E) -> T,
    ) -> Option<T> {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.iter_mut().find(|entry| pred(entry)).map(f)
    }
}

impl<E> Clone for EntryTable<E> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
        }
    }
}

/// id ごとの `Arc<Mutex<()>>` を引ける/無ければ作れるマップ
/// (`sidecar_runtime_locks` / `filesystem_runtime_locks` / `bus_runtime_locks`
/// の `lock_for` パターン)。
///
/// マップ自体を保護する `Mutex` は、id からロックの `Arc` を引く/挿入する
/// 間だけ保持し、返した `Arc<Mutex<()>>` の実際の臨界区間(呼び出し側が別途
/// `.lock()` する)の間は保持しない。
pub(crate) struct IdLocks {
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl IdLocks {
    pub(crate) fn new() -> Self {
        Self {
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// `id` 用のロックを引く。無ければ作る。
    pub(crate) fn lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

impl Clone for IdLocks {
    fn clone(&self) -> Self {
        Self {
            locks: self.locks.clone(),
        }
    }
}

//! settings 群(`values` / `set_values`)の共通内部。
//!
//! Phase 4 タスク8: plugin(`crate::plugin::registry::Registry::values`/
//! `set_values`)と driver(`crate::driver::registry::DriverRegistry::values`/
//! `set_values`)は、`entries` から manifest/`settings_json` を引き、
//! `settings::Storage::effective`/`update_and_effective` を呼んでから
//! `settings_json` バッファを書き換える、という手順が完全に同一だった
//! (分析 `docs/superpowers/specs/2026-07-31-phase4-registry-analysis.md` §3)。
//! その手順だけをここへ抽出する。
//!
//! **実分岐は wrapper 側に残す**: plugin は返ってきた effective 値から
//! `crate::settings::store::split_secrets` で秘密情報を剥がしてから RPC
//! 応答に載せるが、driver は剥がさない(Task 1 の pin
//! `pin_drivers_set_settings_does_not_strip_secret_value` が防衛)。
//! そのため `effective`/`update_and_effective` は `(E::Subject, values)` を
//! 返し、secret 剥がしに必要な manifest を呼び出し側(plugin wrapper)へ
//! 渡す(driver wrapper は manifest 側を無視する)。エラー enum の変換
//! (`RegistryError` → `DriverRegistryError`)も同様に呼び出し側の責務。
//!
//! `St: settings::Storage`(Phase 0 trait)の初の consumer。

use std::sync::{Arc, Mutex};

use crate::plugin::registry::RegistryError;
use crate::registry::entries::EntryTable;
use crate::registry::subject::RegistrySubject;
use crate::settings;
use crate::settings::store::SettingsStore;

/// `EntryTable<E>` の要素 `E` が settings 群に対して持つべき最小限の面。
/// `registry::sidecar::SidecarEntry` と同じパターン(plugin/driver どちらの
/// エントリ型からもフィールド名を晒さずに引く)。
pub(crate) trait SettingsEntry {
    type Subject: RegistrySubject;

    fn manifest(&self) -> &Self::Subject;
    fn settings_json(&self) -> &Arc<Mutex<String>>;
}

impl SettingsEntry for crate::plugin::registry::PluginEntry {
    type Subject = crate::plugin::Manifest;

    fn manifest(&self) -> &crate::plugin::Manifest {
        &self.manifest
    }

    fn settings_json(&self) -> &Arc<Mutex<String>> {
        &self.settings_json
    }
}

impl SettingsEntry for crate::driver::registry::DriverEntry {
    type Subject = crate::driver::manifest::DriverManifest;

    fn manifest(&self) -> &crate::driver::manifest::DriverManifest {
        &self.manifest
    }

    fn settings_json(&self) -> &Arc<Mutex<String>> {
        &self.settings_json
    }
}

/// settings 群(`values` / `set_values`)を束ねるサービス。
///
/// `St: settings::Storage` はディスク実装(`SettingsStore`)を挿すための
/// ジェネリクス。`E: SettingsEntry` は plugin/driver どちらの `EntryTable`
/// 要素も受け付けるためのジェネリクス。
pub(crate) struct SettingsService<St: settings::Storage, E: SettingsEntry> {
    entries: EntryTable<E>,
    settings_store: Arc<St>,
}

/// 手書き `Clone`: `derive(Clone)` は `St`/`E` 自体に `Clone` を要求して
/// しまうが、実際に clone が要るのは `Arc`/`EntryTable`(いずれも中身の型に
/// 関わらず `Clone`)だけなので、要らない境界を足さないよう手で書く
/// (`SidecarService`/`FilesystemService` と同じ理由)。
impl<St: settings::Storage, E: SettingsEntry> Clone for SettingsService<St, E> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            settings_store: self.settings_store.clone(),
        }
    }
}

/// ディスク実装(`SettingsStore`)を挿した公開面。plugin/driver どちらの
/// `EntryTable` 要素かは呼び出し側が `E` で指定する。
pub(crate) type DiskSettingsService<E> = SettingsService<SettingsStore, E>;

impl<St: settings::Storage, E: SettingsEntry> SettingsService<St, E> {
    pub(crate) fn new(entries: EntryTable<E>, settings_store: Arc<St>) -> Self {
        Self {
            entries,
            settings_store,
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

    /// `id` の effective settings(秘密情報を含む生値)と、その manifest
    /// クローンを返す。secret 剥がしは呼び出し側(plugin wrapper)の責務
    /// (このサービスは剥がさない)。
    pub(crate) fn effective(
        &self,
        id: &str,
    ) -> Result<(E::Subject, serde_json::Map<String, serde_json::Value>), RegistryError> {
        let subject = self.find_manifest(id)?;
        let values = self.settings_store.effective(&subject.as_settings_manifest());
        Ok((subject, values))
    }

    /// `id` の settings を検証・永続化し、稼働中プラグイン/ドライバが参照する
    /// 共有 `settings_json` も新しい effective 値(秘密情報を含む生値)で
    /// 上書きする。`crate::plugin::registry::Registry::set_values` の
    /// ドキュメントコメントに書かれたロック規律(`entries` ロックは manifest
    /// と `settings_json` の共有ハンドルを取得する間だけ保持し、ファイル I/O
    /// はロックを解放した後に行う)をそのまま踏襲する。
    ///
    /// secret 剥がしは呼び出し側の責務(このサービスは剥がさない -- driver
    /// 側はそもそも剥がさない挙動そのものが正しい。Task 1 の pin
    /// `pin_drivers_set_settings_does_not_strip_secret_value` が防衛)。
    pub(crate) fn update_and_effective(
        &self,
        id: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(E::Subject, serde_json::Map<String, serde_json::Value>), RegistryError> {
        let (subject, settings_json) = self
            .entries
            .find(
                |entry| entry.manifest().id() == id,
                |entry| (entry.manifest().clone(), entry.settings_json().clone()),
            )
            .ok_or_else(|| E::Subject::unknown_error(id))?;

        let settings_manifest = subject.as_settings_manifest();
        let effective = self
            .settings_store
            .update_and_effective(&settings_manifest, values)
            .map_err(RegistryError::Settings)?;

        let settings_json_string =
            serde_json::to_string(&serde_json::Value::Object(effective.clone()))
                .unwrap_or_else(|_| "{}".to_string());
        *settings_json
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings_json_string;

        Ok((subject, effective))
    }
}

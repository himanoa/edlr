//! `catch-up = true` なスケジュールの「最終発火時刻」を永続化するストア。
//!
//! スケジュール状態はプラグイン起動のたびに新規構築されるため、それだけでは
//! デーモンが動いていなかった間に過ぎた定刻(`cron = "0 9 * * *"` の日次
//! レポートなど)が痕跡も無くスキップされる。ここに最終発火時刻を残しておく
//! ことで、次回起動時に「前回の発火より後に過ぎた定刻があるか」を判定できる。
//!
//! **`catch-up` を宣言したスケジュールの分しか書かない**。flush 系の
//! interval スケジュールは追い掛ける意味が無く、毎分ディスクに書く理由も
//! ないため(`ScheduleRequest::catch_up` のドキュメントコメント参照)。
//!
//! 保存先は `<settings-dir>/<plugin-id>.schedule.json` で、中身は
//! `{"<schedule-name>": "<RFC3339 のローカル時刻>"}`。壊れていたり読めなかったり
//! した場合は「記録なし」として扱う(panic しない) -- 最悪、追い掛け実行が
//! 1 回起きないか、余分に 1 回起きるだけ。

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Local};

/// プラグインごとの最終発火時刻を保存するストア。
///
/// `SettingsStore` と同じく内部に `Mutex<()>` を持ち、read-merge-write を
/// 直列化する(発火はプラグインスレッドから、読み出しは起動時から呼ばれる)。
pub struct ScheduleStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl ScheduleStore {
    pub fn new(dir: PathBuf) -> ScheduleStore {
        ScheduleStore {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn path_for(&self, plugin_id: &str) -> PathBuf {
        self.dir.join(format!("{plugin_id}.schedule.json"))
    }

    /// `plugin_id` の記録済み最終発火時刻を全件返す。ファイルが無い・壊れて
    /// いる場合は空(= 記録なし)。
    pub fn last_fires(&self, plugin_id: &str) -> BTreeMap<String, DateTime<Local>> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.last_fires_locked(plugin_id)
    }

    fn last_fires_locked(&self, plugin_id: &str) -> BTreeMap<String, DateTime<Local>> {
        let Ok(content) = fs::read_to_string(self.path_for(plugin_id)) else {
            return BTreeMap::new();
        };
        let Ok(raw) = serde_json::from_str::<BTreeMap<String, String>>(&content) else {
            return BTreeMap::new();
        };
        raw.into_iter()
            .filter_map(|(name, stamp)| {
                DateTime::parse_from_rfc3339(&stamp)
                    .ok()
                    .map(|parsed| (name, parsed.with_timezone(&Local)))
            })
            .collect()
    }

    /// `name` の最終発火時刻を `at` に更新する。
    ///
    /// 書き込みに失敗しても呼び出し元を失敗させない(warn ログのみ): 記録が
    /// 残らないと次回起動時に追い掛け実行が 1 回余分に起きうるだけで、
    /// 発火そのものを止める理由にはならない。
    pub fn record_fire(&self, plugin_id: &str, name: &str, at: DateTime<Local>) {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let mut current: BTreeMap<String, String> = self
            .last_fires_locked(plugin_id)
            .into_iter()
            .map(|(name, at)| (name, at.to_rfc3339()))
            .collect();
        current.insert(name.to_string(), at.to_rfc3339());

        if let Err(e) = self.write_locked(plugin_id, &current) {
            tracing::warn!(
                plugin_id = %plugin_id,
                schedule = %name,
                "failed to persist the last fire time: {e}"
            );
        }
    }

    fn write_locked(
        &self,
        plugin_id: &str,
        values: &BTreeMap<String, String>,
    ) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let serialized = serde_json::to_string_pretty(values)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // `SettingsStore::update` と同じく tmp + rename で、途中まで書かれた
        // ファイルを次回起動時に読んでしまわないようにする。
        let tmp_path = self.dir.join(format!(
            "{plugin_id}.schedule.json.tmp.{}",
            std::process::id()
        ));
        fs::write(&tmp_path, serialized)?;
        fs::rename(&tmp_path, self.path_for(plugin_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 7, 28, hour, 0, 0).unwrap()
    }

    #[test]
    fn no_file_means_no_recorded_fires() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ScheduleStore::new(tmp.path().join("settings"));
        assert!(store.last_fires("plugin").is_empty());
    }

    #[test]
    fn a_recorded_fire_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ScheduleStore::new(tmp.path().join("settings"));

        store.record_fire("plugin", "daily", at(9));

        let fires = store.last_fires("plugin");
        assert_eq!(fires.get("daily"), Some(&at(9)));
    }

    #[test]
    fn recording_one_schedule_keeps_the_others() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ScheduleStore::new(tmp.path().join("settings"));

        store.record_fire("plugin", "daily", at(9));
        store.record_fire("plugin", "weekly", at(10));
        store.record_fire("plugin", "daily", at(11));

        let fires = store.last_fires("plugin");
        assert_eq!(fires.get("daily"), Some(&at(11)));
        assert_eq!(fires.get("weekly"), Some(&at(10)));
    }

    #[test]
    fn plugins_do_not_share_records() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ScheduleStore::new(tmp.path().join("settings"));

        store.record_fire("a", "daily", at(9));

        assert!(store.last_fires("b").is_empty());
    }

    /// 壊れたファイルは「記録なし」として扱う。最悪でも追い掛け実行が
    /// 1 回余分に起きるだけで、起動を妨げてはならない。
    #[test]
    fn broken_json_is_treated_as_no_record() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("plugin.schedule.json"), "{ not json").unwrap();

        let store = ScheduleStore::new(dir);
        assert!(store.last_fires("plugin").is_empty());
    }

    /// パースできないタイムスタンプのエントリだけを落とし、他は活かす。
    #[test]
    fn unparsable_timestamps_are_skipped_individually() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("settings");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("plugin.schedule.json"),
            serde_json::json!({"daily": "not-a-time", "weekly": at(10).to_rfc3339()}).to_string(),
        )
        .unwrap();

        let store = ScheduleStore::new(dir);
        let fires = store.last_fires("plugin");
        assert!(!fires.contains_key("daily"));
        assert_eq!(fires.get("weekly"), Some(&at(10)));
    }
}

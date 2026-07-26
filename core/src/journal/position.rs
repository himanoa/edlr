//! Journal の読み取り位置の永続化。
//!
//! デーモンを再起動したときに現行 Journal を先頭から読み直さないためのもの。
//! 保存先は `<state-dir>/journal-position.json` で、**Journal ディレクトリを
//! キーにしたマップ**。Settings でディレクトリを切り替えても、別ディレクトリの
//! 位置を誤って適用しないようにするため。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// あるディレクトリで「どのファイルをどこまで読んだか」。
///
/// `offset` は**最後の完全な行の直後**を指す(読み込み途中の不完全な行を
/// 含む位置を保存すると、再起動時にその行が頭を欠いた状態で読まれる)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Position {
    /// Journal ディレクトリ内のファイル名(ディレクトリはキー側が持つ)。
    pub file: String,
    pub offset: u64,
}

/// `<state-dir>/journal-position.json` を読み書きするストア。
///
/// `SettingsStore` などと同じく内部に `Mutex<()>` を持ち、read-merge-write を
/// 直列化する。書き込みは tmp + `rename` で原子的に行う。
pub struct PositionStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl PositionStore {
    pub fn new(dir: PathBuf) -> Self {
        PositionStore {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("journal-position.json")
    }

    fn read_all(&self) -> BTreeMap<String, Position> {
        fs::read_to_string(self.path())
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// 保存済みの位置を返す。ファイルが無い・壊れている・その
    /// ディレクトリの記録が無い場合はいずれも `None`(panic しない)。
    pub fn load(&self, journal_dir: &Path) -> Option<Position> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.read_all()
            .get(&journal_dir.to_string_lossy().to_string())
            .cloned()
    }

    /// 位置を保存する。他ディレクトリの記録は保持する。
    pub fn save(&self, journal_dir: &Path, position: &Position) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let mut all = self.read_all();
        all.insert(journal_dir.to_string_lossy().to_string(), position.clone());

        fs::create_dir_all(&self.dir)?;
        let serialized = serde_json::to_string_pretty(&all)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = self.dir.join(format!(
            "journal-position.json.tmp.{}",
            std::process::id()
        ));
        fs::write(&tmp, serialized)?;
        fs::rename(&tmp, self.path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn round_trips_a_position_per_journal_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PositionStore::new(tmp.path().join("state"));
        let dir_a = Path::new("/journals/a");
        let dir_b = Path::new("/journals/b");

        assert_eq!(store.load(dir_a), None);

        store
            .save(
                dir_a,
                &Position { file: "Journal.a.log".into(), offset: 42 },
            )
            .expect("save should succeed");
        store
            .save(
                dir_b,
                &Position { file: "Journal.b.log".into(), offset: 7 },
            )
            .expect("save should succeed");

        assert_eq!(
            store.load(dir_a),
            Some(Position { file: "Journal.a.log".into(), offset: 42 })
        );
        assert_eq!(
            store.load(dir_b),
            Some(Position { file: "Journal.b.log".into(), offset: 7 })
        );
    }

    #[test]
    fn saving_the_same_directory_twice_overwrites_rather_than_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PositionStore::new(tmp.path().join("state"));
        let dir = Path::new("/journals/a");

        store.save(dir, &Position { file: "Journal.a.log".into(), offset: 1 }).unwrap();
        store.save(dir, &Position { file: "Journal.a.log".into(), offset: 99 }).unwrap();

        assert_eq!(store.load(dir).unwrap().offset, 99);
    }

    #[test]
    fn a_broken_file_reads_as_no_position() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("journal-position.json"), "not json {{{").unwrap();

        let store = PositionStore::new(dir);
        assert_eq!(store.load(Path::new("/journals/a")), None);
    }

    #[test]
    fn saving_creates_the_state_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("state");
        let store = PositionStore::new(dir.clone());

        store
            .save(Path::new("/journals/a"), &Position { file: "j.log".into(), offset: 1 })
            .expect("save should create the directory");

        assert!(dir.join("journal-position.json").is_file());
    }

    #[test]
    fn saving_into_an_unwritable_location_returns_an_error_instead_of_panicking() {
        // 既存のファイルをディレクトリとして使わせることで、確実に失敗させる。
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();

        let store = PositionStore::new(file.join("state"));
        assert!(store
            .save(Path::new("/journals/a"), &Position { file: "j.log".into(), offset: 1 })
            .is_err());
    }
}

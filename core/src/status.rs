use serde_json::Value;
use std::path::PathBuf;

/// Status.json を読む。同一内容は重複配信せず、不完全な書き込みは次回リトライ。
pub struct StatusReader {
    path: PathBuf,
    last: Option<String>,
}

impl StatusReader {
    pub fn new(path: PathBuf) -> Self {
        Self { path, last: None }
    }

    pub fn poll(&mut self) -> Option<Value> {
        let content = std::fs::read_to_string(&self.path).ok()?;
        if content.trim().is_empty() || self.last.as_deref() == Some(content.as_str()) {
            return None;
        }
        let raw: Value = serde_json::from_str(&content).ok()?;
        self.last = Some(content);
        Some(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_only_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Status.json");
        std::fs::write(&path, r#"{"Flags":1}"#).unwrap();
        let mut r = StatusReader::new(path.clone());
        assert_eq!(r.poll().unwrap()["Flags"], 1);
        assert_eq!(r.poll(), None); // 同一内容 → 配信しない
        std::fs::write(&path, r#"{"Flags":2}"#).unwrap();
        assert_eq!(r.poll().unwrap()["Flags"], 2);
    }

    #[test]
    fn tolerates_missing_empty_and_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Status.json");
        let mut r = StatusReader::new(path.clone());
        assert_eq!(r.poll(), None); // 不在
        std::fs::write(&path, "").unwrap();
        assert_eq!(r.poll(), None); // 空
        std::fs::write(&path, r#"{"Flags"#).unwrap();
        assert_eq!(r.poll(), None); // 書き込み途中
        std::fs::write(&path, r#"{"Flags":3}"#).unwrap();
        assert_eq!(r.poll().unwrap()["Flags"], 3); // 完成後に配信
    }
}

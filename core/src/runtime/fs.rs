//! `HostCtx` と `Registry` が共有する `filesystem_json` バッファの組み立てと解釈。
//!
//! `crate::runtime::sidecar` と同じ流儀で、承認状態と実行に必要な値を 1 本の JSON
//! 文字列に載せる。プラグイン側からは参照も改変もできず、`Registry` が
//! 承認・設定変更のたびに上書きすることで、稼働中のプラグインへ再起動なしに
//! 反映される。
//!
//! **未承認のエントリには `path` を載せない**。承認前はアクセス先の情報
//! そのものがバッファに存在しないため、仮に将来 `granted` を見ずに読む
//! 実装が生えても、未承認のルートへアクセスできない。

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FsRuntimeEntry {
    pub name: String,
    pub granted: bool,
    pub mode: String,
    #[serde(default)]
    pub path: String,
}

/// エントリ一覧を `filesystem_json` バッファ用の JSON 文字列へ直列化する。
///
/// **未承認のエントリは `path` を落とす**。`mode` は承認画面に出る情報で
/// 秘密ではないため、承認状態に関わらず載せる。
pub fn filesystem_json_string(entries: &[FsRuntimeEntry]) -> String {
    let redacted: Vec<FsRuntimeEntry> = entries
        .iter()
        .map(|entry| {
            if entry.granted {
                entry.clone()
            } else {
                FsRuntimeEntry {
                    name: entry.name.clone(),
                    granted: false,
                    mode: entry.mode.clone(),
                    path: String::new(),
                }
            }
        })
        .collect();
    serde_json::to_string(&redacted).unwrap_or_else(|_| "[]".to_string())
}

/// `filesystem_json` バッファを解釈し、ルート名をキーにしたマップへ戻す。
/// 不正な JSON は「ルートなし」に安全側でフォールバックする。
pub fn parse_filesystem(raw: &str) -> BTreeMap<String, FsRuntimeEntry> {
    let entries: Vec<FsRuntimeEntry> = serde_json::from_str(raw).unwrap_or_default();
    entries
        .into_iter()
        .map(|entry| (entry.name.clone(), entry))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(granted: bool) -> FsRuntimeEntry {
        FsRuntimeEntry {
            name: "exports".into(),
            granted,
            mode: "read-write".into(),
            path: "/home/u/exports".into(),
        }
    }

    #[test]
    fn ungranted_entries_carry_no_path() {
        let parsed = parse_filesystem(&filesystem_json_string(&[entry(false)]));
        let root = parsed.get("exports").expect("entry survives serialization");
        assert!(!root.granted);
        assert_eq!(root.path, "");
    }

    #[test]
    fn granted_entries_round_trip() {
        let parsed = parse_filesystem(&filesystem_json_string(&[entry(true)]));
        let root = parsed.get("exports").unwrap();
        assert!(root.granted);
        assert_eq!(root.path, "/home/u/exports");
        assert_eq!(root.mode, "read-write");
    }

    #[test]
    fn broken_json_parses_as_no_roots() {
        assert!(parse_filesystem("not json {{{").is_empty());
    }
}

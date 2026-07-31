//! `HostCtx` と `Registry` が共有する `bus_json` バッファの組み立てと解釈。
//!
//! `crate::runtime::fs` と同じ流儀で、承認状態と宣言済みトピックを 1 本の JSON
//! 文字列に載せる。プラグイン側からは参照も改変もできず、`Registry` が
//! 承認・設定変更のたびに上書きすることで、稼働中のプラグインへ再起動なしに
//! 反映される。
//!
//! **未承認のエントリには `publish` / `subscribe` を載せない**。承認前は
//! どのトピックに触れるかという情報そのものがバッファに存在しないため、
//! 仮に将来 `granted` を見ずに読む実装が生えても、未承認のドライバへの
//! トピック情報を取得できない。

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct BusRuntimeEntry {
    pub driver: String,
    pub granted: bool,
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub subscribe: Vec<String>,
}

/// エントリ一覧を `bus_json` バッファ用の JSON 文字列へ直列化する。
///
/// **未承認のエントリは `publish` / `subscribe` を落とす**。
pub fn bus_json_string(entries: &[BusRuntimeEntry]) -> String {
    let redacted: Vec<BusRuntimeEntry> = entries
        .iter()
        .map(|entry| {
            if entry.granted {
                entry.clone()
            } else {
                BusRuntimeEntry {
                    driver: entry.driver.clone(),
                    granted: false,
                    publish: Vec::new(),
                    subscribe: Vec::new(),
                }
            }
        })
        .collect();
    serde_json::to_string(&redacted).unwrap_or_else(|_| "[]".to_string())
}

/// `bus_json` バッファを解釈し、ドライバ名をキーにしたマップへ戻す。
/// 不正な JSON は「エントリなし」に安全側でフォールバックする。
pub fn parse_bus(raw: &str) -> BTreeMap<String, BusRuntimeEntry> {
    let entries: Vec<BusRuntimeEntry> = serde_json::from_str(raw).unwrap_or_default();
    entries
        .into_iter()
        .map(|entry| (entry.driver.clone(), entry))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(granted: bool) -> BusRuntimeEntry {
        BusRuntimeEntry {
            driver: "ed-state".into(),
            granted,
            publish: vec!["ship-status".into()],
            subscribe: vec!["current-system".into()],
        }
    }

    #[test]
    fn ungranted_entries_carry_no_topics() {
        let parsed = parse_bus(&bus_json_string(&[entry(false)]));
        let e = parsed
            .get("ed-state")
            .expect("entry survives serialization");
        assert!(!e.granted);
        assert!(e.publish.is_empty());
        assert!(e.subscribe.is_empty());
    }

    #[test]
    fn granted_entries_round_trip() {
        let parsed = parse_bus(&bus_json_string(&[entry(true)]));
        let e = parsed.get("ed-state").unwrap();
        assert!(e.granted);
        assert_eq!(e.publish, vec!["ship-status".to_string()]);
    }

    #[test]
    fn broken_json_parses_as_no_entries() {
        assert!(parse_bus("not json {{{").is_empty());
    }
}

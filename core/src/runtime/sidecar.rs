//! `HostCtx` と `Registry` が共有する `sidecars_json` バッファの組み立てと解釈。
//!
//! `capabilities_json` と同じ流儀で、承認状態と実行に必要な値を 1 本の JSON
//! 文字列に載せる。プラグイン側からは参照も改変もできず、`Registry` が
//! 承認・設定変更のたびに上書きすることで、稼働中のプラグインへ再起動なしに
//! 反映される。
//!
//! **未承認のエントリには `command` も `ports` も載せない**。承認前は
//! 起動に必要な情報そのものがバッファに存在しないため、仮に将来 `granted`
//! を見ずに読む実装が生えても、未承認のサイドカーを起動できない。

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SidecarRuntimeEntry {
    pub name: String,
    pub granted: bool,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub ports: Vec<u16>,
}

pub fn sidecars_json_string(entries: &[SidecarRuntimeEntry]) -> String {
    let redacted: Vec<SidecarRuntimeEntry> = entries
        .iter()
        .map(|entry| {
            if entry.granted {
                entry.clone()
            } else {
                SidecarRuntimeEntry {
                    name: entry.name.clone(),
                    granted: false,
                    command: String::new(),
                    args: Vec::new(),
                    ports: Vec::new(),
                }
            }
        })
        .collect();
    serde_json::to_string(&redacted).unwrap_or_else(|_| "[]".to_string())
}

pub fn parse_sidecars(raw: &str) -> BTreeMap<String, SidecarRuntimeEntry> {
    let entries: Vec<SidecarRuntimeEntry> = serde_json::from_str(raw).unwrap_or_default();
    entries
        .into_iter()
        .map(|entry| (entry.name.clone(), entry))
        .collect()
}

/// 承認済みサイドカーの採番ポートに対する暗黙の HTTP 許可 origin 一覧。
pub fn implicit_http_hosts(entries: &[SidecarRuntimeEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| entry.granted)
        .flat_map(|entry| {
            entry
                .ports
                .iter()
                .map(|port| format!("http://127.0.0.1:{port}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, granted: bool, ports: Vec<u16>) -> SidecarRuntimeEntry {
        SidecarRuntimeEntry {
            name: name.into(),
            granted,
            command: "/usr/bin/piper".into(),
            args: vec!["--port".into(), "{port}".into()],
            ports,
        }
    }

    #[test]
    fn ungranted_entries_carry_no_command_or_ports() {
        let json = sidecars_json_string(&[entry("tts", false, vec![50021])]);
        let parsed = parse_sidecars(&json);
        let tts = parsed.get("tts").expect("tts entry survives serialization");
        assert!(!tts.granted);
        assert_eq!(tts.command, "");
        assert!(tts.ports.is_empty());
    }

    #[test]
    fn granted_entries_round_trip() {
        let json = sidecars_json_string(&[entry("tts", true, vec![50021, 50022])]);
        let parsed = parse_sidecars(&json);
        let tts = parsed.get("tts").unwrap();
        assert!(tts.granted);
        assert_eq!(tts.command, "/usr/bin/piper");
        assert_eq!(tts.ports, vec![50021, 50022]);
    }

    #[test]
    fn implicit_hosts_cover_granted_ports_only() {
        let hosts = implicit_http_hosts(&[
            entry("tts", true, vec![50021, 50022]),
            entry("tr", false, vec![50030]),
        ]);
        assert_eq!(
            hosts,
            vec![
                "http://127.0.0.1:50021".to_string(),
                "http://127.0.0.1:50022".to_string(),
            ]
        );
    }

    #[test]
    fn broken_json_parses_as_no_sidecars() {
        assert!(parse_sidecars("not json {{{").is_empty());
    }
}

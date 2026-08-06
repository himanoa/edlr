//! `HostCtx`/`DriverCtx` の許可判定(サイドカー/ファイルシステムルート/バス)を
//! 値イン値アウトの純関数へ抽出したもの。エラー文字列は**ここで**一元的に
//! 組み立てて返し、各 ctx(`crate::host::plugin`/`crate::host::driver`)は
//! 自分の world の WIT variant へ写像するだけにする -- 同じ判定ロジックが
//! plugin/driver 双方に複製されていた際、エラー文字列が独立にドリフトしうる
//! 状態を避けるため。
//!
//! `spawn_bus_subscriber`(`crate::runner::plugin`)のドキュメントコメントが
//! 明記する通り、`check_bus_permission` は `HostCtx::check_bus` と
//! **同じ判定材料・同じ判定規則**を使う。

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::runtime::bus::BusRuntimeEntry;
use crate::runtime::fs::FsRuntimeEntry;
use crate::runtime::sidecar::SidecarRuntimeEntry;

/// `resolve_sidecar` の失敗理由。variant ごとにエラー文字列を持つ
/// (WIT の `driver-process.driver-error` へ 1:1 で写像される)。
#[derive(Debug)]
pub(crate) enum SidecarResolveError {
    Unknown(String),
    NotGranted(String),
    NotConfigured(String),
}

/// `sidecars_json` から解いたエントリ一覧(`entries`)から、当該サイドカー
/// (`name`)の実行仕様を解決する。
///
/// 判定順は「manifest に存在するか」→「承認済みか」→「設定済みか」。
pub(crate) fn resolve_sidecar(
    entries: &BTreeMap<String, SidecarRuntimeEntry>,
    name: &str,
) -> Result<edlr_driver_process::ProcessSpec, SidecarResolveError> {
    let Some(entry) = entries.get(name) else {
        return Err(SidecarResolveError::Unknown(format!(
            "no such sidecar: {name}"
        )));
    };
    if !entry.granted {
        return Err(SidecarResolveError::NotGranted(format!(
            "sidecar not granted: {name}"
        )));
    }
    if entry.command.is_empty() {
        return Err(SidecarResolveError::NotConfigured(format!(
            "sidecar {name} has no executable configured"
        )));
    }

    Ok(edlr_driver_process::ProcessSpec {
        command: PathBuf::from(&entry.command),
        args: entry.args.clone(),
        ports: entry.ports.clone(),
    })
}

/// `resolve_root` の失敗理由。variant ごとにエラー文字列を持つ
/// (WIT の `driver-fs.driver-error` へ 1:1 で写像される)。
#[derive(Debug)]
pub(crate) enum RootResolveError {
    Unknown(String),
    NotGranted(String),
    NotConfigured(String),
    ReadOnly(String),
}

/// `resolve_root` の解決結果。`is_file` は当該ルートが `target = "file"`
/// (単一ファイル承認)かどうか。
#[derive(Debug)]
pub(crate) struct ResolvedRoot {
    pub path: PathBuf,
    pub is_file: bool,
}

/// `filesystem_json` から解いたエントリ一覧(`entries`)から、当該ルート
/// (`root`)の実パスを解決する。
///
/// 判定順は `resolve_sidecar` と揃える:「存在するか」→「承認済みか」→
/// 「設定済みか」→(書き込み系なら)「mode が read-write か」。
pub(crate) fn resolve_root(
    entries: &BTreeMap<String, FsRuntimeEntry>,
    root: &str,
    need_write: bool,
) -> Result<ResolvedRoot, RootResolveError> {
    let Some(entry) = entries.get(root) else {
        return Err(RootResolveError::Unknown(format!("no such root: {root}")));
    };
    if !entry.granted {
        return Err(RootResolveError::NotGranted(format!(
            "filesystem root not granted: {root}"
        )));
    }
    if entry.path.is_empty() {
        return Err(RootResolveError::NotConfigured(format!(
            "root {root} has no directory configured"
        )));
    }
    if need_write && entry.mode != "read-write" {
        return Err(RootResolveError::ReadOnly(format!(
            "root {root} is read-only"
        )));
    }
    Ok(ResolvedRoot {
        path: PathBuf::from(&entry.path),
        is_file: entry.target == "file",
    })
}

/// `FsDriver` に渡す `(root_dir, rel)` を組み立てる。
///
/// directory ルートはゲストのパスをそのまま相対パスとして通す。file ルート
/// はゲストのパスを `""` に限定し、承認されたファイルパスを「親ディレクトリ
/// + ファイル名」に分解して返す -- `FsDriver` のパス検証・サイズ上限・
/// 原子的書き込みをそのまま流用するため(ドライバ側に file 分岐を持たない)。
pub(crate) fn effective_fs_target(
    resolved: &ResolvedRoot,
    guest_path: &str,
) -> Result<(PathBuf, String), String> {
    if !resolved.is_file {
        return Ok((resolved.path.clone(), guest_path.to_string()));
    }
    if !guest_path.is_empty() {
        return Err("path must be empty for a file root".to_string());
    }
    let parent = resolved.path.parent();
    let name = resolved.path.file_name().and_then(|n| n.to_str());
    match (parent, name) {
        (Some(parent), Some(name)) => Ok((parent.to_path_buf(), name.to_string())),
        _ => Err(format!(
            "configured file path has no parent directory: {}",
            resolved.path.display()
        )),
    }
}

/// `capabilities_json` から解いた有効ホスト一覧(`hosts`)を使って
/// `driver-http.send` の許可を判定する。空なら「承認なし」として拒否し、
/// そうでなければ `allowlist::check_url` に委ねる。
pub(crate) fn check_http_permission(hosts: &[String], url: &str) -> Result<(), String> {
    if hosts.is_empty() {
        return Err("capability not granted".to_string());
    }
    crate::host::allowlist::check_url(hosts, url)
}

/// バス操作の向き。`publish`(送信)と `subscribe`(受信/`get`)で参照する
/// トピック一覧が異なる。
pub(crate) enum BusDirection {
    Publish,
    Subscribe,
}

/// `bus_json` から解いたエントリ一覧(`entries`)を使って、`driver`/`topic`
/// への `direction` 方向の操作が許可されているかを判定する。
///
/// `spawn_bus_subscriber` のドキュメントコメントが明記する通り、これは
/// `HostCtx::check_bus`(かつては `BusHost::publish`/`get` の中にあった)と
/// **同じ判定材料・同じ判定規則**を使う: 承認済み(`granted`)かつ、
/// 当該方向のトピック一覧に含まれていること。
pub(crate) fn check_bus_permission(
    entries: &BTreeMap<String, BusRuntimeEntry>,
    driver: &str,
    topic: &str,
    direction: BusDirection,
) -> Result<(), String> {
    let entry = entries
        .get(driver)
        .filter(|e| e.granted)
        .ok_or_else(|| format!("bus access to {driver} is not granted"))?;
    let topics = match direction {
        BusDirection::Publish => &entry.publish,
        BusDirection::Subscribe => &entry.subscribe,
    };
    if !topics.iter().any(|t| t == topic) {
        return Err(format!(
            "{driver}/{topic} is not in this plugin's granted bus topics"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidecar_entries(entries: Vec<SidecarRuntimeEntry>) -> BTreeMap<String, SidecarRuntimeEntry> {
        entries.into_iter().map(|e| (e.name.clone(), e)).collect()
    }

    fn sidecar_entry(granted: bool, command: &str) -> SidecarRuntimeEntry {
        SidecarRuntimeEntry {
            name: "tts".to_string(),
            granted,
            command: command.to_string(),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            ports: vec![50201],
        }
    }

    #[test]
    fn resolve_sidecar_granted_and_configured_returns_spec() {
        let entries = sidecar_entries(vec![sidecar_entry(true, "/bin/sh")]);
        let spec = resolve_sidecar(&entries, "tts").expect("granted and configured");
        assert_eq!(spec.command, PathBuf::from("/bin/sh"));
        assert_eq!(spec.ports, vec![50201]);
    }

    #[test]
    fn resolve_sidecar_unknown_name_is_unknown() {
        let entries = sidecar_entries(vec![sidecar_entry(true, "/bin/sh")]);
        let err = resolve_sidecar(&entries, "nope").expect_err("unknown name");
        let SidecarResolveError::Unknown(msg) = err else {
            panic!("expected Unknown, got a different variant");
        };
        assert_eq!(msg, "no such sidecar: nope");
    }

    #[test]
    fn resolve_sidecar_ungranted_is_not_granted() {
        let entries = sidecar_entries(vec![sidecar_entry(false, "/bin/sh")]);
        let err = resolve_sidecar(&entries, "tts").expect_err("ungranted");
        let SidecarResolveError::NotGranted(msg) = err else {
            panic!("expected NotGranted, got a different variant");
        };
        assert_eq!(msg, "sidecar not granted: tts");
    }

    #[test]
    fn resolve_sidecar_granted_but_unconfigured_is_not_configured() {
        let entries = sidecar_entries(vec![sidecar_entry(true, "")]);
        let err = resolve_sidecar(&entries, "tts").expect_err("unconfigured");
        let SidecarResolveError::NotConfigured(msg) = err else {
            panic!("expected NotConfigured, got a different variant");
        };
        assert_eq!(msg, "sidecar tts has no executable configured");
    }

    fn fs_entries(entries: Vec<FsRuntimeEntry>) -> BTreeMap<String, FsRuntimeEntry> {
        entries.into_iter().map(|e| (e.name.clone(), e)).collect()
    }

    fn fs_entry(granted: bool, mode: &str, path: &str) -> FsRuntimeEntry {
        FsRuntimeEntry {
            name: "exports".to_string(),
            granted,
            mode: mode.to_string(),
            path: path.to_string(),
            target: "directory".to_string(),
        }
    }

    fn fs_file_entry(granted: bool, mode: &str, path: &str) -> FsRuntimeEntry {
        FsRuntimeEntry {
            name: "status".to_string(),
            granted,
            mode: mode.to_string(),
            path: path.to_string(),
            target: "file".to_string(),
        }
    }

    #[test]
    fn resolve_root_granted_and_configured_returns_path() {
        let entries = fs_entries(vec![fs_entry(true, "read-write", "/tmp/exports")]);
        let resolved = resolve_root(&entries, "exports", false).expect("granted and configured");
        assert_eq!(resolved.path, PathBuf::from("/tmp/exports"));
    }

    #[test]
    fn resolve_root_unknown_root_is_unknown() {
        let entries = fs_entries(vec![fs_entry(true, "read-write", "/tmp/exports")]);
        let err = resolve_root(&entries, "nope", false).expect_err("unknown root");
        let RootResolveError::Unknown(msg) = err else {
            panic!("expected Unknown, got a different variant");
        };
        assert_eq!(msg, "no such root: nope");
    }

    #[test]
    fn resolve_root_ungranted_is_not_granted() {
        let entries = fs_entries(vec![fs_entry(false, "read-write", "/tmp/exports")]);
        let err = resolve_root(&entries, "exports", false).expect_err("ungranted");
        let RootResolveError::NotGranted(msg) = err else {
            panic!("expected NotGranted, got a different variant");
        };
        assert_eq!(msg, "filesystem root not granted: exports");
    }

    #[test]
    fn resolve_root_granted_but_unconfigured_is_not_configured() {
        let entries = fs_entries(vec![fs_entry(true, "read-write", "")]);
        let err = resolve_root(&entries, "exports", false).expect_err("unconfigured");
        let RootResolveError::NotConfigured(msg) = err else {
            panic!("expected NotConfigured, got a different variant");
        };
        assert_eq!(msg, "root exports has no directory configured");
    }

    #[test]
    fn resolve_root_write_under_read_mode_is_read_only() {
        let entries = fs_entries(vec![fs_entry(true, "read", "/tmp/exports")]);
        let err = resolve_root(&entries, "exports", true).expect_err("read mode, write requested");
        let RootResolveError::ReadOnly(msg) = err else {
            panic!("expected ReadOnly, got a different variant");
        };
        assert_eq!(msg, "root exports is read-only");
    }

    #[test]
    fn resolve_root_directory_entry_is_not_file() {
        let entries = fs_entries(vec![fs_entry(true, "read", "/tmp/exports")]);
        let resolved = resolve_root(&entries, "exports", false).unwrap();
        assert!(!resolved.is_file);
        assert_eq!(resolved.path, PathBuf::from("/tmp/exports"));
    }

    #[test]
    fn resolve_root_file_entry_is_file() {
        let entries = fs_entries(vec![fs_file_entry(true, "read", "/home/u/Status.json")]);
        let resolved = resolve_root(&entries, "status", false).unwrap();
        assert!(resolved.is_file);
    }

    #[test]
    fn effective_fs_target_directory_passes_guest_path_through() {
        let resolved = ResolvedRoot { path: PathBuf::from("/tmp/exports"), is_file: false };
        let (dir, rel) = effective_fs_target(&resolved, "sub/a.txt").unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/exports"));
        assert_eq!(rel, "sub/a.txt");
    }

    #[test]
    fn effective_fs_target_file_splits_into_parent_and_name() {
        let resolved = ResolvedRoot { path: PathBuf::from("/home/u/Status.json"), is_file: true };
        let (dir, rel) = effective_fs_target(&resolved, "").unwrap();
        assert_eq!(dir, PathBuf::from("/home/u"));
        assert_eq!(rel, "Status.json");
    }

    #[test]
    fn effective_fs_target_file_rejects_nonempty_guest_path() {
        let resolved = ResolvedRoot { path: PathBuf::from("/home/u/Status.json"), is_file: true };
        let err = effective_fs_target(&resolved, "other.json").expect_err("non-empty path on file root");
        assert_eq!(err, "path must be empty for a file root");
    }

    #[test]
    fn check_http_permission_with_no_effective_hosts_is_denied() {
        let err = check_http_permission(&[], "https://api.example.com/ping")
            .expect_err("no effective hosts means every call is denied");
        assert_eq!(err, "capability not granted");
    }

    #[test]
    fn check_http_permission_allowlisted_host_is_ok() {
        check_http_permission(
            &["https://api.example.com".to_string()],
            "https://api.example.com/ping",
        )
        .expect("allowlisted host is permitted");
    }

    #[test]
    fn check_http_permission_disallowed_host_is_denied() {
        let err = check_http_permission(
            &["https://api.example.com".to_string()],
            "https://evil.example.com/ping",
        )
        .expect_err("non-allowlisted host must be rejected");
        assert!(!err.is_empty());
    }

    fn bus_entries(entries: Vec<BusRuntimeEntry>) -> BTreeMap<String, BusRuntimeEntry> {
        entries.into_iter().map(|e| (e.driver.clone(), e)).collect()
    }

    fn bus_entry_granted() -> BusRuntimeEntry {
        BusRuntimeEntry {
            driver: "ed-state".to_string(),
            granted: true,
            publish: vec!["ship-status".to_string()],
            subscribe: vec!["current-system".to_string()],
        }
    }

    #[test]
    fn check_bus_permission_publish_declared_topic_is_ok() {
        let entries = bus_entries(vec![bus_entry_granted()]);
        check_bus_permission(&entries, "ed-state", "ship-status", BusDirection::Publish)
            .expect("declared publish topic is permitted");
    }

    #[test]
    fn check_bus_permission_ungranted_driver_is_denied() {
        let mut entry = bus_entry_granted();
        entry.granted = false;
        let entries = bus_entries(vec![entry]);
        let err = check_bus_permission(&entries, "ed-state", "ship-status", BusDirection::Publish)
            .expect_err("ungranted driver must be denied");
        assert_eq!(err, "bus access to ed-state is not granted");
    }

    #[test]
    fn check_bus_permission_undeclared_topic_is_denied() {
        let entries = bus_entries(vec![bus_entry_granted()]);
        let err = check_bus_permission(&entries, "ed-state", "secret", BusDirection::Publish)
            .expect_err("undeclared topic must be denied");
        assert_eq!(
            err,
            "ed-state/secret is not in this plugin's granted bus topics"
        );
    }

    #[test]
    fn check_bus_permission_subscribe_uses_the_subscribe_list() {
        let entries = bus_entries(vec![bus_entry_granted()]);
        // `publish` にしか宣言していないトピックは subscribe 方向では拒否
        // される(逆も同様) -- `HostCtx::check_bus` と同じ規則。
        let err =
            check_bus_permission(&entries, "ed-state", "ship-status", BusDirection::Subscribe)
                .expect_err("publish-only topic must be denied for subscribe");
        assert_eq!(
            err,
            "ed-state/ship-status is not in this plugin's granted bus topics"
        );
        check_bus_permission(
            &entries,
            "ed-state",
            "current-system",
            BusDirection::Subscribe,
        )
        .expect("declared subscribe topic is permitted");
    }
}

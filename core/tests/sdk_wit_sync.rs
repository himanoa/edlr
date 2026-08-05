//! `sdk/go/wit/` は `core/wit/` のコピー(Go module の zip はモジュール
//! ディレクトリしか含まないため、tinygo build 用に WIT を SDK へ同梱して
//! いる。docs/superpowers/specs/2026-08-05-guest-sdk-design.md 参照)。
//! ABI 変更時の cp 忘れをここで機械的に検出する。ずれていたら
//! `cp -r core/wit/. sdk/go/wit/` で追従すること。

use std::collections::BTreeMap;
use std::path::Path;

fn wit_files(dir: &Path) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    collect_wit_files(dir, dir, &mut result);
    result
}

fn collect_wit_files(root: &Path, dir: &Path, map: &mut BTreeMap<String, String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                // Recursively collect from subdirectories
                collect_wit_files(root, &path, map);
            } else if path.extension().is_some_and(|ext| ext == "wit") {
                // Collect relative path from root to maintain directory structure
                if let Ok(rel_path) = path.strip_prefix(root) {
                    let key = rel_path.to_string_lossy().into_owned();
                    let body = std::fs::read_to_string(&path).expect("read wit file");
                    map.insert(key, body);
                }
            }
        }
    }
}

#[test]
fn sdk_go_wit_matches_core_wit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let core = wit_files(&root.join("core/wit"));
    let sdk = wit_files(&root.join("sdk/go/wit"));
    assert_eq!(
        core, sdk,
        "sdk/go/wit must be an exact copy of core/wit; run: cp -r core/wit/. sdk/go/wit/"
    );
}

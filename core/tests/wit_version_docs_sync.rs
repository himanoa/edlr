//! README が宣言する plugin ABI バージョンが `core/wit/plugin.wit` の実体と
//! ずれていないことを確認する。ずれていると、README を見て作ったプラグインが
//! ロードに失敗する world をターゲットしてしまう。

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core/ の親がリポジトリルート")
        .to_path_buf()
}

/// `package edlr:plugin@X.Y.Z;` から `X.Y.Z` を取り出す。
fn wit_package_version() -> String {
    let wit = std::fs::read_to_string(repo_root().join("core/wit/plugin.wit"))
        .expect("core/wit/plugin.wit を読めること");
    let line = wit
        .lines()
        .find(|l| l.trim_start().starts_with("package edlr:plugin@"))
        .expect("plugin.wit に package 宣言があること");
    line.trim()
        .trim_start_matches("package edlr:plugin@")
        .trim_end_matches(';')
        .to_string()
}

#[test]
fn readme_plugin_abi_version_matches_wit() {
    let version = wit_package_version();
    let readme = std::fs::read_to_string(repo_root().join("README.md")).expect("README.md");

    let expected = format!("`edlr:plugin`, currently `@{version}`");
    assert!(
        readme.contains(&expected),
        "README.md の plugin ABI バージョン表記が core/wit/plugin.wit ({version}) とずれている。\
         README の Status セクションを {expected:?} に更新すること。"
    );
}

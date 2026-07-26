//! `Registry` のファイルアクセス配線(設定・承認・共有バッファ反映)の統合
//! テスト。`support::filesystem_env` で `[[filesystem]]` を 1 件持つ
//! `fs-plugin` を起動し、`Registry::set_filesystem_config` /
//! `Registry::set_filesystem_grant` の往復を実際のディスク I/O を通して
//! 検証する。

use edlr_core::plugin::FilesystemConfig;

mod support;

#[tokio::test]
async fn granting_requires_a_configured_directory() {
    let env = support::filesystem_env("exports", "read-write");

    let err = env
        .registry
        .set_filesystem_grant("fs-plugin", "exports", true)
        .expect_err("granting without a directory must be rejected");
    assert!(err.to_string().contains("directory"));

    let dir = env.tmp.path().join("exports");
    std::fs::create_dir(&dir).unwrap();
    env.registry
        .set_filesystem_config(
            "fs-plugin",
            "exports",
            &FilesystemConfig {
                path: dir.to_string_lossy().to_string(),
            },
        )
        .expect("config");
    let roots = env
        .registry
        .set_filesystem_grant("fs-plugin", "exports", true)
        .expect("grant after configuring");
    assert!(roots[0].grant.granted);
}

#[tokio::test]
async fn revoking_removes_the_path_from_the_shared_buffer() {
    let env = support::filesystem_env("exports", "read-write");
    let dir = env.tmp.path().join("exports");
    std::fs::create_dir(&dir).unwrap();
    env.registry
        .set_filesystem_config(
            "fs-plugin",
            "exports",
            &FilesystemConfig {
                path: dir.to_string_lossy().to_string(),
            },
        )
        .unwrap();
    env.registry
        .set_filesystem_grant("fs-plugin", "exports", true)
        .unwrap();
    assert!(support::filesystem_buffer(&env.registry, "fs-plugin")
        .contains(&dir.to_string_lossy().to_string()));

    env.registry
        .set_filesystem_grant("fs-plugin", "exports", false)
        .unwrap();
    assert!(
        !support::filesystem_buffer(&env.registry, "fs-plugin")
            .contains(&dir.to_string_lossy().to_string()),
        "a revoked root must not leave its path in the buffer plugins read"
    );
}

#[tokio::test]
async fn changing_the_directory_takes_effect_without_reapproval() {
    let env = support::filesystem_env("exports", "read-write");
    let first = env.tmp.path().join("one");
    let second = env.tmp.path().join("two");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    env.registry
        .set_filesystem_config(
            "fs-plugin",
            "exports",
            &FilesystemConfig {
                path: first.to_string_lossy().to_string(),
            },
        )
        .unwrap();
    env.registry
        .set_filesystem_grant("fs-plugin", "exports", true)
        .unwrap();

    let roots = env
        .registry
        .set_filesystem_config(
            "fs-plugin",
            "exports",
            &FilesystemConfig {
                path: second.to_string_lossy().to_string(),
            },
        )
        .expect("path change");

    assert!(
        roots[0].grant.granted,
        "changing the path must not revoke the grant"
    );
    assert!(support::filesystem_buffer(&env.registry, "fs-plugin")
        .contains(&second.to_string_lossy().to_string()));
}

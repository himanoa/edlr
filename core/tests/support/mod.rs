//! サイドカー統合テスト用の足場。`plugins-dir` に、実際にロード・init が
//! 成功する(= `PluginState::Running` のまま保たれる)プラグインを 1 件
//! 作り、`Registry` を組み立てる。
//!
//! 以前はここで壊れた wasm バイト列(`\0asm\x01\x00\x00\x00` のみ)を書いて
//! いた。プラグインは常にロードに失敗して `Disabled` になるが、サイドカーの
//! 設定・承認・制御は `Registry` の API で完結するため、それでも大半のテスト
//! には十分だった。しかし再レビューで `control_sidecar` が `PluginState` を
//! 見るようになった(無効化されたプラグインの `start`/`restart` を拒否する)
//! ため、`Disabled` なままだとサイドカーの起動系のテストが (意図せず) 常に
//! 失敗するようになった。そのため `examples/plugins/hello-logger` の実際に
//! ビルドした wasm を使い、ロード・init が成功して `Running` のまま保たれる
//! ようにしている(`hello-logger` 自体は `[[sidecar]]` を一切知らず、
//! サイドカーはあくまで manifest の宣言と `Registry` 側の制御だけで完結する
//! ので、`entry` に流用しても問題ない)。

use std::path::{Path, PathBuf};
use std::process::Command;
#[allow(unused_imports)]
use std::sync::Arc;

use edlr_core::plugin::filesystem::FilesystemConfigStore;
use edlr_core::plugin::{GrantsStore, PluginHost, Registry, SettingsStore, SidecarConfigStore};
use edlr_core::router::Router;

pub struct Env {
    #[allow(dead_code)]
    pub registry: Registry,
    /// テストが `env.tmp.path()` で参照する、`Registry` の各ストアが指す
    /// tempdir。フィールドを保持しているだけで drop すると保存先ごと消える
    /// (このフィールド自体は直接読まれなくても生存させる必要がある)。
    #[allow(dead_code)]
    pub tmp: tempfile::TempDir,
}

/// `examples/plugins/hello-logger` を `wasm32-wasip2` 向けにビルドし、
/// できあがった `.wasm` へのパスを返す。既にビルド済みならほぼ即座に戻る
/// (`cargo build` は成果物が新しければ再ビルドしない)。
#[allow(dead_code)]
pub fn valid_plugin_wasm() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crate_path = manifest_dir
        .join("..")
        .join("examples/plugins/hello-logger");

    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2", "--release"])
        .current_dir(&crate_path)
        .status()
        .expect("failed to spawn cargo build for the sidecar test fixture plugin");
    assert!(status.success(), "hello-logger fixture build failed");

    crate_path
        .join("target/wasm32-wasip2/release")
        .join("hello_logger.wasm")
}

#[allow(dead_code)]
pub fn sidecar_env(name: &str, port: u16, scalable: bool) -> Env {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugin_dir = plugins_dir.join("sc-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(valid_plugin_wasm(), plugin_dir.join("plugin.wasm")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "id = \"sc-plugin\"\nname = \"SC\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n\
             [[sidecar]]\nname = \"{name}\"\nreason = \"test sidecar\"\n\
             args = [\"-c\", \"sleep 30\"]\nport = {port}\nscalable = {scalable}\n"
        ),
    )
    .unwrap();

    let router = Router::new(16);
    let registry = edlr_core::plugin::start_plugins(
        &plugins_dir,
        SettingsStore::new(tmp.path().join("settings")),
        SidecarConfigStore::new(tmp.path().join("settings")),
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new()),
        GrantsStore::new(tmp.path().join("grants")),
        &router,
        PluginHost::new().expect("plugin host"),
    );

    Env { registry, tmp }
}

/// 当該プラグインの `capabilities_json` が現在載せている実効許可ホスト。
///
/// `#[allow(dead_code)]`: このモジュールはテストバイナリごとに個別に
/// コンパイルされる(`core/tests/*.rs` の各ファイルが `mod support;` する
/// たび)。このヘルパを使わないテストバイナリからは「未使用」警告が出るが、
/// 実際には他のテストバイナリで使われている正常な状態(Minor: 最終レビュー
/// で見つかったテスト出力のノイズ)。
#[allow(dead_code)]
pub fn effective_hosts(registry: &Registry, id: &str) -> Vec<String> {
    registry.effective_hosts(id).unwrap_or_default()
}

/// `plugins_dir` に独立した 2 プラグイン(`sc-plugin-a`/`sc-plugin-b`)を
/// 作り、それぞれに同名のサイドカーを 1 つずつ宣言した `Registry` を返す。
/// 「あるプラグインのサイドカー停止待ちが、別プラグインのサイドカー操作を
/// ブロックしないか」を検証するテスト専用のヘルパ。
#[allow(dead_code)]
pub fn two_plugin_sidecar_env(sidecar_name: &str, port_a: u16, port_b: u16) -> Env {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    write_sidecar_plugin(&plugins_dir, "sc-plugin-a", sidecar_name, port_a);
    write_sidecar_plugin(&plugins_dir, "sc-plugin-b", sidecar_name, port_b);

    let router = Router::new(16);
    let registry = edlr_core::plugin::start_plugins(
        &plugins_dir,
        SettingsStore::new(tmp.path().join("settings")),
        SidecarConfigStore::new(tmp.path().join("settings")),
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new()),
        GrantsStore::new(tmp.path().join("grants")),
        &router,
        PluginHost::new().expect("plugin host"),
    );

    Env { registry, tmp }
}

#[allow(dead_code)]
fn write_sidecar_plugin(plugins_dir: &Path, plugin_id: &str, sidecar_name: &str, port: u16) {
    let plugin_dir = plugins_dir.join(plugin_id);
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(valid_plugin_wasm(), plugin_dir.join("plugin.wasm")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "id = \"{plugin_id}\"\nname = \"{plugin_id}\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n\
             [[sidecar]]\nname = \"{sidecar_name}\"\nreason = \"test sidecar\"\n\
             args = [\"-c\", \"sleep 30\"]\nport = {port}\nscalable = false\n"
        ),
    )
    .unwrap();
}

/// `[[filesystem]]`(`name = name`, `mode = mode`)を 1 件持つ `fs-plugin` の
/// manifest を置いた plugins-dir でサーバを起動し、`Registry` を返す
/// (`sidecar_env` と同じ流儀)。
#[allow(dead_code)]
pub fn filesystem_env(name: &str, mode: &str) -> Env {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugin_dir = plugins_dir.join("fs-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(valid_plugin_wasm(), plugin_dir.join("plugin.wasm")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "id = \"fs-plugin\"\nname = \"FS\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n\
             [[filesystem]]\nname = \"{name}\"\nreason = \"test filesystem\"\nmode = \"{mode}\"\n"
        ),
    )
    .unwrap();

    let router = Router::new(16);
    let registry = edlr_core::plugin::start_plugins(
        &plugins_dir,
        SettingsStore::new(tmp.path().join("settings")),
        SidecarConfigStore::new(tmp.path().join("settings")),
        FilesystemConfigStore::new(tmp.path().join("settings"), Vec::new()),
        GrantsStore::new(tmp.path().join("grants")),
        &router,
        PluginHost::new().expect("plugin host"),
    );

    Env { registry, tmp }
}

/// 当該プラグインの `filesystem_json` が現在載せている生の JSON 文字列
/// (テスト用アクセサ。`Registry::filesystem_buffer` をそのまま呼ぶ)。
#[allow(dead_code)]
pub fn filesystem_buffer(registry: &Registry, id: &str) -> String {
    registry.filesystem_buffer(id).unwrap_or_default()
}

//! サイドカー統合テスト用の足場。`plugins-dir` に manifest だけを持つ
//! プラグインを 1 件作り、`Registry` を組み立てる。wasm のロードには
//! 失敗する(= プラグインは `Disabled` になる)が、サイドカーの設定・承認・
//! 制御は `Registry` の API で完結するため、このテストには十分。

// `PathBuf`/`Arc` はこのファイルの現行ヘルパでは直接使わないが、ブリーフの
// 足場コードに合わせてインポートは保持する(将来のヘルパ追加で使う想定)。
#[allow(unused_imports)]
use std::path::PathBuf;
#[allow(unused_imports)]
use std::sync::Arc;

use edlr_core::plugin::{GrantsStore, PluginHost, Registry, SettingsStore, SidecarConfigStore};
use edlr_core::router::Router;

pub struct Env {
    pub registry: Registry,
    _tmp: tempfile::TempDir,
}

pub fn sidecar_env(name: &str, port: u16, scalable: bool) -> Env {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugin_dir = plugins_dir.join("sc-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
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
        GrantsStore::new(tmp.path().join("grants")),
        &router,
        PluginHost::new().expect("plugin host"),
    );

    Env { registry, _tmp: tmp }
}

/// 当該プラグインの `capabilities_json` が現在載せている実効許可ホスト。
pub fn effective_hosts(registry: &Registry, id: &str) -> Vec<String> {
    registry.effective_hosts(id).unwrap_or_default()
}

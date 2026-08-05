//! SDK がホストターゲットでもコンパイルでき、`Plugin` のデフォルト実装で
//! 空実装のプラグインが書けることの確認。`register!` は wasm ターゲット
//! 専用(export シンボルを生やす)なのでここでは呼ばない。

use edlr_plugin_sdk as sdk;

struct Empty;
impl sdk::Plugin for Empty {}

#[test]
fn default_impls_cover_every_export() {
    // 全メソッドがデフォルト実装で呼べる(パニックしない)こと。
    <Empty as sdk::Plugin>::init();
    <Empty as sdk::Plugin>::on_schedule("flush".to_string());
    <Empty as sdk::Plugin>::on_job_complete(1, "{}".to_string());
    <Empty as sdk::Plugin>::on_stop();
}

//! `register!` がダウンストリームの利用コードとしてコンパイルできることの確認
//! (レビュー Important #3)。この crate 自身も `edlr-plugin-sdk` を外部 crate
//! として使う立場になる(integration test はそれぞれ別クレートとしてビルド
//! される)ため、`register!(Empty)` がここで展開できれば、少なくとも
//! `Guest` 実装 + `export!` 配線がホストターゲットでコンパイルを通ることを
//! 保証できる。実際の wasm コンポーネントとしてのリンク確認は Task 1 の
//! Step 5 相当(`cargo build --target wasm32-wasip2 --release`)で行う。

use edlr_plugin_sdk as sdk;

struct Empty;
impl sdk::Plugin for Empty {}

sdk::register!(Empty);

#[test]
fn register_macro_compiles() {
    // register! がコンパイルを通ること自体がこのテストの主目的。
    // 実行時には何もしない(export シンボルの呼び出しは wasm ランタイム側の責務)。
}

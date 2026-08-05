#![allow(clippy::too_many_arguments)]
//! edlr プラグイン SDK。
//!
//! WIT バインディングをこの crate が内包するので、プラグイン作者は
//! `core/wit` の cp も wit-bindgen の実行も不要。使い方は docs/sdk.md 参照。
//!
//! ```ignore
//! use edlr_plugin_sdk as sdk;
//! struct MyPlugin;
//! impl sdk::Plugin for MyPlugin {
//!     fn on_event(_ev: sdk::Event) {}
//! }
//! sdk::register!(MyPlugin);
//! ```

pub mod http;

#[doc(hidden)]
pub mod bindings {
    // Cargo の git 依存はリポジトリ全体を checkout するため、crate 外の
    // core/wit を相対パスで参照できる(spec「Rust SDK」参照)。
    // crates.io 公開時はこの前提が壊れるが、公開はスコープ外。
    wit_bindgen::generate!({
        path: "../../core/wit",
        world: "plugin",
    });
}

pub use bindings::edlr::plugin::{bus, driver_fs, driver_http, driver_process, host_log, host_settings};
pub use bindings::Event;

/// プラグイン本体が実装する trait。bindgen の `Guest`(全 export 必須)との
/// 緩衝層で、全メソッドにデフォルト実装があるため必要なものだけ書けばよい。
/// ABI に export が増えたときも、ここにデフォルト実装を足せば既存プラグインは
/// ソース互換のまま再ビルドだけで追従できる。
pub trait Plugin {
    fn init() {}
    fn on_event(_ev: Event) {}
    fn on_message(_driver: String, _topic: String, _payload: Vec<u8>) {}
    fn on_schedule(_name: String) {}
    /// SDK の `http::submit` を経由しないジョブ(自前で `submit-send` を
    /// 呼んだ場合)の完了だけがここへ届く(`register!` の demux 参照)。
    fn on_job_complete(_job_id: u64, _result_json: String) {}
    fn on_stop() {}
}

/// `Plugin` 実装を wasm の export として配線する。
#[macro_export]
macro_rules! register {
    ($plugin:ty) => {
        struct __EdlrSdkShim;
        impl $crate::bindings::Guest for __EdlrSdkShim {
            fn init() {
                <$plugin as $crate::Plugin>::init()
            }
            fn on_event(ev: $crate::Event) {
                <$plugin as $crate::Plugin>::on_event(ev)
            }
            fn on_message(driver: String, topic: String, payload: Vec<u8>) {
                <$plugin as $crate::Plugin>::on_message(driver, topic, payload)
            }
            fn on_schedule(name: String) {
                <$plugin as $crate::Plugin>::on_schedule(name)
            }
            fn on_job_complete(job_id: u64, result_json: String) {
                // Task 2 で $crate::http::dispatch_job_complete::<$plugin> に差し替える
                <$plugin as $crate::Plugin>::on_job_complete(job_id, result_json)
            }
            fn on_stop() {
                <$plugin as $crate::Plugin>::on_stop()
            }
        }
        $crate::bindings::export!(__EdlrSdkShim);
    };
}

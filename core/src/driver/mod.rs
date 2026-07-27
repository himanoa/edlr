//! ユーザー定義ドライバ(常駐 wasm コンポーネント)のロードと駆動。
//! `crate::plugin` と対称の構造だが、別レイヤーなので無理に共通化しない
//! (共有するのは grants / settings の下位ユーティリティ程度)。

pub mod manifest;

pub use manifest::{load_driver_manifest, DriverManifest};

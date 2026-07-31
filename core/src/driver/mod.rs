//! ユーザー定義ドライバ(常駐 wasm コンポーネント)のロードと駆動。
//! `crate::plugin` と対称の構造だが、別レイヤーなので無理に共通化しない
//! (共有するのは grants / settings の下位ユーティリティ程度)。

// Phase 5 タスク3で host/driver.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::host::driver as host;
// Phase 6 タスク2で manifest/ へ移動(旧パス互換。削除は本 Phase の Task 5)。
pub use crate::manifest::driver as manifest;
// Phase 4 タスク9で registry/driver.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::registry::driver as registry;
// Phase 5 タスク2で runner/driver.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::runner::driver as runner;

pub use manifest::{load_driver_manifest, DriverManifest};
pub use registry::{DriverEntry, DriverInfo, DriverRegistry, DriverRegistryError, DriverState};
pub use runner::start_drivers;

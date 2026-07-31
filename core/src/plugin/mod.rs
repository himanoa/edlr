pub mod allowlist;
pub mod filesystem;
// Phase 1 で capability/grants.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::capability::grants;
// Phase 5 タスク3で host/plugin.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::host::plugin as host;
// Phase 6 タスク2で manifest/ へ移動(旧パス互換。削除は本 Phase の Task 5)。
pub use crate::manifest;
// Phase 4 タスク9で registry/plugin.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::registry::plugin as registry;
// Phase 5 タスク2で runner/plugin.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::runner::plugin as runner;
// Phase 6 タスク3で runtime/ へ移動(旧パス互換。削除は本 Phase の Task 5)。
pub use crate::runtime::bus as bus_runtime;
pub use crate::runtime::dropped;
pub use crate::runtime::fs as fs_runtime;
pub use crate::runtime::sidecar as sidecar_runtime;
// Phase 3 で schedule/ へ移動(旧パス互換。削除は Phase 6)。
pub(crate) use crate::schedule;
pub use crate::schedule::store as schedule_store;
pub(crate) mod select_options;
// Phase 3 で settings/store.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::settings::store as settings;
pub mod sidecar;

pub use bus_runtime::{bus_json_string, parse_bus, BusRuntimeEntry};
pub use filesystem::{FilesystemConfig, FilesystemConfigError, FilesystemConfigStore};
pub use fs_runtime::{filesystem_json_string, parse_filesystem, FsRuntimeEntry};
pub use grants::{GrantState, GrantsError, GrantsStore};
pub use host::PluginHost;
pub use manifest::{
    load_manifest, matches_event, BusRequest, CapabilityRequest, DashboardWidget, FilesystemMode,
    FilesystemRequest, Manifest, ManifestError, OptionsFrom, ScheduleRequest, ScheduleSpec,
    SelectOption, SettingField, SidecarRequest, WidgetSize,
};
pub use registry::{
    FilesystemInfo, PluginEntry, PluginInfo, PluginState, Registry, RegistryError, ScheduleInfo,
    SidecarAction, SidecarInfo,
};
pub use runner::start_plugins;
pub use schedule_store::ScheduleStore;
pub use settings::{SettingsError, SettingsStore};
pub use sidecar::{assign_ports, SidecarConfig, SidecarConfigError, SidecarConfigStore};
pub use sidecar_runtime::{
    implicit_http_hosts, parse_sidecars, sidecars_json_string, SidecarRuntimeEntry,
};

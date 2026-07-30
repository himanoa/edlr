pub mod allowlist;
pub mod bus_runtime;
pub mod dropped;
pub mod filesystem;
pub mod fs_runtime;
// Phase 1 で capability/grants.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::capability::grants;
pub mod host;
pub mod manifest;
pub mod registry;
pub mod runner;
// Phase 3 で schedule/ へ移動(旧パス互換。削除は Phase 6)。
pub(crate) use crate::schedule;
pub use crate::schedule::store as schedule_store;
pub(crate) mod select_options;
// Phase 3 で settings/store.rs へ移動(旧パス互換。削除は Phase 6)。
pub use crate::settings::store as settings;
pub mod sidecar;
pub mod sidecar_runtime;

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

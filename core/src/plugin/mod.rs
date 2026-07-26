pub mod allowlist;
pub mod filesystem;
pub mod grants;
pub mod host;
pub mod manifest;
pub mod registry;
pub mod runner;
pub mod settings;
pub mod sidecar;
pub mod sidecar_runtime;

pub use filesystem::{FilesystemConfig, FilesystemConfigError, FilesystemConfigStore};
pub use grants::{GrantState, GrantsError, GrantsStore};
pub use host::PluginHost;
pub use manifest::{
    load_manifest, matches_event, CapabilityRequest, FilesystemMode, FilesystemRequest, Manifest,
    ManifestError, SettingField, SidecarRequest,
};
pub use registry::{
    PluginEntry, PluginInfo, PluginState, Registry, RegistryError, SidecarAction, SidecarInfo,
};
pub use runner::start_plugins;
pub use settings::{SettingsError, SettingsStore};
pub use sidecar::{assign_ports, SidecarConfig, SidecarConfigError, SidecarConfigStore};
pub use sidecar_runtime::{
    implicit_http_hosts, parse_sidecars, sidecars_json_string, SidecarRuntimeEntry,
};

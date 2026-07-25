pub mod host;
pub mod manifest;
pub mod registry;
pub mod runner;
pub mod settings;

pub use manifest::{load_manifest, matches_event, Manifest, ManifestError, SettingField};
pub use registry::{PluginEntry, PluginState, Registry};
pub use runner::start_plugins;
pub use settings::{SettingsError, SettingsStore};

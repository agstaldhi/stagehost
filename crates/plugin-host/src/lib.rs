//! Plugin hosting abstraction for Built-in DSP, CLAP, VST3, and VST2 plugins plus crash-safe scanning.

pub mod builtin;
pub mod clap_host;
pub mod descriptor;
pub mod error;
pub mod instance;
pub mod scanner;
pub mod vst2_host;
mod vst3_com;
pub mod vst3_host;

pub use descriptor::{PluginDescriptor, PluginFormat, PluginParamInfo};
pub use error::PluginError;
pub use instance::{create_plugin_instance, AudioPluginInstance, CreatedPlugin, PluginUiHandle};
pub use scanner::{
    default_plugin_search_paths, scan_plugin_file_in_process, scan_plugins_detailed,
    scan_plugins_safe, PluginScanIssue, PluginScanReport,
};

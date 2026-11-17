use std::path::PathBuf;
use thiserror::Error;

/// Errors originating from plugin discovery, loading, activation, or state persistence.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("Failed to load plugin binary '{path}': {detail}")]
    BinaryLoad { path: PathBuf, detail: String },

    #[error("Plugin '{plugin_id}' not found in '{path}'")]
    PluginNotFound { path: PathBuf, plugin_id: String },

    #[error("Plugin initialization failed ({context}): {detail}")]
    InitFailed { context: &'static str, detail: String },

    #[error("Plugin processing preparation failed at {sample_rate}Hz / {block_size} frames: {detail}")]
    PrepareFailed {
        sample_rate: f64,
        block_size: usize,
        detail: String,
    },

    #[error("Plugin GUI error ({context}): {detail}")]
    GuiError { context: &'static str, detail: String },

    #[error("Plugin state serialization error ({context}): {detail}")]
    StateError { context: &'static str, detail: String },

    #[error("Crash-safe plugin scan failed for '{path}': {detail}")]
    ScanFailed { path: PathBuf, detail: String },
}

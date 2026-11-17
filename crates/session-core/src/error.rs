use std::path::PathBuf;
use thiserror::Error;

/// Errors originating from session validation or JSON persistence.
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("Failed to read session file '{path}': {source}")]
    IoRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to write session file '{path}': {source}")]
    IoWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse JSON in '{path}': {source}")]
    JsonParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("Failed to serialize JSON for '{path}': {source}")]
    JsonSerialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("Sub-session '{id}' not found in session '{session_name}'")]
    SubSessionNotFound {
        id: uuid::Uuid,
        session_name: String,
    },
}

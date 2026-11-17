use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("Audio device '{device_name}' not found")]
    DeviceNotFound { device_name: String },

    #[error("No default audio output device is available")]
    NoDefaultOutputDevice,

    #[error("Failed to query audio device configuration for '{device_name}': {detail}")]
    ConfigQuery { device_name: String, detail: String },

    #[error("Failed to build or start audio stream ({context}): {detail}")]
    StreamFailure { context: &'static str, detail: String },

    #[error("ASIO driver '{driver_name}' error: {detail}")]
    AsioDriver { driver_name: String, detail: String },

    #[error("Audio engine command queue is full or disconnected")]
    CommandChannelClosed,
}

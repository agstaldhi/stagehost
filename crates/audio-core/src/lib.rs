//! Real-time audio engine, ASIO & WASAPI backends, MMCSS thread priority, and DSP graph.

pub mod asio;
pub mod dsp;
pub mod engine;
pub mod error;
pub mod graph;
pub mod mmcss;
pub mod tuner;
mod worker_pool;

pub use dsp::{db_to_linear, equal_power_pan, linear_to_db, soft_limit, StereoScratchBuffer};
pub use engine::{AudioDeviceInfo, AudioDriverBackend, AudioEngine, DriverNotes};
pub use error::AudioError;
pub use graph::{
    AudioGraphProcessor, AudioTelemetry, EngineCommand, LiveRack, LiveRackParams, LiveSendParams,
    LiveSessionParams, LiveSlot, LiveSlotParams, RackMeterSnapshot,
};
pub use mmcss::{disable_process_power_throttling, DenormalGuard, MmcssGuard};
pub use tuner::{format_midi_note, StageTuner, TunerReading};

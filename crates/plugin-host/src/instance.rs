use session_core::{BuiltInEffectKind, PluginKind};

use crate::builtin::{
    DynamicsGateCompPlugin, GainLimiterPlugin, StereoChorusDelayPlugin, StereoReverbPlugin,
    ThreeBandEqPlugin,
};
use crate::clap_host::ClapPluginInstance;
use crate::vst2_host::Vst2PluginInstance;
use crate::vst3_host::Vst3PluginInstance;
use crate::{PluginDescriptor, PluginError, PluginParamInfo};

/// Real-time audio-thread plugin processor interface.
pub trait AudioPluginInstance: Send {
    fn descriptor(&self) -> &PluginDescriptor;

    /// Allocates internal DSP scratch buffers before real-time processing starts.
    fn prepare(&mut self, sample_rate: f64, max_block_size: usize) -> Result<(), PluginError>;

    /// Processes a single audio block without allocating or blocking.
    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]);

    /// Returns `true` if the plugin exposes an auxiliary/sidechain audio input bus.
    fn supports_sidechain(&self) -> bool {
        self.descriptor().has_sidechain
    }

    /// Processes a single audio block with an optional stereo sidechain key input (`[sc_l, sc_r]`).
    fn process_with_sidechain(
        &mut self,
        inputs: &[&[f32]],
        sidechain: Option<&[&[f32]]>,
        outputs: &mut [&mut [f32]],
    ) {
        let _ = sidechain;
        self.process(inputs, outputs);
    }

    fn parameters(&self) -> Vec<PluginParamInfo>;

    fn set_parameter(&mut self, param_id: u32, value: f32);

    fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }

    /// Returns the plugin's reported processing delay (latency) in samples.
    fn latency_samples(&self) -> u32 {
        0
    }
}

/// Main-thread UI and preset-state handle paired with an external VST3/CLAP instance.
pub trait PluginUiHandle {
    fn has_editor(&self) -> bool;

    fn open_editor(&mut self, parent_hwnd: isize) -> Result<(), PluginError>;

    fn close_editor(&mut self);

    fn preferred_size(&self) -> Option<(u32, u32)> {
        None
    }

    fn resize_editor(&mut self, _width: u32, _height: u32) {}

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError>;

    fn parameters(&self) -> Vec<PluginParamInfo>;
}

pub type CreatedPlugin = (Box<dyn AudioPluginInstance>, Option<Box<dyn PluginUiHandle>>);

pub fn create_plugin_instance(kind: &PluginKind) -> Result<CreatedPlugin, PluginError> {
    match kind {
        PluginKind::BuiltInEffect(BuiltInEffectKind::StereoChorusDelay) => {
            Ok((Box::new(StereoChorusDelayPlugin::new()), None))
        }
        PluginKind::BuiltInEffect(BuiltInEffectKind::ThreeBandEq) => {
            Ok((Box::new(ThreeBandEqPlugin::new()), None))
        }
        PluginKind::BuiltInEffect(BuiltInEffectKind::DynamicsGateComp) => {
            Ok((Box::new(DynamicsGateCompPlugin::new()), None))
        }
        PluginKind::BuiltInEffect(BuiltInEffectKind::StereoReverb) => {
            Ok((Box::new(StereoReverbPlugin::new()), None))
        }
        PluginKind::BuiltInEffect(BuiltInEffectKind::GainLimiter) => {
            Ok((Box::new(GainLimiterPlugin::new()), None))
        }
        PluginKind::Clap { path, plugin_id } => {
            let (instance, ui) = ClapPluginInstance::load_with_ui(path, plugin_id)?;
            Ok((Box::new(instance), Some(Box::new(ui))))
        }
        PluginKind::Vst3 { path, class_id } => {
            let (instance, ui) = Vst3PluginInstance::load_with_ui(path, class_id)?;
            Ok((Box::new(instance), Some(Box::new(ui))))
        }
        PluginKind::Vst2 { path, unique_id } => {
            let (instance, ui) = Vst2PluginInstance::load_with_ui(path, *unique_id)?;
            Ok((Box::new(instance), Some(Box::new(ui))))
        }
    }
}

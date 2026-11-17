use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use session_core::{BuiltInEffectKind, PluginKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginFormat {
    BuiltIn,
    Clap,
    Vst3,
    Vst2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDescriptor {
    pub format: PluginFormat,
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub path: Option<PathBuf>,
    pub is_instrument: bool,
    pub audio_inputs: usize,
    pub audio_outputs: usize,
    #[serde(default)]
    pub has_sidechain: bool,
}

impl PluginDescriptor {
    pub fn to_plugin_kind(&self) -> Option<PluginKind> {
        match self.format {
            PluginFormat::BuiltIn => match self.id.as_str() {
                "builtin.stereo_chorus_delay" => Some(PluginKind::BuiltInEffect(
                    BuiltInEffectKind::StereoChorusDelay,
                )),
                "builtin.three_band_eq" => {
                    Some(PluginKind::BuiltInEffect(BuiltInEffectKind::ThreeBandEq))
                }
                "builtin.dynamics_gate_comp" => Some(PluginKind::BuiltInEffect(
                    BuiltInEffectKind::DynamicsGateComp,
                )),
                "builtin.stereo_reverb" => {
                    Some(PluginKind::BuiltInEffect(BuiltInEffectKind::StereoReverb))
                }
                "builtin.gain_limiter" => {
                    Some(PluginKind::BuiltInEffect(BuiltInEffectKind::GainLimiter))
                }
                _ => None,
            },
            PluginFormat::Clap => self.path.clone().map(|path| PluginKind::Clap {
                path,
                plugin_id: self.id.clone(),
            }),
            PluginFormat::Vst3 => self.path.clone().map(|path| PluginKind::Vst3 {
                path,
                class_id: self.id.clone(),
            }),
            PluginFormat::Vst2 => self.path.clone().map(|path| {
                let unique_id = self
                    .id
                    .strip_prefix("vst2:")
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .map(|u| u as i32)
                    .unwrap_or(0);
                PluginKind::Vst2 { path, unique_id }
            }),
        }
    }
}

/// Describes a single automatable parameter exposed by a plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginParamInfo {
    pub id: u32,
    pub name: String,
    pub value: f32,
    pub min: f32,
    pub max: f32,
}

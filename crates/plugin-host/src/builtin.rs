use std::f32::consts::TAU;

use crate::{AudioPluginInstance, PluginDescriptor, PluginError, PluginFormat, PluginParamInfo};

pub fn builtin_descriptors() -> Vec<PluginDescriptor> {
    vec![
        StereoChorusDelayPlugin::descriptor_static(),
        ThreeBandEqPlugin::descriptor_static(),
        DynamicsGateCompPlugin::descriptor_static(),
        StereoReverbPlugin::descriptor_static(),
        GainLimiterPlugin::descriptor_static(),
    ]
}

/// Stereo ping-pong chorus/delay effect with a pre-allocated circular buffer.
pub struct StereoChorusDelayPlugin {
    descriptor: PluginDescriptor,
    sample_rate: f32,
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    write_pos: usize,
    delay_sec: f32,
    feedback: f32,
    lfo_phase: f32,
}

impl Default for StereoChorusDelayPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl StereoChorusDelayPlugin {
    pub fn descriptor_static() -> PluginDescriptor {
        PluginDescriptor {
            format: PluginFormat::BuiltIn,
            id: "builtin.stereo_chorus_delay".to_string(),
            name: "Stereo Chorus Delay".to_string(),
            vendor: "StageHost DSP".to_string(),
            version: "1.0.0".to_string(),
            path: None,
            is_instrument: false,
            audio_inputs: 2,
            audio_outputs: 2,
            has_sidechain: false,
        }
    }

    pub fn new() -> Self {
        Self {
            descriptor: Self::descriptor_static(),
            sample_rate: 48_000.0,
            delay_l: Vec::new(),
            delay_r: Vec::new(),
            write_pos: 0,
            delay_sec: 0.24,
            feedback: 0.32,
            lfo_phase: 0.0,
        }
    }
}

impl AudioPluginInstance for StereoChorusDelayPlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn prepare(&mut self, sample_rate: f64, _max_block_size: usize) -> Result<(), PluginError> {
        self.sample_rate = (sample_rate as f32).max(8_000.0);
        let needed = (self.sample_rate as usize) * 2;
        if self.delay_l.len() < needed {
            self.delay_l.resize(needed, 0.0);
            self.delay_r.resize(needed, 0.0);
        }
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
        self.write_pos = 0;
        Ok(())
    }

    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
        if inputs.len() < 2 || outputs.len() < 2 || self.delay_l.is_empty() {
            return;
        }
        let frames = inputs[0]
            .len()
            .min(inputs[1].len())
            .min(outputs[0].len())
            .min(outputs[1].len());
        let cap = self.delay_l.len();
        let base_samples = (self.delay_sec * self.sample_rate) as usize;
        let lfo_inc = 0.6 / self.sample_rate;

        for i in 0..frames {
            self.lfo_phase = (self.lfo_phase + lfo_inc).fract();
            let mod_offset = ((self.lfo_phase * TAU).sin() * 0.004 * self.sample_rate) as isize;
            let tap_l =
                (self.write_pos + cap).wrapping_sub((base_samples as isize + mod_offset).max(1) as usize) % cap;
            let tap_r =
                (self.write_pos + cap).wrapping_sub((base_samples as isize * 3 / 2 - mod_offset).max(1) as usize) % cap;

            let delayed_l = self.delay_l[tap_l];
            let delayed_r = self.delay_r[tap_r];

            let in_l = inputs[0][i];
            let in_r = inputs[1][i];

            self.delay_l[self.write_pos] = in_l + delayed_r * self.feedback;
            self.delay_r[self.write_pos] = in_r + delayed_l * self.feedback;
            self.write_pos = (self.write_pos + 1) % cap;

            outputs[0][i] = in_l * 0.8 + delayed_l * 0.45;
            outputs[1][i] = in_r * 0.8 + delayed_r * 0.45;
        }
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        vec![
            PluginParamInfo {
                id: 0,
                name: "Delay Time (s)".to_string(),
                value: self.delay_sec,
                min: 0.02,
                max: 1.0,
            },
            PluginParamInfo {
                id: 1,
                name: "Feedback".to_string(),
                value: self.feedback,
                min: 0.0,
                max: 0.85,
            },
        ]
    }

    fn set_parameter(&mut self, param_id: u32, value: f32) {
        match param_id {
            0 => self.delay_sec = value.clamp(0.02, 1.0),
            1 => self.feedback = value.clamp(0.0, 0.85),
            _ => {}
        }
    }
}

/// Output gain and soft-knee safety limiter.
pub struct GainLimiterPlugin {
    descriptor: PluginDescriptor,
    gain: f32,
}

impl Default for GainLimiterPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl GainLimiterPlugin {
    pub fn descriptor_static() -> PluginDescriptor {
        PluginDescriptor {
            format: PluginFormat::BuiltIn,
            id: "builtin.gain_limiter".to_string(),
            name: "Gain & Safety Limiter".to_string(),
            vendor: "StageHost DSP".to_string(),
            version: "1.0.0".to_string(),
            path: None,
            is_instrument: false,
            audio_inputs: 2,
            audio_outputs: 2,
            has_sidechain: false,
        }
    }

    pub fn new() -> Self {
        Self {
            descriptor: Self::descriptor_static(),
            gain: 1.0,
        }
    }
}

impl AudioPluginInstance for GainLimiterPlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn prepare(&mut self, _sample_rate: f64, _max_block_size: usize) -> Result<(), PluginError> {
        Ok(())
    }

    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
        if inputs.len() < 2 || outputs.len() < 2 {
            return;
        }
        let frames = inputs[0]
            .len()
            .min(inputs[1].len())
            .min(outputs[0].len())
            .min(outputs[1].len());
        for i in 0..frames {
            outputs[0][i] = (inputs[0][i] * self.gain).tanh();
            outputs[1][i] = (inputs[1][i] * self.gain).tanh();
        }
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        vec![PluginParamInfo {
            id: 0,
            name: "Drive Gain".to_string(),
            value: self.gain,
            min: 0.0,
            max: 4.0,
        }]
    }

    fn set_parameter(&mut self, param_id: u32, value: f32) {
        if param_id == 0 {
            self.gain = value.clamp(0.0, 4.0);
        }
    }
}

#[derive(Clone, Copy)]
struct StereoBiquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1_l: f32,
    z2_l: f32,
    z1_r: f32,
    z2_r: f32,
}

impl Default for StereoBiquad {
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1_l: 0.0,
            z2_l: 0.0,
            z1_r: 0.0,
            z2_r: 0.0,
        }
    }
}

impl StereoBiquad {
    fn reset(&mut self) {
        self.z1_l = 0.0;
        self.z2_l = 0.0;
        self.z1_r = 0.0;
        self.z2_r = 0.0;
    }

    #[inline]
    fn process_stereo(&mut self, x_l: f32, x_r: f32) -> (f32, f32) {
        let y_l = self.b0 * x_l + self.z1_l;
        self.z1_l = self.b1 * x_l - self.a1 * y_l + self.z2_l;
        self.z2_l = self.b2 * x_l - self.a2 * y_l;

        let y_r = self.b0 * x_r + self.z1_r;
        self.z1_r = self.b1 * x_r - self.a1 * y_r + self.z2_r;
        self.z2_r = self.b2 * x_r - self.a2 * y_r;
        (y_l, y_r)
    }

    fn set_low_shelf(&mut self, sample_rate: f32, freq_hz: f32, gain_db: f32) {
        let (a, cos_w, sin_w) = biquad_prep(sample_rate, freq_hz, gain_db);
        let two_sqrt_a_alpha = shelf_term(a, sin_w);
        let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w);
        let a2 = (a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha;
        self.assign_normalized(b0, b1, b2, a0, a1, a2);
    }

    fn set_peaking(&mut self, sample_rate: f32, freq_hz: f32, q: f32, gain_db: f32) {
        let (a, cos_w, sin_w) = biquad_prep(sample_rate, freq_hz, gain_db);
        let alpha = sin_w / (2.0 * q.max(0.1));
        self.assign_normalized(
            1.0 + alpha * a,
            -2.0 * cos_w,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos_w,
            1.0 - alpha / a,
        );
    }

    fn set_high_shelf(&mut self, sample_rate: f32, freq_hz: f32, gain_db: f32) {
        let (a, cos_w, sin_w) = biquad_prep(sample_rate, freq_hz, gain_db);
        let two_sqrt_a_alpha = shelf_term(a, sin_w);
        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha;
        self.assign_normalized(b0, b1, b2, a0, a1, a2);
    }

    fn assign_normalized(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        let inv = 1.0 / a0.max(1e-6);
        self.b0 = b0 * inv;
        self.b1 = b1 * inv;
        self.b2 = b2 * inv;
        self.a1 = a1 * inv;
        self.a2 = a2 * inv;
    }
}

fn biquad_prep(sample_rate: f32, freq_hz: f32, gain_db: f32) -> (f32, f32, f32) {
    let a = 10.0f32.powf(gain_db / 40.0);
    let clamped_f = freq_hz.clamp(20.0, sample_rate * 0.45);
    let w0 = TAU * clamped_f / sample_rate.max(8_000.0);
    (a, w0.cos(), w0.sin())
}

fn shelf_term(a: f32, sin_w: f32) -> f32 {
    let slope_factor = 0.25; // S = 0.8 -> (1/S - 1) = 0.25
    let alpha = sin_w * 0.5 * ((a + 1.0 / a) * slope_factor + 2.0).max(0.0).sqrt();
    2.0 * a.sqrt() * alpha
}

/// Three-band stereo tone EQ (Low Shelf 120 Hz, Mid Peak 1 kHz, High Shelf 4.5 kHz).
pub struct ThreeBandEqPlugin {
    descriptor: PluginDescriptor,
    sample_rate: f32,
    low_db: f32,
    mid_db: f32,
    high_db: f32,
    low: StereoBiquad,
    mid: StereoBiquad,
    high: StereoBiquad,
}

impl Default for ThreeBandEqPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreeBandEqPlugin {
    pub fn descriptor_static() -> PluginDescriptor {
        PluginDescriptor {
            format: PluginFormat::BuiltIn,
            id: "builtin.three_band_eq".to_string(),
            name: "3-Band Tone EQ".to_string(),
            vendor: "StageHost DSP".to_string(),
            version: "1.0.0".to_string(),
            path: None,
            is_instrument: false,
            audio_inputs: 2,
            audio_outputs: 2,
            has_sidechain: false,
        }
    }

    pub fn new() -> Self {
        let mut eq = Self {
            descriptor: Self::descriptor_static(),
            sample_rate: 48_000.0,
            low_db: 0.0,
            mid_db: 0.0,
            high_db: 0.0,
            low: StereoBiquad::default(),
            mid: StereoBiquad::default(),
            high: StereoBiquad::default(),
        };
        eq.recompute_filters();
        eq
    }

    fn recompute_filters(&mut self) {
        self.low.set_low_shelf(self.sample_rate, 120.0, self.low_db);
        self.mid.set_peaking(self.sample_rate, 1_000.0, 1.0, self.mid_db);
        self.high.set_high_shelf(self.sample_rate, 4_500.0, self.high_db);
    }
}

impl AudioPluginInstance for ThreeBandEqPlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn prepare(&mut self, sample_rate: f64, _max_block_size: usize) -> Result<(), PluginError> {
        self.sample_rate = (sample_rate as f32).max(8_000.0);
        self.low.reset();
        self.mid.reset();
        self.high.reset();
        self.recompute_filters();
        Ok(())
    }

    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
        if inputs.len() < 2 || outputs.len() < 2 {
            return;
        }
        let frames = inputs[0]
            .len()
            .min(inputs[1].len())
            .min(outputs[0].len())
            .min(outputs[1].len());
        for i in 0..frames {
            let (l1, r1) = self.low.process_stereo(inputs[0][i], inputs[1][i]);
            let (l2, r2) = self.mid.process_stereo(l1, r1);
            let (l3, r3) = self.high.process_stereo(l2, r2);
            outputs[0][i] = l3;
            outputs[1][i] = r3;
        }
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        vec![
            PluginParamInfo {
                id: 0,
                name: "Low 120Hz (dB)".to_string(),
                value: self.low_db,
                min: -12.0,
                max: 12.0,
            },
            PluginParamInfo {
                id: 1,
                name: "Mid 1kHz (dB)".to_string(),
                value: self.mid_db,
                min: -12.0,
                max: 12.0,
            },
            PluginParamInfo {
                id: 2,
                name: "High 4.5kHz (dB)".to_string(),
                value: self.high_db,
                min: -12.0,
                max: 12.0,
            },
        ]
    }

    fn set_parameter(&mut self, param_id: u32, value: f32) {
        let clamped = value.clamp(-12.0, 12.0);
        match param_id {
            0 => self.low_db = clamped,
            1 => self.mid_db = clamped,
            2 => self.high_db = clamped,
            _ => return,
        }
        self.recompute_filters();
    }
}

/// Stereo-linked noise gate and compressor for live vocal and instrument dynamics control.
pub struct DynamicsGateCompPlugin {
    descriptor: PluginDescriptor,
    sample_rate: f32,
    gate_thresh_db: f32,
    comp_thresh_db: f32,
    comp_ratio: f32,
    makeup_db: f32,
    sc_listen: f32,
    env: f32,
    gate_gain: f32,
    env_att: f32,
    env_rel: f32,
    gate_smooth: f32,
}

impl Default for DynamicsGateCompPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicsGateCompPlugin {
    pub fn descriptor_static() -> PluginDescriptor {
        PluginDescriptor {
            format: PluginFormat::BuiltIn,
            id: "builtin.dynamics_gate_comp".to_string(),
            name: "Gate & Compressor".to_string(),
            vendor: "StageHost DSP".to_string(),
            version: "1.0.0".to_string(),
            path: None,
            is_instrument: false,
            audio_inputs: 2,
            audio_outputs: 2,
            has_sidechain: true,
        }
    }

    pub fn new() -> Self {
        let mut p = Self {
            descriptor: Self::descriptor_static(),
            sample_rate: 48_000.0,
            gate_thresh_db: -60.0,
            comp_thresh_db: -18.0,
            comp_ratio: 3.0,
            makeup_db: 0.0,
            sc_listen: 0.0,
            env: 0.0,
            gate_gain: 1.0,
            env_att: 0.01,
            env_rel: 0.001,
            gate_smooth: 0.002,
        };
        p.recompute_coeffs();
        p
    }

    fn recompute_coeffs(&mut self) {
        let sr = self.sample_rate.max(8_000.0);
        self.env_att = 1.0 - (-1.0 / (0.005 * sr)).exp();
        self.env_rel = 1.0 - (-1.0 / (0.120 * sr)).exp();
        self.gate_smooth = 1.0 - (-1.0 / (0.015 * sr)).exp();
    }
}

impl AudioPluginInstance for DynamicsGateCompPlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn prepare(&mut self, sample_rate: f64, _max_block_size: usize) -> Result<(), PluginError> {
        self.sample_rate = sample_rate.max(8_000.0) as f32;
        self.env = 0.0;
        self.gate_gain = 1.0;
        self.recompute_coeffs();
        Ok(())
    }

    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
        self.process_with_sidechain(inputs, None, outputs);
    }

    fn process_with_sidechain(
        &mut self,
        inputs: &[&[f32]],
        sidechain: Option<&[&[f32]]>,
        outputs: &mut [&mut [f32]],
    ) {
        if inputs.len() < 2 || outputs.len() < 2 {
            return;
        }
        let mut frames = outputs[0]
            .len()
            .min(outputs[1].len())
            .min(inputs[0].len())
            .min(inputs[1].len());
        let sc_slices = sidechain.filter(|sc| sc.len() >= 2);
        if let Some(sc) = sc_slices {
            frames = frames.min(sc[0].len()).min(sc[1].len());
        }
        let (out_l_slice, out_r_slice) = outputs.split_at_mut(1);
        let out_l = &mut out_l_slice[0][..frames];
        let out_r = &mut out_r_slice[0][..frames];
        let in_l = &inputs[0][..frames];
        let in_r = &inputs[1][..frames];
        let (key_l, key_r) = match sc_slices {
            Some(sc) => (&sc[0][..frames], &sc[1][..frames]),
            None => (in_l, in_r),
        };

        let gate_bypassed = self.gate_thresh_db <= -79.5;
        let listen_sc = self.sc_listen >= 0.5;
        let slope = if self.comp_ratio > 1.001 {
            1.0 - (1.0 / self.comp_ratio)
        } else {
            0.0
        };

        for i in 0..frames {
            let level = key_l[i].abs().max(key_r[i].abs());
            let coeff = if level > self.env {
                self.env_att
            } else {
                self.env_rel
            };
            self.env += coeff * (level - self.env);
            let level_db = 20.0 * self.env.max(1e-6).log10();

            let gate_target = if gate_bypassed || level_db >= self.gate_thresh_db {
                1.0
            } else {
                0.0
            };
            self.gate_gain += self.gate_smooth * (gate_target - self.gate_gain);

            let comp_gr_db = if level_db > self.comp_thresh_db {
                -(level_db - self.comp_thresh_db) * slope
            } else {
                0.0
            };
            let gain = self.gate_gain * 10.0f32.powf((comp_gr_db + self.makeup_db) * 0.05);
            if listen_sc {
                out_l[i] = key_l[i];
                out_r[i] = key_r[i];
            } else {
                out_l[i] = in_l[i] * gain;
                out_r[i] = in_r[i] * gain;
            }
        }
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        vec![
            PluginParamInfo {
                id: 0,
                name: "Gate Thresh (dB)".to_string(),
                value: self.gate_thresh_db,
                min: -80.0,
                max: -20.0,
            },
            PluginParamInfo {
                id: 1,
                name: "Comp Thresh (dB)".to_string(),
                value: self.comp_thresh_db,
                min: -40.0,
                max: 0.0,
            },
            PluginParamInfo {
                id: 2,
                name: "Comp Ratio".to_string(),
                value: self.comp_ratio,
                min: 1.0,
                max: 12.0,
            },
            PluginParamInfo {
                id: 3,
                name: "Makeup (dB)".to_string(),
                value: self.makeup_db,
                min: 0.0,
                max: 18.0,
            },
            PluginParamInfo {
                id: 4,
                name: "SC Listen".to_string(),
                value: self.sc_listen,
                min: 0.0,
                max: 1.0,
            },
        ]
    }

    fn set_parameter(&mut self, param_id: u32, value: f32) {
        match param_id {
            0 => self.gate_thresh_db = value.clamp(-80.0, -20.0),
            1 => self.comp_thresh_db = value.clamp(-40.0, 0.0),
            2 => self.comp_ratio = value.clamp(1.0, 12.0),
            3 => self.makeup_db = value.clamp(0.0, 18.0),
            4 => self.sc_listen = value.clamp(0.0, 1.0),
            _ => {}
        }
    }
}

struct CombFilter {
    buf: Vec<f32>,
    idx: usize,
    filter_store: f32,
}

impl CombFilter {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            idx: 0,
            filter_store: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32, damp1: f32, damp2: f32) -> f32 {
        let out = self.buf[self.idx];
        self.filter_store = out * damp2 + self.filter_store * damp1;
        self.buf[self.idx] = input + self.filter_store * feedback;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

struct AllpassFilter {
    buf: Vec<f32>,
    idx: usize,
}

impl AllpassFilter {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            idx: 0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let buf_out = self.buf[self.idx];
        let out = -input + buf_out;
        self.buf[self.idx] = input + buf_out * 0.5;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

/// Stereo Schroeder/Freeverb algorithmic reverb with pre-allocated delay lines.
pub struct StereoReverbPlugin {
    descriptor: PluginDescriptor,
    sample_rate: f32,
    room_size: f32,
    damping: f32,
    pre_delay_ms: f32,
    width: f32,
    pre_delay_buf: Vec<f32>,
    pre_delay_write: usize,
    combs_l: Vec<CombFilter>,
    combs_r: Vec<CombFilter>,
    allpasses_l: Vec<AllpassFilter>,
    allpasses_r: Vec<AllpassFilter>,
}

impl Default for StereoReverbPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl StereoReverbPlugin {
    pub fn descriptor_static() -> PluginDescriptor {
        PluginDescriptor {
            format: PluginFormat::BuiltIn,
            id: "builtin.stereo_reverb".to_string(),
            name: "Stereo Reverb".to_string(),
            vendor: "StageHost DSP".to_string(),
            version: "1.0.0".to_string(),
            path: None,
            is_instrument: false,
            audio_inputs: 2,
            audio_outputs: 2,
            has_sidechain: false,
        }
    }

    pub fn new() -> Self {
        Self {
            descriptor: Self::descriptor_static(),
            sample_rate: 48_000.0,
            room_size: 0.55,
            damping: 0.40,
            pre_delay_ms: 12.0,
            width: 0.85,
            pre_delay_buf: Vec::new(),
            pre_delay_write: 0,
            combs_l: Vec::new(),
            combs_r: Vec::new(),
            allpasses_l: Vec::new(),
            allpasses_r: Vec::new(),
        }
    }

    fn rebuild_buffers(&mut self) {
        const COMB_TUNINGS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
        const ALLPASS_TUNINGS: [usize; 4] = [556, 441, 341, 225];
        const STEREO_SPREAD: usize = 23;

        let scale = self.sample_rate / 44_100.0;
        let pre_cap = ((0.100 * self.sample_rate) as usize).max(64);
        self.pre_delay_buf = vec![0.0; pre_cap];
        self.pre_delay_write = 0;

        self.combs_l = COMB_TUNINGS
            .iter()
            .map(|&n| CombFilter::new(((n as f32) * scale).round() as usize))
            .collect();
        self.combs_r = COMB_TUNINGS
            .iter()
            .map(|&n| CombFilter::new((((n + STEREO_SPREAD) as f32) * scale).round() as usize))
            .collect();
        self.allpasses_l = ALLPASS_TUNINGS
            .iter()
            .map(|&n| AllpassFilter::new(((n as f32) * scale).round() as usize))
            .collect();
        self.allpasses_r = ALLPASS_TUNINGS
            .iter()
            .map(|&n| AllpassFilter::new((((n + STEREO_SPREAD) as f32) * scale).round() as usize))
            .collect();
    }
}

impl AudioPluginInstance for StereoReverbPlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn prepare(&mut self, sample_rate: f64, _max_block_size: usize) -> Result<(), PluginError> {
        self.sample_rate = sample_rate.max(8_000.0) as f32;
        self.rebuild_buffers();
        Ok(())
    }

    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
        if inputs.len() < 2 || outputs.len() < 2 || self.pre_delay_buf.is_empty() {
            return;
        }
        let frames = outputs[0]
            .len()
            .min(outputs[1].len())
            .min(inputs[0].len())
            .min(inputs[1].len());
        let (out_l_slice, out_r_slice) = outputs.split_at_mut(1);
        let out_l = &mut out_l_slice[0][..frames];
        let out_r = &mut out_r_slice[0][..frames];
        let in_l = &inputs[0][..frames];
        let in_r = &inputs[1][..frames];

        let feedback = 0.70 + self.room_size * 0.28;
        let damp1 = self.damping * 0.45;
        let damp2 = 1.0 - damp1;
        let wet1 = 0.5 * (1.0 + self.width);
        let wet2 = 0.5 * (1.0 - self.width);
        let pd_len = self.pre_delay_buf.len();
        let pd_samples = ((self.pre_delay_ms * 0.001 * self.sample_rate) as usize).min(pd_len - 1);

        for i in 0..frames {
            let mono_in = 0.5 * (in_l[i] + in_r[i]) * 0.015;
            self.pre_delay_buf[self.pre_delay_write] = mono_in;
            let read_idx = (self.pre_delay_write + pd_len - pd_samples) % pd_len;
            let delayed = self.pre_delay_buf[read_idx];
            self.pre_delay_write = (self.pre_delay_write + 1) % pd_len;

            let mut acc_l = 0.0f32;
            let mut acc_r = 0.0f32;
            for c in &mut self.combs_l {
                acc_l += c.process(delayed, feedback, damp1, damp2);
            }
            for c in &mut self.combs_r {
                acc_r += c.process(delayed, feedback, damp1, damp2);
            }
            for ap in &mut self.allpasses_l {
                acc_l = ap.process(acc_l);
            }
            for ap in &mut self.allpasses_r {
                acc_r = ap.process(acc_r);
            }
            out_l[i] = acc_l * wet1 + acc_r * wet2;
            out_r[i] = acc_r * wet1 + acc_l * wet2;
        }
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        vec![
            PluginParamInfo {
                id: 0,
                name: "Room Size".to_string(),
                value: self.room_size,
                min: 0.0,
                max: 1.0,
            },
            PluginParamInfo {
                id: 1,
                name: "Damping".to_string(),
                value: self.damping,
                min: 0.0,
                max: 1.0,
            },
            PluginParamInfo {
                id: 2,
                name: "Pre-Delay (ms)".to_string(),
                value: self.pre_delay_ms,
                min: 0.0,
                max: 80.0,
            },
            PluginParamInfo {
                id: 3,
                name: "Width".to_string(),
                value: self.width,
                min: 0.0,
                max: 1.0,
            },
        ]
    }

    fn set_parameter(&mut self, param_id: u32, value: f32) {
        match param_id {
            0 => self.room_size = value.clamp(0.0, 1.0),
            1 => self.damping = value.clamp(0.0, 1.0),
            2 => self.pre_delay_ms = value.clamp(0.0, 80.0),
            3 => self.width = value.clamp(0.0, 1.0),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chorus_and_limiter_process_stereo_audio() {
        let mut chorus = StereoChorusDelayPlugin::new();
        chorus.prepare(48_000.0, 128).expect("prepare chorus");
        let mut limiter = GainLimiterPlugin::new();
        limiter.prepare(48_000.0, 128).expect("prepare limiter");
        limiter.set_parameter(0, 2.0);

        let in_l = vec![0.5f32; 128];
        let in_r = vec![-0.5f32; 128];
        let mut mid_l = vec![0.0f32; 128];
        let mut mid_r = vec![0.0f32; 128];
        let mut out_l = vec![0.0f32; 128];
        let mut out_r = vec![0.0f32; 128];

        {
            let ins: [&[f32]; 2] = [&in_l, &in_r];
            let mut outs: [&mut [f32]; 2] = [&mut mid_l, &mut mid_r];
            chorus.process(&ins, &mut outs);
        }
        {
            let ins: [&[f32]; 2] = [&mid_l, &mid_r];
            let mut outs: [&mut [f32]; 2] = [&mut out_l, &mut out_r];
            limiter.process(&ins, &mut outs);
        }

        let peak = out_l.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(peak > 0.5 && peak <= 1.0, "expected soft-limited signal, got {peak}");
    }

    #[test]
    fn three_band_eq_boosts_low_frequency_and_passes_unity_at_zero() {
        let mut eq = ThreeBandEqPlugin::new();
        eq.prepare(48_000.0, 256).expect("prepare eq");

        // 100 Hz sine wave inside the low shelf region.
        let mut in_l = vec![0.0f32; 256];
        for (i, sample) in in_l.iter_mut().enumerate() {
            *sample = 0.25 * (TAU * 100.0 * (i as f32) / 48_000.0).sin();
        }
        let in_r = in_l.clone();
        let mut flat_l = vec![0.0f32; 256];
        let mut flat_r = vec![0.0f32; 256];
        eq.process(&[&in_l, &in_r], &mut [&mut flat_l, &mut flat_r]);
        let flat_peak = flat_l.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!((flat_peak - 0.25).abs() < 0.01);

        eq.set_parameter(0, 6.0);
        let mut boosted_l = vec![0.0f32; 256];
        let mut boosted_r = vec![0.0f32; 256];
        eq.process(&[&in_l, &in_r], &mut [&mut boosted_l, &mut boosted_r]);
        let boosted_peak = boosted_l[128..].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(boosted_peak > flat_peak * 1.4, "expected +6 dB low boost, got {boosted_peak}");
    }

    #[test]
    fn dynamics_gate_silences_noise_and_compresses_peaks() {
        let mut dyn_fx = DynamicsGateCompPlugin::new();
        dyn_fx.prepare(48_000.0, 512).expect("prepare dynamics");
        dyn_fx.set_parameter(0, -40.0); // Gate -40 dB
        dyn_fx.set_parameter(1, -18.0); // Comp -18 dB
        dyn_fx.set_parameter(2, 6.0);   // 6:1 ratio

        // Low-level stage hum (~ -66 dBFS) should trigger gate closure after settle window.
        let hum = vec![0.0005f32; 512];
        let mut out_l = vec![0.0f32; 512];
        let mut out_r = vec![0.0f32; 512];
        for _ in 0..8 {
            dyn_fx.process(&[&hum, &hum], &mut [&mut out_l, &mut out_r]);
        }
        let gated_peak = out_l[256..].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(gated_peak < 0.0001, "expected noise gate attenuation, got {gated_peak}");

        // Loud signal (~ -2 dBFS) should open gate and compress well below raw peak.
        let loud = vec![0.8f32; 512];
        for _ in 0..4 {
            dyn_fx.process(&[&loud, &loud], &mut [&mut out_l, &mut out_r]);
        }
        let comp_peak = out_l[256..].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            comp_peak > 0.15 && comp_peak < 0.45,
            "expected compressed output between 0.15 and 0.45, got {comp_peak}"
        );
    }

    #[test]
    fn dynamics_external_sidechain_ducks_main_signal() {
        let mut dyn_fx = DynamicsGateCompPlugin::new();
        dyn_fx.prepare(48_000.0, 512).expect("prepare dynamics");
        dyn_fx.set_parameter(0, -80.0); // Gate off
        dyn_fx.set_parameter(1, -24.0); // Comp thresh -24 dB
        dyn_fx.set_parameter(2, 8.0);   // 8:1 ratio

        // Quiet main signal (-26 dBFS -> below -24 dB threshold, should pass unattenuated when SC is silent).
        let main = vec![0.05f32; 512];
        let sc_silent = vec![0.0f32; 512];
        let sc_loud = vec![0.9f32; 512];
        let mut out_l = vec![0.0f32; 512];
        let mut out_r = vec![0.0f32; 512];

        for _ in 0..4 {
            dyn_fx.process_with_sidechain(
                &[&main, &main],
                Some(&[&sc_silent, &sc_silent]),
                &mut [&mut out_l, &mut out_r],
            );
        }
        let un_ducked = out_l[256..].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!((un_ducked - 0.05).abs() < 0.002, "silent sidechain should not duck, got {un_ducked}");

        for _ in 0..4 {
            dyn_fx.process_with_sidechain(
                &[&main, &main],
                Some(&[&sc_loud, &sc_loud]),
                &mut [&mut out_l, &mut out_r],
            );
        }
        let ducked = out_l[256..].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(ducked < 0.02, "loud external sidechain must duck main signal, got {ducked}");
    }

    #[test]
    fn stereo_reverb_produces_decaying_stereo_tail() {
        let mut rev = StereoReverbPlugin::new();
        rev.prepare(48_000.0, 512).expect("prepare reverb");
        rev.set_parameter(0, 0.8);
        rev.set_parameter(2, 0.0);

        let mut impulse = vec![0.0f32; 512];
        impulse[0..64].fill(0.9);
        let silence = vec![0.0f32; 512];
        let mut out_l = vec![0.0f32; 512];
        let mut out_r = vec![0.0f32; 512];

        rev.process(&[&impulse, &impulse], &mut [&mut out_l, &mut out_r]);
        let mut tail_peak = 0.0f32;
        for _ in 0..8 {
            rev.process(&[&silence, &silence], &mut [&mut out_l, &mut out_r]);
            tail_peak = tail_peak.max(out_l.iter().fold(0.0f32, |a, &b| a.max(b.abs())));
        }
        assert!(tail_peak > 0.005, "expected reverb tail, got {tail_peak}");
    }
}

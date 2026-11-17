use std::f32::consts::TAU;
use std::time::Instant;

use plugin_host::AudioPluginInstance;
use rtrb::Producer;
use session_core::{
    InputChannelMode, RackConfig, RackSendConfig, SessionConfig, SidechainConfig,
    SidechainSourceKind,
};
use uuid::Uuid;

use crate::dsp::{db_to_linear, equal_power_pan, soft_limit, StereoScratchBuffer};
use crate::tuner::{StageTuner, TunerReading};
use crate::worker_pool::RackWorkerPool;

#[derive(Debug, Clone, Copy, Default)]
pub struct AudioTelemetry {
    pub input_peak_l: f32,
    pub input_peak_r: f32,
    pub master_peak_l: f32,
    pub master_peak_r: f32,
    pub cpu_load_pct: f32,
    pub xrun_count: u64,
    pub sample_rate: u32,
    pub buffer_frames: u32,
    pub plugin_latency_samples: u32,
    pub tuner: TunerReading,
}

#[derive(Debug, Clone, Copy)]
pub struct RackMeterSnapshot {
    pub rack_id: Uuid,
    pub peak_l: f32,
    pub peak_r: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct LiveSendParams {
    pub target_rack_id: Uuid,
    pub level_db: f32,
    pub enabled: bool,
}

impl From<&RackSendConfig> for LiveSendParams {
    fn from(s: &RackSendConfig) -> Self {
        Self {
            target_rack_id: s.target_rack_id,
            level_db: s.level_db,
            enabled: s.enabled,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveSlotParams {
    pub id: Uuid,
    pub enabled: bool,
    pub bypassed: bool,
    pub gain_db: f32,
    pub pan: f32,
    pub wet_mix: f32,
    pub sidechain: Option<SidechainConfig>,
    pub params: Vec<(u32, f32)>,
}

#[derive(Debug, Clone)]
pub struct LiveRackParams {
    pub id: Uuid,
    pub enabled: bool,
    pub muted: bool,
    pub solo: bool,
    pub gain_db: f32,
    pub pan: f32,
    pub width: f32,
    pub receive_input: bool,
    pub route_to_master: bool,
    pub sends: Vec<LiveSendParams>,
    pub slots: Vec<LiveSlotParams>,
}

#[derive(Debug, Clone)]
pub struct LiveSessionParams {
    pub master_gain_db: f32,
    pub racks: Vec<LiveRackParams>,
}

impl LiveSessionParams {
    /// Builds lightweight audio-thread parameters without cloning plugin state blobs or UI metadata.
    pub fn from_session(session: &SessionConfig, include_slot_params: bool) -> Self {
        Self {
            master_gain_db: session.master_gain_db,
            racks: session
                .racks
                .iter()
                .map(|r| LiveRackParams {
                    id: r.id,
                    enabled: r.enabled,
                    muted: r.muted,
                    solo: r.solo,
                    gain_db: r.gain_db,
                    pan: r.pan,
                    width: r.width,
                    receive_input: r.receive_input,
                    route_to_master: r.route_to_master,
                    sends: r.sends.iter().map(LiveSendParams::from).collect(),
                    slots: r
                        .slots
                        .iter()
                        .map(|s| LiveSlotParams {
                            id: s.id,
                            enabled: s.enabled,
                            bypassed: s.bypassed,
                            gain_db: s.gain_db,
                            pan: s.pan,
                            wet_mix: s.wet_mix,
                            sidechain: s.sidechain.clone(),
                            params: if include_slot_params {
                                s.params.iter().map(|(&k, &v)| (k, v)).collect()
                            } else {
                                Vec::new()
                            },
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
struct LiveSend {
    target_rack_id: Uuid,
    linear_gain: f32,
    enabled: bool,
    target_rack_idx: Option<usize>,
}

impl LiveSend {
    fn from_params(p: &LiveSendParams) -> Self {
        Self {
            target_rack_id: p.target_rack_id,
            linear_gain: db_to_linear(p.level_db),
            enabled: p.enabled,
            target_rack_idx: None,
        }
    }
}

pub struct LiveSlot {
    pub id: Uuid,
    pub enabled: bool,
    pub bypassed: bool,
    pub gain_db: f32,
    pub pan: f32,
    pub wet_mix: f32,
    pub latency_samples: u32,
    pub sidechain: Option<SidechainConfig>,
    pub sidechain_rack_idx: Option<usize>,
    pub instance: Box<dyn AudioPluginInstance>,
    linear_gain: f32,
    pan_l: f32,
    pan_r: f32,
    wet: f32,
    dry: f32,
    sc_linear_gain: f32,
}

impl LiveSlot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Uuid,
        enabled: bool,
        bypassed: bool,
        gain_db: f32,
        pan: f32,
        wet_mix: f32,
        latency_samples: u32,
        sidechain: Option<SidechainConfig>,
        instance: Box<dyn AudioPluginInstance>,
    ) -> Self {
        let mut slot = Self {
            id,
            enabled,
            bypassed,
            gain_db,
            pan,
            wet_mix,
            latency_samples,
            sidechain,
            sidechain_rack_idx: None,
            instance,
            linear_gain: 1.0,
            pan_l: 1.0,
            pan_r: 1.0,
            wet: 1.0,
            dry: 0.0,
            sc_linear_gain: 1.0,
        };
        slot.refresh_cached_dsp();
        slot
    }

    pub fn refresh_cached_dsp(&mut self) {
        self.linear_gain = db_to_linear(self.gain_db);
        let (pl, pr) = equal_power_pan(self.pan);
        self.pan_l = pl;
        self.pan_r = pr;
        self.wet = self.wet_mix.clamp(0.0, 1.0);
        self.dry = 1.0 - self.wet;
        self.sc_linear_gain = self
            .sidechain
            .as_ref()
            .map_or(1.0, |sc| db_to_linear(sc.gain_db));
    }
}

pub struct LiveRack {
    pub id: Uuid,
    pub enabled: bool,
    pub muted: bool,
    pub solo: bool,
    pub width: f32,
    pub receive_input: bool,
    pub route_to_master: bool,
    linear_gain: f32,
    pan_l: f32,
    pan_r: f32,
    sends: Vec<LiveSend>,
    pub slots: Vec<LiveSlot>,
    scratch_a: StereoScratchBuffer,
    scratch_b: StereoScratchBuffer,
    pre_fader_out: StereoScratchBuffer,
    bus_in: StereoScratchBuffer,
    sc_scratch: StereoScratchBuffer,
    pub last_peak_l: f32,
    pub last_peak_r: f32,
    accum_peak_l: f32,
    accum_peak_r: f32,
}

impl LiveRack {
    pub fn new(config: RackConfig, mut slots: Vec<LiveSlot>, max_block_size: usize) -> Self {
        for slot in &mut slots {
            slot.refresh_cached_dsp();
        }
        let (pan_l, pan_r) = equal_power_pan(config.pan);
        let sends = config
            .sends
            .iter()
            .map(|s| LiveSend::from_params(&LiveSendParams::from(s)))
            .collect();
        Self {
            id: config.id,
            enabled: config.enabled,
            muted: config.muted,
            solo: config.solo,
            width: config.width.clamp(0.0, 2.0),
            receive_input: config.receive_input,
            route_to_master: config.route_to_master,
            linear_gain: db_to_linear(config.gain_db),
            pan_l,
            pan_r,
            sends,
            slots,
            scratch_a: StereoScratchBuffer::new(max_block_size),
            scratch_b: StereoScratchBuffer::new(max_block_size),
            pre_fader_out: StereoScratchBuffer::new(max_block_size),
            bus_in: StereoScratchBuffer::new(max_block_size),
            sc_scratch: StereoScratchBuffer::new(max_block_size),
            last_peak_l: 0.0,
            last_peak_r: 0.0,
            accum_peak_l: 0.0,
            accum_peak_r: 0.0,
        }
    }

    pub fn ensure_capacity(&mut self, frames: usize) {
        self.scratch_a.ensure_capacity(frames);
        self.scratch_b.ensure_capacity(frames);
        self.pre_fader_out.ensure_capacity(frames);
        self.bus_in.ensure_capacity(frames);
        self.sc_scratch.ensure_capacity(frames);
    }

    pub fn take_accumulated_peaks(&mut self) -> (f32, f32) {
        let p = (self.accum_peak_l, self.accum_peak_r);
        self.accum_peak_l = 0.0;
        self.accum_peak_r = 0.0;
        p
    }
}

pub enum EngineCommand {
    ReplaceRacks {
        racks: Vec<LiveRack>,
        master_gain_db: f32,
    },
    AddRack {
        rack: Box<LiveRack>,
    },
    RemoveRack {
        rack_id: Uuid,
    },
    ReorderRacks {
        order: Vec<Uuid>,
    },
    AddSlot {
        rack_id: Uuid,
        index: Option<usize>,
        slot: Box<LiveSlot>,
    },
    RemoveSlot {
        rack_id: Uuid,
        slot_id: Uuid,
    },
    ReorderSlots {
        rack_id: Uuid,
        order: Vec<Uuid>,
    },
    SyncSessionParams {
        config: Box<SessionConfig>,
    },
    SyncLiveParams {
        params: Box<LiveSessionParams>,
    },
    SetMasterGainDb(f32),
    SetInputConfig {
        enabled: bool,
        gain_db: f32,
        mode: InputChannelMode,
    },
    SetInputCaptureActive(bool),
    SetOutputMuted(bool),
    SetTestTone(bool),
    SetTunerEnabled(bool),
    SetUiVisible(bool),
    PanicReset,
    SetSlotParameter {
        rack_id: Uuid,
        slot_id: Uuid,
        param_id: u32,
        value: f32,
    },
}

/// Real-time audio graph processor executing inside the audio callback without heap allocations.
pub struct AudioGraphProcessor {
    sample_rate: f64,
    max_block_size: usize,
    racks: Vec<LiveRack>,
    rack_exec_order: Vec<usize>,
    rack_waves: Vec<Vec<usize>>,
    rack_wave_count: usize,
    topo_in_degree: Vec<usize>,
    topo_visited: Vec<bool>,
    active_wave_scratch: Vec<usize>,
    worker_pool: Option<RackWorkerPool>,
    master_gain_db: f32,
    master_linear_gain: f32,
    output_muted: bool,
    input_enabled: bool,
    input_gain_db: f32,
    input_linear_gain: f32,
    input_mode: InputChannelMode,
    test_tone_enabled: bool,
    test_tone_phase: f32,
    tuner_enabled: bool,
    tuner: StageTuner,
    tuner_tx: Option<Producer<f32>>,
    ui_visible: bool,
    in_scratch: StereoScratchBuffer,
    raw_in_scratch: StereoScratchBuffer,
    retired_tx: Option<Producer<Vec<LiveRack>>>,
    retired_slot_tx: Option<Producer<Box<LiveSlot>>>,
    retired_params_tx: Option<Producer<Box<LiveSessionParams>>>,
    retired_cfg_tx: Option<Producer<Box<SessionConfig>>>,
    xrun_count: u64,
    plugin_chain_latency_samples: u32,
    latency_scratch: Vec<u32>,
}

impl AudioGraphProcessor {
    pub fn new(sample_rate: f64, max_block_size: usize) -> Self {
        let max_block = max_block_size.max(64);
        let sr = sample_rate.max(8_000.0);
        Self {
            sample_rate: sr,
            max_block_size: max_block,
            racks: Vec::new(),
            rack_exec_order: Vec::with_capacity(32),
            rack_waves: (0..16).map(|_| Vec::with_capacity(16)).collect(),
            rack_wave_count: 0,
            topo_in_degree: Vec::with_capacity(32),
            topo_visited: Vec::with_capacity(32),
            active_wave_scratch: Vec::with_capacity(32),
            worker_pool: None,
            master_gain_db: 0.0,
            master_linear_gain: 1.0,
            output_muted: false,
            input_enabled: true,
            input_gain_db: 0.0,
            input_linear_gain: 1.0,
            input_mode: InputChannelMode::Stereo,
            test_tone_enabled: false,
            test_tone_phase: 0.0,
            tuner_enabled: false,
            tuner: StageTuner::new(sr),
            tuner_tx: None,
            ui_visible: true,
            in_scratch: StereoScratchBuffer::new(max_block),
            raw_in_scratch: StereoScratchBuffer::new(max_block),
            retired_tx: None,
            retired_slot_tx: None,
            retired_params_tx: None,
            retired_cfg_tx: None,
            xrun_count: 0,
            plugin_chain_latency_samples: 0,
            latency_scratch: Vec::with_capacity(32),
        }
    }

    pub fn attach_retired_tx(&mut self, tx: Producer<Vec<LiveRack>>) {
        self.retired_tx = Some(tx);
    }

    pub fn attach_retired_aux_tx(
        &mut self,
        slot_tx: Producer<Box<LiveSlot>>,
        params_tx: Producer<Box<LiveSessionParams>>,
        cfg_tx: Producer<Box<SessionConfig>>,
    ) {
        self.retired_slot_tx = Some(slot_tx);
        self.retired_params_tx = Some(params_tx);
        self.retired_cfg_tx = Some(cfg_tx);
    }

    pub fn attach_tuner_tx(&mut self, tx: Producer<f32>) {
        self.tuner_tx = Some(tx);
    }

    pub fn ui_visible(&self) -> bool {
        self.ui_visible
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn reconfigure(&mut self, sample_rate: f64, max_block_size: usize) {
        self.sample_rate = sample_rate.max(8_000.0);
        self.max_block_size = max_block_size.max(64);
        self.tuner.set_sample_rate(self.sample_rate);
        self.in_scratch.ensure_capacity(self.max_block_size);
        self.raw_in_scratch.ensure_capacity(self.max_block_size);
        for rack in &mut self.racks {
            rack.ensure_capacity(self.max_block_size);
            for slot in &mut rack.slots {
                let _ = slot.instance.prepare(self.sample_rate, self.max_block_size);
                slot.latency_samples = slot.instance.latency_samples();
            }
        }
        self.plugin_chain_latency_samples = compute_chain_latency_samples(
            &self.racks,
            &self.rack_exec_order,
            &mut self.latency_scratch,
        );
    }

    fn refresh_topology_cache(&mut self) {
        resolve_routing_indices(&mut self.racks);
        compute_rack_waves(
            &self.racks,
            &mut self.rack_exec_order,
            &mut self.rack_waves,
            &mut self.rack_wave_count,
            &mut self.topo_in_degree,
            &mut self.topo_visited,
        );
        self.plugin_chain_latency_samples = compute_chain_latency_samples(
            &self.racks,
            &self.rack_exec_order,
            &mut self.latency_scratch,
        );
        let needs_pool = self.rack_waves[..self.rack_wave_count]
            .iter()
            .any(|w| w.len() >= 2);
        if needs_pool && self.worker_pool.is_none() && RackWorkerPool::desired_worker_count() > 0 {
            self.worker_pool = Some(RackWorkerPool::new());
        }
    }

    pub fn apply_command(&mut self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::ReplaceRacks {
                mut racks,
                master_gain_db,
            } => {
                for rack in &mut racks {
                    rack.ensure_capacity(self.max_block_size);
                    for slot in &mut rack.slots {
                        let _ = slot.instance.prepare(self.sample_rate, self.max_block_size);
                        slot.latency_samples = slot.instance.latency_samples();
                        slot.refresh_cached_dsp();
                    }
                }
                let old_racks = std::mem::replace(&mut self.racks, racks);
                if !old_racks.is_empty() {
                    if let Some(ref mut tx) = self.retired_tx {
                        let _ = tx.push(old_racks);
                    }
                }
                self.master_gain_db = master_gain_db;
                self.master_linear_gain = db_to_linear(master_gain_db);
                self.refresh_topology_cache();
            }
            EngineCommand::AddRack { mut rack } => {
                rack.ensure_capacity(self.max_block_size);
                for slot in &mut rack.slots {
                    let _ = slot.instance.prepare(self.sample_rate, self.max_block_size);
                    slot.latency_samples = slot.instance.latency_samples();
                    slot.refresh_cached_dsp();
                }
                self.racks.push(*rack);
                self.refresh_topology_cache();
            }
            EngineCommand::RemoveRack { rack_id } => {
                if let Some(pos) = self.racks.iter().position(|r| r.id == rack_id) {
                    let removed = self.racks.remove(pos);
                    if let Some(ref mut tx) = self.retired_tx {
                        let _ = tx.push(vec![removed]);
                    }
                    self.refresh_topology_cache();
                }
            }
            EngineCommand::ReorderRacks { order } => {
                let mut reordered = Vec::with_capacity(self.racks.len());
                for id in order {
                    if let Some(pos) = self.racks.iter().position(|r| r.id == id) {
                        reordered.push(self.racks.remove(pos));
                    }
                }
                reordered.append(&mut self.racks);
                self.racks = reordered;
                self.refresh_topology_cache();
            }
            EngineCommand::AddSlot {
                rack_id,
                index,
                mut slot,
            } => {
                if let Some(rack) = self.racks.iter_mut().find(|r| r.id == rack_id) {
                    let _ = slot.instance.prepare(self.sample_rate, self.max_block_size);
                    slot.latency_samples = slot.instance.latency_samples();
                    slot.refresh_cached_dsp();
                    let insert_at = index.unwrap_or(rack.slots.len()).min(rack.slots.len());
                    rack.slots.insert(insert_at, *slot);
                    self.refresh_topology_cache();
                } else if let Some(ref mut tx) = self.retired_slot_tx {
                    let _ = tx.push(slot);
                }
            }
            EngineCommand::RemoveSlot { rack_id, slot_id } => {
                if let Some(rack) = self.racks.iter_mut().find(|r| r.id == rack_id) {
                    if let Some(pos) = rack.slots.iter().position(|s| s.id == slot_id) {
                        let removed = rack.slots.remove(pos);
                        if let Some(ref mut tx) = self.retired_slot_tx {
                            let _ = tx.push(Box::new(removed));
                        }
                        self.refresh_topology_cache();
                    }
                }
            }
            EngineCommand::ReorderSlots { rack_id, order } => {
                if let Some(rack) = self.racks.iter_mut().find(|r| r.id == rack_id) {
                    let mut reordered = Vec::with_capacity(rack.slots.len());
                    for id in order {
                        if let Some(pos) = rack.slots.iter().position(|s| s.id == id) {
                            reordered.push(rack.slots.remove(pos));
                        }
                    }
                    reordered.append(&mut rack.slots);
                    rack.slots = reordered;
                    self.refresh_topology_cache();
                }
            }
            EngineCommand::SyncSessionParams { config } => {
                let params = LiveSessionParams::from_session(&config, true);
                self.sync_live_params(&params);
                self.refresh_topology_cache();
                if let Some(ref mut tx) = self.retired_cfg_tx {
                    let _ = tx.push(config);
                }
            }
            EngineCommand::SyncLiveParams { params } => {
                self.sync_live_params(&params);
                self.refresh_topology_cache();
                if let Some(ref mut tx) = self.retired_params_tx {
                    let _ = tx.push(params);
                }
            }
            EngineCommand::SetMasterGainDb(db) => {
                self.master_gain_db = db;
                self.master_linear_gain = db_to_linear(db);
            }
            EngineCommand::SetInputConfig {
                enabled,
                gain_db,
                mode,
            } => {
                self.input_enabled = enabled;
                self.input_gain_db = gain_db;
                self.input_linear_gain = db_to_linear(gain_db);
                self.input_mode = mode;
            }
            EngineCommand::SetInputCaptureActive(_) => {}
            EngineCommand::SetOutputMuted(muted) => self.output_muted = muted,
            EngineCommand::SetTestTone(enabled) => {
                self.test_tone_enabled = enabled;
                if !enabled {
                    self.test_tone_phase = 0.0;
                }
            }
            EngineCommand::SetTunerEnabled(enabled) => {
                self.tuner_enabled = enabled;
                if !enabled {
                    self.tuner.reset();
                }
            }
            EngineCommand::SetUiVisible(visible) => {
                self.ui_visible = visible;
            }
            EngineCommand::PanicReset => {
                self.test_tone_enabled = false;
                self.test_tone_phase = 0.0;
                self.in_scratch.clear(self.max_block_size);
                self.raw_in_scratch.clear(self.max_block_size);
                self.reconfigure(self.sample_rate, self.max_block_size);
            }
            EngineCommand::SetSlotParameter {
                rack_id,
                slot_id,
                param_id,
                value,
            } => {
                if let Some(slot) = self.find_slot_mut(rack_id, slot_id) {
                    slot.instance.set_parameter(param_id, value);
                }
            }
        }
    }

    fn sync_live_params(&mut self, params: &LiveSessionParams) {
        self.master_gain_db = params.master_gain_db;
        self.master_linear_gain = db_to_linear(params.master_gain_db);
        for cfg_rack in &params.racks {
            let Some(live_rack) = self.racks.iter_mut().find(|r| r.id == cfg_rack.id) else {
                continue;
            };
            live_rack.enabled = cfg_rack.enabled;
            live_rack.muted = cfg_rack.muted;
            live_rack.solo = cfg_rack.solo;
            live_rack.width = cfg_rack.width.clamp(0.0, 2.0);
            live_rack.receive_input = cfg_rack.receive_input;
            live_rack.route_to_master = cfg_rack.route_to_master;
            live_rack.linear_gain = db_to_linear(cfg_rack.gain_db);
            let (pl, pr) = equal_power_pan(cfg_rack.pan);
            live_rack.pan_l = pl;
            live_rack.pan_r = pr;

            if live_rack.sends.len() == cfg_rack.sends.len() {
                for (dst, src) in live_rack.sends.iter_mut().zip(&cfg_rack.sends) {
                    dst.target_rack_id = src.target_rack_id;
                    dst.linear_gain = db_to_linear(src.level_db);
                    dst.enabled = src.enabled;
                }
            } else {
                live_rack.sends.clear();
                live_rack
                    .sends
                    .extend(cfg_rack.sends.iter().map(LiveSend::from_params));
            }

            for cfg_slot in &cfg_rack.slots {
                let Some(live_slot) = live_rack.slots.iter_mut().find(|s| s.id == cfg_slot.id)
                else {
                    continue;
                };
                live_slot.enabled = cfg_slot.enabled;
                live_slot.bypassed = cfg_slot.bypassed;
                live_slot.gain_db = cfg_slot.gain_db;
                live_slot.pan = cfg_slot.pan;
                live_slot.wet_mix = cfg_slot.wet_mix;
                live_slot.sidechain = cfg_slot.sidechain.clone();
                live_slot.refresh_cached_dsp();
                for &(param_id, val) in &cfg_slot.params {
                    live_slot.instance.set_parameter(param_id, val);
                }
            }
        }
    }

    fn find_slot_mut(&mut self, rack_id: Uuid, slot_id: Uuid) -> Option<&mut LiveSlot> {
        self.racks
            .iter_mut()
            .find(|r| r.id == rack_id)
            .and_then(|r| r.slots.iter_mut().find(|s| s.id == slot_id))
    }

    pub fn write_rack_meters(&self, mut emit: impl FnMut(RackMeterSnapshot)) {
        for rack in &self.racks {
            emit(RackMeterSnapshot {
                rack_id: rack.id,
                peak_l: rack.last_peak_l,
                peak_r: rack.last_peak_r,
            });
        }
    }

    pub fn write_accumulated_rack_meters(&mut self, mut emit: impl FnMut(RackMeterSnapshot)) {
        for rack in &mut self.racks {
            let (peak_l, peak_r) = rack.take_accumulated_peaks();
            emit(RackMeterSnapshot {
                rack_id: rack.id,
                peak_l,
                peak_r,
            });
        }
    }

    pub fn process_block(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) -> AudioTelemetry {
        let start = Instant::now();
        let frames = out_l.len().min(out_r.len()).min(self.max_block_size);
        out_l[..frames].fill(0.0);
        out_r[..frames].fill(0.0);

        let (in_peak_l, in_peak_r) = self.prepare_input_buffer(in_l, in_r, frames);
        let tuner = if self.tuner_enabled {
            if let Some(ref mut tx) = self.tuner_tx {
                if self.ui_visible {
                    let l_buf = &self.in_scratch.left[..frames];
                    let r_buf = &self.in_scratch.right[..frames];
                    for i in 0..frames {
                        if tx.push(0.5 * (l_buf[i] + r_buf[i])).is_err() {
                            break;
                        }
                    }
                }
                TunerReading::default()
            } else {
                self.tuner
                    .push_samples(&self.in_scratch.left[..frames], &self.in_scratch.right[..frames])
            }
        } else {
            TunerReading::default()
        };

        for rack in &mut self.racks {
            rack.bus_in.clear(frames);
            rack.pre_fader_out.clear(frames);
        }

        let any_solo = self.racks.iter().any(|r| r.enabled && r.solo);
        let racks_len = self.racks.len();
        for wave_idx in 0..self.rack_wave_count {
            self.active_wave_scratch.clear();
            for &r_idx in &self.rack_waves[wave_idx] {
                if r_idx >= racks_len {
                    continue;
                }
                let rack = &mut self.racks[r_idx];
                if !rack.enabled || rack.muted || (any_solo && !rack.solo) {
                    rack.scratch_a.clear(frames);
                    rack.pre_fader_out.clear(frames);
                    rack.last_peak_l = 0.0;
                    rack.last_peak_r = 0.0;
                } else {
                    self.active_wave_scratch.push(r_idx);
                }
            }

            let active_count = self.active_wave_scratch.len();
            if active_count == 0 {
                continue;
            }
            if active_count == 1 || self.worker_pool.is_none() {
                let racks_ptr = self.racks.as_mut_ptr();
                let in_l_slice = &self.in_scratch.left[..frames];
                let in_r_slice = &self.in_scratch.right[..frames];
                for &r_idx in &self.active_wave_scratch {
                    // SAFETY: Each `r_idx` in a wave is distinct and reads only earlier-wave racks.
                    unsafe {
                        process_single_rack_dsp(
                            racks_ptr,
                            racks_len,
                            r_idx,
                            in_l_slice,
                            in_r_slice,
                            &self.raw_in_scratch,
                            frames,
                        );
                    }
                }
            } else if let Some(ref mut pool) = self.worker_pool {
                let ctx = ParallelWaveCtx {
                    racks_ptr: self.racks.as_mut_ptr(),
                    racks_len,
                    active_indices_ptr: self.active_wave_scratch.as_ptr(),
                    in_l_ptr: self.in_scratch.left.as_ptr(),
                    in_r_ptr: self.in_scratch.right.as_ptr(),
                    raw_in_ptr: &raw const self.raw_in_scratch,
                    frames,
                };
                pool.dispatch_and_wait(
                    active_count,
                    (&raw const ctx).cast::<()>(),
                    run_parallel_rack_task,
                );
            }

            for &r_idx in &self.active_wave_scratch {
                mix_rack_to_master(&self.racks[r_idx], out_l, out_r, frames);
                emit_rack_sends(&mut self.racks, r_idx, frames);
            }
        }

        let master_gain = if self.output_muted {
            0.0
        } else {
            self.master_linear_gain
        };
        let (mut m_peak_l, mut m_peak_r) = (0.0f32, 0.0f32);
        for i in 0..frames {
            let l = soft_limit(out_l[i] * master_gain);
            let r = soft_limit(out_r[i] * master_gain);
            out_l[i] = l;
            out_r[i] = r;
            m_peak_l = m_peak_l.max(l.abs());
            m_peak_r = m_peak_r.max(r.abs());
        }

        let elapsed_sec = start.elapsed().as_secs_f64();
        let budget_sec = (frames as f64) / self.sample_rate;
        let cpu_load_pct = ((elapsed_sec / budget_sec.max(1e-6)) * 100.0) as f32;
        if cpu_load_pct >= 100.0 {
            self.xrun_count += 1;
        }

        AudioTelemetry {
            input_peak_l: in_peak_l,
            input_peak_r: in_peak_r,
            master_peak_l: m_peak_l,
            master_peak_r: m_peak_r,
            cpu_load_pct,
            xrun_count: self.xrun_count,
            sample_rate: self.sample_rate as u32,
            buffer_frames: frames as u32,
            plugin_latency_samples: self.plugin_chain_latency_samples,
            tuner,
        }
    }

    fn prepare_input_buffer(&mut self, in_l: &[f32], in_r: &[f32], frames: usize) -> (f32, f32) {
        let (mut in_peak_l, mut in_peak_r) = (0.0f32, 0.0f32);
        if self.input_enabled {
            let in_gain = self.input_linear_gain;
            for i in 0..frames {
                let raw_l = in_l.get(i).copied().unwrap_or(0.0);
                let raw_r = in_r.get(i).copied().unwrap_or(0.0);
                self.raw_in_scratch.left[i] = raw_l * in_gain;
                self.raw_in_scratch.right[i] = raw_r * in_gain;
                let (l, r) = match self.input_mode {
                    InputChannelMode::Stereo => (raw_l, raw_r),
                    InputChannelMode::MonoLeft => (raw_l, raw_l),
                    InputChannelMode::MonoRight => (raw_r, raw_r),
                    InputChannelMode::MonoSum => {
                        let m = 0.5 * (raw_l + raw_r);
                        (m, m)
                    }
                };
                self.in_scratch.left[i] = l * in_gain;
                self.in_scratch.right[i] = r * in_gain;
            }
        } else {
            self.in_scratch.clear(frames);
            self.raw_in_scratch.clear(frames);
        }

        if self.test_tone_enabled {
            // 440 Hz reference sine at ~ -18 dBFS (0.125 amplitude) fed through FX racks.
            let phase_inc = 440.0 / (self.sample_rate.max(8_000.0) as f32);
            for i in 0..frames {
                let s = 0.125 * (self.test_tone_phase * TAU).sin();
                self.test_tone_phase += phase_inc;
                if self.test_tone_phase >= 1.0 {
                    self.test_tone_phase -= 1.0;
                }
                self.in_scratch.left[i] += s;
                self.in_scratch.right[i] += s;
                self.raw_in_scratch.left[i] += s;
                self.raw_in_scratch.right[i] += s;
            }
        }

        if self.input_enabled || self.test_tone_enabled {
            for i in 0..frames {
                in_peak_l = in_peak_l.max(self.in_scratch.left[i].abs());
                in_peak_r = in_peak_r.max(self.in_scratch.right[i].abs());
            }
        }
        (in_peak_l, in_peak_r)
    }
}

fn resolve_routing_indices(racks: &mut [LiveRack]) {
    let n = racks.len();
    for r_idx in 0..n {
        let slot_count = racks[r_idx].slots.len();
        for s_idx in 0..slot_count {
            let target_id = match racks[r_idx].slots[s_idx].sidechain {
                Some(ref sc) => match sc.source {
                    SidechainSourceKind::Rack { rack_id, .. } => Some(rack_id),
                    SidechainSourceKind::LiveInput { .. } => None,
                },
                None => None,
            };
            let resolved = target_id
                .and_then(|id| racks.iter().position(|r| r.id == id))
                .filter(|&idx| idx != r_idx);
            racks[r_idx].slots[s_idx].sidechain_rack_idx = resolved;
        }
        let send_count = racks[r_idx].sends.len();
        for send_idx in 0..send_count {
            let target_id = racks[r_idx].sends[send_idx].target_rack_id;
            let resolved = racks
                .iter()
                .position(|r| r.id == target_id)
                .filter(|&idx| idx != r_idx);
            racks[r_idx].sends[send_idx].target_rack_idx = resolved;
        }
    }
}

fn compute_rack_waves(
    racks: &[LiveRack],
    out_order: &mut Vec<usize>,
    out_waves: &mut Vec<Vec<usize>>,
    out_wave_count: &mut usize,
    in_degree: &mut Vec<usize>,
    visited: &mut Vec<bool>,
) {
    out_order.clear();
    *out_wave_count = 0;
    let n = racks.len();
    if n == 0 {
        return;
    }

    in_degree.clear();
    in_degree.resize(n, 0);
    visited.clear();
    visited.resize(n, false);

    for (src_idx, rack) in racks.iter().enumerate() {
        for send in &rack.sends {
            if !send.enabled {
                continue;
            }
            if let Some(dst_idx) = send.target_rack_idx {
                if dst_idx != src_idx {
                    in_degree[dst_idx] += 1;
                }
            }
        }
    }
    for (dst_idx, rack) in racks.iter().enumerate() {
        for slot in &rack.slots {
            if let Some(ref sc) = slot.sidechain {
                if !sc.enabled {
                    continue;
                }
                if let Some(src_idx) = slot.sidechain_rack_idx {
                    if src_idx != dst_idx {
                        in_degree[dst_idx] += 1;
                    }
                }
            }
        }
    }

    while out_order.len() < n {
        if *out_wave_count >= out_waves.len() {
            out_waves.push(Vec::with_capacity(n));
        }
        let wave = &mut out_waves[*out_wave_count];
        wave.clear();

        for i in 0..n {
            if !visited[i] && in_degree[i] == 0 {
                wave.push(i);
            }
        }
        if wave.is_empty() {
            // Break cycles via a singleton sequential wave so no intra-wave dependency exists.
            if let Some(i) = (0..n).find(|&i| !visited[i]) {
                wave.push(i);
            } else {
                break;
            }
        }

        for &picked in wave.iter() {
            visited[picked] = true;
            out_order.push(picked);
        }
        for &picked in wave.iter() {
            for send in &racks[picked].sends {
                if !send.enabled {
                    continue;
                }
                if let Some(dst_idx) = send.target_rack_idx {
                    if dst_idx != picked && in_degree[dst_idx] > 0 {
                        in_degree[dst_idx] -= 1;
                    }
                }
            }
            for (dst_idx, rack) in racks.iter().enumerate() {
                if dst_idx == picked {
                    continue;
                }
                for slot in &rack.slots {
                    if let Some(ref sc) = slot.sidechain {
                        if sc.enabled
                            && slot.sidechain_rack_idx == Some(picked)
                            && in_degree[dst_idx] > 0
                        {
                            in_degree[dst_idx] -= 1;
                        }
                    }
                }
            }
        }
        *out_wave_count += 1;
    }
}

fn compute_chain_latency_samples(
    racks: &[LiveRack],
    exec_order: &[usize],
    rack_in_delays: &mut Vec<u32>,
) -> u32 {
    const NO_SIGNAL: u32 = u32::MAX;
    rack_in_delays.clear();
    rack_in_delays.resize(racks.len(), NO_SIGNAL);
    let any_solo = racks.iter().any(|r| r.enabled && r.solo);
    let mut max_total = 0u32;

    for &r_idx in exec_order {
        if r_idx >= racks.len() {
            continue;
        }
        let rack = &racks[r_idx];
        if !rack.enabled || rack.muted || (any_solo && !rack.solo) {
            continue;
        }
        let in_delay = if rack_in_delays[r_idx] != NO_SIGNAL {
            rack_in_delays[r_idx]
        } else if rack.receive_input {
            0
        } else {
            // Bus-only rack with no live input and no active upstream send carries no audio.
            continue;
        };
        let rack_slot_delay = rack
            .slots
            .iter()
            .filter(|s| s.enabled && !s.bypassed)
            .map(|s| s.latency_samples)
            .fold(0u32, u32::saturating_add);
        let out_delay = in_delay.saturating_add(rack_slot_delay).min(NO_SIGNAL - 1);
        if rack.route_to_master {
            max_total = max_total.max(out_delay);
        }

        for send in &rack.sends {
            if !send.enabled {
                continue;
            }
            if let Some(dst_idx) = send.target_rack_idx {
                if dst_idx != r_idx {
                    let prev = rack_in_delays[dst_idx];
                    rack_in_delays[dst_idx] = if prev == NO_SIGNAL {
                        out_delay
                    } else {
                        prev.max(out_delay)
                    };
                }
            }
        }
    }
    max_total
}

fn emit_rack_sends(racks: &mut [LiveRack], src_idx: usize, frames: usize) {
    let send_count = racks[src_idx].sends.len();
    for s_idx in 0..send_count {
        let send = &racks[src_idx].sends[s_idx];
        if !send.enabled {
            continue;
        }
        let Some(dst_idx) = send.target_rack_idx else {
            continue;
        };
        let send_gain = send.linear_gain;
        if dst_idx == src_idx || dst_idx >= racks.len() {
            continue;
        }
        accumulate_rack_bus(racks, src_idx, dst_idx, send_gain, frames);
    }
}

fn accumulate_rack_bus(
    racks: &mut [LiveRack],
    src_idx: usize,
    dst_idx: usize,
    send_gain: f32,
    frames: usize,
) {
    if src_idx < dst_idx {
        let (left_slice, right_slice) = racks.split_at_mut(dst_idx);
        let src = &left_slice[src_idx].scratch_a;
        let dst = &mut right_slice[0].bus_in;
        for i in 0..frames {
            dst.left[i] += src.left[i] * send_gain;
            dst.right[i] += src.right[i] * send_gain;
        }
    } else {
        let (left_slice, right_slice) = racks.split_at_mut(src_idx);
        let dst = &mut left_slice[dst_idx].bus_in;
        let src = &right_slice[0].scratch_a;
        for i in 0..frames {
            dst.left[i] += src.left[i] * send_gain;
            dst.right[i] += src.right[i] * send_gain;
        }
    }
}

struct ParallelWaveCtx {
    racks_ptr: *mut LiveRack,
    racks_len: usize,
    active_indices_ptr: *const usize,
    in_l_ptr: *const f32,
    in_r_ptr: *const f32,
    raw_in_ptr: *const StereoScratchBuffer,
    frames: usize,
}

fn run_parallel_rack_task(ctx_ptr: *const (), task_idx: usize) {
    // SAFETY: `ctx_ptr` and its pointed buffers remain valid for the duration of `dispatch_and_wait`,
    // and each `r_idx` in `active_indices_ptr` is unique and disjoint from earlier-wave sidechain sources.
    unsafe {
        let ctx = &*ctx_ptr.cast::<ParallelWaveCtx>();
        let r_idx = *ctx.active_indices_ptr.add(task_idx);
        let in_l = std::slice::from_raw_parts(ctx.in_l_ptr, ctx.frames);
        let in_r = std::slice::from_raw_parts(ctx.in_r_ptr, ctx.frames);
        let raw_in = &*ctx.raw_in_ptr;
        process_single_rack_dsp(
            ctx.racks_ptr,
            ctx.racks_len,
            r_idx,
            in_l,
            in_r,
            raw_in,
            ctx.frames,
        );
    }
}

/// # Safety
/// `racks_ptr` must point to `racks_len` initialized `LiveRack` entries, `rack_idx < racks_len`,
/// and no concurrent thread may mutably access `racks[src_idx]` for any sidechain source `src_idx`.
unsafe fn fill_sidechain_scratch(
    racks_ptr: *const LiveRack,
    racks_len: usize,
    rack_idx: usize,
    rack: &mut LiveRack,
    slot_idx: usize,
    raw_in: &StereoScratchBuffer,
    frames: usize,
) -> bool {
    let slot = &rack.slots[slot_idx];
    let Some(ref sc) = slot.sidechain else {
        return false;
    };
    if !sc.enabled {
        return false;
    }
    let gain = slot.sc_linear_gain;
    match sc.source {
        SidechainSourceKind::Rack { pre_fader, .. } => {
            let Some(src_idx) = slot.sidechain_rack_idx else {
                return false;
            };
            if src_idx == rack_idx || src_idx >= racks_len {
                return false;
            }
            // SAFETY: `src_idx != rack_idx` and `src_idx < racks_len`, belonging to an earlier wave.
            let src_rack = unsafe { &*racks_ptr.add(src_idx) };
            let src_buf = if pre_fader {
                &src_rack.pre_fader_out
            } else {
                &src_rack.scratch_a
            };
            for i in 0..frames {
                rack.sc_scratch.left[i] = src_buf.left[i] * gain;
                rack.sc_scratch.right[i] = src_buf.right[i] * gain;
            }
            true
        }
        SidechainSourceKind::LiveInput { mode } => {
            for i in 0..frames {
                let raw_l = raw_in.left[i];
                let raw_r = raw_in.right[i];
                let (l, r) = match mode {
                    InputChannelMode::Stereo => (raw_l, raw_r),
                    InputChannelMode::MonoLeft => (raw_l, raw_l),
                    InputChannelMode::MonoRight => (raw_r, raw_r),
                    InputChannelMode::MonoSum => {
                        let m = 0.5 * (raw_l + raw_r);
                        (m, m)
                    }
                };
                rack.sc_scratch.left[i] = l * gain;
                rack.sc_scratch.right[i] = r * gain;
            }
            true
        }
    }
}

/// # Safety
/// `racks_ptr` must point to `racks_len` initialized `LiveRack` entries, `r_idx < racks_len`,
/// `racks_ptr.add(r_idx)` must not be aliased by any other thread, and any sidechain source rack
/// referenced by `r_idx` must belong to a completed earlier wave.
unsafe fn process_single_rack_dsp(
    racks_ptr: *mut LiveRack,
    racks_len: usize,
    r_idx: usize,
    in_l: &[f32],
    in_r: &[f32],
    raw_in: &StereoScratchBuffer,
    frames: usize,
) {
    // SAFETY: Caller guarantees exclusive access to `racks[r_idx]`.
    let rack = unsafe { &mut *racks_ptr.add(r_idx) };

    if rack.receive_input {
        let copy_l = in_l.len().min(frames);
        let copy_r = in_r.len().min(frames);
        for i in 0..copy_l {
            rack.scratch_a.left[i] = in_l[i] + rack.bus_in.left[i];
        }
        if copy_l < frames {
            rack.scratch_a.left[copy_l..frames].copy_from_slice(&rack.bus_in.left[copy_l..frames]);
        }
        for i in 0..copy_r {
            rack.scratch_a.right[i] = in_r[i] + rack.bus_in.right[i];
        }
        if copy_r < frames {
            rack.scratch_a.right[copy_r..frames]
                .copy_from_slice(&rack.bus_in.right[copy_r..frames]);
        }
    } else {
        rack.scratch_a.left[..frames].copy_from_slice(&rack.bus_in.left[..frames]);
        rack.scratch_a.right[..frames].copy_from_slice(&rack.bus_in.right[..frames]);
    }

    let slot_count = rack.slots.len();
    for s_idx in 0..slot_count {
        if !rack.slots[s_idx].enabled || rack.slots[s_idx].bypassed {
            continue;
        }
        // SAFETY: `racks_ptr` is immutable for earlier-wave racks while `rack` is exclusively borrowed.
        let has_sc = unsafe {
            fill_sidechain_scratch(
                racks_ptr.cast_const(),
                racks_len,
                r_idx,
                rack,
                s_idx,
                raw_in,
                frames,
            )
        };
        rack.scratch_b.clear(frames);
        let ins: [&[f32]; 2] = [&rack.scratch_a.left[..frames], &rack.scratch_a.right[..frames]];
        let mut outs: [&mut [f32]; 2] = [
            &mut rack.scratch_b.left[..frames],
            &mut rack.scratch_b.right[..frames],
        ];
        if has_sc {
            let sc_ins: [&[f32]; 2] =
                [&rack.sc_scratch.left[..frames], &rack.sc_scratch.right[..frames]];
            rack.slots[s_idx]
                .instance
                .process_with_sidechain(&ins, Some(&sc_ins), &mut outs);
        } else {
            rack.slots[s_idx]
                .instance
                .process_with_sidechain(&ins, None, &mut outs);
        }

        let slot = &rack.slots[s_idx];
        let wet = slot.wet;
        let dry = slot.dry;
        let gain_l = slot.linear_gain * slot.pan_l;
        let gain_r = slot.linear_gain * slot.pan_r;
        for i in 0..frames {
            let mixed_l = rack.scratch_a.left[i] * dry + rack.scratch_b.left[i] * wet;
            let mixed_r = rack.scratch_a.right[i] * dry + rack.scratch_b.right[i] * wet;
            rack.scratch_a.left[i] = mixed_l * gain_l;
            rack.scratch_a.right[i] = mixed_r * gain_r;
        }
    }

    rack.pre_fader_out.left[..frames].copy_from_slice(&rack.scratch_a.left[..frames]);
    rack.pre_fader_out.right[..frames].copy_from_slice(&rack.scratch_a.right[..frames]);

    let gain_l = rack.linear_gain * rack.pan_l;
    let gain_r = rack.linear_gain * rack.pan_r;
    let width = rack.width;
    let (mut r_peak_l, mut r_peak_r) = (0.0f32, 0.0f32);
    for i in 0..frames {
        let mid = 0.5 * (rack.scratch_a.left[i] + rack.scratch_a.right[i]);
        let side = 0.5 * (rack.scratch_a.left[i] - rack.scratch_a.right[i]) * width;
        let sl = (mid + side) * gain_l;
        let sr = (mid - side) * gain_r;
        rack.scratch_a.left[i] = sl;
        rack.scratch_a.right[i] = sr;
        r_peak_l = r_peak_l.max(sl.abs());
        r_peak_r = r_peak_r.max(sr.abs());
    }
    rack.last_peak_l = r_peak_l;
    rack.last_peak_r = r_peak_r;
    rack.accum_peak_l = rack.accum_peak_l.max(r_peak_l);
    rack.accum_peak_r = rack.accum_peak_r.max(r_peak_r);
}

#[inline]
fn mix_rack_to_master(rack: &LiveRack, out_l: &mut [f32], out_r: &mut [f32], frames: usize) {
    if !rack.route_to_master {
        return;
    }
    for i in 0..frames {
        out_l[i] += rack.scratch_a.left[i];
        out_r[i] += rack.scratch_a.right[i];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_host::create_plugin_instance;
    use rtrb::RingBuffer;

    fn build_test_rack(rack_cfg: &RackConfig) -> LiveRack {
        let mut slots = Vec::new();
        for s in &rack_cfg.slots {
            let (mut inst, _ui) = create_plugin_instance(&s.kind).expect("builtin");
            inst.prepare(48_000.0, 512).expect("prepare");
            let latency_samples = inst.latency_samples();
            slots.push(LiveSlot::new(
                s.id,
                s.enabled,
                s.bypassed,
                s.gain_db,
                s.pan,
                s.wet_mix,
                latency_samples,
                s.sidechain.clone(),
                inst,
            ));
        }
        LiveRack::new(rack_cfg.clone(), slots, 512)
    }

    #[test]
    fn audio_graph_processes_effect_rack_and_retires_old_racks() {
        let session = SessionConfig::default();
        let rack_cfg = session.racks[0].clone();

        let (retired_tx, mut retired_rx) = RingBuffer::<Vec<LiveRack>>::new(4);
        let mut graph = AudioGraphProcessor::new(48_000.0, 512);
        graph.attach_retired_tx(retired_tx);
        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![build_test_rack(&rack_cfg)],
            master_gain_db: 0.0,
        });

        let in_l = vec![0.4f32; 512];
        let in_r = vec![-0.4f32; 512];
        let mut out_l = vec![0.0f32; 512];
        let mut out_r = vec![0.0f32; 512];
        let telemetry = graph.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
        assert!(telemetry.input_peak_l > 0.35);
        assert!(telemetry.master_peak_l > 0.1);
        assert!(telemetry.cpu_load_pct < 100.0);

        graph.apply_command(EngineCommand::SetInputConfig {
            enabled: false,
            gain_db: 0.0,
            mode: InputChannelMode::Stereo,
        });
        let muted = graph.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
        assert_eq!(muted.input_peak_l, 0.0);

        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![build_test_rack(&rack_cfg)],
            master_gain_db: 0.0,
        });
        assert!(retired_rx.pop().is_ok(), "replaced racks must be sent back for UI cleanup");
    }

    #[test]
    fn panic_reset_clears_delay_tail_and_respects_solo_mute() {
        let mut rack_a = SessionConfig::default().racks.remove(0);
        rack_a.name = "Rack A".to_string();
        let mut rack_b = SessionConfig::default().racks.remove(0);
        rack_b.name = "Rack B".to_string();
        rack_b.solo = true;

        let mut graph = AudioGraphProcessor::new(48_000.0, 512);
        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![build_test_rack(&rack_a), build_test_rack(&rack_b)],
            master_gain_db: 0.0,
        });

        let impulse_l = vec![0.8f32; 512];
        let impulse_r = vec![0.8f32; 512];
        let mut out_l = vec![0.0f32; 512];
        let mut out_r = vec![0.0f32; 512];
        let _ = graph.process_block(&impulse_l, &impulse_r, &mut out_l, &mut out_r);

        let mut meters = Vec::new();
        graph.write_rack_meters(|snap| meters.push((snap.rack_id, snap.peak_l)));
        let peak_a = meters
            .iter()
            .find(|&&(id, _)| id == rack_a.id)
            .map(|&(_, p)| p)
            .unwrap_or(-1.0);
        let peak_b = meters
            .iter()
            .find(|&&(id, _)| id == rack_b.id)
            .map(|&(_, p)| p)
            .unwrap_or(-1.0);
        assert_eq!(peak_a, 0.0, "non-solo rack should be silenced when solo is active");
        assert!(peak_b > 0.1, "solo rack should remain audible");

        // Feed silence through the 240 ms delay window (~25 blocks) to observe the delay tail,
        // then verify PanicReset zeroes out the plugin buffers immediately.
        let silence = vec![0.0f32; 512];
        let mut max_tail_peak = 0.0f32;
        for _ in 0..26 {
            let tail = graph.process_block(&silence, &silence, &mut out_l, &mut out_r);
            max_tail_peak = max_tail_peak.max(tail.master_peak_l);
        }
        assert!(max_tail_peak > 0.01, "expected delay tail before PanicReset");

        graph.apply_command(EngineCommand::PanicReset);
        for _ in 0..26 {
            let cleared = graph.process_block(&silence, &silence, &mut out_l, &mut out_r);
            assert_eq!(cleared.master_peak_l, 0.0);
            assert_eq!(cleared.master_peak_r, 0.0);
        }
    }

    #[test]
    fn mono_input_mode_and_test_tone_route_to_both_channels() {
        let rack_cfg = SessionConfig::default().racks.remove(0);
        let mut graph = AudioGraphProcessor::new(48_000.0, 256);
        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![build_test_rack(&rack_cfg)],
            master_gain_db: 0.0,
        });
        graph.apply_command(EngineCommand::SetInputConfig {
            enabled: true,
            gain_db: 0.0,
            mode: InputChannelMode::MonoLeft,
        });

        let in_l = vec![0.5f32; 256];
        let in_r = vec![0.0f32; 256];
        let mut out_l = vec![0.0f32; 256];
        let mut out_r = vec![0.0f32; 256];
        let mono_tel = graph.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
        assert!((mono_tel.input_peak_r - 0.5).abs() < 1e-4, "MonoLeft must mirror In 1 to R");
        assert!(mono_tel.master_peak_r > 0.1);

        graph.apply_command(EngineCommand::SetInputConfig {
            enabled: false,
            gain_db: 0.0,
            mode: InputChannelMode::Stereo,
        });
        graph.apply_command(EngineCommand::SetTestTone(true));
        let tone_tel = graph.process_block(&in_r, &in_r, &mut out_l, &mut out_r);
        assert!(tone_tel.input_peak_l > 0.1);
        assert!(tone_tel.master_peak_l > 0.05);

        graph.apply_command(EngineCommand::PanicReset);
        for _ in 0..26 {
            let _ = graph.process_block(&in_r, &in_r, &mut out_l, &mut out_r);
        }
        let silent = graph.process_block(&in_r, &in_r, &mut out_l, &mut out_r);
        assert_eq!(silent.input_peak_l, 0.0);
        assert_eq!(silent.master_peak_l, 0.0);
    }

    #[test]
    fn rack_aux_send_routes_into_bus_and_applies_width() {
        use session_core::RackSendConfig;

        let mut bus_rack = RackConfig::new("Aux Bus");
        bus_rack.receive_input = false;
        bus_rack.width = 0.0; // Mono collapse M/S check

        let mut src_rack = RackConfig::new("Dry Source");
        src_rack.muted = false;
        src_rack.sends.push(RackSendConfig {
            target_rack_id: bus_rack.id,
            level_db: 0.0,
            enabled: true,
        });

        // Order Bus BEFORE Source intentionally to verify topological ordering runs Source first!
        let mut graph = AudioGraphProcessor::new(48_000.0, 256);
        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![build_test_rack(&bus_rack), build_test_rack(&src_rack)],
            master_gain_db: 0.0,
        });

        let in_l = vec![0.4f32; 256];
        let in_r = vec![0.0f32; 256];
        let mut out_l = vec![0.0f32; 256];
        let mut out_r = vec![0.0f32; 256];
        let _ = graph.process_block(&in_l, &in_r, &mut out_l, &mut out_r);

        let mut meters = Vec::new();
        graph.write_rack_meters(|snap| meters.push((snap.rack_id, snap.peak_l, snap.peak_r)));
        let bus = meters.iter().find(|m| m.0 == bus_rack.id).copied().expect("bus meter");
        // Aux Bus received 0.4 on L and 0.0 on R from Source, collapsed to Mono (width=0 -> L=R=0.2)
        assert!(bus.1 > 0.10, "aux bus should receive send from source rack, got {}", bus.1);
        assert!(
            (bus.1 - bus.2).abs() < 1e-4,
            "width=0 should collapse bus to mono: L={} R={}",
            bus.1,
            bus.2
        );
    }

    #[test]
    fn output_mute_silences_speakers_while_keeping_input_and_rack_meters() {
        let rack_cfg = SessionConfig::default().racks.remove(0);
        let rack_id = rack_cfg.id;
        let mut graph = AudioGraphProcessor::new(48_000.0, 256);
        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![build_test_rack(&rack_cfg)],
            master_gain_db: 0.0,
        });
        graph.apply_command(EngineCommand::SetOutputMuted(true));

        let in_l = vec![0.5f32; 256];
        let in_r = vec![-0.5f32; 256];
        let mut out_l = vec![0.0f32; 256];
        let mut out_r = vec![0.0f32; 256];
        let tel = graph.process_block(&in_l, &in_r, &mut out_l, &mut out_r);

        assert!(tel.input_peak_l > 0.4, "input meter must stay active when output is muted");
        assert_eq!(tel.master_peak_l, 0.0, "master output must be silent when muted");
        assert_eq!(tel.master_peak_r, 0.0, "master output must be silent when muted");

        let mut rack_peak = 0.0f32;
        graph.write_rack_meters(|snap| {
            if snap.rack_id == rack_id {
                rack_peak = snap.peak_l;
            }
        });
        assert!(rack_peak > 0.1, "rack meter must stay active when monitor output is muted");
    }

    struct LatencyMockPlugin {
        desc: plugin_host::PluginDescriptor,
        latency: u32,
    }

    impl LatencyMockPlugin {
        fn new(latency: u32) -> Self {
            Self {
                desc: plugin_host::PluginDescriptor {
                    format: plugin_host::PluginFormat::BuiltIn,
                    id: "test.latency_mock".to_string(),
                    name: "LatencyMock".to_string(),
                    vendor: "Test".to_string(),
                    version: "1.0".to_string(),
                    path: None,
                    is_instrument: false,
                    audio_inputs: 2,
                    audio_outputs: 2,
                    has_sidechain: false,
                },
                latency,
            }
        }
    }

    impl plugin_host::AudioPluginInstance for LatencyMockPlugin {
        fn descriptor(&self) -> &plugin_host::PluginDescriptor {
            &self.desc
        }

        fn prepare(
            &mut self,
            _sample_rate: f64,
            _max_block_size: usize,
        ) -> Result<(), plugin_host::PluginError> {
            Ok(())
        }

        fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
            for (ch, out) in outputs.iter_mut().enumerate() {
                if let Some(inp) = inputs.get(ch) {
                    let n = out.len().min(inp.len());
                    out[..n].copy_from_slice(&inp[..n]);
                }
            }
        }

        fn latency_samples(&self) -> u32 {
            self.latency
        }

        fn parameters(&self) -> Vec<plugin_host::PluginParamInfo> {
            Vec::new()
        }

        fn set_parameter(&mut self, _id: u32, _normalized: f32) {}
    }

    fn make_latency_slot(id: uuid::Uuid, latency: u32, enabled: bool, bypassed: bool) -> LiveSlot {
        LiveSlot::new(
            id,
            enabled,
            bypassed,
            0.0,
            0.0,
            1.0,
            latency,
            None,
            Box::new(LatencyMockPlugin::new(latency)),
        )
    }

    #[test]
    fn chain_latency_sums_active_slots_and_propagates_aux_sends() {
        let mut src_cfg = RackConfig::new("Source");
        let mut aux_cfg = RackConfig::new("Aux");
        aux_cfg.receive_input = false;
        src_cfg.sends.push(session_core::RackSendConfig {
            target_rack_id: aux_cfg.id,
            level_db: 0.0,
            enabled: true,
        });

        let src_rack = LiveRack::new(
            src_cfg.clone(),
            vec![
                make_latency_slot(uuid::Uuid::new_v4(), 128, true, false),
                make_latency_slot(uuid::Uuid::new_v4(), 64, true, true), // bypassed
                make_latency_slot(uuid::Uuid::new_v4(), 32, false, false), // disabled
                make_latency_slot(uuid::Uuid::new_v4(), 256, true, false),
            ],
            256,
        );
        let aux_rack = LiveRack::new(
            aux_cfg.clone(),
            vec![make_latency_slot(uuid::Uuid::new_v4(), 512, true, false)],
            256,
        );

        let mut graph = AudioGraphProcessor::new(48_000.0, 256);
        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![src_rack, aux_rack],
            master_gain_db: 0.0,
        });

        let in_l = vec![0.0f32; 128];
        let in_r = vec![0.0f32; 128];
        let mut out_l = vec![0.0f32; 128];
        let mut out_r = vec![0.0f32; 128];
        let tel = graph.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
        // Active in Source = 128 + 256 = 384; sent to Aux (+512) => 896 samples.
        assert_eq!(tel.plugin_latency_samples, 896);

        // Disabling aux send falls back to Source active delay (384).
        let mut updated = SessionConfig::default();
        src_cfg.sends[0].enabled = false;
        updated.racks = vec![src_cfg, aux_cfg];
        graph.apply_command(EngineCommand::SyncSessionParams {
            config: Box::new(updated),
        });
        let tel2 = graph.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
        assert_eq!(tel2.plugin_latency_samples, 384);
    }

    #[test]
    fn rack_to_rack_sidechain_respects_topo_order_and_pre_fader_tap() {
        use session_core::{BuiltInEffectKind, PluginKind, PluginSlotConfig};

        // Target rack receives live input (or test tone) and holds a compressor sidechained by Key rack.
        let mut comp_slot = PluginSlotConfig::new(
            "Ducker",
            PluginKind::BuiltInEffect(BuiltInEffectKind::DynamicsGateComp),
        );
        comp_slot.params.insert(0, -80.0); // Gate off
        comp_slot.params.insert(1, -24.0); // Comp thresh -24 dB
        comp_slot.params.insert(2, 8.0);   // 8:1 ratio

        let mut key_rack = RackConfig::new("Key Source");
        key_rack.receive_input = false;
        key_rack.gain_db = -48.0; // Fader pulled down on Key rack
        let mut key_gain = PluginSlotConfig::new(
            "Key Tone Gen",
            PluginKind::BuiltInEffect(BuiltInEffectKind::GainLimiter),
        );
        key_gain.params.insert(0, 1.0);
        key_rack.slots.push(key_gain);

        // Route live input to Key rack via an Aux Source rack? Better: test LiveInput MonoRight SC
        // AND Rack-to-Rack SC! First test LiveInput MonoRight while main input is MonoLeft.
        comp_slot.sidechain = Some(SidechainConfig {
            enabled: true,
            source: SidechainSourceKind::LiveInput {
                mode: InputChannelMode::MonoRight,
            },
            gain_db: 0.0,
        });

        let mut target_rack = RackConfig::new("Pad Target");
        target_rack.slots.push(comp_slot);

        let mut graph = AudioGraphProcessor::new(48_000.0, 512);
        graph.apply_command(EngineCommand::SetInputConfig {
            enabled: true,
            gain_db: 0.0,
            mode: InputChannelMode::MonoLeft,
        });
        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![build_test_rack(&target_rack)],
            master_gain_db: 0.0,
        });
        let sync_cfg = SessionConfig {
            racks: vec![target_rack.clone()],
            ..Default::default()
        };
        graph.apply_command(EngineCommand::SyncSessionParams {
            config: Box::new(sync_cfg),
        });

        // Main signal on L is quiet (0.04); Sidechain key on R is loud (0.8).
        let in_l = vec![0.04f32; 512];
        let in_r_silent = vec![0.0f32; 512];
        let in_r_loud = vec![0.8f32; 512];
        let mut out_l = vec![0.0f32; 512];
        let mut out_r = vec![0.0f32; 512];

        for _ in 0..4 {
            let _ = graph.process_block(&in_l, &in_r_silent, &mut out_l, &mut out_r);
        }
        let unducked_peak = out_l[256..].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            (unducked_peak - 0.04).abs() < 0.005,
            "expected unducked main signal ~0.04 when SC channel R is silent, got {unducked_peak}"
        );

        for _ in 0..4 {
            let _ = graph.process_block(&in_l, &in_r_loud, &mut out_l, &mut out_r);
        }
        let ducked_peak = out_l[256..].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            ducked_peak < 0.018,
            "expected MonoRight sidechain key to duck MonoLeft rack signal, got {ducked_peak}"
        );

        // Now test Rack-to-Rack Pre-Fader sidechain where Target appears BEFORE Key rack (topo order!).
        key_rack.receive_input = true;
        target_rack.slots[0].sidechain = Some(SidechainConfig {
            enabled: true,
            source: SidechainSourceKind::Rack {
                rack_id: key_rack.id,
                pre_fader: true,
            },
            gain_db: 0.0,
        });
        graph.apply_command(EngineCommand::SetInputConfig {
            enabled: true,
            gain_db: 0.0,
            mode: InputChannelMode::Stereo,
        });
        let mut target_live = build_test_rack(&target_rack);
        target_live.slots[0].instance.set_parameter(0, -80.0);
        target_live.slots[0].instance.set_parameter(1, -24.0);
        target_live.slots[0].instance.set_parameter(2, 8.0);
        let key_live = build_test_rack(&key_rack);
        // Intentionally place Target before Key to verify topological sorting runs Key first.
        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![target_live, key_live],
            master_gain_db: 0.0,
        });

        let hot = vec![0.8f32; 512];
        for _ in 0..4 {
            let _ = graph.process_block(&hot, &hot, &mut out_l, &mut out_r);
        }
        let mut meters = Vec::new();
        graph.write_rack_meters(|snap| meters.push((snap.rack_id, snap.peak_l)));
        let target_peak = meters.iter().find(|m| m.0 == target_rack.id).unwrap().1;
        // Key rack has gain_db = -48 dB, but pre_fader = true taps its 0.8 pre-fader output (~tanh(0.8)=0.66),
        // which compresses target_rack heavily below 0.35!
        assert!(
            target_peak < 0.35,
            "pre-fader rack sidechain should duck target despite -48 dB fader, got {target_peak}"
        );
    }

    #[test]
    fn rack_unrouted_from_master_only_feeds_connected_bus() {
        let mut mic_rack = RackConfig::new("Mic");
        mic_rack.route_to_master = false;
        let mut bus_rack = RackConfig::new("Bus Comp");
        bus_rack.receive_input = false;
        bus_rack.route_to_master = true;
        bus_rack.gain_db = -6.0;
        mic_rack.sends.push(session_core::RackSendConfig {
            target_rack_id: bus_rack.id,
            level_db: 0.0,
            enabled: true,
        });

        let mut graph = AudioGraphProcessor::new(48_000.0, 256);
        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![
                LiveRack::new(mic_rack, vec![], 256),
                LiveRack::new(bus_rack, vec![], 256),
            ],
            master_gain_db: 0.0,
        });

        let in_l = vec![0.5f32; 128];
        let in_r = vec![0.5f32; 128];
        let mut out_l = vec![0.0f32; 128];
        let mut out_r = vec![0.0f32; 128];
        let tel = graph.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
        // Mic (0.5) is unrouted from Master and sent 0 dB to Bus Comp (-6 dB => ~0.25).
        assert!(
            (tel.master_peak_l - 0.25).abs() < 0.02,
            "expected only bus output ~0.25 at master, got {}",
            tel.master_peak_l
        );
    }

    #[test]
    fn incremental_commands_preserve_existing_racks_and_retire_removed_items_off_thread() {
        use session_core::{BuiltInEffectKind, PluginKind, PluginSlotConfig};

        let mut session = SessionConfig::default();
        let first_rack = session.racks.remove(0);
        let first_rack_id = first_rack.id;
        let second_rack = RackConfig::new("Empty Aux".to_string());
        let second_rack_id = second_rack.id;

        let (retired_racks_tx, mut retired_racks_rx) = RingBuffer::<Vec<LiveRack>>::new(4);
        let (retired_slots_tx, mut retired_slots_rx) = RingBuffer::<Box<LiveSlot>>::new(4);
        let (retired_params_tx, mut retired_params_rx) =
            RingBuffer::<Box<LiveSessionParams>>::new(4);
        let (retired_cfg_tx, _retired_cfg_rx) = RingBuffer::<Box<SessionConfig>>::new(4);

        let mut graph = AudioGraphProcessor::new(48_000.0, 256);
        graph.attach_retired_tx(retired_racks_tx);
        graph.attach_retired_aux_tx(retired_slots_tx, retired_params_tx, retired_cfg_tx);

        // Add first rack incrementally
        graph.apply_command(EngineCommand::AddRack {
            rack: Box::new(build_test_rack(&first_rack)),
        });
        // Add second rack at end
        graph.apply_command(EngineCommand::AddRack {
            rack: Box::new(LiveRack::new(second_rack.clone(), vec![], 256)),
        });
        assert_eq!(graph.racks.len(), 2);

        // Add a slot onto second rack without touching first rack
        let mut slot_cfg = PluginSlotConfig::new(
            "StageHost 3-Band EQ".to_string(),
            PluginKind::BuiltInEffect(BuiltInEffectKind::ThreeBandEq),
        );
        let slot_id = slot_cfg.id;
        slot_cfg.wet_mix = 1.0;
        let (mut eq, _ui) = create_plugin_instance(&slot_cfg.kind).expect("builtin eq");
        eq.prepare(48_000.0, 256).expect("prepare eq");
        let latency = eq.latency_samples();
        graph.apply_command(EngineCommand::AddSlot {
            rack_id: second_rack_id,
            index: Some(0),
            slot: Box::new(LiveSlot::new(
                slot_cfg.id,
                slot_cfg.enabled,
                slot_cfg.bypassed,
                slot_cfg.gain_db,
                slot_cfg.pan,
                slot_cfg.wet_mix,
                latency,
                slot_cfg.sidechain.clone(),
                eq,
            )),
        });
        assert_eq!(graph.racks[1].slots.len(), 1);

        // Sync live params with updated master gain and verify Box<LiveSessionParams> retires off-thread
        let mut params_cfg = SessionConfig {
            master_gain_db: -6.0,
            racks: vec![first_rack.clone(), second_rack],
            ..SessionConfig::default()
        };
        params_cfg.racks[1].slots.push(slot_cfg);
        graph.apply_command(EngineCommand::SyncLiveParams {
            params: Box::new(LiveSessionParams::from_session(&params_cfg, false)),
        });
        assert!(
            retired_params_rx.pop().is_ok(),
            "applied LiveSessionParams box must retire off the audio thread"
        );
        assert!(
            (graph.master_gain_db - (-6.0)).abs() < 0.001,
            "SyncLiveParams should update master gain without replacing racks"
        );

        // Reorder racks: swap first and second
        graph.apply_command(EngineCommand::ReorderRacks {
            order: vec![second_rack_id, first_rack_id],
        });
        assert_eq!(graph.racks[0].id, second_rack_id);
        assert_eq!(graph.racks[1].id, first_rack_id);

        // Remove the added slot and verify it is retired off-thread
        graph.apply_command(EngineCommand::RemoveSlot {
            rack_id: second_rack_id,
            slot_id,
        });
        assert!(graph.racks[0].slots.is_empty());
        assert!(
            retired_slots_rx.pop().is_ok(),
            "removed slot should retire off audio thread"
        );

        // Remove second rack incrementally
        graph.apply_command(EngineCommand::RemoveRack {
            rack_id: second_rack_id,
        });
        assert_eq!(graph.racks.len(), 1);
        assert!(
            retired_racks_rx.pop().is_ok(),
            "removed rack should retire off audio thread"
        );
    }

    #[test]
    fn parallel_independent_racks_and_downstream_aux_wave_process_accurately() {
        let bus_id = uuid::Uuid::new_v4();
        let rack_a_id = uuid::Uuid::new_v4();
        let rack_b_id = uuid::Uuid::new_v4();

        let mut rack_a = RackConfig::new("Rack A");
        rack_a.id = rack_a_id;
        rack_a.gain_db = -6.0;
        rack_a.sends.push(RackSendConfig {
            target_rack_id: bus_id,
            level_db: 0.0,
            enabled: true,
        });

        let mut rack_b = RackConfig::new("Rack B");
        rack_b.id = rack_b_id;
        rack_b.gain_db = -6.0;
        rack_b.sends.push(RackSendConfig {
            target_rack_id: bus_id,
            level_db: 0.0,
            enabled: true,
        });

        let mut bus_rack = RackConfig::new("Aux Bus");
        bus_rack.id = bus_id;
        bus_rack.receive_input = false;
        bus_rack.route_to_master = true;
        bus_rack.gain_db = 0.0;

        let mut graph = AudioGraphProcessor::new(48_000.0, 256);
        graph.apply_command(EngineCommand::ReplaceRacks {
            // Put bus first to verify topological wave grouping runs [rack_a, rack_b] in wave 0 and [bus] in wave 1.
            racks: vec![
                build_test_rack(&bus_rack),
                build_test_rack(&rack_a),
                build_test_rack(&rack_b),
            ],
            master_gain_db: 0.0,
        });

        assert_eq!(graph.rack_wave_count, 2);
        assert_eq!(graph.rack_waves[0].len(), 2);
        assert_eq!(graph.rack_waves[1], vec![0]);

        let in_l = vec![0.25f32; 128];
        let in_r = vec![0.25f32; 128];
        let mut out_l = vec![0.0f32; 128];
        let mut out_r = vec![0.0f32; 128];
        graph.process_block(&in_l, &in_r, &mut out_l, &mut out_r);

        // Each of Rack A & B outputs ~0.12529 (-6 dB), routed to master AND sent at 0 dB to Aux Bus.
        // Total before soft_limit = 4 * (0.25 * 10^(-6/20)) ≈ 0.501187.
        let expected_linear = soft_limit(4.0 * 0.25 * db_to_linear(-6.0));
        assert!(
            (out_l[10] - expected_linear).abs() < 1e-4,
            "expected {expected_linear}, got {}",
            out_l[10]
        );
        assert!((out_r[10] - expected_linear).abs() < 1e-4);
    }

    #[test]
    fn cyclic_send_topology_breaks_into_singleton_waves() {
        let id_a = uuid::Uuid::new_v4();
        let id_b = uuid::Uuid::new_v4();
        let mut rack_a = RackConfig::new("Cycle A");
        rack_a.id = id_a;
        rack_a.sends.push(RackSendConfig {
            target_rack_id: id_b,
            level_db: -6.0,
            enabled: true,
        });
        let mut rack_b = RackConfig::new("Cycle B");
        rack_b.id = id_b;
        rack_b.sends.push(RackSendConfig {
            target_rack_id: id_a,
            level_db: -6.0,
            enabled: true,
        });

        let mut graph = AudioGraphProcessor::new(48_000.0, 128);
        graph.apply_command(EngineCommand::ReplaceRacks {
            racks: vec![build_test_rack(&rack_a), build_test_rack(&rack_b)],
            master_gain_db: 0.0,
        });
        assert_eq!(graph.rack_wave_count, 2);
        assert_eq!(graph.rack_waves[0].len(), 1);
        assert_eq!(graph.rack_waves[1].len(), 1);

        let in_l = vec![0.2f32; 64];
        let in_r = vec![0.2f32; 64];
        let mut out_l = vec![0.0f32; 64];
        let mut out_r = vec![0.0f32; 64];
        graph.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
        assert!(out_l[0].is_finite());
        assert!(out_r[0].is_finite());
    }
}

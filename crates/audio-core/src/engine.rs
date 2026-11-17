use std::collections::HashMap;
use std::process::Command;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, Host, SampleFormat, Stream, StreamConfig};
use rtrb::{Consumer, Producer, RingBuffer};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::asio::{self, AsioDriverMetadata, AsioStartConfig, AsioStreamSession};
use crate::graph::{
    AudioGraphProcessor, AudioTelemetry, EngineCommand, LiveRack, LiveSessionParams, LiveSlot,
    RackMeterSnapshot,
};
use crate::mmcss::{
    disable_process_power_throttling, ensure_pro_audio_for_current_thread, DenormalGuard,
};
use crate::tuner::{StageTuner, TunerReading};
use crate::AudioError;

pub const ASIO_PREFIX: &str = "ASIO - ";
pub const WASAPI_PREFIX: &str = "WASAPI - ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDriverBackend {
    Wasapi,
    Asio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub id_string: String,
    pub name: String,
    pub backend: AudioDriverBackend,
    pub is_default: bool,
    pub min_sample_rate: u32,
    pub max_sample_rate: u32,
}

#[derive(Debug, Clone)]
pub struct DriverNotes {
    pub title: String,
    pub backend: AudioDriverBackend,
    pub summary_lines: Vec<String>,
    pub input_channels: usize,
    pub output_channels: usize,
    pub input_channel_names: Vec<String>,
    pub output_channel_names: Vec<String>,
    pub supported_sample_rates: Vec<u32>,
    pub min_buffer_size: u32,
    pub max_buffer_size: u32,
    pub preferred_buffer_size: u32,
}

/// Parses a persisted/selected driver identifier into backend + raw driver/device name.
pub fn parse_driver_selection(selected: Option<&str>) -> (AudioDriverBackend, Option<&str>) {
    let Some(raw) = selected.map(str::trim).filter(|s| !s.is_empty()) else {
        return (AudioDriverBackend::Wasapi, None);
    };
    if let Some(asio_name) = raw.strip_prefix(ASIO_PREFIX) {
        return (AudioDriverBackend::Asio, Some(asio_name));
    }
    if let Some(wasapi_name) = raw.strip_prefix(WASAPI_PREFIX) {
        if wasapi_name == "<System Default>" {
            return (AudioDriverBackend::Wasapi, None);
        }
        return (AudioDriverBackend::Wasapi, Some(wasapi_name));
    }
    (AudioDriverBackend::Wasapi, Some(raw))
}

/// Manages real-time ASIO / WASAPI streams and lock-free communication rings with the UI thread.
pub struct AudioEngine {
    stream: Option<Stream>,
    input_stream: Option<Stream>,
    input_stream_active: bool,
    asio_session: Option<AsioStreamSession>,
    cmd_tx: Option<Producer<EngineCommand>>,
    telemetry_rx: Option<Consumer<AudioTelemetry>>,
    rack_meters_rx: Option<Consumer<RackMeterSnapshot>>,
    retired_racks_rx: Option<Consumer<Vec<LiveRack>>>,
    retired_slots_rx: Option<Consumer<Box<LiveSlot>>>,
    retired_params_rx: Option<Consumer<Box<LiveSessionParams>>>,
    retired_cfg_rx: Option<Consumer<Box<session_core::SessionConfig>>>,
    tuner_rx: Option<Consumer<f32>>,
    active_backend: AudioDriverBackend,
    active_device_name: String,
    active_input_device_name: String,
    active_sample_rate: u32,
    active_buffer_frames: u32,
    active_driver_notes: Option<DriverNotes>,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    pub fn new() -> Self {
        Self {
            stream: None,
            input_stream: None,
            input_stream_active: true,
            asio_session: None,
            cmd_tx: None,
            telemetry_rx: None,
            rack_meters_rx: None,
            retired_racks_rx: None,
            retired_slots_rx: None,
            retired_params_rx: None,
            retired_cfg_rx: None,
            tuner_rx: None,
            active_backend: AudioDriverBackend::Wasapi,
            active_device_name: String::new(),
            active_input_device_name: String::new(),
            active_sample_rate: 48_000,
            active_buffer_frames: 256,
            active_driver_notes: None,
        }
    }

    /// Lists unified output drivers: installed ASIO drivers followed by WASAPI output devices.
    pub fn list_output_devices() -> Result<Vec<AudioDeviceInfo>, AudioError> {
        let mut combined = Vec::new();
        for drv in asio::enumerate_asio_drivers() {
            combined.push(AudioDeviceInfo {
                id_string: format!("{ASIO_PREFIX}{}", drv.display_name),
                name: drv.display_name,
                backend: AudioDriverBackend::Asio,
                is_default: false,
                min_sample_rate: 44_100,
                max_sample_rate: 192_000,
            });
        }
        if let Ok(wasapi_devs) = enumerate_devices(&cpal::default_host(), false) {
            combined.extend(wasapi_devs);
        }
        Ok(combined)
    }

    pub fn list_input_devices() -> Result<Vec<AudioDeviceInfo>, AudioError> {
        enumerate_devices(&cpal::default_host(), true)
    }

    pub fn start(
        &mut self,
        preferred_device: Option<&str>,
        preferred_input_device: Option<&str>,
        requested_sample_rate: Option<u32>,
        requested_buffer_frames: Option<u32>,
    ) -> Result<(), AudioError> {
        self.start_with_channels(
            preferred_device,
            preferred_input_device,
            requested_sample_rate,
            requested_buffer_frames,
            0,
            0,
        )
    }

    pub fn start_with_channels(
        &mut self,
        preferred_device: Option<&str>,
        preferred_input_device: Option<&str>,
        requested_sample_rate: Option<u32>,
        requested_buffer_frames: Option<u32>,
        asio_input_channel: usize,
        asio_output_channel: usize,
    ) -> Result<(), AudioError> {
        self.stop();
        disable_process_power_throttling();

        let (backend, parsed_name) = parse_driver_selection(preferred_device);
        match backend {
            AudioDriverBackend::Asio => {
                let driver_name = parsed_name.ok_or(AudioError::NoDefaultOutputDevice)?;
                self.start_asio(
                    driver_name,
                    requested_sample_rate,
                    requested_buffer_frames,
                    asio_input_channel,
                    asio_output_channel,
                )
            }
            AudioDriverBackend::Wasapi => self.start_wasapi(
                parsed_name,
                preferred_input_device,
                requested_sample_rate,
                requested_buffer_frames,
            ),
        }
    }

    fn start_asio(
        &mut self,
        driver_name: &str,
        requested_sample_rate: Option<u32>,
        requested_buffer_frames: Option<u32>,
        input_channel_offset: usize,
        output_channel_offset: usize,
    ) -> Result<(), AudioError> {
        let (cmd_tx, cmd_rx) = RingBuffer::<EngineCommand>::new(256);
        let (telemetry_tx, telemetry_rx) = RingBuffer::<AudioTelemetry>::new(64);
        let (rack_meters_tx, rack_meters_rx) = RingBuffer::<RackMeterSnapshot>::new(256);
        let (retired_tx, retired_rx) = RingBuffer::<Vec<LiveRack>>::new(16);
        let (retired_slot_tx, retired_slot_rx) = RingBuffer::<Box<LiveSlot>>::new(32);
        let (retired_params_tx, retired_params_rx) = RingBuffer::<Box<LiveSessionParams>>::new(64);
        let (retired_cfg_tx, retired_cfg_rx) = RingBuffer::<Box<session_core::SessionConfig>>::new(16);
        let (tuner_tx, tuner_rx) = RingBuffer::<f32>::new(8192);

        let outcome = AsioStreamSession::start(AsioStartConfig {
            driver_name: driver_name.to_string(),
            requested_sample_rate,
            requested_buffer_frames,
            input_channel_offset,
            output_channel_offset,
            cmd_rx,
            telemetry_tx,
            rack_meters_tx,
            retired_tx,
            retired_slot_tx,
            retired_params_tx,
            retired_cfg_tx,
            tuner_tx,
        })?;

        let display_id = format!("{ASIO_PREFIX}{driver_name}");
        let notes = notes_from_asio_metadata(&display_id, &outcome.metadata);
        self.asio_session = Some(outcome.session);
        self.cmd_tx = Some(cmd_tx);
        self.telemetry_rx = Some(telemetry_rx);
        self.rack_meters_rx = Some(rack_meters_rx);
        self.retired_racks_rx = Some(retired_rx);
        self.retired_slots_rx = Some(retired_slot_rx);
        self.retired_params_rx = Some(retired_params_rx);
        self.retired_cfg_rx = Some(retired_cfg_rx);
        self.tuner_rx = Some(tuner_rx);
        self.active_backend = AudioDriverBackend::Asio;
        self.active_device_name = display_id;
        self.active_input_device_name = format!(
            "ASIO Ch {}-{}",
            input_channel_offset + 1,
            input_channel_offset + 2
        );
        self.active_sample_rate = outcome.active_sample_rate;
        self.active_buffer_frames = outcome.active_buffer_frames;
        self.active_driver_notes = Some(notes);
        Ok(())
    }

    fn start_wasapi(
        &mut self,
        preferred_device: Option<&str>,
        preferred_input_device: Option<&str>,
        requested_sample_rate: Option<u32>,
        requested_buffer_frames: Option<u32>,
    ) -> Result<(), AudioError> {
        let host = cpal::default_host();
        let (device, device_name, default_cfg) = select_output_device(&host, preferred_device)?;
        let sample_rate = requested_sample_rate.unwrap_or(default_cfg.sample_rate());
        let buffer_frames = requested_buffer_frames.unwrap_or(256);
        let channels = default_cfg.channels().max(2);

        let (cmd_tx, mut cmd_rx) = RingBuffer::<EngineCommand>::new(256);
        let (mut telemetry_tx, telemetry_rx) = RingBuffer::<AudioTelemetry>::new(64);
        let (rack_meters_tx, rack_meters_rx) = RingBuffer::<RackMeterSnapshot>::new(256);
        let (retired_tx, retired_rx) = RingBuffer::<Vec<LiveRack>>::new(16);
        let (retired_slot_tx, retired_slot_rx) = RingBuffer::<Box<LiveSlot>>::new(32);
        let (retired_params_tx, retired_params_rx) = RingBuffer::<Box<LiveSessionParams>>::new(64);
        let (retired_cfg_tx, retired_cfg_rx) = RingBuffer::<Box<session_core::SessionConfig>>::new(16);
        let (tuner_tx, tuner_rx) = RingBuffer::<f32>::new(8192);

        let ring_frames = (buffer_frames as usize * 8).max(4096);
        let (in_ring_tx, in_ring_rx) = RingBuffer::<[f32; 2]>::new(ring_frames);
        let (input_stream, input_name, in_sample_rate) = try_start_input_stream(
            &host,
            preferred_input_device,
            sample_rate,
            buffer_frames,
            in_ring_tx,
        )?;

        let max_block = buffer_frames.max(2048) as usize;
        let mut graph = AudioGraphProcessor::new(sample_rate as f64, max_block);
        graph.attach_retired_tx(retired_tx);
        graph.attach_retired_aux_tx(retired_slot_tx, retired_params_tx, retired_cfg_tx);
        graph.attach_tuner_tx(tuner_tx);

        let mut in_resampler = InputReader::new(in_ring_rx, in_sample_rate, sample_rate, max_block);
        let mut out_l = vec![0.0f32; max_block];
        let mut out_r = vec![0.0f32; max_block];
        let mut rack_meter_tx_opt = Some(rack_meters_tx);
        let mut decimator = TelemetryDecimator::new(sample_rate);

        let stream_config = StreamConfig {
            channels,
            sample_rate,
            buffer_size: BufferSize::Fixed(buffer_frames),
        };
        let stream_name = device_name.clone();
        let build_res = match default_cfg.sample_format() {
            SampleFormat::F32 => device.build_output_stream(
                stream_config,
                move |data: &mut [f32], _| {
                    ensure_pro_audio_for_current_thread();
                    run_audio_callback_f32(
                        data,
                        channels as usize,
                        &mut graph,
                        &mut cmd_rx,
                        &mut telemetry_tx,
                        &mut rack_meter_tx_opt,
                        &mut decimator,
                        &mut in_resampler,
                        &mut out_l,
                        &mut out_r,
                    );
                },
                move |err| {
                    error!("Audio output stream error on '{stream_name}': {err}");
                },
                None,
            ),
            other => {
                return Err(AudioError::ConfigQuery {
                    device_name,
                    detail: format!("unsupported sample format {other:?}; expected F32"),
                });
            }
        };

        let stream = build_res.map_err(|e| AudioError::StreamFailure {
            context: "build_output_stream",
            detail: e.to_string(),
        })?;
        let mut input_active = false;
        if let Some(ref in_s) = input_stream {
            if let Err(err) = in_s.play() {
                warn!("Failed to start WASAPI input stream '{input_name}': {err}");
            } else {
                input_active = true;
            }
        }
        stream.play().map_err(|e| AudioError::StreamFailure {
            context: "stream::play",
            detail: e.to_string(),
        })?;

        let display_id = format!("{WASAPI_PREFIX}{device_name}");
        let (min_sr, max_sr) = query_sample_rate_bounds(&device, false);
        let notes = notes_for_wasapi(&display_id, channels as usize, min_sr, max_sr, &input_name);
        self.stream = Some(stream);
        self.input_stream = input_stream;
        self.input_stream_active = input_active;
        self.cmd_tx = Some(cmd_tx);
        self.telemetry_rx = Some(telemetry_rx);
        self.rack_meters_rx = Some(rack_meters_rx);
        self.retired_racks_rx = Some(retired_rx);
        self.retired_slots_rx = Some(retired_slot_rx);
        self.retired_params_rx = Some(retired_params_rx);
        self.retired_cfg_rx = Some(retired_cfg_rx);
        self.tuner_rx = Some(tuner_rx);
        self.active_backend = AudioDriverBackend::Wasapi;
        self.active_device_name = display_id;
        self.active_input_device_name = input_name;
        self.active_sample_rate = sample_rate;
        self.active_buffer_frames = buffer_frames;
        self.active_driver_notes = Some(notes);

        info!(
            "Started WASAPI output '{}' (input: '{}') @ {}Hz ({} frames, {} ch)",
            self.active_device_name,
            if self.active_input_device_name.is_empty() {
                "none"
            } else {
                &self.active_input_device_name
            },
            sample_rate,
            buffer_frames,
            channels
        );
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(mut asio) = self.asio_session.take() {
            asio.stop();
        }
        if self.input_stream.take().is_some() {
            info!("Stopped input stream '{}'", self.active_input_device_name);
        }
        self.input_stream_active = false;
        if self.stream.take().is_some() {
            info!("Stopped audio stream '{}'", self.active_device_name);
        }
        self.poll_retired_racks();
        self.cmd_tx = None;
        self.telemetry_rx = None;
        self.rack_meters_rx = None;
        self.retired_racks_rx = None;
        self.retired_slots_rx = None;
        self.retired_params_rx = None;
        self.retired_cfg_rx = None;
        self.tuner_rx = None;
        self.active_driver_notes = None;
    }

    pub fn is_running(&self) -> bool {
        self.stream.is_some() || self.asio_session.is_some()
    }

    pub fn active_backend(&self) -> AudioDriverBackend {
        self.active_backend
    }

    pub fn active_device_name(&self) -> &str {
        &self.active_device_name
    }

    pub fn active_input_device_name(&self) -> &str {
        &self.active_input_device_name
    }

    pub fn active_driver_notes(&self) -> Option<&DriverNotes> {
        self.active_driver_notes.as_ref()
    }

    pub fn take_reset_requested(&self) -> bool {
        self.asio_session
            .as_ref()
            .is_some_and(AsioStreamSession::take_reset_requested)
    }

    /// Opens the selected ASIO driver's Control Panel or Windows Sound Control Panel for WASAPI.
    pub fn open_control_panel(&self, selected_driver: Option<&str>) -> Result<(), AudioError> {
        let (backend, parsed_name) = parse_driver_selection(selected_driver);
        match backend {
            AudioDriverBackend::Asio => {
                let name = parsed_name.ok_or(AudioError::NoDefaultOutputDevice)?;
                let running_same = self.asio_session.as_ref().filter(|_| {
                    self.active_backend == AudioDriverBackend::Asio
                        && self.active_device_name == format!("{ASIO_PREFIX}{name}")
                });
                if let Some(session) = running_same {
                    session.open_control_panel()
                } else {
                    asio::open_asio_control_panel_detached(name)
                }
            }
            AudioDriverBackend::Wasapi => {
                Command::new("control")
                    .arg("mmsys.cpl,,0")
                    .spawn()
                    .map_err(|e| AudioError::StreamFailure {
                        context: "open_sound_control_panel",
                        detail: e.to_string(),
                    })?;
                Ok(())
            }
        }
    }

    /// Probes driver capabilities and technical notes for the selected driver.
    pub fn query_driver_notes(&self, selected_driver: Option<&str>) -> Result<DriverNotes, AudioError> {
        if let Some(live) = &self.active_driver_notes {
            if let Some(sel) = selected_driver {
                if live.title == sel {
                    return Ok(live.clone());
                }
            } else if self.is_running() {
                return Ok(live.clone());
            }
        }

        let (backend, parsed_name) = parse_driver_selection(selected_driver);
        match backend {
            AudioDriverBackend::Asio => {
                let name = parsed_name.ok_or(AudioError::NoDefaultOutputDevice)?;
                let meta = asio::probe_asio_driver(name)?;
                Ok(notes_from_asio_metadata(&format!("{ASIO_PREFIX}{name}"), &meta))
            }
            AudioDriverBackend::Wasapi => {
                let host = cpal::default_host();
                let (device, device_name, default_cfg) = select_output_device(&host, parsed_name)?;
                let (min_sr, max_sr) = query_sample_rate_bounds(&device, false);
                Ok(notes_for_wasapi(
                    &format!("{WASAPI_PREFIX}{device_name}"),
                    default_cfg.channels() as usize,
                    min_sr,
                    max_sr,
                    "",
                ))
            }
        }
    }

    pub fn active_sample_rate(&self) -> u32 {
        self.active_sample_rate
    }

    pub fn active_buffer_frames(&self) -> u32 {
        self.active_buffer_frames
    }

    pub fn send_command(&mut self, cmd: EngineCommand) -> Result<(), AudioError> {
        if let EngineCommand::SetInputCaptureActive(active) = cmd {
            if let Some(ref in_s) = self.input_stream {
                if active && !self.input_stream_active {
                    if let Err(err) = in_s.play() {
                        warn!("Failed to resume WASAPI input stream: {err}");
                    } else {
                        self.input_stream_active = true;
                    }
                } else if !active && self.input_stream_active {
                    if let Err(err) = in_s.pause() {
                        warn!("Failed to pause WASAPI input stream: {err}");
                    } else {
                        self.input_stream_active = false;
                    }
                }
            }
            return Ok(());
        }
        let Some(ref mut tx) = self.cmd_tx else {
            return Err(AudioError::CommandChannelClosed);
        };
        tx.push(cmd).map_err(|_| {
            warn!("Audio command ring full");
            AudioError::CommandChannelClosed
        })
    }

    pub fn poll_telemetry(&mut self) -> Option<AudioTelemetry> {
        let rx = self.telemetry_rx.as_mut()?;
        let mut latest = None;
        while let Ok(item) = rx.pop() {
            latest = Some(item);
        }
        latest
    }

    pub fn poll_tuner(&mut self, tuner: &mut StageTuner) -> Option<TunerReading> {
        let rx = self.tuner_rx.as_mut()?;
        let mut buf = [0.0f32; 512];
        let mut latest = None;
        loop {
            let mut count = 0usize;
            while count < buf.len() {
                match rx.pop() {
                    Ok(s) => {
                        buf[count] = s;
                        count += 1;
                    }
                    Err(_) => break,
                }
            }
            if count == 0 {
                break;
            }
            latest = Some(tuner.push_samples(&buf[..count], &buf[..count]));
        }
        latest
    }

    pub fn poll_rack_meters(&mut self, out_map: &mut HashMap<Uuid, (f32, f32)>) {
        let Some(ref mut rx) = self.rack_meters_rx else {
            return;
        };
        while let Ok(snap) = rx.pop() {
            out_map.insert(snap.rack_id, (snap.peak_l, snap.peak_r));
        }
    }

    /// Drops retired audio objects on the main UI thread so DLL unloading and heap frees never
    /// occur inside the real-time audio callback.
    pub fn poll_retired_racks(&mut self) {
        if let Some(ref mut rx) = self.retired_racks_rx {
            while let Ok(retired) = rx.pop() {
                drop(retired);
            }
        }
        if let Some(ref mut rx) = self.retired_slots_rx {
            while let Ok(retired) = rx.pop() {
                drop(retired);
            }
        }
        if let Some(ref mut rx) = self.retired_params_rx {
            while let Ok(retired) = rx.pop() {
                drop(retired);
            }
        }
        if let Some(ref mut rx) = self.retired_cfg_rx {
            while let Ok(retired) = rx.pop() {
                drop(retired);
            }
        }
    }
}

fn enumerate_devices(host: &Host, is_input: bool) -> Result<Vec<AudioDeviceInfo>, AudioError> {
    let default_name = if is_input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
    .and_then(|d| d.description().ok())
    .map(|desc| desc.name().to_string());

    let devices = if is_input {
        host.input_devices()
    } else {
        host.output_devices()
    }
    .map_err(|e| AudioError::StreamFailure {
        context: if is_input { "input_devices" } else { "output_devices" },
        detail: e.to_string(),
    })?;

    let mut list = Vec::new();
    for device in devices {
        let Ok(desc) = device.description() else {
            continue;
        };
        let name = desc.name().to_string();
        let (min_sr, max_sr) = query_sample_rate_bounds(&device, is_input);
        list.push(AudioDeviceInfo {
            id_string: if is_input {
                name.clone()
            } else {
                format!("{WASAPI_PREFIX}{name}")
            },
            is_default: default_name.as_deref() == Some(name.as_str()),
            backend: AudioDriverBackend::Wasapi,
            name,
            min_sample_rate: min_sr,
            max_sample_rate: max_sr,
        });
    }
    Ok(list)
}

fn query_sample_rate_bounds(device: &Device, is_input: bool) -> (u32, u32) {
    let res = if is_input {
        device.supported_input_configs().ok().map(|cfgs| {
            cfgs.fold((u32::MAX, 0u32), |(lo, hi), c| {
                (lo.min(c.min_sample_rate()), hi.max(c.max_sample_rate()))
            })
        })
    } else {
        device.supported_output_configs().ok().map(|cfgs| {
            cfgs.fold((u32::MAX, 0u32), |(lo, hi), c| {
                (lo.min(c.min_sample_rate()), hi.max(c.max_sample_rate()))
            })
        })
    };
    res.filter(|&(lo, hi)| lo <= hi).unwrap_or((44_100, 48_000))
}

fn select_output_device(
    host: &Host,
    preferred: Option<&str>,
) -> Result<(Device, String, cpal::SupportedStreamConfig), AudioError> {
    let device = if let Some(target) = preferred {
        host.output_devices()
            .map_err(|e| AudioError::StreamFailure {
                context: "output_devices",
                detail: e.to_string(),
            })?
            .find(|d| d.description().ok().map(|desc| desc.name() == target).unwrap_or(false))
            .ok_or_else(|| AudioError::DeviceNotFound {
                device_name: target.to_string(),
            })?
    } else {
        host.default_output_device()
            .ok_or(AudioError::NoDefaultOutputDevice)?
    };
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Unknown Audio Device".to_string());
    let default_cfg = device.default_output_config().map_err(|e| AudioError::ConfigQuery {
        device_name: name.clone(),
        detail: e.to_string(),
    })?;
    Ok((device, name, default_cfg))
}

fn try_start_input_stream(
    host: &Host,
    preferred_input: Option<&str>,
    target_sample_rate: u32,
    buffer_frames: u32,
    mut ring_tx: Producer<[f32; 2]>,
) -> Result<(Option<Stream>, String, u32), AudioError> {
    let device_opt = if let Some(target) = preferred_input {
        let found = host
            .input_devices()
            .map_err(|e| AudioError::StreamFailure {
                context: "input_devices",
                detail: e.to_string(),
            })?
            .find(|d| d.description().ok().map(|desc| desc.name() == target).unwrap_or(false));
        if found.is_none() {
            return Err(AudioError::DeviceNotFound {
                device_name: target.to_string(),
            });
        }
        found
    } else {
        host.default_input_device()
    };

    let Some(device) = device_opt else {
        warn!("No default WASAPI input capture device found; running output-only");
        return Ok((None, String::new(), target_sample_rate));
    };

    let device_name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Input Device".to_string());
    let Ok(default_cfg) = device.default_input_config() else {
        if preferred_input.is_some() {
            return Err(AudioError::ConfigQuery {
                device_name,
                detail: "failed to read default input format".to_string(),
            });
        }
        return Ok((None, String::new(), target_sample_rate));
    };

    let (min_sr, max_sr) = query_sample_rate_bounds(&device, true);
    let capture_sr = if target_sample_rate >= min_sr && target_sample_rate <= max_sr {
        target_sample_rate
    } else {
        default_cfg.sample_rate()
    };
    let channels = default_cfg.channels().max(1);
    let stream_cfg = StreamConfig {
        channels,
        sample_rate: capture_sr,
        buffer_size: BufferSize::Fixed(buffer_frames),
    };
    let err_name = device_name.clone();
    let ch_count = channels as usize;

    let build_res = match default_cfg.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            stream_cfg,
            move |data: &[f32], _| {
                ensure_pro_audio_for_current_thread();
                push_captured_samples(data, ch_count, &mut ring_tx);
            },
            move |err| error!("Audio input stream error on '{err_name}': {err}"),
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            stream_cfg,
            move |data: &[i16], _| {
                ensure_pro_audio_for_current_thread();
                push_captured_samples_i16(data, ch_count, &mut ring_tx);
            },
            move |err| error!("Audio input stream error on '{err_name}': {err}"),
            None,
        ),
        other => {
            let msg = format!("unsupported input sample format {other:?}; expected F32 or I16");
            if preferred_input.is_some() {
                return Err(AudioError::ConfigQuery {
                    device_name,
                    detail: msg,
                });
            }
            warn!("Skipping default input '{device_name}': {msg}");
            return Ok((None, String::new(), target_sample_rate));
        }
    };

    match build_res {
        Ok(stream) => Ok((Some(stream), device_name, capture_sr)),
        Err(err) => {
            if preferred_input.is_some() {
                Err(AudioError::StreamFailure {
                    context: "build_input_stream",
                    detail: err.to_string(),
                })
            } else {
                warn!("Running output-only; failed to open default input '{device_name}': {err}");
                Ok((None, String::new(), target_sample_rate))
            }
        }
    }
}

fn push_captured_samples(interleaved: &[f32], channels: usize, tx: &mut Producer<[f32; 2]>) {
    for chunk in interleaved.chunks_exact(channels.max(1)) {
        let l = chunk[0];
        let r = if chunk.len() > 1 { chunk[1] } else { l };
        if tx.push([l, r]).is_err() {
            break;
        }
    }
}

fn push_captured_samples_i16(interleaved: &[i16], channels: usize, tx: &mut Producer<[f32; 2]>) {
    const INV_I16: f32 = 1.0 / 32768.0;
    for chunk in interleaved.chunks_exact(channels.max(1)) {
        let l = (chunk[0] as f32) * INV_I16;
        let r = if chunk.len() > 1 {
            (chunk[1] as f32) * INV_I16
        } else {
            l
        };
        if tx.push([l, r]).is_err() {
            break;
        }
    }
}

struct InputReader {
    rx: Consumer<[f32; 2]>,
    step_ratio: f64,
    frac: f64,
    prev: [f32; 2],
    curr: [f32; 2],
    primed: bool,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
}

impl InputReader {
    fn new(rx: Consumer<[f32; 2]>, in_rate: u32, out_rate: u32, max_block: usize) -> Self {
        let ratio = (in_rate.max(8_000) as f64) / (out_rate.max(8_000) as f64);
        Self {
            rx,
            step_ratio: ratio,
            frac: 0.0,
            prev: [0.0, 0.0],
            curr: [0.0, 0.0],
            primed: false,
            buf_l: vec![0.0; max_block],
            buf_r: vec![0.0; max_block],
        }
    }

    fn pull_block(&mut self, frames: usize) {
        let count = frames.min(self.buf_l.len());
        if count == 0 {
            return;
        }

        // Keep at least one callback block buffered before starting consumption so separate
        // WASAPI input/output threads do not phase-starve on startup, and trim clock-drift excess.
        let required_prime = ((count as f64) * self.step_ratio).ceil() as usize;
        let max_queued = required_prime.saturating_mul(6).max(64);
        let target_queued = required_prime.saturating_mul(2).max(16);
        let available = self.rx.slots();
        if available > max_queued {
            let excess = available - target_queued;
            for _ in 0..excess {
                if self.rx.pop().is_err() {
                    break;
                }
            }
        }

        if !self.primed {
            if self.rx.slots() < required_prime.max(1) {
                self.buf_l[..count].fill(0.0);
                self.buf_r[..count].fill(0.0);
                return;
            }
            if (self.step_ratio - 1.0).abs() >= 1e-5 {
                self.prev = self.rx.pop().unwrap_or([0.0, 0.0]);
                self.curr = self.rx.pop().unwrap_or(self.prev);
                self.frac = 0.0;
            }
            self.primed = true;
        }

        if (self.step_ratio - 1.0).abs() < 1e-5 {
            let mut popped = 0usize;
            for i in 0..count {
                if let Ok(frame) = self.rx.pop() {
                    self.buf_l[i] = frame[0];
                    self.buf_r[i] = frame[1];
                    popped += 1;
                } else {
                    self.buf_l[i] = 0.0;
                    self.buf_r[i] = 0.0;
                }
            }
            if popped == 0 {
                self.primed = false;
            }
            return;
        }

        for i in 0..count {
            while self.frac >= 1.0 {
                self.prev = self.curr;
                self.curr = self.rx.pop().unwrap_or([0.0, 0.0]);
                self.frac -= 1.0;
            }
            let t = self.frac as f32;
            self.buf_l[i] = self.prev[0] + (self.curr[0] - self.prev[0]) * t;
            self.buf_r[i] = self.prev[1] + (self.curr[1] - self.prev[1]) * t;
            self.frac += self.step_ratio;
        }
    }
}

pub(crate) struct TelemetryDecimator {
    emit_interval_frames: usize,
    frames_accum: usize,
    in_peak_l: f32,
    in_peak_r: f32,
    master_peak_l: f32,
    master_peak_r: f32,
    max_cpu_load_pct: f32,
}

impl TelemetryDecimator {
    pub(crate) fn new(sample_rate: u32) -> Self {
        Self {
            emit_interval_frames: (sample_rate.max(8_000) as usize / 45).max(256),
            frames_accum: 0,
            in_peak_l: 0.0,
            in_peak_r: 0.0,
            master_peak_l: 0.0,
            master_peak_r: 0.0,
            max_cpu_load_pct: 0.0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.frames_accum = 0;
        self.in_peak_l = 0.0;
        self.in_peak_r = 0.0;
        self.master_peak_l = 0.0;
        self.master_peak_r = 0.0;
        self.max_cpu_load_pct = 0.0;
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_graph_block(
    frames: usize,
    in_l: &[f32],
    in_r: &[f32],
    out_l: &mut [f32],
    out_r: &mut [f32],
    graph: &mut AudioGraphProcessor,
    cmd_rx: &mut Consumer<EngineCommand>,
    telemetry_tx: &mut Producer<AudioTelemetry>,
    rack_meters_tx: &mut Option<Producer<RackMeterSnapshot>>,
    decimator: &mut TelemetryDecimator,
) {
    let _denormal_guard = DenormalGuard::enter();
    while let Ok(cmd) = cmd_rx.pop() {
        graph.apply_command(cmd);
    }

    let mut telemetry = graph.process_block(
        &in_l[..frames],
        &in_r[..frames],
        &mut out_l[..frames],
        &mut out_r[..frames],
    );

    if !graph.ui_visible() {
        decimator.reset();
        return;
    }

    decimator.frames_accum += frames;
    decimator.in_peak_l = decimator.in_peak_l.max(telemetry.input_peak_l);
    decimator.in_peak_r = decimator.in_peak_r.max(telemetry.input_peak_r);
    decimator.master_peak_l = decimator.master_peak_l.max(telemetry.master_peak_l);
    decimator.master_peak_r = decimator.master_peak_r.max(telemetry.master_peak_r);
    decimator.max_cpu_load_pct = decimator.max_cpu_load_pct.max(telemetry.cpu_load_pct);

    if decimator.frames_accum >= decimator.emit_interval_frames {
        telemetry.input_peak_l = decimator.in_peak_l;
        telemetry.input_peak_r = decimator.in_peak_r;
        telemetry.master_peak_l = decimator.master_peak_l;
        telemetry.master_peak_r = decimator.master_peak_r;
        telemetry.cpu_load_pct = decimator.max_cpu_load_pct;
        decimator.reset();

        let _ = telemetry_tx.push(telemetry);
        if let Some(ref mut tx) = rack_meters_tx {
            graph.write_accumulated_rack_meters(|snap| {
                let _ = tx.push(snap);
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_audio_callback_f32(
    interleaved_out: &mut [f32],
    channels: usize,
    graph: &mut AudioGraphProcessor,
    cmd_rx: &mut Consumer<EngineCommand>,
    telemetry_tx: &mut Producer<AudioTelemetry>,
    rack_meters_tx: &mut Option<Producer<RackMeterSnapshot>>,
    decimator: &mut TelemetryDecimator,
    input_reader: &mut InputReader,
    out_l: &mut [f32],
    out_r: &mut [f32],
) {
    let frames = (interleaved_out.len() / channels.max(1)).min(out_l.len());
    input_reader.pull_block(frames);
    process_graph_block(
        frames,
        &input_reader.buf_l[..frames],
        &input_reader.buf_r[..frames],
        &mut out_l[..frames],
        &mut out_r[..frames],
        graph,
        cmd_rx,
        telemetry_tx,
        rack_meters_tx,
        decimator,
    );

    for (frame_idx, chunk) in interleaved_out.chunks_exact_mut(channels).take(frames).enumerate() {
        let l = out_l[frame_idx];
        let r = out_r[frame_idx];
        if !chunk.is_empty() {
            chunk[0] = l;
        }
        if chunk.len() > 1 {
            chunk[1] = r;
        }
        for extra in chunk.iter_mut().skip(2) {
            *extra = 0.0;
        }
    }
}

fn notes_from_asio_metadata(title: &str, meta: &AsioDriverMetadata) -> DriverNotes {
    let rates = if meta.supported_sample_rates.is_empty() {
        vec![44_100, 48_000]
    } else {
        meta.supported_sample_rates.clone()
    };
    let gran_label = match meta.granularity {
        g if g < 0 => "power-of-two steps".to_string(),
        0 => "fixed (controlled via driver Control Panel)".to_string(),
        g => format!("{g} sample increments"),
    };
    DriverNotes {
        title: title.to_string(),
        backend: AudioDriverBackend::Asio,
        summary_lines: vec![
            format!("Driver: {} (ver. {})", meta.driver_name, meta.driver_version),
            format!(
                "Full-duplex single-clock ASIO driver: {} input ch, {} output ch",
                meta.input_channels, meta.output_channels
            ),
            format!("Sample format: {}", meta.sample_type_name),
            format!("Clock source: {}", meta.clock_source_name),
            format!(
                "Buffer range: {} .. {} samples (preferred {}, {})",
                meta.min_buffer_size, meta.max_buffer_size, meta.preferred_buffer_size, gran_label
            ),
            format!(
                "Reported driver latency: {} in / {} out samples",
                meta.input_latency_frames, meta.output_latency_frames
            ),
        ],
        input_channels: meta.input_channels,
        output_channels: meta.output_channels,
        input_channel_names: meta.input_channel_names.clone(),
        output_channel_names: meta.output_channel_names.clone(),
        supported_sample_rates: rates,
        min_buffer_size: meta.min_buffer_size,
        max_buffer_size: meta.max_buffer_size,
        preferred_buffer_size: meta.preferred_buffer_size,
    }
}

fn notes_for_wasapi(
    title: &str,
    out_channels: usize,
    min_sr: u32,
    max_sr: u32,
    input_name: &str,
) -> DriverNotes {
    let mut rates = Vec::new();
    for &sr in &[44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000] {
        if sr >= min_sr && sr <= max_sr {
            rates.push(sr);
        }
    }
    if rates.is_empty() {
        rates.extend([44_100, 48_000]);
    }
    let in_desc = if input_name.is_empty() {
        "Selected separately below (Windows WASAPI capture endpoint)".to_string()
    } else {
        input_name.to_string()
    };
    DriverNotes {
        title: title.to_string(),
        backend: AudioDriverBackend::Wasapi,
        summary_lines: vec![
            "Backend: Windows WASAPI (shared-mode MMCSS Pro Audio)".to_string(),
            format!("Output channels: {out_channels}"),
            format!("Input endpoint: {in_desc}"),
            format!("Supported sample rate range: {min_sr} .. {max_sr} Hz"),
            "Note: WASAPI uses separate Windows playback and capture endpoints. For lowest round-trip latency and synchronized In/Out on one clock, choose an ASIO driver.".to_string(),
        ],
        input_channels: 2,
        output_channels: out_channels,
        input_channel_names: Vec::new(),
        output_channel_names: Vec::new(),
        supported_sample_rates: rates,
        min_buffer_size: 64,
        max_buffer_size: 2048,
        preferred_buffer_size: 256,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_asio_wasapi_and_legacy_driver_selections() {
        assert_eq!(parse_driver_selection(None), (AudioDriverBackend::Wasapi, None));
        assert_eq!(
            parse_driver_selection(Some("ASIO - Voicemeeter Insert Virtual ASIO")),
            (AudioDriverBackend::Asio, Some("Voicemeeter Insert Virtual ASIO"))
        );
        assert_eq!(
            parse_driver_selection(Some("WASAPI - Speakers (Realtek)")),
            (AudioDriverBackend::Wasapi, Some("Speakers (Realtek)"))
        );
        assert_eq!(
            parse_driver_selection(Some("WASAPI - <System Default>")),
            (AudioDriverBackend::Wasapi, None)
        );
        assert_eq!(
            parse_driver_selection(Some("Legacy Headphone Out")),
            (AudioDriverBackend::Wasapi, Some("Legacy Headphone Out"))
        );
    }

    #[test]
    fn input_reader_primes_before_emitting_and_trims_overflow() {
        let (mut tx, rx) = RingBuffer::<[f32; 2]>::new(1024);
        let mut reader = InputReader::new(rx, 48_000, 48_000, 128);

        // Push fewer frames than one block (64): should hold them without popping until primed.
        for i in 0..32 {
            tx.push([0.5 + i as f32 * 0.001, -0.5]).unwrap();
        }
        reader.pull_block(64);
        assert!(reader.buf_l[..64].iter().all(|&s| s == 0.0));

        // Complete the block: reader should now prime and emit starting from the first sample.
        for i in 32..64 {
            tx.push([0.5 + i as f32 * 0.001, -0.5]).unwrap();
        }
        reader.pull_block(64);
        assert!((reader.buf_l[0] - 0.5).abs() < 1e-6);
        assert!((reader.buf_l[63] - 0.563).abs() < 1e-5);

        // Simulate excessive input drift (> 6x block = 384 frames): trim down cleanly to bounded queue.
        for i in 0..500 {
            tx.push([i as f32, i as f32]).unwrap();
        }
        reader.pull_block(64);
        assert!(reader.rx.slots() <= 128);
    }
}

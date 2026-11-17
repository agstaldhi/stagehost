use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tracing::warn;
use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, BusDirections_, BusInfo, BusTypes_, Chord, FrameRate,
    IAudioProcessor, IAudioProcessorTrait, IComponent, IComponentHandler, IComponentTrait,
    IConnectionPoint, IConnectionPointTrait, IEditController, IEditControllerTrait, MediaTypes_,
    ProcessContext, ProcessContext_, ProcessData, ProcessModes_, ProcessSetup, SpeakerArr,
    SymbolicSampleSizes_,
};
use vst3::Steinberg::{
    char16, char8, int32, kResultOk, kResultTrue, FUnknown, IBStream, IPlugFrame, IPlugView,
    IPlugViewTrait, IPluginBaseTrait, IPluginFactory, IPluginFactory2, IPluginFactory2Trait,
    IPluginFactoryTrait, PClassInfo, PClassInfo2, PFactoryInfo, ViewRect, TUID, kPlatformTypeHWND,
};
use vst3::{ComPtr, ComWrapper, Interface};

use crate::vst3_com::{
    HostApplication, HostComponentHandler, HostPlugFrame, MemoryBStream, Vst3ModuleGuard,
};
use crate::{
    AudioPluginInstance, PluginDescriptor, PluginError, PluginFormat, PluginParamInfo,
    PluginUiHandle,
};

/// Resolves a `.vst3` file or Windows bundle directory (`foo.vst3/Contents/x86_64-win/foo.vst3`)
/// and enumerates all exposed Audio Module classes.
pub fn scan_vst3_path(path: &Path) -> Result<Vec<PluginDescriptor>, PluginError> {
    let binary_path = resolve_vst3_binary_path(path)?;
    let (_guard, factory) = Vst3ModuleGuard::load_factory(&binary_path)?;
    Ok(describe_classes_in_factory(&factory, path))
}

fn describe_classes_in_factory(
    factory: &ComPtr<IPluginFactory>,
    path: &Path,
) -> Vec<PluginDescriptor> {
    let mut factory_info: PFactoryInfo = unsafe { std::mem::zeroed() };
    let vendor_fallback = unsafe {
        if factory.getFactoryInfo(&mut factory_info) == kResultOk {
            char8_buf_to_string(&factory_info.vendor)
        } else {
            String::new()
        }
    };

    let factory2 = factory.cast::<IPluginFactory2>();
    let count = unsafe { factory.countClasses() };
    let mut results = Vec::new();

    for idx in 0..count {
        if let Some(ref f2) = factory2 {
            let mut info2: PClassInfo2 = unsafe { std::mem::zeroed() };
            if unsafe { f2.getClassInfo2(idx, &mut info2) } == kResultOk {
                let category = char8_buf_to_string(&info2.category);
                if category == "Audio Module Class" {
                    let subcats = char8_buf_to_string(&info2.subCategories);
                    let is_instrument = subcats.contains("Instrument");
                    let vendor = char8_buf_to_string(&info2.vendor);
                    results.push(PluginDescriptor {
                        format: PluginFormat::Vst3,
                        id: tuid_to_hex(&info2.cid),
                        name: char8_buf_to_string(&info2.name),
                        vendor: if vendor.is_empty() {
                            vendor_fallback.clone()
                        } else {
                            vendor
                        },
                        version: char8_buf_to_string(&info2.version),
                        path: Some(path.to_path_buf()),
                        is_instrument,
                        audio_inputs: if is_instrument { 0 } else { 2 },
                        audio_outputs: 2,
                        has_sidechain: false,
                    });
                }
                continue;
            }
        }

        let mut info: PClassInfo = unsafe { std::mem::zeroed() };
        if unsafe { factory.getClassInfo(idx, &mut info) } == kResultOk {
            let category = char8_buf_to_string(&info.category);
            if category == "Audio Module Class" {
                results.push(PluginDescriptor {
                    format: PluginFormat::Vst3,
                    id: tuid_to_hex(&info.cid),
                    name: char8_buf_to_string(&info.name),
                    vendor: vendor_fallback.clone(),
                    version: String::new(),
                    path: Some(path.to_path_buf()),
                    is_instrument: false,
                    audio_inputs: 2,
                    audio_outputs: 2,
                    has_sidechain: false,
                });
            }
        }
    }

    results
}

struct Vst3Shared {
    // Field order matters: COM pointers (`connection_points`, `controller`, `processor`, `component`)
    // MUST be dropped BEFORE `_guard` calls `ExitDll` / `FreeLibrary`!
    connection_points: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    controller: Option<ComPtr<IEditController>>,
    processor: ComPtr<IAudioProcessor>,
    component: ComPtr<IComponent>,
    _comp_handler: ComWrapper<HostComponentHandler>,
    _host_app: ComWrapper<HostApplication>,
    _guard: Vst3ModuleGuard,
    controller_is_separate: bool,
    active: AtomicBool,
    processing: AtomicBool,
}

// Safety: `IAudioProcessor::process` executes exclusively on the audio thread (`Vst3PluginInstance`),
// while GUI, state, and controller queries execute on the main UI thread (`Vst3UiHandle`).
unsafe impl Send for Vst3Shared {}
unsafe impl Sync for Vst3Shared {}

impl Vst3Shared {
    fn query_parameters(&self) -> Vec<PluginParamInfo> {
        let Some(ref ctrl) = self.controller else {
            return Vec::new();
        };
        let count = unsafe { ctrl.getParameterCount() };
        let mut list = Vec::with_capacity(count.max(0) as usize);
        for idx in 0..count {
            let mut info = unsafe { std::mem::zeroed() };
            if unsafe { ctrl.getParameterInfo(idx, &mut info) } == kResultOk {
                let val = unsafe { ctrl.getParamNormalized(info.id) };
                list.push(PluginParamInfo {
                    id: info.id,
                    name: char16_buf_to_string(&info.title),
                    value: val as f32,
                    min: 0.0,
                    max: 1.0,
                });
            }
        }
        list
    }

    fn stop_and_deactivate(&self) {
        unsafe {
            if self.processing.swap(false, Ordering::AcqRel) {
                let _ = self.processor.setProcessing(0);
            }
            if self.active.swap(false, Ordering::AcqRel) {
                let _ = self.component.setActive(0);
            }
        }
    }
}

impl Drop for Vst3Shared {
    fn drop(&mut self) {
        self.stop_and_deactivate();
        unsafe {
            if let Some((ref comp_cp, ref ctrl_cp)) = self.connection_points {
                let _ = comp_cp.disconnect(ctrl_cp.as_ptr());
                let _ = ctrl_cp.disconnect(comp_cp.as_ptr());
            }
            if let Some(ref ctrl) = self.controller {
                let _ = ctrl.setComponentHandler(ptr::null_mut());
                if self.controller_is_separate {
                    let _ = ctrl.terminate();
                }
            }
            let _ = self.component.terminate();
        }
    }
}

/// Live VST3 plugin audio-thread instance backed by `IAudioProcessor`.
pub struct Vst3PluginInstance {
    descriptor: PluginDescriptor,
    shared: Arc<Vst3Shared>,
    max_block_size: usize,
    prepared_sample_rate: f64,
    sample_cursor: i64,
    in_audio_bus_count: usize,
    main_bus_index: Option<usize>,
    sidechain_bus_index: Option<usize>,
    sidechain_channels: usize,
    output_channels: usize,
    in_scratch_l: Vec<f32>,
    in_scratch_r: Vec<f32>,
    sc_scratch_l: Vec<f32>,
    sc_scratch_r: Vec<f32>,
    silent_scratch: Vec<f32>,
    out_scratch_l: Vec<f32>,
    out_scratch_r: Vec<f32>,
    in_buses: Vec<AudioBusBuffers>,
}

/// Main-thread UI and state handle paired with a [`Vst3PluginInstance`].
pub struct Vst3UiHandle {
    shared: Arc<Vst3Shared>,
    plug_view: Option<ComPtr<IPlugView>>,
    plug_frame: Option<ComWrapper<HostPlugFrame>>,
    requested_size: Arc<Mutex<Option<(u32, u32)>>>,
    editor_unavailable: bool,
}

fn obtain_edit_controller(
    factory: &ComPtr<IPluginFactory>,
    component: &ComPtr<IComponent>,
    host_unknown: &ComPtr<FUnknown>,
) -> (Option<ComPtr<IEditController>>, bool) {
    if let Some(ctrl) = component.cast::<IEditController>() {
        return (Some(ctrl), false);
    }

    let mut ctrl_cid: TUID = [0; 16];
    if unsafe { component.getControllerClassId(&mut ctrl_cid) } != kResultOk {
        return (None, false);
    }

    let mut ctrl_obj: *mut c_void = ptr::null_mut();
    let rc = unsafe {
        factory.createInstance(
            ctrl_cid.as_ptr(),
            IEditController::IID.as_ptr() as *const i8,
            &mut ctrl_obj,
        )
    };
    let separate_ctrl = if rc == kResultOk && !ctrl_obj.is_null() {
        unsafe { ComPtr::from_raw(ctrl_obj as *mut IEditController) }
    } else {
        // Fallback for plugins that only answer FUnknown on createInstance.
        let mut unk_obj: *mut c_void = ptr::null_mut();
        let unk_rc = unsafe {
            factory.createInstance(
                ctrl_cid.as_ptr(),
                FUnknown::IID.as_ptr() as *const i8,
                &mut unk_obj,
            )
        };
        if unk_rc == kResultOk && !unk_obj.is_null() {
            unsafe { ComPtr::from_raw(unk_obj as *mut FUnknown) }
                .and_then(|u| u.cast::<IEditController>())
        } else {
            None
        }
    };

    if let Some(ref c) = separate_ctrl {
        let init_ctrl = unsafe { c.initialize(host_unknown.as_ptr()) };
        if init_ctrl != kResultOk {
            warn!("VST3 IEditController::initialize returned tresult={init_ctrl}");
        }
        let is_same_com_obj = match (component.cast::<FUnknown>(), c.cast::<FUnknown>()) {
            (Some(a), Some(b)) => a.as_ptr() == b.as_ptr(),
            _ => false,
        };
        (separate_ctrl, !is_same_com_obj)
    } else {
        (None, false)
    }
}

fn sync_initial_controller_state(component: &ComPtr<IComponent>, ctrl: &ComPtr<IEditController>) {
    let write_stream = ComWrapper::new(MemoryBStream::default());
    let Some(write_ptr) = write_stream.to_com_ptr::<IBStream>() else {
        return;
    };
    if unsafe { component.getState(write_ptr.as_ptr()) } == kResultOk {
        let bytes = write_stream.take_bytes();
        let read_stream = ComWrapper::new(MemoryBStream::from_bytes(&bytes));
        if let Some(read_ptr) = read_stream.to_com_ptr::<IBStream>() {
            unsafe {
                let _ = ctrl.setComponentState(read_ptr.as_ptr());
            }
        }
    }
}

impl Vst3PluginInstance {
    /// Loads a VST3 Audio Module class identified by 32-hex `class_id`, returning the
    /// audio-thread processor and main-thread UI/state handle.
    pub fn load_with_ui(path: &Path, class_id: &str) -> Result<(Self, Vst3UiHandle), PluginError> {
        let binary_path = resolve_vst3_binary_path(path)?;
        let (guard, factory) = Vst3ModuleGuard::load_factory(&binary_path)?;
        let target_cid = hex_to_tuid(class_id).ok_or_else(|| PluginError::PluginNotFound {
            path: path.to_path_buf(),
            plugin_id: class_id.to_string(),
        })?;

        let mut obj: *mut c_void = ptr::null_mut();
        let res = unsafe {
            factory.createInstance(
                target_cid.as_ptr(),
                IComponent::IID.as_ptr() as *const i8,
                &mut obj,
            )
        };
        if res != kResultOk || obj.is_null() {
            return Err(PluginError::PluginNotFound {
                path: path.to_path_buf(),
                plugin_id: class_id.to_string(),
            });
        }
        let component = unsafe { ComPtr::from_raw(obj as *mut IComponent) }.ok_or_else(|| {
            PluginError::InitFailed {
                context: "IComponent::from_raw",
                detail: "null component pointer".to_string(),
            }
        })?;

        let host_app = ComWrapper::new(HostApplication);
        let host_unknown = host_app
            .to_com_ptr::<FUnknown>()
            .ok_or_else(|| PluginError::InitFailed {
                context: "HostApplication",
                detail: "failed to obtain FUnknown".to_string(),
            })?;

        let init_res = unsafe { component.initialize(host_unknown.as_ptr()) };
        if init_res != kResultOk {
            return Err(PluginError::InitFailed {
                context: "IComponent::initialize",
                detail: format!("VST3 initialize returned tresult={init_res}"),
            });
        }

        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or_else(|| PluginError::InitFailed {
                context: "IAudioProcessor",
                detail: "component does not expose IAudioProcessor".to_string(),
            })?;

        let (controller, controller_is_separate) =
            obtain_edit_controller(&factory, &component, &host_unknown);

        let comp_handler = ComWrapper::new(HostComponentHandler);
        let mut connection_points = None;
        if let Some(ref ctrl) = controller {
            if let Some(handler_ptr) = comp_handler.to_com_ptr::<IComponentHandler>() {
                unsafe {
                    let _ = ctrl.setComponentHandler(handler_ptr.as_ptr());
                }
            }
            if controller_is_separate {
                if let (Some(comp_cp), Some(ctrl_cp)) = (
                    component.cast::<IConnectionPoint>(),
                    ctrl.cast::<IConnectionPoint>(),
                ) {
                    unsafe {
                        let _ = comp_cp.connect(ctrl_cp.as_ptr());
                        let _ = ctrl_cp.connect(comp_cp.as_ptr());
                    }
                    connection_points = Some((comp_cp, ctrl_cp));
                }
                sync_initial_controller_state(&component, ctrl);
            }
        }

        let mut descriptor = describe_classes_in_factory(&factory, path)
            .into_iter()
            .find(|d| d.id == class_id)
            .unwrap_or(PluginDescriptor {
                format: PluginFormat::Vst3,
                id: class_id.to_string(),
                name: class_id.to_string(),
                vendor: String::new(),
                version: String::new(),
                path: Some(path.to_path_buf()),
                is_instrument: false,
                audio_inputs: 2,
                audio_outputs: 2,
                has_sidechain: false,
            });

        // Inspect and activate main & auxiliary/sidechain audio and event buses when present.
        let mut in_audio_bus_count = 0usize;
        let mut main_bus_index: Option<usize> = None;
        let mut sidechain_bus_index: Option<usize> = None;
        let mut sidechain_channels: usize = 0;
        unsafe {
            let in_audio = component.getBusCount(MediaTypes_::kAudio, BusDirections_::kInput);
            if in_audio > 0 {
                in_audio_bus_count = in_audio as usize;
                let mut total_in_channels = 0usize;
                for idx in 0..in_audio {
                    let mut info: BusInfo = std::mem::zeroed();
                    if component.getBusInfo(
                        MediaTypes_::kAudio,
                        BusDirections_::kInput,
                        idx,
                        &mut info,
                    ) == kResultOk
                    {
                        total_in_channels += info.channelCount.max(0) as usize;
                        if info.busType == BusTypes_::kMain && main_bus_index.is_none() {
                            main_bus_index = Some(idx as usize);
                        } else if info.busType == BusTypes_::kAux
                            && sidechain_bus_index.is_none()
                            && info.channelCount >= 1
                        {
                            sidechain_bus_index = Some(idx as usize);
                            sidechain_channels = (info.channelCount as usize).clamp(1, 2);
                        }
                    }
                }
                if main_bus_index.is_none()
                    && !descriptor.is_instrument
                    && sidechain_bus_index != Some(0)
                {
                    main_bus_index = Some(0);
                }
                if let Some(m_idx) = main_bus_index {
                    let _ = component.activateBus(
                        MediaTypes_::kAudio,
                        BusDirections_::kInput,
                        m_idx as int32,
                        1,
                    );
                }
                if let Some(sc_idx) = sidechain_bus_index {
                    let _ = component.activateBus(
                        MediaTypes_::kAudio,
                        BusDirections_::kInput,
                        sc_idx as int32,
                        1,
                    );
                }
                if total_in_channels > 0 {
                    descriptor.audio_inputs = total_in_channels;
                }
                descriptor.has_sidechain = sidechain_bus_index.is_some();
            }
            let out_audio = component.getBusCount(MediaTypes_::kAudio, BusDirections_::kOutput);
            if out_audio > 0 {
                let _ = component.activateBus(MediaTypes_::kAudio, BusDirections_::kOutput, 0, 1);
            }
            let in_events = component.getBusCount(MediaTypes_::kEvent, BusDirections_::kInput);
            if in_events > 0 {
                let _ = component.activateBus(MediaTypes_::kEvent, BusDirections_::kInput, 0, 1);
            }
        }

        let active_in_bus_len = if in_audio_bus_count > 0 {
            in_audio_bus_count
        } else if descriptor.audio_inputs > 0 {
            main_bus_index = Some(0);
            1
        } else {
            0
        };

        let shared = Arc::new(Vst3Shared {
            _guard: guard,
            _host_app: host_app,
            _comp_handler: comp_handler,
            component,
            processor,
            controller,
            controller_is_separate,
            connection_points,
            active: AtomicBool::new(false),
            processing: AtomicBool::new(false),
        });
        let audio = Self {
            descriptor,
            shared: Arc::clone(&shared),
            max_block_size: 1024,
            prepared_sample_rate: 0.0,
            sample_cursor: 0,
            in_audio_bus_count: active_in_bus_len,
            main_bus_index,
            sidechain_bus_index,
            sidechain_channels,
            output_channels: 2,
            in_scratch_l: vec![0.0; 1024],
            in_scratch_r: vec![0.0; 1024],
            sc_scratch_l: vec![0.0; 1024],
            sc_scratch_r: vec![0.0; 1024],
            silent_scratch: vec![0.0; 1024],
            out_scratch_l: vec![0.0; 1024],
            out_scratch_r: vec![0.0; 1024],
            in_buses: (0..active_in_bus_len)
                .map(|_| AudioBusBuffers {
                    numChannels: 0,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: ptr::null_mut(),
                    },
                })
                .collect(),
        };
        let ui = Vst3UiHandle {
            shared,
            plug_view: None,
            plug_frame: None,
            requested_size: Arc::new(Mutex::new(None)),
            editor_unavailable: false,
        };
        Ok((audio, ui))
    }
}

impl AudioPluginInstance for Vst3PluginInstance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn prepare(&mut self, sample_rate: f64, max_block_size: usize) -> Result<(), PluginError> {
        let clamped_block = max_block_size.max(64);
        if self.shared.active.load(Ordering::Acquire)
            && self.prepared_sample_rate == sample_rate
            && self.max_block_size == clamped_block
        {
            return Ok(());
        }

        self.max_block_size = clamped_block;
        self.in_scratch_l.resize(self.max_block_size, 0.0);
        self.in_scratch_r.resize(self.max_block_size, 0.0);
        self.sc_scratch_l.resize(self.max_block_size, 0.0);
        self.sc_scratch_r.resize(self.max_block_size, 0.0);
        self.silent_scratch.resize(self.max_block_size, 0.0);
        self.out_scratch_l.resize(self.max_block_size, 0.0);
        self.out_scratch_r.resize(self.max_block_size, 0.0);

        self.shared.stop_and_deactivate();
        unsafe {
            if self.main_bus_index == Some(0)
                && self.sidechain_bus_index == Some(1)
                && self.in_audio_bus_count == 2
            {
                let sc_arr = if self.sidechain_channels == 1 {
                    SpeakerArr::kMono
                } else {
                    SpeakerArr::kStereo
                };
                let mut in_arrs = [SpeakerArr::kStereo, sc_arr];
                let mut out_arrs = [SpeakerArr::kStereo];
                let rc = self.shared.processor.setBusArrangements(
                    in_arrs.as_mut_ptr(),
                    2,
                    out_arrs.as_mut_ptr(),
                    1,
                );
                if rc != kResultOk && self.sidechain_channels == 2 {
                    let mut in_arrs_mono = [SpeakerArr::kStereo, SpeakerArr::kMono];
                    if self.shared.processor.setBusArrangements(
                        in_arrs_mono.as_mut_ptr(),
                        2,
                        out_arrs.as_mut_ptr(),
                        1,
                    ) == kResultOk
                    {
                        self.sidechain_channels = 1;
                    }
                }
            } else if self.main_bus_index == Some(0) && self.in_audio_bus_count == 1 {
                let mut in_arrs = [SpeakerArr::kStereo];
                let mut out_arrs = [SpeakerArr::kStereo];
                let _ = self.shared.processor.setBusArrangements(
                    in_arrs.as_mut_ptr(),
                    1,
                    out_arrs.as_mut_ptr(),
                    1,
                );
            }

            let mut out_arr = SpeakerArr::kStereo;
            if self.shared.processor.getBusArrangement(BusDirections_::kOutput, 0, &mut out_arr)
                == kResultOk
                && out_arr == SpeakerArr::kMono
            {
                self.output_channels = 1;
            } else {
                self.output_channels = 2;
            }

            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32,
                maxSamplesPerBlock: self.max_block_size as int32,
                sampleRate: sample_rate,
            };
            let rc = self.shared.processor.setupProcessing(&mut setup);
            if rc != kResultOk {
                return Err(PluginError::PrepareFailed {
                    sample_rate,
                    block_size: max_block_size,
                    detail: format!("IAudioProcessor::setupProcessing failed ({rc})"),
                });
            }
            let act_rc = self.shared.component.setActive(1);
            if act_rc != kResultOk {
                return Err(PluginError::PrepareFailed {
                    sample_rate,
                    block_size: max_block_size,
                    detail: format!("IComponent::setActive(1) failed ({act_rc})"),
                });
            }
            self.prepared_sample_rate = sample_rate;
            self.shared.active.store(true, Ordering::Release);
            let proc_rc = self.shared.processor.setProcessing(1);
            self.shared
                .processing
                .store(proc_rc == kResultOk || proc_rc == 1, Ordering::Release);
        }
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
        if !self.shared.active.load(Ordering::Acquire) || outputs.len() < 2 {
            return;
        }
        let frames = outputs[0]
            .len()
            .min(outputs[1].len())
            .min(self.max_block_size);

        if inputs.len() >= 2 {
            let n0 = inputs[0].len().min(frames);
            let n1 = inputs[1].len().min(frames);
            self.in_scratch_l[..n0].copy_from_slice(&inputs[0][..n0]);
            self.in_scratch_r[..n1].copy_from_slice(&inputs[1][..n1]);
        } else {
            self.in_scratch_l[..frames].fill(0.0);
            self.in_scratch_r[..frames].fill(0.0);
        }

        let sc_active = if self.sidechain_bus_index.is_some() {
            if let Some(sc) = sidechain.filter(|s| s.len() >= 2) {
                let n0 = sc[0].len().min(frames);
                let n1 = sc[1].len().min(frames);
                if self.sidechain_channels == 1 {
                    let n = n0.min(n1);
                    for i in 0..n {
                        self.sc_scratch_l[i] = 0.5 * (sc[0][i] + sc[1][i]);
                    }
                    self.sc_scratch_l[n..frames].fill(0.0);
                    self.sc_scratch_r[..frames].fill(0.0);
                } else {
                    self.sc_scratch_l[..n0].copy_from_slice(&sc[0][..n0]);
                    self.sc_scratch_l[n0..frames].fill(0.0);
                    self.sc_scratch_r[..n1].copy_from_slice(&sc[1][..n1]);
                    self.sc_scratch_r[n1..frames].fill(0.0);
                }
                true
            } else {
                self.sc_scratch_l[..frames].fill(0.0);
                self.sc_scratch_r[..frames].fill(0.0);
                false
            }
        } else {
            false
        };

        self.out_scratch_l[..frames].fill(0.0);
        self.out_scratch_r[..frames].fill(0.0);

        let mut in_ptrs = [self.in_scratch_l.as_mut_ptr(), self.in_scratch_r.as_mut_ptr()];
        let mut sc_ptrs = [self.sc_scratch_l.as_mut_ptr(), self.sc_scratch_r.as_mut_ptr()];
        let mut silent_ptrs = [self.silent_scratch.as_mut_ptr(), self.silent_scratch.as_mut_ptr()];
        let mut out_ptrs = [self.out_scratch_l.as_mut_ptr(), self.out_scratch_r.as_mut_ptr()];

        for (idx, bus) in self.in_buses.iter_mut().enumerate() {
            if Some(idx) == self.main_bus_index {
                *bus = AudioBusBuffers {
                    numChannels: 2,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: in_ptrs.as_mut_ptr(),
                    },
                };
            } else if Some(idx) == self.sidechain_bus_index {
                let ch = self.sidechain_channels.clamp(1, 2);
                *bus = AudioBusBuffers {
                    numChannels: ch as int32,
                    silenceFlags: if sc_active { 0 } else { (1u64 << ch) - 1 },
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: sc_ptrs.as_mut_ptr(),
                    },
                };
            } else {
                *bus = AudioBusBuffers {
                    numChannels: 2,
                    silenceFlags: 0b11,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: silent_ptrs.as_mut_ptr(),
                    },
                };
            }
        }
        let mut out_bus = AudioBusBuffers {
            numChannels: self.output_channels as int32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: out_ptrs.as_mut_ptr(),
            },
        };

        let sr = if self.prepared_sample_rate > 0.0 {
            self.prepared_sample_rate
        } else {
            48_000.0
        };
        let mut process_context = ProcessContext {
            state: (ProcessContext_::StatesAndFlags_::kPlaying
                | ProcessContext_::StatesAndFlags_::kTempoValid
                | ProcessContext_::StatesAndFlags_::kTimeSigValid
                | ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid) as u32,
            sampleRate: sr,
            projectTimeSamples: self.sample_cursor,
            systemTime: 0,
            continousTimeSamples: self.sample_cursor,
            projectTimeMusic: (self.sample_cursor as f64 / sr) * 2.0,
            barPositionMusic: 0.0,
            cycleStartMusic: 0.0,
            cycleEndMusic: 0.0,
            tempo: 120.0,
            timeSigNumerator: 4,
            timeSigDenominator: 4,
            chord: Chord {
                keyNote: 0,
                rootNote: 0,
                chordMask: 0,
            },
            smpteOffsetSubframes: 0,
            frameRate: FrameRate {
                framesPerSecond: 30,
                flags: 0,
            },
            samplesToNextClock: 0,
        };

        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32,
            numSamples: frames as int32,
            numInputs: self.in_buses.len() as int32,
            numOutputs: 1,
            inputs: if self.in_buses.is_empty() {
                ptr::null_mut()
            } else {
                self.in_buses.as_mut_ptr()
            },
            outputs: &mut out_bus,
            inputParameterChanges: ptr::null_mut(),
            outputParameterChanges: ptr::null_mut(),
            inputEvents: ptr::null_mut(),
            outputEvents: ptr::null_mut(),
            processContext: &mut process_context,
        };

        let rc = unsafe { self.shared.processor.process(&mut data) };
        self.sample_cursor = self.sample_cursor.wrapping_add(frames as i64);
        if rc == kResultOk {
            outputs[0][..frames].copy_from_slice(&self.out_scratch_l[..frames]);
            if self.output_channels == 1 {
                outputs[1][..frames].copy_from_slice(&self.out_scratch_l[..frames]);
            } else {
                outputs[1][..frames].copy_from_slice(&self.out_scratch_r[..frames]);
            }
        }
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        self.shared.query_parameters()
    }

    fn set_parameter(&mut self, param_id: u32, value: f32) {
        if let Some(ref ctrl) = self.shared.controller {
            unsafe {
                let _ = ctrl.setParamNormalized(param_id, value as f64);
            }
        }
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        let stream = ComWrapper::new(MemoryBStream::from_bytes(data));
        let stream_ptr = stream
            .to_com_ptr::<IBStream>()
            .ok_or_else(|| PluginError::StateError {
                context: "IBStream",
                detail: "failed to construct IBStream".to_string(),
            })?;
        let rc = unsafe { self.shared.component.setState(stream_ptr.as_ptr()) };
        if rc != kResultOk {
            return Err(PluginError::StateError {
                context: "IComponent::setState",
                detail: format!("VST3 setState failed ({rc})"),
            });
        }
        if let Some(ref ctrl) = self.shared.controller {
            let stream_ctrl = ComWrapper::new(MemoryBStream::from_bytes(data));
            if let Some(ptr) = stream_ctrl.to_com_ptr::<IBStream>() {
                unsafe {
                    let _ = ctrl.setComponentState(ptr.as_ptr());
                }
            }
        }
        Ok(())
    }

    fn latency_samples(&self) -> u32 {
        if !self.shared.active.load(Ordering::Acquire) {
            return 0;
        }
        unsafe { self.shared.processor.getLatencySamples() }
    }
}

impl Drop for Vst3UiHandle {
    fn drop(&mut self) {
        self.close_editor();
    }
}

impl PluginUiHandle for Vst3UiHandle {
    fn has_editor(&self) -> bool {
        self.shared.controller.is_some() && !self.editor_unavailable
    }

    fn open_editor(&mut self, parent_hwnd: isize) -> Result<(), PluginError> {
        self.close_editor();
        if let Ok(mut slot) = self.requested_size.lock() {
            *slot = None;
        }

        let ctrl = self
            .shared
            .controller
            .as_ref()
            .ok_or_else(|| PluginError::GuiError {
                context: "IEditController",
                detail: "plugin has no IEditController".to_string(),
            })?;
        let raw_view = unsafe { ctrl.createView(c"editor".as_ptr()) };
        let Some(view) = (unsafe { ComPtr::from_raw(raw_view) }) else {
            self.editor_unavailable = true;
            return Err(PluginError::GuiError {
                context: "IEditController::createView",
                detail: "plugin returned null IPlugView (no native editor GUI; use Params instead)"
                    .to_string(),
            });
        };

        let frame = ComWrapper::new(HostPlugFrame::new(Arc::clone(&self.requested_size)));
        if let Some(frame_ptr) = frame.to_com_ptr::<IPlugFrame>() {
            unsafe {
                let _ = view.setFrame(frame_ptr.as_ptr());
            }
        }

        let supported = unsafe { view.isPlatformTypeSupported(kPlatformTypeHWND) };
        if supported != kResultTrue {
            unsafe {
                let _ = view.setFrame(ptr::null_mut());
            }
            return Err(PluginError::GuiError {
                context: "IPlugView::isPlatformTypeSupported",
                detail: "HWND platform view is not supported".to_string(),
            });
        }

        let rc = unsafe { view.attached(parent_hwnd as *mut c_void, kPlatformTypeHWND) };
        if rc != kResultOk {
            unsafe {
                let _ = view.setFrame(ptr::null_mut());
            }
            return Err(PluginError::GuiError {
                context: "IPlugView::attached",
                detail: format!("IPlugView::attached failed ({rc})"),
            });
        }
        self.plug_frame = Some(frame);
        self.plug_view = Some(view);
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(view) = self.plug_view.take() {
            unsafe {
                let _ = view.setFrame(ptr::null_mut());
                let _ = view.removed();
            }
        }
        self.plug_frame.take();
    }

    fn preferred_size(&self) -> Option<(u32, u32)> {
        if let Some(ref view) = self.plug_view {
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if unsafe { view.getSize(&mut rect) } == kResultOk {
                let raw_w = rect.right - rect.left;
                let raw_h = rect.bottom - rect.top;
                if raw_w > 0 && raw_h > 0 {
                    return Some((raw_w.clamp(160, 3840) as u32, raw_h.clamp(120, 2160) as u32));
                }
            }
        }
        self.requested_size.lock().ok().and_then(|s| *s)
    }

    fn resize_editor(&mut self, width: u32, height: u32) {
        let Some(ref view) = self.plug_view else {
            return;
        };
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: width.max(160) as int32,
            bottom: height.max(120) as int32,
        };
        unsafe {
            let _ = view.onSize(&mut rect);
        }
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        let stream = ComWrapper::new(MemoryBStream::default());
        let stream_ptr = stream
            .to_com_ptr::<IBStream>()
            .ok_or_else(|| PluginError::StateError {
                context: "IBStream",
                detail: "failed to construct IBStream".to_string(),
            })?;
        let rc = unsafe { self.shared.component.getState(stream_ptr.as_ptr()) };
        if rc != kResultOk {
            return Err(PluginError::StateError {
                context: "IComponent::getState",
                detail: format!("VST3 getState failed ({rc})"),
            });
        }
        Ok(stream.take_bytes())
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        self.shared.query_parameters()
    }
}

fn resolve_vst3_binary_path(path: &Path) -> Result<PathBuf, PluginError> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if path.is_dir() {
        let file_name = path
            .file_name()
            .ok_or_else(|| PluginError::BinaryLoad {
                path: path.to_path_buf(),
                detail: "invalid VST3 bundle path".to_string(),
            })?;
        let candidate = path
            .join("Contents")
            .join("x86_64-win")
            .join(file_name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(PluginError::BinaryLoad {
        path: path.to_path_buf(),
        detail: "no x86_64-win VST3 binary found".to_string(),
    })
}

fn tuid_to_hex(tuid: &TUID) -> String {
    let mut s = String::with_capacity(32);
    for &b in tuid {
        s.push_str(&format!("{:02X}", b as u8));
    }
    s
}

fn hex_to_tuid(hex: &str) -> Option<TUID> {
    if hex.len() != 32 {
        return None;
    }
    let mut out: TUID = [0; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        let byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        *slot = byte as i8;
    }
    Some(out)
}

fn char8_buf_to_string(buf: &[char8]) -> String {
    let ptr = buf.as_ptr();
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    String::from_utf8_lossy(slice).into_owned()
}

fn char16_buf_to_string(buf: &[char16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let slice = unsafe { std::slice::from_raw_parts(buf.as_ptr(), len) };
    String::from_utf16_lossy(slice)
}

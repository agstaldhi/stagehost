use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    clap_event_header, clap_event_param_value, clap_input_events, clap_output_events,
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_VALUE,
};
use clap_sys::ext::audio_ports::{
    clap_audio_port_info, clap_plugin_audio_ports, CLAP_AUDIO_PORT_IS_MAIN, CLAP_EXT_AUDIO_PORTS,
};
use clap_sys::ext::gui::{clap_plugin_gui, clap_window, clap_window_handle, CLAP_EXT_GUI, CLAP_WINDOW_API_WIN32};
use clap_sys::ext::latency::{clap_plugin_latency, CLAP_EXT_LATENCY};
use clap_sys::ext::params::{clap_param_info, clap_plugin_params, CLAP_EXT_PARAMS};
use clap_sys::ext::state::{clap_plugin_state, CLAP_EXT_STATE};
use clap_sys::factory::plugin_factory::{clap_plugin_factory, CLAP_PLUGIN_FACTORY_ID};
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::{clap_process, CLAP_PROCESS_ERROR};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use libloading::Library;

use crate::{AudioPluginInstance, PluginDescriptor, PluginError, PluginFormat, PluginParamInfo, PluginUiHandle};

struct ClapLibraryGuard {
    _lib: Library,
    entry: *const clap_plugin_entry,
}

impl Drop for ClapLibraryGuard {
    fn drop(&mut self) {
        if let Some(entry) = unsafe { self.entry.as_ref() } {
            if let Some(deinit) = entry.deinit {
                unsafe { deinit() };
            }
        }
    }
}

/// Scans a `.clap` DLL and returns all plugin descriptors exported by its factory.
pub fn scan_clap_file(path: &Path) -> Result<Vec<PluginDescriptor>, PluginError> {
    let guard = load_clap_entry(path)?;
    let factory = get_plugin_factory(&guard, path)?;
    let count = unsafe {
        factory
            .get_plugin_count
            .map(|f| f(factory))
            .unwrap_or(0)
    };

    let mut descriptors = Vec::with_capacity(count as usize);
    for idx in 0..count {
        let desc_ptr = unsafe {
            factory
                .get_plugin_descriptor
                .map(|f| f(factory, idx))
                .unwrap_or(ptr::null())
        };
        if let Some(desc) = unsafe { desc_ptr.as_ref() } {
            descriptors.push(descriptor_from_clap(desc, path));
        }
    }
    Ok(descriptors)
}

/// Live CLAP plugin instance hosted via C ABI (`clap-sys`).
struct ClapShared {
    plugin: *const clap_plugin,
    _host: Box<clap_host>,
    activated: AtomicBool,
    processing: AtomicBool,
    // Keep DLL guard last so library unloads only after `plugin->destroy()`.
    _guard: ClapLibraryGuard,
}

// Safety: Audio processing methods are called exclusively on the audio thread (`ClapPluginInstance`),
// while GUI/state/parameter queries run on the main thread (`ClapUiHandle`).
unsafe impl Send for ClapShared {}
unsafe impl Sync for ClapShared {}

impl ClapShared {
    unsafe fn get_ext<T>(&self, id: &CStr) -> Option<&T> {
        let fn_ptr = unsafe { (*self.plugin).get_extension? };
        let ext_ptr = unsafe { fn_ptr(self.plugin, id.as_ptr()) } as *const T;
        unsafe { ext_ptr.as_ref() }
    }

    fn stop_and_deactivate(&self) {
        unsafe {
            if self.processing.swap(false, Ordering::AcqRel) {
                if let Some(stop) = (*self.plugin).stop_processing {
                    stop(self.plugin);
                }
            }
            if self.activated.swap(false, Ordering::AcqRel) {
                if let Some(deactivate) = (*self.plugin).deactivate {
                    deactivate(self.plugin);
                }
            }
        }
    }

    fn query_parameters(&self) -> Vec<PluginParamInfo> {
        let Some(ext) = (unsafe { self.get_ext::<clap_plugin_params>(CLAP_EXT_PARAMS) }) else {
            return Vec::new();
        };
        let count = ext.count.map(|f| unsafe { f(self.plugin) }).unwrap_or(0);
        let mut out = Vec::with_capacity(count as usize);
        for idx in 0..count {
            let mut info: clap_param_info = unsafe { std::mem::zeroed() };
            let ok = ext
                .get_info
                .map(|f| unsafe { f(self.plugin, idx, &mut info) })
                .unwrap_or(false);
            if ok {
                let mut val = info.default_value;
                if let Some(get_val) = ext.get_value {
                    unsafe {
                        let _ = get_val(self.plugin, info.id, &mut val);
                    }
                }
                out.push(PluginParamInfo {
                    id: info.id,
                    name: cstr_buf_to_string(&info.name),
                    value: val as f32,
                    min: info.min_value as f32,
                    max: info.max_value as f32,
                });
            }
        }
        out
    }

    fn save_state_bytes(&self) -> Result<Vec<u8>, PluginError> {
        let ext = unsafe { self.get_ext::<clap_plugin_state>(CLAP_EXT_STATE) }.ok_or_else(|| {
            PluginError::StateError {
                context: "clap.state",
                detail: "plugin does not implement clap.state".to_string(),
            }
        })?;
        let save_fn = ext.save.ok_or_else(|| PluginError::StateError {
            context: "clap.state::save",
            detail: "missing save callback".to_string(),
        })?;

        let mut buffer = Vec::new();
        let stream = clap_ostream {
            ctx: (&mut buffer as *mut Vec<u8>) as *mut c_void,
            write: Some(ostream_write),
        };
        if unsafe { save_fn(self.plugin, &stream) } {
            Ok(buffer)
        } else {
            Err(PluginError::StateError {
                context: "clap.state::save",
                detail: "plugin returned false".to_string(),
            })
        }
    }

    fn load_state_bytes(&self, data: &[u8]) -> Result<(), PluginError> {
        let ext = unsafe { self.get_ext::<clap_plugin_state>(CLAP_EXT_STATE) }.ok_or_else(|| {
            PluginError::StateError {
                context: "clap.state",
                detail: "plugin does not implement clap.state".to_string(),
            }
        })?;
        let load_fn = ext.load.ok_or_else(|| PluginError::StateError {
            context: "clap.state::load",
            detail: "missing load callback".to_string(),
        })?;

        let mut cursor = ClapReadCursor { data, pos: 0 };
        let stream = clap_istream {
            ctx: (&mut cursor as *mut ClapReadCursor) as *mut c_void,
            read: Some(istream_read),
        };
        if unsafe { load_fn(self.plugin, &stream) } {
            Ok(())
        } else {
            Err(PluginError::StateError {
                context: "clap.state::load",
                detail: "plugin returned false".to_string(),
            })
        }
    }
}

impl Drop for ClapShared {
    fn drop(&mut self) {
        self.stop_and_deactivate();
        unsafe {
            if let Some(destroy) = (*self.plugin).destroy {
                destroy(self.plugin);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ClapAudioPortsLayout {
    input_port_count: usize,
    main_input_port: Option<u32>,
    sidechain_input_port: Option<u32>,
    sidechain_channels: u32,
}

/// Live CLAP plugin audio-thread instance backed by `clap_plugin`.
pub struct ClapPluginInstance {
    descriptor: PluginDescriptor,
    shared: Arc<ClapShared>,
    steady_time: i64,
    max_block_size: usize,
    prepared_sample_rate: f64,
    ports_layout: ClapAudioPortsLayout,
    in_scratch_l: Vec<f32>,
    in_scratch_r: Vec<f32>,
    sc_scratch_l: Vec<f32>,
    sc_scratch_r: Vec<f32>,
    silent_scratch: Vec<f32>,
    out_scratch_l: Vec<f32>,
    out_scratch_r: Vec<f32>,
    in_bufs: Vec<clap_audio_buffer>,
    event_queue: Vec<clap_event_param_value>,
    pending_params: Vec<(u32, f64)>,
}

/// Main-thread UI and state handle paired with a [`ClapPluginInstance`].
pub struct ClapUiHandle {
    shared: Arc<ClapShared>,
    gui_created: bool,
}

impl ClapPluginInstance {
    /// Loads a CLAP plugin by `plugin_id` from the given `.clap` binary, returning the
    /// audio-thread processor and main-thread UI/state handle.
    pub fn load_with_ui(path: &Path, plugin_id: &str) -> Result<(Self, ClapUiHandle), PluginError> {
        let guard = load_clap_entry(path)?;
        let factory = get_plugin_factory(&guard, path)?;
        let c_id = CString::new(plugin_id).map_err(|_| PluginError::PluginNotFound {
            path: path.to_path_buf(),
            plugin_id: plugin_id.to_string(),
        })?;

        let host = Box::new(build_clap_host());
        let create_fn = factory.create_plugin.ok_or_else(|| PluginError::InitFailed {
            context: "clap_plugin_factory::create_plugin",
            detail: "missing create_plugin function pointer".to_string(),
        })?;

        let plugin = unsafe { create_fn(factory, &*host, c_id.as_ptr()) };
        if plugin.is_null() {
            return Err(PluginError::PluginNotFound {
                path: path.to_path_buf(),
                plugin_id: plugin_id.to_string(),
            });
        }

        let init_ok = unsafe {
            (*plugin)
                .init
                .map(|f| f(plugin))
                .unwrap_or(false)
        };
        if !init_ok {
            if let Some(destroy) = unsafe { (*plugin).destroy } {
                unsafe { destroy(plugin) };
            }
            return Err(PluginError::InitFailed {
                context: "clap_plugin::init",
                detail: format!("plugin '{plugin_id}' returned false from init()"),
            });
        }

        let mut descriptor = unsafe {
            (*plugin)
                .desc
                .as_ref()
                .map(|d| descriptor_from_clap(d, path))
                .unwrap_or_else(|| PluginDescriptor {
                    format: PluginFormat::Clap,
                    id: plugin_id.to_string(),
                    name: plugin_id.to_string(),
                    vendor: String::new(),
                    version: String::new(),
                    path: Some(path.to_path_buf()),
                    is_instrument: false,
                    audio_inputs: 2,
                    audio_outputs: 2,
                    has_sidechain: false,
                })
        };

        let ports_layout = query_audio_ports(plugin, &mut descriptor);

        let shared = Arc::new(ClapShared {
            plugin,
            _host: host,
            activated: AtomicBool::new(false),
            processing: AtomicBool::new(false),
            _guard: guard,
        });
        let audio = Self {
            descriptor,
            shared: Arc::clone(&shared),
            steady_time: 0,
            max_block_size: 1024,
            prepared_sample_rate: 0.0,
            ports_layout,
            in_scratch_l: vec![0.0; 1024],
            in_scratch_r: vec![0.0; 1024],
            sc_scratch_l: vec![0.0; 1024],
            sc_scratch_r: vec![0.0; 1024],
            silent_scratch: vec![0.0; 1024],
            out_scratch_l: vec![0.0; 1024],
            out_scratch_r: vec![0.0; 1024],
            in_bufs: vec![
                clap_audio_buffer {
                    data32: ptr::null_mut(),
                    data64: ptr::null_mut(),
                    channel_count: 2,
                    latency: 0,
                    constant_mask: 0,
                };
                ports_layout.input_port_count
            ],
            event_queue: Vec::with_capacity(256),
            pending_params: Vec::with_capacity(64),
        };
        let ui = ClapUiHandle {
            shared,
            gui_created: false,
        };
        Ok((audio, ui))
    }
}

impl AudioPluginInstance for ClapPluginInstance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn prepare(&mut self, sample_rate: f64, max_block_size: usize) -> Result<(), PluginError> {
        let clamped_block = max_block_size.max(64);
        if self.shared.activated.load(Ordering::Acquire)
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
        let plugin = self.shared.plugin;
        unsafe {
            let activate = (*plugin).activate.ok_or_else(|| PluginError::PrepareFailed {
                sample_rate,
                block_size: max_block_size,
                detail: "missing activate()".to_string(),
            })?;
            if !activate(plugin, sample_rate, 1, self.max_block_size as u32) {
                return Err(PluginError::PrepareFailed {
                    sample_rate,
                    block_size: max_block_size,
                    detail: "clap_plugin::activate returned false".to_string(),
                });
            }
            self.prepared_sample_rate = sample_rate;
            self.shared.activated.store(true, Ordering::Release);
            let proc_ok = if let Some(start) = (*plugin).start_processing {
                start(plugin)
            } else {
                true
            };
            self.shared.processing.store(proc_ok, Ordering::Release);
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
        if !self.shared.activated.load(Ordering::Acquire) || outputs.len() < 2 {
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

        let sc_active = if self.ports_layout.sidechain_input_port.is_some() {
            if let Some(sc) = sidechain.filter(|s| s.len() >= 2) {
                let n0 = sc[0].len().min(frames);
                let n1 = sc[1].len().min(frames);
                if self.ports_layout.sidechain_channels == 1 {
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

        self.event_queue.clear();
        for (param_id, val) in self.pending_params.drain(..) {
            self.event_queue.push(clap_event_param_value {
                header: clap_event_header {
                    size: std::mem::size_of::<clap_event_param_value>() as u32,
                    time: 0,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: CLAP_EVENT_PARAM_VALUE,
                    flags: 0,
                },
                param_id,
                cookie: ptr::null_mut(),
                note_id: -1,
                port_index: 0,
                channel: -1,
                key: -1,
                value: val,
            });
        }

        let in_events = clap_input_events {
            ctx: (&self.event_queue as *const Vec<clap_event_param_value>) as *mut c_void,
            size: Some(input_events_size),
            get: Some(input_events_get),
        };
        let out_events = clap_output_events {
            ctx: ptr::null_mut(),
            try_push: Some(output_events_try_push),
        };

        let mut in_ptrs = [self.in_scratch_l.as_mut_ptr(), self.in_scratch_r.as_mut_ptr()];
        let mut sc_ptrs = [self.sc_scratch_l.as_mut_ptr(), self.sc_scratch_r.as_mut_ptr()];
        let mut silent_ptrs = [self.silent_scratch.as_mut_ptr(), self.silent_scratch.as_mut_ptr()];
        let mut out_ptrs = [self.out_scratch_l.as_mut_ptr(), self.out_scratch_r.as_mut_ptr()];

        for (idx, buf) in self.in_bufs.iter_mut().enumerate() {
            let port_idx = idx as u32;
            if Some(port_idx) == self.ports_layout.main_input_port {
                *buf = clap_audio_buffer {
                    data32: in_ptrs.as_mut_ptr(),
                    data64: ptr::null_mut(),
                    channel_count: 2,
                    latency: 0,
                    constant_mask: 0,
                };
            } else if Some(port_idx) == self.ports_layout.sidechain_input_port {
                let ch = self.ports_layout.sidechain_channels.clamp(1, 2);
                *buf = clap_audio_buffer {
                    data32: sc_ptrs.as_mut_ptr(),
                    data64: ptr::null_mut(),
                    channel_count: ch,
                    latency: 0,
                    constant_mask: if sc_active { 0 } else { (1 << ch) - 1 },
                };
            } else {
                *buf = clap_audio_buffer {
                    data32: silent_ptrs.as_mut_ptr(),
                    data64: ptr::null_mut(),
                    channel_count: 2,
                    latency: 0,
                    constant_mask: 0b11,
                };
            }
        }
        let mut out_buf = clap_audio_buffer {
            data32: out_ptrs.as_mut_ptr(),
            data64: ptr::null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };

        let process_ctx = clap_process {
            steady_time: self.steady_time,
            frames_count: frames as u32,
            transport: ptr::null(),
            audio_inputs: if self.in_bufs.is_empty() {
                ptr::null()
            } else {
                self.in_bufs.as_ptr()
            },
            audio_outputs: &mut out_buf,
            audio_inputs_count: self.in_bufs.len() as u32,
            audio_outputs_count: 1,
            in_events: &in_events,
            out_events: &out_events,
        };

        let plugin = self.shared.plugin;
        if let Some(proc_fn) = unsafe { (*plugin).process } {
            let status = unsafe { proc_fn(plugin, &process_ctx) };
            if status != CLAP_PROCESS_ERROR {
                outputs[0][..frames].copy_from_slice(&self.out_scratch_l[..frames]);
                outputs[1][..frames].copy_from_slice(&self.out_scratch_r[..frames]);
            }
        }
        self.steady_time += frames as i64;
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        self.shared.query_parameters()
    }

    fn set_parameter(&mut self, param_id: u32, value: f32) {
        self.pending_params.push((param_id, value as f64));
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        self.shared.load_state_bytes(data)
    }

    fn latency_samples(&self) -> u32 {
        if !self.shared.activated.load(Ordering::Acquire) {
            return 0;
        }
        let Some(ext) = (unsafe { self.shared.get_ext::<clap_plugin_latency>(CLAP_EXT_LATENCY) })
        else {
            return 0;
        };
        unsafe { ext.get.map_or(0, |get_fn| get_fn(self.shared.plugin)) }
    }
}

impl Drop for ClapUiHandle {
    fn drop(&mut self) {
        self.close_editor();
    }
}

impl PluginUiHandle for ClapUiHandle {
    fn has_editor(&self) -> bool {
        let Some(ext) = (unsafe { self.shared.get_ext::<clap_plugin_gui>(CLAP_EXT_GUI) }) else {
            return false;
        };
        ext.is_api_supported
            .map(|f| unsafe { f(self.shared.plugin, CLAP_WINDOW_API_WIN32.as_ptr(), false) })
            .unwrap_or(false)
    }

    fn open_editor(&mut self, parent_hwnd: isize) -> Result<(), PluginError> {
        let plugin = self.shared.plugin;
        let ext = *unsafe { self.shared.get_ext::<clap_plugin_gui>(CLAP_EXT_GUI) }.ok_or_else(|| {
            PluginError::GuiError {
                context: "clap.gui",
                detail: "plugin does not expose clap.gui".to_string(),
            }
        })?;
        let create_fn = ext.create.ok_or_else(|| PluginError::GuiError {
            context: "clap.gui::create",
            detail: "missing create".to_string(),
        })?;
        if !unsafe { create_fn(plugin, CLAP_WINDOW_API_WIN32.as_ptr(), false) } {
            return Err(PluginError::GuiError {
                context: "clap.gui::create",
                detail: "failed to create Win32 plugin GUI".to_string(),
            });
        }
        self.gui_created = true;

        let win = clap_window {
            api: CLAP_WINDOW_API_WIN32.as_ptr(),
            specific: clap_window_handle {
                win32: parent_hwnd as *mut c_void,
            },
        };
        if let Some(set_parent) = ext.set_parent {
            if !unsafe { set_parent(plugin, &win) } {
                return Err(PluginError::GuiError {
                    context: "clap.gui::set_parent",
                    detail: "set_parent returned false".to_string(),
                });
            }
        }
        if let Some(show) = ext.show {
            unsafe {
                let _ = show(plugin);
            }
        }
        Ok(())
    }

    fn close_editor(&mut self) {
        if !self.gui_created {
            return;
        }
        if let Some(ext) = unsafe { self.shared.get_ext::<clap_plugin_gui>(CLAP_EXT_GUI) } {
            if let Some(hide) = ext.hide {
                unsafe {
                    let _ = hide(self.shared.plugin);
                }
            }
            if let Some(destroy) = ext.destroy {
                unsafe { destroy(self.shared.plugin) };
            }
        }
        self.gui_created = false;
    }

    fn preferred_size(&self) -> Option<(u32, u32)> {
        if !self.gui_created {
            return None;
        }
        let ext = unsafe { self.shared.get_ext::<clap_plugin_gui>(CLAP_EXT_GUI) }?;
        let get_size = ext.get_size?;
        let (mut w, mut h) = (0u32, 0u32);
        if unsafe { get_size(self.shared.plugin, &mut w, &mut h) } && w > 0 && h > 0 {
            Some((w.clamp(160, 3840), h.clamp(120, 2160)))
        } else {
            None
        }
    }

    fn resize_editor(&mut self, width: u32, height: u32) {
        if !self.gui_created {
            return;
        }
        if let Some(ext) = unsafe { self.shared.get_ext::<clap_plugin_gui>(CLAP_EXT_GUI) } {
            if let Some(set_size) = ext.set_size {
                unsafe {
                    let _ = set_size(self.shared.plugin, width.max(160), height.max(120));
                }
            }
        }
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        self.shared.save_state_bytes()
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        self.shared.query_parameters()
    }
}

fn load_clap_entry(path: &Path) -> Result<ClapLibraryGuard, PluginError> {
    let lib = unsafe { Library::new(path) }.map_err(|e| PluginError::BinaryLoad {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;

    let entry_sym: libloading::Symbol<*const clap_plugin_entry> =
        unsafe { lib.get(b"clap_entry\0") }.map_err(|e| PluginError::BinaryLoad {
            path: path.to_path_buf(),
            detail: format!("missing 'clap_entry' symbol: {e}"),
        })?;

    let entry = *entry_sym;
    if entry.is_null() {
        return Err(PluginError::BinaryLoad {
            path: path.to_path_buf(),
            detail: "clap_entry pointer is null".to_string(),
        });
    }

    let path_str = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| {
        PluginError::BinaryLoad {
            path: path.to_path_buf(),
            detail: "invalid NUL byte in path".to_string(),
        }
    })?;

    let init_ok = unsafe {
        (*entry)
            .init
            .map(|f| f(path_str.as_ptr()))
            .unwrap_or(false)
    };
    if !init_ok {
        return Err(PluginError::InitFailed {
            context: "clap_plugin_entry::init",
            detail: format!("clap_entry->init returned false for '{}'", path.display()),
        });
    }

    Ok(ClapLibraryGuard { _lib: lib, entry })
}

fn get_plugin_factory<'a>(
    guard: &'a ClapLibraryGuard,
    path: &Path,
) -> Result<&'a clap_plugin_factory, PluginError> {
    let get_factory = unsafe { (*guard.entry).get_factory }.ok_or_else(|| PluginError::BinaryLoad {
        path: path.to_path_buf(),
        detail: "missing get_factory function".to_string(),
    })?;

    let ptr = unsafe { get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) } as *const clap_plugin_factory;
    unsafe { ptr.as_ref() }.ok_or_else(|| PluginError::BinaryLoad {
        path: path.to_path_buf(),
        detail: "plugin does not export clap.plugin-factory".to_string(),
    })
}

fn query_audio_ports(
    plugin: *const clap_plugin,
    descriptor: &mut PluginDescriptor,
) -> ClapAudioPortsLayout {
    let fallback_ports = if descriptor.audio_inputs > 0 { 1 } else { 0 };
    let mut layout = ClapAudioPortsLayout {
        input_port_count: fallback_ports,
        main_input_port: if fallback_ports > 0 { Some(0) } else { None },
        sidechain_input_port: None,
        sidechain_channels: 0,
    };

    let Some(get_ext) = (unsafe { (*plugin).get_extension }) else {
        return layout;
    };
    let ext_ptr = unsafe { get_ext(plugin, CLAP_EXT_AUDIO_PORTS.as_ptr()) } as *const clap_plugin_audio_ports;
    let Some(ext) = (unsafe { ext_ptr.as_ref() }) else {
        return layout;
    };
    let Some(count_fn) = ext.count else {
        return layout;
    };
    let Some(get_fn) = ext.get else {
        return layout;
    };

    let in_count = unsafe { count_fn(plugin, true) };
    let mut in_channels = 0usize;
    let mut main_port = None;
    let mut sc_port = None;
    let mut sc_channels = 0u32;

    for idx in 0..in_count {
        let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
        if unsafe { get_fn(plugin, idx, true, &mut info) } {
            in_channels += info.channel_count as usize;
            let is_main = (info.flags & CLAP_AUDIO_PORT_IS_MAIN) != 0;
            if is_main && main_port.is_none() {
                main_port = Some(idx);
            } else if !is_main && sc_port.is_none() && info.channel_count >= 1 {
                sc_port = Some(idx);
                sc_channels = info.channel_count.clamp(1, 2);
            }
        }
    }
    if main_port.is_none() && in_count > 0 && !descriptor.is_instrument && sc_port != Some(0) {
        main_port = Some(0);
    }
    if sc_port == main_port {
        sc_port = None;
    }

    let out_count = unsafe { count_fn(plugin, false) };
    let mut out_channels = 0usize;
    for idx in 0..out_count {
        let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
        if unsafe { get_fn(plugin, idx, false, &mut info) } {
            out_channels += info.channel_count as usize;
        }
    }

    descriptor.audio_inputs = in_channels;
    descriptor.audio_outputs = out_channels.max(2);
    descriptor.has_sidechain = sc_port.is_some();

    layout.input_port_count = in_count as usize;
    layout.main_input_port = main_port;
    layout.sidechain_input_port = sc_port;
    layout.sidechain_channels = sc_channels;
    layout
}

fn descriptor_from_clap(desc: &clap_plugin_descriptor, path: &Path) -> PluginDescriptor {
    let mut is_instrument = false;
    if !desc.features.is_null() {
        let mut ptr = desc.features;
        unsafe {
            while !(*ptr).is_null() {
                if let Ok(feat) = CStr::from_ptr(*ptr).to_str() {
                    if feat == "instrument" {
                        is_instrument = true;
                        break;
                    }
                }
                ptr = ptr.add(1);
            }
        }
    }

    PluginDescriptor {
        format: PluginFormat::Clap,
        id: cptr_to_string(desc.id),
        name: cptr_to_string(desc.name),
        vendor: cptr_to_string(desc.vendor),
        version: cptr_to_string(desc.version),
        path: Some(path.to_path_buf()),
        is_instrument,
        audio_inputs: if is_instrument { 0 } else { 2 },
        audio_outputs: 2,
        has_sidechain: false,
    }
}

fn build_clap_host() -> clap_host {
    clap_host {
        clap_version: CLAP_VERSION,
        host_data: ptr::null_mut(),
        name: c"StageHost".as_ptr(),
        vendor: c"StageHost Project".as_ptr(),
        url: c"https://localhost".as_ptr(),
        version: c"0.1.0".as_ptr(),
        get_extension: Some(host_get_extension),
        request_restart: Some(host_noop),
        request_process: Some(host_noop),
        request_callback: Some(host_noop),
    }
}

unsafe extern "C" fn host_get_extension(
    _host: *const clap_host,
    _extension_id: *const c_char,
) -> *const c_void {
    ptr::null()
}

unsafe extern "C" fn host_noop(_host: *const clap_host) {}

unsafe extern "C" fn input_events_size(list: *const clap_input_events) -> u32 {
    let vec_ref = unsafe { &*((*list).ctx as *const Vec<clap_event_param_value>) };
    vec_ref.len() as u32
}

unsafe extern "C" fn input_events_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    let vec_ref = unsafe { &*((*list).ctx as *const Vec<clap_event_param_value>) };
    vec_ref
        .get(index as usize)
        .map(|e| &e.header as *const clap_event_header)
        .unwrap_or(ptr::null())
}

unsafe extern "C" fn output_events_try_push(
    _list: *const clap_output_events,
    _event: *const clap_event_header,
) -> bool {
    true
}

struct ClapReadCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

unsafe extern "C" fn ostream_write(
    stream: *const clap_ostream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    let vec = unsafe { &mut *((*stream).ctx as *mut Vec<u8>) };
    let slice = unsafe { std::slice::from_raw_parts(buffer as *const u8, size as usize) };
    vec.extend_from_slice(slice);
    size as i64
}

unsafe extern "C" fn istream_read(
    stream: *const clap_istream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    let cursor = unsafe { &mut *((*stream).ctx as *mut ClapReadCursor) };
    let remaining = cursor.data.len().saturating_sub(cursor.pos);
    let to_copy = remaining.min(size as usize);
    if to_copy > 0 {
        unsafe {
            ptr::copy_nonoverlapping(
                cursor.data[cursor.pos..].as_ptr(),
                buffer as *mut u8,
                to_copy,
            );
        }
        cursor.pos += to_copy;
    }
    to_copy as i64
}

fn cptr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn cstr_buf_to_string(buf: &[c_char]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let bytes: Vec<u8> = buf[..len].iter().map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

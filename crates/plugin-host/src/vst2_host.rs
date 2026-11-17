use std::cell::Cell;
use std::ffi::{c_char, c_void, CStr};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use libloading::Library;

use crate::{
    AudioPluginInstance, PluginDescriptor, PluginError, PluginFormat, PluginParamInfo,
    PluginUiHandle,
};

const K_EFFECT_MAGIC: i32 = 0x5673_7450; // "VstP"

const EFF_FLAGS_HAS_EDITOR: i32 = 1 << 0;
const EFF_FLAGS_PROGRAM_CHUNKS: i32 = 1 << 5;
const EFF_FLAGS_IS_SYNTH: i32 = 1 << 8;

const K_PLUG_CATEG_SYNTH: isize = 2;
const K_PLUG_CATEG_SHELL: isize = 9;

const EFF_OPEN: i32 = 0;
const EFF_CLOSE: i32 = 1;
const EFF_GET_PARAM_NAME: i32 = 8;
const EFF_SET_SAMPLE_RATE: i32 = 10;
const EFF_SET_BLOCK_SIZE: i32 = 11;
const EFF_MAINS_CHANGED: i32 = 12;
const EFF_EDIT_GET_RECT: i32 = 13;
const EFF_EDIT_OPEN: i32 = 14;
const EFF_EDIT_CLOSE: i32 = 15;
const EFF_EDIT_IDLE: i32 = 19;
const EFF_GET_CHUNK: i32 = 23;
const EFF_SET_CHUNK: i32 = 24;
const EFF_GET_PLUG_CATEGORY: i32 = 35;
const EFF_GET_EFFECT_NAME: i32 = 45;
const EFF_GET_VENDOR_STRING: i32 = 47;
const EFF_GET_PRODUCT_STRING: i32 = 48;
const EFF_GET_VENDOR_VERSION: i32 = 49;
const EFF_CAN_DO: i32 = 51;
const EFF_SHELL_GET_NEXT_PLUGIN: i32 = 70;
const EFF_START_PROCESS: i32 = 71;
const EFF_STOP_PROCESS: i32 = 72;

const AM_AUTOMATE: i32 = 0;
const AM_VERSION: i32 = 1;
const AM_CURRENT_ID: i32 = 6;
const AM_GET_TIME: i32 = 7;
const AM_NEED_IDLE: i32 = 14;
const AM_SIZE_WINDOW: i32 = 15;
const AM_GET_SAMPLE_RATE: i32 = 16;
const AM_GET_BLOCK_SIZE: i32 = 17;
const AM_GET_CURRENT_PROCESS_LEVEL: i32 = 23;
const AM_GET_AUTOMATION_STATE: i32 = 24;
const AM_GET_VENDOR_STRING: i32 = 32;
const AM_GET_PRODUCT_STRING: i32 = 33;
const AM_GET_VENDOR_VERSION: i32 = 34;
const AM_CAN_DO: i32 = 37;
const AM_GET_LANGUAGE: i32 = 38;
const AM_BEGIN_EDIT: i32 = 43;
const AM_END_EDIT: i32 = 44;

const K_VST_TRANSPORT_PLAYING: i32 = 1 << 1;
const K_VST_TEMPO_VALID: i32 = 1 << 10;
const K_VST_TIME_SIG_VALID: i32 = 1 << 13;

type AudioMasterCallback = unsafe extern "C" fn(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    ptr: *mut c_void,
    opt: f32,
) -> isize;

type Vst2EntryFn = unsafe extern "C" fn(audio_master: AudioMasterCallback) -> *mut AEffect;

#[repr(C)]
struct AEffect {
    magic: i32,
    dispatcher: Option<
        unsafe extern "C" fn(
            effect: *mut AEffect,
            opcode: i32,
            index: i32,
            value: isize,
            ptr: *mut c_void,
            opt: f32,
        ) -> isize,
    >,
    _process_deprecated: *const c_void,
    set_parameter: Option<unsafe extern "C" fn(effect: *mut AEffect, index: i32, value: f32)>,
    get_parameter: Option<unsafe extern "C" fn(effect: *mut AEffect, index: i32) -> f32>,
    num_programs: i32,
    num_params: i32,
    num_inputs: i32,
    num_outputs: i32,
    flags: i32,
    resvd1: isize,
    resvd2: isize,
    initial_delay: i32,
    real_qualities: i32,
    off_qualities: i32,
    io_ratio: f32,
    object: *mut c_void,
    user: *mut c_void,
    unique_id: i32,
    version: i32,
    process_replacing: Option<
        unsafe extern "C" fn(
            effect: *mut AEffect,
            inputs: *const *const f32,
            outputs: *mut *mut f32,
            sample_frames: i32,
        ),
    >,
    process_double_replacing: Option<
        unsafe extern "C" fn(
            effect: *mut AEffect,
            inputs: *const *const f64,
            outputs: *mut *mut f64,
            sample_frames: i32,
        ),
    >,
    future: [u8; 56],
}

#[repr(C)]
#[derive(Default, Copy, Clone)]
struct ERect {
    top: i16,
    left: i16,
    bottom: i16,
    right: i16,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct VstTimeInfo {
    sample_pos: f64,
    sample_rate: f64,
    nano_seconds: f64,
    ppq_pos: f64,
    tempo: f64,
    bar_start_pos: f64,
    cycle_start_pos: f64,
    cycle_end_pos: f64,
    time_sig_numerator: i32,
    time_sig_denominator: i32,
    smpte_offset: i32,
    smpte_frame_rate: i32,
    samples_to_next_clock: i32,
    flags: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Vst2DllCheck {
    Supported64Bit,
    Unsupported32Bit,
    NotVst2Plugin,
}

#[derive(Clone, Copy)]
struct PeSection {
    virtual_addr: u32,
    virtual_size: u32,
    raw_size: u32,
    raw_ptr: u32,
}

thread_local! {
    static PENDING_SHELL_PLUGIN_ID: Cell<i32> = const { Cell::new(0) };
    static THREAD_TIME_INFO: Cell<VstTimeInfo> = const {
        Cell::new(VstTimeInfo {
            sample_pos: 0.0,
            sample_rate: 48_000.0,
            nano_seconds: 0.0,
            ppq_pos: 0.0,
            tempo: 120.0,
            bar_start_pos: 0.0,
            cycle_start_pos: 0.0,
            cycle_end_pos: 4.0,
            time_sig_numerator: 4,
            time_sig_denominator: 4,
            smpte_offset: 0,
            smpte_frame_rate: 30,
            samples_to_next_clock: 0,
            flags: K_VST_TRANSPORT_PLAYING | K_VST_TEMPO_VALID | K_VST_TIME_SIG_VALID,
        })
    };
}

struct Vst2HostContext {
    sample_rate_hz: AtomicI32,
    block_size: AtomicUsize,
    requested_size: Arc<Mutex<Option<(u32, u32)>>>,
    shell_plugin_id: AtomicI32,
}

impl Default for Vst2HostContext {
    fn default() -> Self {
        Self {
            sample_rate_hz: AtomicI32::new(48_000),
            block_size: AtomicUsize::new(1024),
            requested_size: Arc::new(Mutex::new(None)),
            shell_plugin_id: AtomicI32::new(0),
        }
    }
}

/// Inspects a Windows PE `.dll` without executing it to tell 64-bit VST2 plugins apart from
/// 32-bit plugins and non-plugin companion DLLs (core DSP/resource libraries).
pub(crate) fn inspect_dll_vst2_candidate(path: &Path) -> Result<Vst2DllCheck, String> {
    let mut file = File::open(path).map_err(|e| format!("failed to open '{}': {e}", path.display()))?;
    let mut header = [0u8; 64];
    if file.read_exact(&mut header).is_err() || u16::from_le_bytes([header[0], header[1]]) != 0x5A4D
    {
        return Ok(Vst2DllCheck::NotVst2Plugin);
    }

    let pe_offset = u32::from_le_bytes([header[0x3C], header[0x3D], header[0x3E], header[0x3F]]) as u64;
    file.seek(SeekFrom::Start(pe_offset))
        .map_err(|e| format!("seek PE header failed: {e}"))?;
    let mut sig = [0u8; 4];
    file.read_exact(&mut sig)
        .map_err(|_| "invalid PE signature".to_string())?;
    if &sig != b"PE\0\0" {
        return Ok(Vst2DllCheck::NotVst2Plugin);
    }

    let mut coff = [0u8; 20];
    file.read_exact(&mut coff)
        .map_err(|_| "truncated PE COFF header".to_string())?;
    let machine = u16::from_le_bytes([coff[0], coff[1]]);
    let num_sections = u16::from_le_bytes([coff[2], coff[3]]) as usize;
    let opt_size = u16::from_le_bytes([coff[16], coff[17]]) as usize;
    if machine == 0x014C {
        return Ok(Vst2DllCheck::Unsupported32Bit);
    }
    if machine != 0x8664 || opt_size < 112 {
        return Ok(Vst2DllCheck::NotVst2Plugin);
    }

    let mut opt = vec![0u8; opt_size];
    file.read_exact(&mut opt)
        .map_err(|_| "truncated PE optional header".to_string())?;
    let magic = u16::from_le_bytes([opt[0], opt[1]]);
    if magic != 0x020B {
        return Ok(Vst2DllCheck::NotVst2Plugin);
    }

    // PE32+ DataDirectory[0] (Export Table) starts at byte 112 of Optional Header.
    let export_rva = u32::from_le_bytes([opt[112], opt[113], opt[114], opt[115]]);
    let export_size = u32::from_le_bytes([opt[116], opt[117], opt[118], opt[119]]);
    if export_rva == 0 || export_size == 0 {
        return Ok(Vst2DllCheck::NotVst2Plugin);
    }

    let mut sec_buf = vec![0u8; num_sections * 40];
    file.read_exact(&mut sec_buf)
        .map_err(|_| "truncated PE section headers".to_string())?;
    let (sec_chunks, _) = sec_buf.as_chunks::<40>();
    let sections: Vec<PeSection> = sec_chunks
        .iter()
        .map(|s| PeSection {
            virtual_size: u32::from_le_bytes([s[8], s[9], s[10], s[11]]),
            virtual_addr: u32::from_le_bytes([s[12], s[13], s[14], s[15]]),
            raw_size: u32::from_le_bytes([s[16], s[17], s[18], s[19]]),
            raw_ptr: u32::from_le_bytes([s[20], s[21], s[22], s[23]]),
        })
        .collect();

    let Some(exp_off) = rva_to_offset(&sections, export_rva) else {
        return Ok(Vst2DllCheck::NotVst2Plugin);
    };
    let mut exp_dir = [0u8; 40];
    file.seek(SeekFrom::Start(exp_off))
        .and_then(|_| file.read_exact(&mut exp_dir))
        .map_err(|_| "invalid PE export directory".to_string())?;

    let num_names = u32::from_le_bytes([exp_dir[24], exp_dir[25], exp_dir[26], exp_dir[27]]) as usize;
    let names_rva = u32::from_le_bytes([exp_dir[32], exp_dir[33], exp_dir[34], exp_dir[35]]);
    if num_names == 0 || names_rva == 0 {
        return Ok(Vst2DllCheck::NotVst2Plugin);
    }
    let Some(names_off) = rva_to_offset(&sections, names_rva) else {
        return Ok(Vst2DllCheck::NotVst2Plugin);
    };

    let mut name_rvas = vec![0u8; num_names.min(4096) * 4];
    file.seek(SeekFrom::Start(names_off))
        .and_then(|_| file.read_exact(&mut name_rvas))
        .map_err(|_| "failed to read PE export name table".to_string())?;

    let (rva_chunks, _) = name_rvas.as_chunks::<4>();
    for chunk in rva_chunks {
        let name_rva = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if let Some(name_off) = rva_to_offset(&sections, name_rva) {
            if let Some(name) = read_cstr_at(&mut file, name_off, 64) {
                if name == "VSTPluginMain" || name == "main" {
                    return Ok(Vst2DllCheck::Supported64Bit);
                }
            }
        }
    }
    Ok(Vst2DllCheck::NotVst2Plugin)
}

fn rva_to_offset(sections: &[PeSection], rva: u32) -> Option<u64> {
    for sec in sections {
        let span = sec.virtual_size.max(sec.raw_size);
        if rva >= sec.virtual_addr && rva < sec.virtual_addr.saturating_add(span) {
            let delta = rva - sec.virtual_addr;
            if delta < sec.raw_size {
                return Some((sec.raw_ptr + delta) as u64);
            }
        }
    }
    None
}

fn read_cstr_at(file: &mut File, offset: u64, max_len: usize) -> Option<String> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; max_len];
    let n = file.read(&mut buf).ok()?;
    let slice = &buf[..n];
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    Some(String::from_utf8_lossy(&slice[..end]).into_owned())
}

/// Scans a single `.dll` file as a 64-bit VST2 plugin.
/// Returns `Ok(Vec::new())` for non-plugin companion DLLs so callers can skip them cleanly.
pub fn scan_vst2_file(path: &Path) -> Result<Vec<PluginDescriptor>, PluginError> {
    match inspect_dll_vst2_candidate(path) {
        Ok(Vst2DllCheck::NotVst2Plugin) => return Ok(Vec::new()),
        Ok(Vst2DllCheck::Unsupported32Bit) => {
            return Err(PluginError::BinaryLoad {
                path: path.to_path_buf(),
                detail: "32-bit x86 VST2 DLL cannot run in 64-bit host".to_string(),
            });
        }
        Ok(Vst2DllCheck::Supported64Bit) => {}
        Err(detail) => {
            return Err(PluginError::BinaryLoad {
                path: path.to_path_buf(),
                detail,
            });
        }
    }

    let shared = Vst2Shared::open(path, 0)?;
    let fallback_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("VST2 Plugin")
        .to_string();

    let category = unsafe { shared.dispatch(EFF_GET_PLUG_CATEGORY, 0, 0, ptr::null_mut(), 0.0) };
    if category == K_PLUG_CATEG_SHELL {
        let mut results = Vec::new();
        for _ in 0..512 {
            let mut sub_name = [0u8; 128];
            let sub_id = unsafe {
                shared.dispatch(
                    EFF_SHELL_GET_NEXT_PLUGIN,
                    0,
                    0,
                    sub_name.as_mut_ptr() as *mut c_void,
                    0.0,
                )
            } as i32;
            if sub_id == 0 {
                break;
            }
            let parsed_name = c_buf_to_string(&sub_name);
            let name = if parsed_name.is_empty() {
                format!("{fallback_name} #{sub_id:08X}")
            } else {
                parsed_name
            };
            results.push(shared.build_descriptor(path, Some(name), Some(sub_id)));
        }
        if !results.is_empty() {
            return Ok(results);
        }
    }

    Ok(vec![shared.build_descriptor(path, None, None)])
}

struct Vst2Shared {
    effect: *mut AEffect,
    host_ctx: Box<Vst2HostContext>,
    active: AtomicBool,
    processing: AtomicBool,
    // Keep DLL loaded until after `effMainsChanged(0)`, `effStopProcess`, and `effClose`.
    _lib: Library,
}

unsafe impl Send for Vst2Shared {}
unsafe impl Sync for Vst2Shared {}

impl Vst2Shared {
    fn open(path: &Path, shell_unique_id: i32) -> Result<Self, PluginError> {
        let lib = unsafe { Library::new(path) }.map_err(|e| PluginError::BinaryLoad {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;

        let entry: Vst2EntryFn = unsafe {
            if let Ok(sym) = lib.get::<Vst2EntryFn>(b"VSTPluginMain\0") {
                *sym
            } else if let Ok(sym) = lib.get::<Vst2EntryFn>(b"main\0") {
                *sym
            } else {
                return Err(PluginError::BinaryLoad {
                    path: path.to_path_buf(),
                    detail: "missing VSTPluginMain/main export".to_string(),
                });
            }
        };

        let host_ctx = Box::new(Vst2HostContext::default());
        host_ctx
            .shell_plugin_id
            .store(shell_unique_id, Ordering::Release);

        PENDING_SHELL_PLUGIN_ID.with(|cell| cell.set(shell_unique_id));
        let effect = unsafe { entry(vst2_host_callback) };
        PENDING_SHELL_PLUGIN_ID.with(|cell| cell.set(0));

        if effect.is_null() {
            return Err(PluginError::InitFailed {
                context: "VSTPluginMain",
                detail: format!("VST2 entry point returned null for '{}'", path.display()),
            });
        }

        let magic = unsafe { (*effect).magic };
        if magic != K_EFFECT_MAGIC {
            return Err(PluginError::InitFailed {
                context: "AEffect::magic",
                detail: format!("unexpected VST2 magic 0x{magic:08X}"),
            });
        }

        unsafe {
            (*effect).resvd1 = (&*host_ctx as *const Vst2HostContext) as isize;
        }

        let shared = Self {
            effect,
            host_ctx,
            active: AtomicBool::new(false),
            processing: AtomicBool::new(false),
            _lib: lib,
        };
        unsafe {
            shared.dispatch(EFF_OPEN, 0, 0, ptr::null_mut(), 0.0);
        }
        Ok(shared)
    }

    unsafe fn dispatch(
        &self,
        opcode: i32,
        index: i32,
        value: isize,
        ptr: *mut c_void,
        opt: f32,
    ) -> isize {
        unsafe {
            let Some(disp) = (*self.effect).dispatcher else {
                return 0;
            };
            disp(self.effect, opcode, index, value, ptr, opt)
        }
    }

    fn stop_and_deactivate(&self) {
        if self.processing.swap(false, Ordering::AcqRel) {
            unsafe {
                self.dispatch(EFF_STOP_PROCESS, 0, 0, ptr::null_mut(), 0.0);
            }
        }
        if self.active.swap(false, Ordering::AcqRel) {
            unsafe {
                self.dispatch(EFF_MAINS_CHANGED, 0, 0, ptr::null_mut(), 0.0);
            }
        }
    }

    fn build_descriptor(
        &self,
        path: &Path,
        override_name: Option<String>,
        override_id: Option<i32>,
    ) -> PluginDescriptor {
        let mut name_buf = [0u8; 128];
        let mut vendor_buf = [0u8; 128];
        let mut product_buf = [0u8; 128];
        let (flags, num_inputs, num_outputs, raw_id, eff_version) = unsafe {
            self.dispatch(EFF_GET_EFFECT_NAME, 0, 0, name_buf.as_mut_ptr() as *mut c_void, 0.0);
            self.dispatch(
                EFF_GET_VENDOR_STRING,
                0,
                0,
                vendor_buf.as_mut_ptr() as *mut c_void,
                0.0,
            );
            self.dispatch(
                EFF_GET_PRODUCT_STRING,
                0,
                0,
                product_buf.as_mut_ptr() as *mut c_void,
                0.0,
            );
            let ver = self.dispatch(EFF_GET_VENDOR_VERSION, 0, 0, ptr::null_mut(), 0.0);
            let e = &*self.effect;
            (
                e.flags,
                e.num_inputs.max(0) as usize,
                e.num_outputs.max(0) as usize,
                e.unique_id,
                if ver > 0 { ver as i32 } else { e.version },
            )
        };

        let fallback_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("VST2 Plugin")
            .to_string();
        let name = override_name.unwrap_or_else(|| {
            let eff = c_buf_to_string(&name_buf);
            if !eff.is_empty() {
                eff
            } else {
                let prod = c_buf_to_string(&product_buf);
                if !prod.is_empty() {
                    prod
                } else {
                    fallback_stem
                }
            }
        });
        let vendor = c_buf_to_string(&vendor_buf);
        let category = unsafe { self.dispatch(EFF_GET_PLUG_CATEGORY, 0, 0, ptr::null_mut(), 0.0) };
        let is_instrument = (flags & EFF_FLAGS_IS_SYNTH) != 0 || category == K_PLUG_CATEG_SYNTH;
        let effective_id = override_id.unwrap_or(raw_id);

        PluginDescriptor {
            format: PluginFormat::Vst2,
            id: format!("vst2:{:08X}", effective_id as u32),
            name,
            vendor,
            version: format_vst2_version(eff_version),
            path: Some(path.to_path_buf()),
            is_instrument,
            audio_inputs: num_inputs,
            audio_outputs: num_outputs,
            has_sidechain: !is_instrument && num_inputs >= 4,
        }
    }

    fn query_parameters(&self) -> Vec<PluginParamInfo> {
        let count = unsafe { (*self.effect).num_params.max(0) as usize }.min(512);
        let get_param = unsafe { (*self.effect).get_parameter };
        let mut out = Vec::with_capacity(count);
        for idx in 0..count {
            let mut label = [0u8; 64];
            unsafe {
                self.dispatch(
                    EFF_GET_PARAM_NAME,
                    idx as i32,
                    0,
                    label.as_mut_ptr() as *mut c_void,
                    0.0,
                );
            }
            let parsed = c_buf_to_string(&label);
            let name = if parsed.is_empty() {
                format!("Param {}", idx + 1)
            } else {
                parsed
            };
            let value = get_param
                .map(|f| unsafe { f(self.effect, idx as i32) })
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            out.push(PluginParamInfo {
                id: idx as u32,
                name,
                value,
                min: 0.0,
                max: 1.0,
            });
        }
        out
    }

    fn load_state_bytes(&self, data: &[u8]) -> Result<(), PluginError> {
        if data.is_empty() {
            return Ok(());
        }
        let flags = unsafe { (*self.effect).flags };
        if (flags & EFF_FLAGS_PROGRAM_CHUNKS) != 0 {
            let rc = unsafe {
                self.dispatch(
                    EFF_SET_CHUNK,
                    0,
                    data.len() as isize,
                    data.as_ptr() as *mut c_void,
                    0.0,
                )
            };
            if rc == 0 {
                // Retry with preset-level chunk flag `index = 1` when bank flag `0` returns 0.
                unsafe {
                    self.dispatch(
                        EFF_SET_CHUNK,
                        1,
                        data.len() as isize,
                        data.as_ptr() as *mut c_void,
                        0.0,
                    );
                }
            }
            return Ok(());
        }

        if let Some(set_param) = unsafe { (*self.effect).set_parameter } {
            let count = unsafe { (*self.effect).num_params.max(0) as usize };
            let (f32_chunks, _) = data.as_chunks::<4>();
            for (idx, chunk) in f32_chunks.iter().take(count).enumerate() {
                let val = f32::from_le_bytes(*chunk);
                if val.is_finite() {
                    unsafe {
                        set_param(self.effect, idx as i32, val.clamp(0.0, 1.0));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Drop for Vst2Shared {
    fn drop(&mut self) {
        self.stop_and_deactivate();
        unsafe {
            self.dispatch(EFF_CLOSE, 0, 0, ptr::null_mut(), 0.0);
        }
    }
}

/// Live 64-bit VST2 plugin audio-thread instance backed by `AEffect`.
pub struct Vst2PluginInstance {
    descriptor: PluginDescriptor,
    shared: Arc<Vst2Shared>,
    max_block_size: usize,
    prepared_sample_rate: f64,
    num_outputs: usize,
    in_scratch: Vec<Vec<f32>>,
    out_scratch: Vec<Vec<f32>>,
    in_ptrs: Vec<*const f32>,
    out_ptrs: Vec<*mut f32>,
}

// Safety: `in_ptrs`/`out_ptrs` are overwritten each audio block using `self.in_scratch`/`self.out_scratch`.
unsafe impl Send for Vst2PluginInstance {}

/// Main-thread UI and state handle paired with a [`Vst2PluginInstance`].
pub struct Vst2UiHandle {
    shared: Arc<Vst2Shared>,
    editor_open: bool,
}

impl Vst2PluginInstance {
    pub fn load_with_ui(path: &Path, unique_id: i32) -> Result<(Self, Vst2UiHandle), PluginError> {
        let shared = Arc::new(Vst2Shared::open(path, unique_id)?);
        let descriptor = shared.build_descriptor(path, None, (unique_id != 0).then_some(unique_id));
        let num_inputs = descriptor.audio_inputs;
        let num_outputs = descriptor.audio_outputs;
        let in_buf_count = num_inputs;
        let out_buf_count = num_outputs.max(2);

        let audio = Self {
            descriptor,
            shared: Arc::clone(&shared),
            max_block_size: 1024,
            prepared_sample_rate: 0.0,
            num_outputs,
            in_scratch: vec![vec![0.0; 1024]; in_buf_count],
            out_scratch: vec![vec![0.0; 1024]; out_buf_count],
            in_ptrs: vec![ptr::null(); in_buf_count],
            out_ptrs: vec![ptr::null_mut(); out_buf_count],
        };
        let ui = Vst2UiHandle {
            shared,
            editor_open: false,
        };
        Ok((audio, ui))
    }
}

impl AudioPluginInstance for Vst2PluginInstance {
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
        for buf in &mut self.in_scratch {
            buf.resize(self.max_block_size, 0.0);
        }
        for buf in &mut self.out_scratch {
            buf.resize(self.max_block_size, 0.0);
        }

        self.shared.stop_and_deactivate();
        self.shared
            .host_ctx
            .sample_rate_hz
            .store(sample_rate.round() as i32, Ordering::Release);
        self.shared
            .host_ctx
            .block_size
            .store(self.max_block_size, Ordering::Release);

        unsafe {
            self.shared
                .dispatch(EFF_SET_SAMPLE_RATE, 0, 0, ptr::null_mut(), sample_rate as f32);
            self.shared.dispatch(
                EFF_SET_BLOCK_SIZE,
                0,
                self.max_block_size as isize,
                ptr::null_mut(),
                0.0,
            );
            self.shared
                .dispatch(EFF_MAINS_CHANGED, 0, 1, ptr::null_mut(), 0.0);
            self.shared.active.store(true, Ordering::Release);
            self.shared
                .dispatch(EFF_START_PROCESS, 0, 0, ptr::null_mut(), 0.0);
            self.shared.processing.store(true, Ordering::Release);
        }
        self.prepared_sample_rate = sample_rate;
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
        if frames == 0 {
            return;
        }

        let in_l = inputs.first().copied().unwrap_or(&[]);
        let in_r = inputs.get(1).copied().unwrap_or(in_l);
        if let Some(buf) = self.in_scratch.get_mut(0) {
            let n = in_l.len().min(frames);
            buf[..n].copy_from_slice(&in_l[..n]);
            buf[n..frames].fill(0.0);
        }
        if let Some(buf) = self.in_scratch.get_mut(1) {
            let n = in_r.len().min(frames);
            buf[..n].copy_from_slice(&in_r[..n]);
            buf[n..frames].fill(0.0);
        }

        if let Some(sc) = sidechain {
            let sc_l = sc.first().copied().unwrap_or(&[]);
            let sc_r = sc.get(1).copied().unwrap_or(sc_l);
            if let Some(buf) = self.in_scratch.get_mut(2) {
                let n = sc_l.len().min(frames);
                buf[..n].copy_from_slice(&sc_l[..n]);
                buf[n..frames].fill(0.0);
            }
            if let Some(buf) = self.in_scratch.get_mut(3) {
                let n = sc_r.len().min(frames);
                buf[..n].copy_from_slice(&sc_r[..n]);
                buf[n..frames].fill(0.0);
            }
        } else {
            for buf in self.in_scratch.iter_mut().skip(2) {
                buf[..frames].fill(0.0);
            }
        }
        for buf in self.in_scratch.iter_mut().skip(4) {
            buf[..frames].fill(0.0);
        }

        for buf in &mut self.out_scratch {
            buf[..frames].fill(0.0);
        }
        for (idx, buf) in self.in_scratch.iter().enumerate() {
            self.in_ptrs[idx] = buf.as_ptr();
        }
        for (idx, buf) in self.out_scratch.iter_mut().enumerate() {
            self.out_ptrs[idx] = buf.as_mut_ptr();
        }

        let process_fn = unsafe { (*self.shared.effect).process_replacing };
        let Some(process_replacing) = process_fn else {
            outputs[0][..frames].copy_from_slice(&in_l[..in_l.len().min(frames)]);
            outputs[1][..frames].copy_from_slice(&in_r[..in_r.len().min(frames)]);
            return;
        };

        let in_ptr = if self.in_ptrs.is_empty() {
            ptr::null()
        } else {
            self.in_ptrs.as_ptr()
        };
        unsafe {
            process_replacing(
                self.shared.effect,
                in_ptr,
                self.out_ptrs.as_mut_ptr(),
                frames as i32,
            );
        }

        if self.num_outputs == 1 {
            outputs[0][..frames].copy_from_slice(&self.out_scratch[0][..frames]);
            outputs[1][..frames].copy_from_slice(&self.out_scratch[0][..frames]);
        } else {
            outputs[0][..frames].copy_from_slice(&self.out_scratch[0][..frames]);
            outputs[1][..frames].copy_from_slice(&self.out_scratch[1][..frames]);
        }
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        self.shared.query_parameters()
    }

    fn set_parameter(&mut self, param_id: u32, value: f32) {
        if let Some(set_param) = unsafe { (*self.shared.effect).set_parameter } {
            unsafe {
                set_param(self.shared.effect, param_id as i32, value.clamp(0.0, 1.0));
            }
        }
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        self.shared.load_state_bytes(data)
    }

    fn latency_samples(&self) -> u32 {
        unsafe { (*self.shared.effect).initial_delay.max(0) as u32 }
    }
}

impl PluginUiHandle for Vst2UiHandle {
    fn has_editor(&self) -> bool {
        let flags = unsafe { (*self.shared.effect).flags };
        (flags & EFF_FLAGS_HAS_EDITOR) != 0
    }

    fn open_editor(&mut self, parent_hwnd: isize) -> Result<(), PluginError> {
        self.close_editor();
        if let Ok(mut slot) = self.shared.host_ctx.requested_size.lock() {
            *slot = None;
        }
        let rc = unsafe {
            self.shared
                .dispatch(EFF_EDIT_OPEN, 0, 0, parent_hwnd as *mut c_void, 0.0)
        };
        if rc == 0 {
            return Err(PluginError::GuiError {
                context: "effEditOpen",
                detail: "VST2 plugin returned 0 from effEditOpen".to_string(),
            });
        }
        self.editor_open = true;
        unsafe {
            self.shared.dispatch(EFF_EDIT_IDLE, 0, 0, ptr::null_mut(), 0.0);
        }
        Ok(())
    }

    fn close_editor(&mut self) {
        if self.editor_open {
            unsafe {
                self.shared.dispatch(EFF_EDIT_CLOSE, 0, 0, ptr::null_mut(), 0.0);
            }
            self.editor_open = false;
        }
    }

    fn preferred_size(&self) -> Option<(u32, u32)> {
        if self.editor_open {
            let mut rect_ptr: *mut ERect = ptr::null_mut();
            let rc = unsafe {
                self.shared.dispatch(
                    EFF_EDIT_GET_RECT,
                    0,
                    0,
                    (&mut rect_ptr as *mut *mut ERect) as *mut c_void,
                    0.0,
                )
            };
            if rc != 0 && !rect_ptr.is_null() {
                let rect = unsafe { *rect_ptr };
                let w = (rect.right - rect.left) as i32;
                let h = (rect.bottom - rect.top) as i32;
                if w > 0 && h > 0 {
                    return Some((w.clamp(160, 3840) as u32, h.clamp(120, 2160) as u32));
                }
            }
        }
        self.shared
            .host_ctx
            .requested_size
            .lock()
            .ok()
            .and_then(|s| *s)
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        let flags = unsafe { (*self.shared.effect).flags };
        if (flags & EFF_FLAGS_PROGRAM_CHUNKS) != 0 {
            let mut chunk_ptr: *mut c_void = ptr::null_mut();
            let size = unsafe {
                self.shared.dispatch(
                    EFF_GET_CHUNK,
                    0,
                    0,
                    (&mut chunk_ptr as *mut *mut c_void) as *mut c_void,
                    0.0,
                )
            };
            if size > 0 && !chunk_ptr.is_null() {
                let slice = unsafe { std::slice::from_raw_parts(chunk_ptr as *const u8, size as usize) };
                return Ok(slice.to_vec());
            }
        }

        let params = self.shared.query_parameters();
        let mut bytes = Vec::with_capacity(params.len() * 4);
        for p in params {
            bytes.extend_from_slice(&p.value.to_le_bytes());
        }
        Ok(bytes)
    }

    fn parameters(&self) -> Vec<PluginParamInfo> {
        self.shared.query_parameters()
    }
}

unsafe extern "C" fn vst2_host_callback(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    ptr: *mut c_void,
    _opt: f32,
) -> isize {
    let host_ctx = if !effect.is_null() {
        let raw = unsafe { (*effect).resvd1 };
        if raw != 0 {
            Some(unsafe { &*(raw as *const Vst2HostContext) })
        } else {
            None
        }
    } else {
        None
    };

    match opcode {
        AM_AUTOMATE | AM_NEED_IDLE | AM_BEGIN_EDIT | AM_END_EDIT => 1,
        AM_VERSION => 2400,
        AM_CURRENT_ID => {
            if let Some(ctx) = host_ctx {
                ctx.shell_plugin_id.load(Ordering::Acquire) as isize
            } else {
                PENDING_SHELL_PLUGIN_ID.with(|cell| cell.get()) as isize
            }
        }
        AM_GET_TIME => {
            let sr = host_ctx
                .map(|c| c.sample_rate_hz.load(Ordering::Acquire).max(1) as f64)
                .unwrap_or(48_000.0);
            THREAD_TIME_INFO.with(|cell| {
                let mut info = cell.get();
                info.sample_rate = sr;
                cell.set(info);
                cell.as_ptr() as isize
            })
        }
        AM_SIZE_WINDOW => {
            if let Some(ctx) = host_ctx {
                let w = (index.max(160) as u32).clamp(160, 3840);
                let h = ((value as i32).max(120) as u32).clamp(120, 2160);
                if let Ok(mut slot) = ctx.requested_size.lock() {
                    *slot = Some((w, h));
                }
                1
            } else {
                0
            }
        }
        AM_GET_SAMPLE_RATE => host_ctx
            .map(|c| c.sample_rate_hz.load(Ordering::Acquire).max(1) as isize)
            .unwrap_or(48_000),
        AM_GET_BLOCK_SIZE => host_ctx
            .map(|c| c.block_size.load(Ordering::Acquire).max(64) as isize)
            .unwrap_or(1024),
        AM_GET_CURRENT_PROCESS_LEVEL => 2,
        AM_GET_AUTOMATION_STATE => 1,
        AM_GET_VENDOR_STRING => write_host_cstr(ptr, "StageHost\0"),
        AM_GET_PRODUCT_STRING => write_host_cstr(ptr, "StageHost RS\0"),
        AM_GET_VENDOR_VERSION => 1000,
        AM_CAN_DO => {
            if ptr.is_null() {
                return 0;
            }
            let cstr = unsafe { CStr::from_ptr(ptr as *const c_char) };
            match cstr.to_str() {
                Ok(
                    "sendVstEvents"
                    | "sendVstTimeInfo"
                    | "sendVstMidiEvent"
                    | "sizeWindow"
                    | "supplyIdle"
                    | "shellCategory",
                ) => 1,
                _ => 0,
            }
        }
        AM_GET_LANGUAGE => 1,
        _ => {
            let _ = EFF_CAN_DO;
            0
        }
    }
}

fn write_host_cstr(dst: *mut c_void, text: &str) -> isize {
    if dst.is_null() {
        return 0;
    }
    let bytes = text.as_bytes();
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len());
    }
    1
}

fn c_buf_to_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_string()
}

fn format_vst2_version(raw: i32) -> String {
    if raw <= 0 {
        return String::new();
    }
    if raw >= 1000 {
        let major = raw / 1000;
        let minor = (raw % 1000) / 100;
        let patch = raw % 100;
        format!("{major}.{minor}.{patch}")
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_pe_files_cleanly_as_non_vst2() {
        let dir = std::env::temp_dir();
        let path = dir.join("stagehost_non_pe_test.dll");
        std::fs::write(&path, b"not a dll binary").expect("write temp");
        let res = inspect_dll_vst2_candidate(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(res, Ok(Vst2DllCheck::NotVst2Plugin));
    }

    #[test]
    fn formats_vst2_version_digits() {
        assert_eq!(format_vst2_version(2400), "2.4.0");
        assert_eq!(format_vst2_version(1102), "1.1.2");
        assert_eq!(format_vst2_version(0), "");
    }
}

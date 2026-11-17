//! Native Windows COM ASIO 2.x backend using existing `windows` bindings.

use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use rtrb::{Consumer, Producer};
use tracing::{info, warn};
use windows::core::{GUID, HRESULT, HSTRING, PCWSTR};
use windows::Win32::System::Com::{
    CLSIDFromString, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_READ, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetDesktopWindow, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
};

use crate::graph::{AudioGraphProcessor, AudioTelemetry, EngineCommand, RackMeterSnapshot};
use crate::mmcss::ensure_pro_audio_for_current_thread;
use crate::AudioError;

const ASE_OK: i32 = 0;
const ASE_SUCCESS: i32 = 0x3f48_4000;

const K_ASIO_SELECTOR_SUPPORTED: i32 = 1;
const K_ASIO_ENGINE_VERSION: i32 = 2;
const K_ASIO_RESET_REQUEST: i32 = 3;
const K_ASIO_BUFFER_SIZE_CHANGE: i32 = 4;
const K_ASIO_RESYNC_REQUEST: i32 = 5;
const K_ASIO_LATENCIES_CHANGED: i32 = 6;
const K_ASIO_SUPPORTS_TIME_INFO: i32 = 7;

#[link(name = "ole32")]
extern "system" {
    fn CoCreateInstance(
        rclsid: *const GUID,
        p_unk_outer: *mut c_void,
        dw_cls_context: u32,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsioDriverEntry {
    pub key_name: String,
    pub display_name: String,
    pub clsid_str: String,
}

#[derive(Debug, Clone)]
pub struct AsioDriverMetadata {
    pub key_name: String,
    pub driver_name: String,
    pub driver_version: i32,
    pub input_channels: usize,
    pub output_channels: usize,
    pub input_channel_names: Vec<String>,
    pub output_channel_names: Vec<String>,
    pub min_buffer_size: u32,
    pub max_buffer_size: u32,
    pub preferred_buffer_size: u32,
    pub granularity: i32,
    pub current_sample_rate: u32,
    pub supported_sample_rates: Vec<u32>,
    pub input_latency_frames: i32,
    pub output_latency_frames: i32,
    pub sample_type_name: String,
    pub clock_source_name: String,
}

#[repr(C)]
struct IAsioVtbl {
    query_interface:
        unsafe extern "system" fn(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    init: unsafe extern "system" fn(this: *mut c_void, sys_handle: *mut c_void) -> i32,
    get_driver_name: unsafe extern "system" fn(this: *mut c_void, name: *mut u8),
    get_driver_version: unsafe extern "system" fn(this: *mut c_void) -> i32,
    get_error_message: unsafe extern "system" fn(this: *mut c_void, string: *mut u8),
    start: unsafe extern "system" fn(this: *mut c_void) -> i32,
    stop: unsafe extern "system" fn(this: *mut c_void) -> i32,
    get_channels: unsafe extern "system" fn(
        this: *mut c_void,
        num_input_channels: *mut i32,
        num_output_channels: *mut i32,
    ) -> i32,
    get_latencies: unsafe extern "system" fn(
        this: *mut c_void,
        input_latency: *mut i32,
        output_latency: *mut i32,
    ) -> i32,
    get_buffer_size: unsafe extern "system" fn(
        this: *mut c_void,
        min_size: *mut i32,
        max_size: *mut i32,
        preferred_size: *mut i32,
        granularity: *mut i32,
    ) -> i32,
    can_sample_rate: unsafe extern "system" fn(this: *mut c_void, sample_rate: f64) -> i32,
    get_sample_rate: unsafe extern "system" fn(this: *mut c_void, sample_rate: *mut f64) -> i32,
    set_sample_rate: unsafe extern "system" fn(this: *mut c_void, sample_rate: f64) -> i32,
    get_clock_sources: unsafe extern "system" fn(
        this: *mut c_void,
        clocks: *mut AsioClockSource,
        num_sources: *mut i32,
    ) -> i32,
    set_clock_source: unsafe extern "system" fn(this: *mut c_void, reference: i32) -> i32,
    get_sample_position: unsafe extern "system" fn(
        this: *mut c_void,
        s_pos: *mut AsioSamples,
        t_stamp: *mut AsioTimeStamp,
    ) -> i32,
    get_channel_info:
        unsafe extern "system" fn(this: *mut c_void, info: *mut AsioChannelInfo) -> i32,
    create_buffers: unsafe extern "system" fn(
        this: *mut c_void,
        buffer_infos: *mut AsioBufferInfo,
        num_channels: i32,
        buffer_size: i32,
        callbacks: *const AsioCallbacks,
    ) -> i32,
    dispose_buffers: unsafe extern "system" fn(this: *mut c_void) -> i32,
    control_panel: unsafe extern "system" fn(this: *mut c_void) -> i32,
    future: unsafe extern "system" fn(this: *mut c_void, selector: i32, opt: *mut c_void) -> i32,
    output_ready: unsafe extern "system" fn(this: *mut c_void) -> i32,
}

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct AsioSamples {
    hi: u32,
    lo: u32,
}

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct AsioTimeStamp {
    hi: u32,
    lo: u32,
}

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct AsioClockSource {
    index: i32,
    associated_channel: i32,
    associated_group: i32,
    is_current_source: i32,
    name: [u8; 32],
}

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct AsioChannelInfo {
    channel: i32,
    is_input: i32,
    is_active: i32,
    channel_group: i32,
    sample_type: i32,
    name: [u8; 32],
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct AsioBufferInfo {
    is_input: i32,
    channel_num: i32,
    buffers: [*mut c_void; 2],
}

impl Default for AsioBufferInfo {
    fn default() -> Self {
        Self {
            is_input: 0,
            channel_num: 0,
            buffers: [std::ptr::null_mut(); 2],
        }
    }
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct AsioTimeInfo {
    speed: f64,
    system_time: AsioTimeStamp,
    sample_position: AsioSamples,
    sample_rate: f64,
    flags: u32,
    reserved: [u8; 12],
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct AsioTimeCode {
    speed: f64,
    time_code_samples: AsioSamples,
    flags: u32,
    future: [u8; 64],
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct AsioTime {
    reserved: [i32; 4],
    time_info: AsioTimeInfo,
    time_code: AsioTimeCode,
}

#[repr(C)]
struct AsioCallbacks {
    buffer_switch: unsafe extern "C" fn(double_buffer_index: i32, direct_process: i32),
    sample_rate_did_change: unsafe extern "C" fn(s_rate: f64),
    asio_message:
        unsafe extern "C" fn(selector: i32, value: i32, message: *mut c_void, opt: *mut f64) -> i32,
    buffer_switch_time_info: unsafe extern "C" fn(
        params: *mut AsioTime,
        double_buffer_index: i32,
        direct_process: i32,
    ) -> *mut AsioTime,
}

fn asio_succeeded(code: i32) -> bool {
    code == ASE_OK || code == ASE_SUCCESS
}

/// Supported Steinberg ASIO sample formats on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsioSampleType {
    Int16Msb,
    Int24Msb,
    Int32Msb,
    Float32Msb,
    Float64Msb,
    Int32Msb16,
    Int32Msb18,
    Int32Msb20,
    Int32Msb24,
    Int16Lsb,
    Int24Lsb,
    Int32Lsb,
    Float32Lsb,
    Float64Lsb,
    Int32Lsb16,
    Int32Lsb18,
    Int32Lsb20,
    Int32Lsb24,
    Unsupported(i32),
}

impl AsioSampleType {
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::Int16Msb,
            1 => Self::Int24Msb,
            2 => Self::Int32Msb,
            3 => Self::Float32Msb,
            4 => Self::Float64Msb,
            8 => Self::Int32Msb16,
            9 => Self::Int32Msb18,
            10 => Self::Int32Msb20,
            11 => Self::Int32Msb24,
            16 => Self::Int16Lsb,
            17 => Self::Int24Lsb,
            18 => Self::Int32Lsb,
            19 => Self::Float32Lsb,
            20 => Self::Float64Lsb,
            24 => Self::Int32Lsb16,
            25 => Self::Int32Lsb18,
            26 => Self::Int32Lsb20,
            27 => Self::Int32Lsb24,
            other => Self::Unsupported(other),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Int16Msb => "Int16 MSB",
            Self::Int24Msb => "Int24 MSB",
            Self::Int32Msb => "Int32 MSB",
            Self::Float32Msb => "Float32 MSB",
            Self::Float64Msb => "Float64 MSB",
            Self::Int32Msb16 => "Int32 MSB (16-bit)",
            Self::Int32Msb18 => "Int32 MSB (18-bit)",
            Self::Int32Msb20 => "Int32 MSB (20-bit)",
            Self::Int32Msb24 => "Int32 MSB (24-bit)",
            Self::Int16Lsb => "Int16 LSB",
            Self::Int24Lsb => "Int24 LSB",
            Self::Int32Lsb => "Int32 LSB",
            Self::Float32Lsb => "Float32 LSB",
            Self::Float64Lsb => "Float64 LSB",
            Self::Int32Lsb16 => "Int32 LSB (16-bit)",
            Self::Int32Lsb18 => "Int32 LSB (18-bit)",
            Self::Int32Lsb20 => "Int32 LSB (20-bit)",
            Self::Int32Lsb24 => "Int32 LSB (24-bit)",
            Self::Unsupported(_) => "Unsupported",
        }
    }

    pub fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported(_))
    }
}

/// Reads an ASIO hardware buffer into `dst` normalized to `[-1.0, 1.0]`.
///
/// # Safety
/// `src` must point to at least `frames` samples of the given `sample_type`.
pub unsafe fn read_asio_channel(
    src: *const c_void,
    sample_type: AsioSampleType,
    dst: &mut [f32],
    frames: usize,
) {
    let count = frames.min(dst.len());
    if src.is_null() {
        dst[..count].fill(0.0);
        return;
    }

    match sample_type {
        AsioSampleType::Float32Lsb => {
            // SAFETY: caller guarantees `count` floats in buffer.
            let slice = unsafe { std::slice::from_raw_parts(src as *const f32, count) };
            dst[..count].copy_from_slice(slice);
        }
        AsioSampleType::Float32Msb => {
            let bytes = unsafe { std::slice::from_raw_parts(src as *const u8, count * 4) };
            for (i, &b) in bytes.as_chunks::<4>().0.iter().enumerate() {
                dst[i] = f32::from_be_bytes(b);
            }
        }
        AsioSampleType::Float64Lsb => {
            let slice = unsafe { std::slice::from_raw_parts(src as *const f64, count) };
            for (i, &s) in slice.iter().enumerate() {
                dst[i] = s as f32;
            }
        }
        AsioSampleType::Float64Msb => {
            let bytes = unsafe { std::slice::from_raw_parts(src as *const u8, count * 8) };
            for (i, &b) in bytes.as_chunks::<8>().0.iter().enumerate() {
                dst[i] = f64::from_be_bytes(b) as f32;
            }
        }
        AsioSampleType::Int32Lsb | AsioSampleType::Int32Lsb24 => unsafe {
            read_scaled_i32_le(src, &mut dst[..count], 1.0 / 2_147_483_648.0);
        },
        AsioSampleType::Int32Msb | AsioSampleType::Int32Msb24 => unsafe {
            read_scaled_i32_be(src, &mut dst[..count], 1.0 / 2_147_483_648.0);
        },
        AsioSampleType::Int32Lsb16 => unsafe {
            read_scaled_i32_le(src, &mut dst[..count], 1.0 / 32_768.0);
        },
        AsioSampleType::Int32Lsb18 => unsafe {
            read_scaled_i32_le(src, &mut dst[..count], 1.0 / 131_072.0);
        },
        AsioSampleType::Int32Lsb20 => unsafe {
            read_scaled_i32_le(src, &mut dst[..count], 1.0 / 524_288.0);
        },
        AsioSampleType::Int32Msb16 => unsafe {
            read_scaled_i32_be(src, &mut dst[..count], 1.0 / 32_768.0);
        },
        AsioSampleType::Int32Msb18 => unsafe {
            read_scaled_i32_be(src, &mut dst[..count], 1.0 / 131_072.0);
        },
        AsioSampleType::Int32Msb20 => unsafe {
            read_scaled_i32_be(src, &mut dst[..count], 1.0 / 524_288.0);
        },
        AsioSampleType::Int24Lsb => {
            const SCALE: f32 = 1.0 / 8_388_608.0;
            let bytes = unsafe { std::slice::from_raw_parts(src as *const u8, count * 3) };
            for (i, &b) in bytes.as_chunks::<3>().0.iter().enumerate() {
                let val = ((b[0] as i32) << 8 | (b[1] as i32) << 16 | (b[2] as i32) << 24) >> 8;
                dst[i] = val as f32 * SCALE;
            }
        }
        AsioSampleType::Int24Msb => {
            const SCALE: f32 = 1.0 / 8_388_608.0;
            let bytes = unsafe { std::slice::from_raw_parts(src as *const u8, count * 3) };
            for (i, &b) in bytes.as_chunks::<3>().0.iter().enumerate() {
                let val = ((b[2] as i32) << 8 | (b[1] as i32) << 16 | (b[0] as i32) << 24) >> 8;
                dst[i] = val as f32 * SCALE;
            }
        }
        AsioSampleType::Int16Lsb => {
            const SCALE: f32 = 1.0 / 32_768.0;
            let bytes = unsafe { std::slice::from_raw_parts(src as *const u8, count * 2) };
            for (i, &b) in bytes.as_chunks::<2>().0.iter().enumerate() {
                dst[i] = i16::from_le_bytes(b) as f32 * SCALE;
            }
        }
        AsioSampleType::Int16Msb => {
            const SCALE: f32 = 1.0 / 32_768.0;
            let bytes = unsafe { std::slice::from_raw_parts(src as *const u8, count * 2) };
            for (i, &b) in bytes.as_chunks::<2>().0.iter().enumerate() {
                dst[i] = i16::from_be_bytes(b) as f32 * SCALE;
            }
        }
        AsioSampleType::Unsupported(_) => dst[..count].fill(0.0),
    }
}

unsafe fn read_scaled_i32_le(src: *const c_void, dst: &mut [f32], scale: f32) {
    let bytes = unsafe { std::slice::from_raw_parts(src as *const u8, dst.len() * 4) };
    for (i, &b) in bytes.as_chunks::<4>().0.iter().enumerate() {
        dst[i] = i32::from_le_bytes(b) as f32 * scale;
    }
}

unsafe fn read_scaled_i32_be(src: *const c_void, dst: &mut [f32], scale: f32) {
    let bytes = unsafe { std::slice::from_raw_parts(src as *const u8, dst.len() * 4) };
    for (i, &b) in bytes.as_chunks::<4>().0.iter().enumerate() {
        dst[i] = i32::from_be_bytes(b) as f32 * scale;
    }
}

/// Writes `src` (`[-1.0, 1.0]`) into an ASIO hardware buffer of `sample_type`.
///
/// # Safety
/// `dst` must point to writable memory for at least `frames` samples of `sample_type`.
pub unsafe fn write_asio_channel(
    dst: *mut c_void,
    sample_type: AsioSampleType,
    src: &[f32],
    frames: usize,
) {
    let count = frames.min(src.len());
    if dst.is_null() {
        return;
    }

    match sample_type {
        AsioSampleType::Float32Lsb => {
            let slice = unsafe { std::slice::from_raw_parts_mut(dst as *mut f32, count) };
            slice.copy_from_slice(&src[..count]);
        }
        AsioSampleType::Float32Msb => {
            let bytes = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, count * 4) };
            for (i, &s) in src[..count].iter().enumerate() {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
            }
        }
        AsioSampleType::Float64Lsb => {
            let slice = unsafe { std::slice::from_raw_parts_mut(dst as *mut f64, count) };
            for (i, &s) in src[..count].iter().enumerate() {
                slice[i] = s as f64;
            }
        }
        AsioSampleType::Float64Msb => {
            let bytes = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, count * 8) };
            for (i, &s) in src[..count].iter().enumerate() {
                bytes[i * 8..i * 8 + 8].copy_from_slice(&(s as f64).to_be_bytes());
            }
        }
        AsioSampleType::Int32Lsb | AsioSampleType::Int32Lsb24 => {
            let bytes = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, count * 4) };
            for (i, &s) in src[..count].iter().enumerate() {
                let q = (s.clamp(-1.0, 1.0) * 2_147_483_647.0).round() as i32;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&q.to_le_bytes());
            }
        }
        AsioSampleType::Int32Msb | AsioSampleType::Int32Msb24 => {
            let bytes = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, count * 4) };
            for (i, &s) in src[..count].iter().enumerate() {
                let q = (s.clamp(-1.0, 1.0) * 2_147_483_647.0).round() as i32;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&q.to_be_bytes());
            }
        }
        AsioSampleType::Int32Lsb16 => {
            write_scaled_i32_le(dst, &src[..count], 32_767.0);
        }
        AsioSampleType::Int32Lsb18 => {
            write_scaled_i32_le(dst, &src[..count], 131_071.0);
        }
        AsioSampleType::Int32Lsb20 => {
            write_scaled_i32_le(dst, &src[..count], 524_287.0);
        }
        AsioSampleType::Int32Msb16 => {
            write_scaled_i32_be(dst, &src[..count], 32_767.0);
        }
        AsioSampleType::Int32Msb18 => {
            write_scaled_i32_be(dst, &src[..count], 131_071.0);
        }
        AsioSampleType::Int32Msb20 => {
            write_scaled_i32_be(dst, &src[..count], 524_287.0);
        }
        AsioSampleType::Int24Lsb => {
            let bytes = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, count * 3) };
            for (i, &s) in src[..count].iter().enumerate() {
                let q = (s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                let le = q.to_le_bytes();
                bytes[i * 3] = le[0];
                bytes[i * 3 + 1] = le[1];
                bytes[i * 3 + 2] = le[2];
            }
        }
        AsioSampleType::Int24Msb => {
            let bytes = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, count * 3) };
            for (i, &s) in src[..count].iter().enumerate() {
                let q = (s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                let le = q.to_le_bytes();
                bytes[i * 3] = le[2];
                bytes[i * 3 + 1] = le[1];
                bytes[i * 3 + 2] = le[0];
            }
        }
        AsioSampleType::Int16Lsb => {
            let bytes = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, count * 2) };
            for (i, &s) in src[..count].iter().enumerate() {
                let q = (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
                bytes[i * 2..i * 2 + 2].copy_from_slice(&q.to_le_bytes());
            }
        }
        AsioSampleType::Int16Msb => {
            let bytes = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, count * 2) };
            for (i, &s) in src[..count].iter().enumerate() {
                let q = (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
                bytes[i * 2..i * 2 + 2].copy_from_slice(&q.to_be_bytes());
            }
        }
        AsioSampleType::Unsupported(_) => {}
    }
}

fn write_scaled_i32_le(dst: *mut c_void, src: &[f32], max_val: f32) {
    let bytes = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, src.len() * 4) };
    for (i, &s) in src.iter().enumerate() {
        let q = (s.clamp(-1.0, 1.0) * max_val).round() as i32;
        bytes[i * 4..i * 4 + 4].copy_from_slice(&q.to_le_bytes());
    }
}

fn write_scaled_i32_be(dst: *mut c_void, src: &[f32], max_val: f32) {
    let bytes = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, src.len() * 4) };
    for (i, &s) in src.iter().enumerate() {
        let q = (s.clamp(-1.0, 1.0) * max_val).round() as i32;
        bytes[i * 4..i * 4 + 4].copy_from_slice(&q.to_be_bytes());
    }
}

/// Enumerates installed 64-bit ASIO drivers from `HKLM\SOFTWARE\ASIO`.
pub fn enumerate_asio_drivers() -> Vec<AsioDriverEntry> {
    let mut drivers = Vec::new();
    let subkey = HSTRING::from("SOFTWARE\\ASIO");
    let mut asio_root = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            KEY_READ,
            &mut asio_root,
        )
    };
    if status.0 != 0 {
        return drivers;
    }

    let mut idx = 0u32;
    loop {
        let mut name_buf = [0u16; 256];
        let mut name_len = name_buf.len() as u32;
        let enum_res = unsafe {
            RegEnumKeyExW(
                asio_root,
                idx,
                Some(windows::core::PWSTR(name_buf.as_mut_ptr())),
                &mut name_len,
                None,
                None,
                None,
                None,
            )
        };
        if enum_res.0 != 0 {
            break;
        }
        idx += 1;

        let key_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        let key_w = HSTRING::from(key_name.as_str());
        let mut drv_key = HKEY::default();
        if unsafe {
            RegOpenKeyExW(
                asio_root,
                PCWSTR(key_w.as_ptr()),
                Some(0),
                KEY_READ,
                &mut drv_key,
            )
        }
        .0 != 0
        {
            continue;
        }

        let clsid_str = read_reg_sz(drv_key, "CLSID");
        let desc = read_reg_sz(drv_key, "Description");
        unsafe {
            let _ = RegCloseKey(drv_key);
        }

        if let Some(clsid_str) = clsid_str {
            let display_name = desc
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| key_name.clone());
            drivers.push(AsioDriverEntry {
                key_name,
                display_name,
                clsid_str,
            });
        }
    }

    unsafe {
        let _ = RegCloseKey(asio_root);
    }
    drivers.sort_by_key(|a| a.display_name.to_lowercase());
    drivers
}

fn read_reg_sz(hkey: HKEY, value_name: &str) -> Option<String> {
    let val_w = HSTRING::from(value_name);
    let mut reg_type = REG_VALUE_TYPE::default();
    let mut byte_len = 0u32;
    let q1 = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(val_w.as_ptr()),
            None,
            Some(&mut reg_type),
            None,
            Some(&mut byte_len),
        )
    };
    if q1.0 != 0 || reg_type != REG_SZ || byte_len < 2 {
        return None;
    }

    let mut buf = vec![0u16; (byte_len as usize).div_ceil(2)];
    let q2 = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(val_w.as_ptr()),
            None,
            Some(&mut reg_type),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut byte_len),
        )
    };
    if q2.0 != 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

struct ComApartmentGuard {
    should_uninit: bool,
}

impl ComApartmentGuard {
    fn init_sta() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        Self {
            should_uninit: hr.is_ok(),
        }
    }
}

impl Drop for ComApartmentGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

struct AsioComDriver {
    ptr: *mut c_void,
    name: String,
}

impl AsioComDriver {
    fn create_and_init(entry: &AsioDriverEntry) -> Result<Self, AudioError> {
        let clsid_w = HSTRING::from(entry.clsid_str.as_str());
        let clsid = unsafe { CLSIDFromString(PCWSTR(clsid_w.as_ptr())) }.map_err(|e| {
            AudioError::AsioDriver {
                driver_name: entry.display_name.clone(),
                detail: format!("invalid CLSID '{}': {e}", entry.clsid_str),
            }
        })?;

        let mut ptr: *mut c_void = std::ptr::null_mut();
        let hr = unsafe {
            CoCreateInstance(
                &clsid,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER.0,
                &clsid,
                &mut ptr,
            )
        };
        if hr.is_err() || ptr.is_null() {
            return Err(AudioError::AsioDriver {
                driver_name: entry.display_name.clone(),
                detail: format!("CoCreateInstance failed (HRESULT 0x{:08X})", hr.0 as u32),
            });
        }

        let drv = Self {
            ptr,
            name: entry.display_name.clone(),
        };
        let hwnd = unsafe { GetDesktopWindow() };
        let ok = unsafe { (drv.vtbl().init)(drv.ptr, hwnd.0) };
        if ok == 0 {
            let msg = drv.error_message();
            return Err(AudioError::AsioDriver {
                driver_name: entry.display_name.clone(),
                detail: if msg.is_empty() {
                    "IASIO::init returned FALSE (device unavailable or disconnected)".to_string()
                } else {
                    msg
                },
            });
        }
        Ok(drv)
    }

    #[inline]
    unsafe fn vtbl(&self) -> &IAsioVtbl {
        let vtbl_ptr = unsafe { *(self.ptr as *const *const IAsioVtbl) };
        unsafe { &*vtbl_ptr }
    }

    fn error_message(&self) -> String {
        let mut buf = [0u8; 256];
        unsafe { (self.vtbl().get_error_message)(self.ptr, buf.as_mut_ptr()) };
        c_buf_to_string(&buf)
    }

    fn driver_name(&self) -> String {
        let mut buf = [0u8; 64];
        unsafe { (self.vtbl().get_driver_name)(self.ptr, buf.as_mut_ptr()) };
        c_buf_to_string(&buf)
    }

    fn driver_version(&self) -> i32 {
        unsafe { (self.vtbl().get_driver_version)(self.ptr) }
    }

    fn channels(&self) -> Result<(usize, usize), AudioError> {
        let mut ins = 0i32;
        let mut outs = 0i32;
        let code = unsafe { (self.vtbl().get_channels)(self.ptr, &mut ins, &mut outs) };
        if !asio_succeeded(code) {
            return Err(self.err("getChannels", code));
        }
        Ok((ins.max(0) as usize, outs.max(0) as usize))
    }

    fn buffer_size(&self) -> Result<(u32, u32, u32, i32), AudioError> {
        let mut min_sz = 0i32;
        let mut max_sz = 0i32;
        let mut pref_sz = 0i32;
        let mut gran = 0i32;
        let code = unsafe {
            (self.vtbl().get_buffer_size)(
                self.ptr,
                &mut min_sz,
                &mut max_sz,
                &mut pref_sz,
                &mut gran,
            )
        };
        if !asio_succeeded(code) {
            return Err(self.err("getBufferSize", code));
        }
        Ok((
            min_sz.max(16) as u32,
            max_sz.max(min_sz).max(16) as u32,
            pref_sz.max(16) as u32,
            gran,
        ))
    }

    fn latencies(&self) -> (i32, i32) {
        let mut in_lat = 0i32;
        let mut out_lat = 0i32;
        let code = unsafe { (self.vtbl().get_latencies)(self.ptr, &mut in_lat, &mut out_lat) };
        if asio_succeeded(code) {
            (in_lat.max(0), out_lat.max(0))
        } else {
            (0, 0)
        }
    }

    fn sample_rate(&self) -> Result<u32, AudioError> {
        let mut sr = 0.0f64;
        let code = unsafe { (self.vtbl().get_sample_rate)(self.ptr, &mut sr) };
        if !asio_succeeded(code) || sr <= 0.0 {
            return Err(self.err("getSampleRate", code));
        }
        Ok(sr.round() as u32)
    }

    fn can_sample_rate(&self, sr: u32) -> bool {
        let code = unsafe { (self.vtbl().can_sample_rate)(self.ptr, sr as f64) };
        asio_succeeded(code)
    }

    fn set_sample_rate(&self, sr: u32) -> Result<(), AudioError> {
        let code = unsafe { (self.vtbl().set_sample_rate)(self.ptr, sr as f64) };
        if !asio_succeeded(code) {
            return Err(self.err("setSampleRate", code));
        }
        Ok(())
    }

    fn channel_info(&self, channel: usize, is_input: bool) -> Result<AsioChannelInfo, AudioError> {
        let mut info = AsioChannelInfo {
            channel: channel as i32,
            is_input: if is_input { 1 } else { 0 },
            ..Default::default()
        };
        let code = unsafe { (self.vtbl().get_channel_info)(self.ptr, &mut info) };
        if !asio_succeeded(code) {
            return Err(self.err("getChannelInfo", code));
        }
        Ok(info)
    }

    fn current_clock_source(&self) -> String {
        let mut clocks = [AsioClockSource::default(); 8];
        let mut count = clocks.len() as i32;
        let code = unsafe {
            (self.vtbl().get_clock_sources)(self.ptr, clocks.as_mut_ptr(), &mut count)
        };
        if !asio_succeeded(code) || count <= 0 {
            return "Internal".to_string();
        }
        let n = (count as usize).min(clocks.len());
        for clock in &clocks[..n] {
            if clock.is_current_source != 0 {
                return c_buf_to_string(&clock.name);
            }
        }
        c_buf_to_string(&clocks[0].name)
    }

    fn create_buffers(
        &self,
        infos: &mut [AsioBufferInfo],
        buffer_size: u32,
        callbacks: &AsioCallbacks,
    ) -> Result<(), AudioError> {
        let code = unsafe {
            (self.vtbl().create_buffers)(
                self.ptr,
                infos.as_mut_ptr(),
                infos.len() as i32,
                buffer_size as i32,
                callbacks,
            )
        };
        if !asio_succeeded(code) {
            return Err(self.err("createBuffers", code));
        }
        Ok(())
    }

    fn dispose_buffers(&self) {
        unsafe {
            let _ = (self.vtbl().dispose_buffers)(self.ptr);
        }
    }

    fn start(&self) -> Result<(), AudioError> {
        let code = unsafe { (self.vtbl().start)(self.ptr) };
        if !asio_succeeded(code) {
            return Err(self.err("start", code));
        }
        Ok(())
    }

    fn stop(&self) {
        unsafe {
            let _ = (self.vtbl().stop)(self.ptr);
        }
    }

    fn control_panel(&self) -> Result<(), AudioError> {
        let code = unsafe { (self.vtbl().control_panel)(self.ptr) };
        if !asio_succeeded(code) {
            return Err(self.err("controlPanel", code));
        }
        Ok(())
    }

    fn err(&self, op: &str, code: i32) -> AudioError {
        let msg = self.error_message();
        AudioError::AsioDriver {
            driver_name: self.name.clone(),
            detail: if msg.is_empty() {
                format!("{op} failed (ASIO error {code})")
            } else {
                format!("{op} failed (ASIO error {code}): {msg}")
            },
        }
    }
}

impl Drop for AsioComDriver {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.vtbl().release)(self.ptr);
        }
    }
}

fn c_buf_to_string(buf: &[u8]) -> String {
    if let Some(nul) = buf.iter().position(|&b| b == 0) {
        String::from_utf8_lossy(&buf[..nul]).trim().to_string()
    } else if let Ok(cstr) = CStr::from_bytes_until_nul(buf) {
        cstr.to_string_lossy().trim().to_string()
    } else {
        String::from_utf8_lossy(buf).trim().to_string()
    }
}

fn find_driver_entry(driver_name: &str) -> Result<AsioDriverEntry, AudioError> {
    enumerate_asio_drivers()
        .into_iter()
        .find(|d| d.display_name == driver_name || d.key_name == driver_name)
        .ok_or_else(|| AudioError::DeviceNotFound {
            device_name: format!("ASIO - {driver_name}"),
        })
}

/// Queries live driver metadata (channels, supported sample rates, buffer size range, latencies).
pub fn probe_asio_driver(driver_name: &str) -> Result<AsioDriverMetadata, AudioError> {
    let entry = find_driver_entry(driver_name)?;
    let entry_clone = entry.clone();
    let handle = std::thread::Builder::new()
        .name("stagehost-asio-probe".to_string())
        .spawn(move || -> Result<AsioDriverMetadata, AudioError> {
            let _com = ComApartmentGuard::init_sta();
            let drv = AsioComDriver::create_and_init(&entry_clone)?;
            query_driver_metadata(&drv, &entry_clone.key_name)
        })
        .map_err(|e| AudioError::StreamFailure {
            context: "spawn_asio_probe",
            detail: e.to_string(),
        })?;

    handle.join().map_err(|_| AudioError::AsioDriver {
        driver_name: entry.display_name,
        detail: "ASIO probe thread panicked".to_string(),
    })?
}

fn query_driver_metadata(
    drv: &AsioComDriver,
    key_name: &str,
) -> Result<AsioDriverMetadata, AudioError> {
    let (in_ch, out_ch) = drv.channels()?;
    let (min_buf, max_buf, pref_buf, gran) = drv.buffer_size()?;
    let (in_lat, out_lat) = drv.latencies();
    let current_sr = drv.sample_rate().unwrap_or(48_000);
    let mut rates = Vec::new();
    for &sr in &[44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000] {
        if drv.can_sample_rate(sr) {
            rates.push(sr);
        }
    }
    if !rates.contains(&current_sr) {
        rates.push(current_sr);
        rates.sort_unstable();
    }
    let input_channel_names: Vec<String> = (0..in_ch)
        .map(|i| {
            drv.channel_info(i, true)
                .map(|info| c_buf_to_string(&info.name))
                .unwrap_or_default()
        })
        .collect();
    let output_channel_names: Vec<String> = (0..out_ch)
        .map(|o| {
            drv.channel_info(o, false)
                .map(|info| c_buf_to_string(&info.name))
                .unwrap_or_default()
        })
        .collect();
    let sample_type_name = if out_ch > 0 {
        drv.channel_info(0, false)
            .map(|i| AsioSampleType::from_raw(i.sample_type).label().to_string())
            .unwrap_or_else(|_| "Unknown".to_string())
    } else {
        "Unknown".to_string()
    };
    let drv_name = drv.driver_name();
    Ok(AsioDriverMetadata {
        key_name: key_name.to_string(),
        driver_name: if drv_name.is_empty() {
            drv.name.clone()
        } else {
            drv_name
        },
        driver_version: drv.driver_version(),
        input_channels: in_ch,
        output_channels: out_ch,
        input_channel_names,
        output_channel_names,
        min_buffer_size: min_buf,
        max_buffer_size: max_buf,
        preferred_buffer_size: pref_buf,
        granularity: gran,
        current_sample_rate: current_sr,
        supported_sample_rates: rates,
        input_latency_frames: in_lat,
        output_latency_frames: out_lat,
        sample_type_name,
        clock_source_name: drv.current_clock_source(),
    })
}

/// Opens an ASIO driver's Control Panel on a detached STA thread (used when engine is stopped).
pub fn open_asio_control_panel_detached(driver_name: &str) -> Result<(), AudioError> {
    let entry = find_driver_entry(driver_name)?;
    let (tx, rx) = mpsc::channel::<Result<(), AudioError>>();
    std::thread::Builder::new()
        .name("stagehost-asio-cp".to_string())
        .spawn(move || {
            let _com = ComApartmentGuard::init_sta();
            match AsioComDriver::create_and_init(&entry) {
                Ok(drv) => {
                    let _ = tx.send(Ok(()));
                    if let Err(err) = drv.control_panel() {
                        warn!("ASIO controlPanel returned error: {err}");
                    }
                    pump_win32_messages();
                }
                Err(err) => {
                    let _ = tx.send(Err(err));
                }
            }
        })
        .map_err(|e| AudioError::StreamFailure {
            context: "spawn_asio_control_panel",
            detail: e.to_string(),
        })?;

    rx.recv().unwrap_or(Ok(()))
}

fn pump_win32_messages() {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

struct ActiveChannelBuffers {
    is_input: bool,
    sample_type: AsioSampleType,
    buf_ptrs: [*mut c_void; 2],
}

struct AsioRuntimeContext {
    graph: AudioGraphProcessor,
    cmd_rx: Consumer<EngineCommand>,
    telemetry_tx: Producer<AudioTelemetry>,
    rack_meters_tx: Option<Producer<RackMeterSnapshot>>,
    decimator: crate::engine::TelemetryDecimator,
    buffer_frames: usize,
    in_channels: Vec<ActiveChannelBuffers>,
    out_channels: Vec<ActiveChannelBuffers>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    output_ready_fn: Option<(unsafe extern "system" fn(*mut c_void) -> i32, *mut c_void)>,
    reset_requested: AtomicBool,
    mmcss_applied: AtomicBool,
}

static ACTIVE_ASIO_CTX: AtomicPtr<AsioRuntimeContext> = AtomicPtr::new(std::ptr::null_mut());
static CALLBACK_DEPTH: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn asio_buffer_switch(double_buffer_index: i32, _direct_process: i32) {
    CALLBACK_DEPTH.fetch_add(1, Ordering::Acquire);
    let ctx_ptr = ACTIVE_ASIO_CTX.load(Ordering::Acquire);
    if !ctx_ptr.is_null() {
        let ctx = unsafe { &mut *ctx_ptr };
        if !ctx.mmcss_applied.swap(true, Ordering::Relaxed) {
            ensure_pro_audio_for_current_thread();
        }
        process_asio_switch(ctx, double_buffer_index);
    }
    CALLBACK_DEPTH.fetch_sub(1, Ordering::Release);
}

unsafe extern "C" fn asio_sample_rate_did_change(_s_rate: f64) {
    let ctx_ptr = ACTIVE_ASIO_CTX.load(Ordering::Acquire);
    if !ctx_ptr.is_null() {
        let ctx = unsafe { &*ctx_ptr };
        ctx.reset_requested.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn asio_message(
    selector: i32,
    value: i32,
    _message: *mut c_void,
    _opt: *mut f64,
) -> i32 {
    match selector {
        K_ASIO_SELECTOR_SUPPORTED => match value {
            K_ASIO_SELECTOR_SUPPORTED
            | K_ASIO_ENGINE_VERSION
            | K_ASIO_RESET_REQUEST
            | K_ASIO_BUFFER_SIZE_CHANGE
            | K_ASIO_RESYNC_REQUEST
            | K_ASIO_LATENCIES_CHANGED
            | K_ASIO_SUPPORTS_TIME_INFO => 1,
            _ => 0,
        },
        K_ASIO_ENGINE_VERSION => 2,
        K_ASIO_RESET_REQUEST | K_ASIO_BUFFER_SIZE_CHANGE => {
            let ctx_ptr = ACTIVE_ASIO_CTX.load(Ordering::Acquire);
            if !ctx_ptr.is_null() {
                let ctx = unsafe { &*ctx_ptr };
                ctx.reset_requested.store(true, Ordering::Release);
            }
            1
        }
        K_ASIO_RESYNC_REQUEST | K_ASIO_LATENCIES_CHANGED | K_ASIO_SUPPORTS_TIME_INFO => 1,
        _ => 0,
    }
}

unsafe extern "C" fn asio_buffer_switch_time_info(
    params: *mut AsioTime,
    double_buffer_index: i32,
    direct_process: i32,
) -> *mut AsioTime {
    unsafe { asio_buffer_switch(double_buffer_index, direct_process) };
    params
}

fn process_asio_switch(ctx: &mut AsioRuntimeContext, double_buffer_index: i32) {
    let buf_idx = usize::from(double_buffer_index != 0);
    let frames = ctx.buffer_frames.min(ctx.in_l.len()).min(ctx.out_l.len());

    if let Some(ch_l) = ctx.in_channels.first() {
        unsafe {
            read_asio_channel(ch_l.buf_ptrs[buf_idx], ch_l.sample_type, &mut ctx.in_l, frames);
        }
    } else {
        ctx.in_l[..frames].fill(0.0);
    }

    if let Some(ch_r) = ctx.in_channels.get(1) {
        unsafe {
            read_asio_channel(ch_r.buf_ptrs[buf_idx], ch_r.sample_type, &mut ctx.in_r, frames);
        }
    } else {
        ctx.in_r[..frames].copy_from_slice(&ctx.in_l[..frames]);
    }

    crate::engine::process_graph_block(
        frames,
        &ctx.in_l[..frames],
        &ctx.in_r[..frames],
        &mut ctx.out_l[..frames],
        &mut ctx.out_r[..frames],
        &mut ctx.graph,
        &mut ctx.cmd_rx,
        &mut ctx.telemetry_tx,
        &mut ctx.rack_meters_tx,
        &mut ctx.decimator,
    );

    if let Some(ch_l) = ctx.out_channels.first() {
        unsafe {
            write_asio_channel(ch_l.buf_ptrs[buf_idx], ch_l.sample_type, &ctx.out_l, frames);
        }
    }
    if let Some(ch_r) = ctx.out_channels.get(1) {
        unsafe {
            write_asio_channel(ch_r.buf_ptrs[buf_idx], ch_r.sample_type, &ctx.out_r, frames);
        }
    }

    if let Some((out_ready, this_ptr)) = ctx.output_ready_fn {
        unsafe {
            let _ = out_ready(this_ptr);
        }
    }
}

fn choose_asio_buffer_size(
    requested: Option<u32>,
    min_sz: u32,
    max_sz: u32,
    preferred: u32,
    granularity: i32,
) -> u32 {
    let target = requested.unwrap_or(preferred).clamp(min_sz, max_sz);
    if min_sz == max_sz || granularity == 0 {
        return preferred.clamp(min_sz, max_sz);
    }
    if granularity > 0 {
        let g = granularity as u32;
        let steps = (target - min_sz) / g;
        (min_sz + steps * g).clamp(min_sz, max_sz)
    } else {
        // Power-of-two granularity (-1 per ASIO SDK specification).
        let mut cur = min_sz.next_power_of_two();
        let mut best = cur.clamp(min_sz, max_sz);
        while cur <= max_sz {
            if cur.abs_diff(target) < best.abs_diff(target) {
                best = cur;
            }
            match cur.checked_mul(2) {
                Some(next) => cur = next,
                None => break,
            }
        }
        best
    }
}

enum AsioCtrlCmd {
    OpenControlPanel,
    Stop,
}

pub struct AsioStartConfig {
    pub driver_name: String,
    pub requested_sample_rate: Option<u32>,
    pub requested_buffer_frames: Option<u32>,
    pub input_channel_offset: usize,
    pub output_channel_offset: usize,
    pub cmd_rx: Consumer<EngineCommand>,
    pub telemetry_tx: Producer<AudioTelemetry>,
    pub rack_meters_tx: Producer<RackMeterSnapshot>,
    pub retired_tx: Producer<Vec<crate::graph::LiveRack>>,
    pub retired_slot_tx: Producer<Box<crate::graph::LiveSlot>>,
    pub retired_params_tx: Producer<Box<crate::graph::LiveSessionParams>>,
    pub retired_cfg_tx: Producer<Box<session_core::SessionConfig>>,
    pub tuner_tx: Producer<f32>,
}

pub struct AsioStartOutcome {
    pub session: AsioStreamSession,
    pub active_sample_rate: u32,
    pub active_buffer_frames: u32,
    pub metadata: AsioDriverMetadata,
}

/// Owns a live ASIO driver instance on a dedicated STA COM control thread.
pub struct AsioStreamSession {
    ctrl_tx: Sender<AsioCtrlCmd>,
    join_handle: Option<JoinHandle<()>>,
    reset_flag: std::sync::Arc<AtomicBool>,
}

impl AsioStreamSession {
    pub fn start(cfg: AsioStartConfig) -> Result<AsioStartOutcome, AudioError> {
        let entry = find_driver_entry(&cfg.driver_name)?;
        let driver_label = cfg.driver_name.clone();
        let (ready_tx, ready_rx) =
            mpsc::channel::<Result<(u32, u32, AsioDriverMetadata), AudioError>>();
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<AsioCtrlCmd>();
        let reset_flag = std::sync::Arc::new(AtomicBool::new(false));
        let reset_flag_thread = reset_flag.clone();

        let join_handle = std::thread::Builder::new()
            .name("stagehost-asio-ctrl".to_string())
            .spawn(move || {
                run_asio_control_thread(entry, cfg, ready_tx, ctrl_rx, reset_flag_thread);
            })
            .map_err(|e| AudioError::StreamFailure {
                context: "spawn_asio_ctrl",
                detail: e.to_string(),
            })?;

        let init_res = ready_rx.recv().map_err(|_| AudioError::AsioDriver {
            driver_name: driver_label,
            detail: "ASIO control thread terminated unexpectedly during init".to_string(),
        })?;

        match init_res {
            Ok((active_sr, active_buf, metadata)) => Ok(AsioStartOutcome {
                session: Self {
                    ctrl_tx,
                    join_handle: Some(join_handle),
                    reset_flag,
                },
                active_sample_rate: active_sr,
                active_buffer_frames: active_buf,
                metadata,
            }),
            Err(err) => {
                let _ = join_handle.join();
                Err(err)
            }
        }
    }

    pub fn open_control_panel(&self) -> Result<(), AudioError> {
        self.ctrl_tx
            .send(AsioCtrlCmd::OpenControlPanel)
            .map_err(|_| AudioError::CommandChannelClosed)
    }

    pub fn take_reset_requested(&self) -> bool {
        self.reset_flag.swap(false, Ordering::AcqRel)
    }

    pub fn stop(&mut self) {
        let _ = self.ctrl_tx.send(AsioCtrlCmd::Stop);
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for AsioStreamSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_asio_control_thread(
    entry: AsioDriverEntry,
    cfg: AsioStartConfig,
    ready_tx: Sender<Result<(u32, u32, AsioDriverMetadata), AudioError>>,
    ctrl_rx: Receiver<AsioCtrlCmd>,
    reset_flag: std::sync::Arc<AtomicBool>,
) {
    let _com = ComApartmentGuard::init_sta();
    let drv = match AsioComDriver::create_and_init(&entry) {
        Ok(d) => d,
        Err(err) => {
            let _ = ready_tx.send(Err(err));
            return;
        }
    };

    if let Some(req_sr) = cfg.requested_sample_rate {
        if drv.can_sample_rate(req_sr) {
            if let Err(err) = drv.set_sample_rate(req_sr) {
                warn!("ASIO '{}' ignored setSampleRate({req_sr}): {err}", entry.display_name);
            }
        }
    }

    let mut metadata = match query_driver_metadata(&drv, &entry.key_name) {
        Ok(m) => m,
        Err(err) => {
            let _ = ready_tx.send(Err(err));
            return;
        }
    };
    let active_sr = metadata.current_sample_rate;
    let chosen_buf = choose_asio_buffer_size(
        cfg.requested_buffer_frames,
        metadata.min_buffer_size,
        metadata.max_buffer_size,
        metadata.preferred_buffer_size,
        metadata.granularity,
    );

    if metadata.output_channels == 0 {
        let _ = ready_tx.send(Err(AudioError::AsioDriver {
            driver_name: entry.display_name,
            detail: "driver reports 0 output channels".to_string(),
        }));
        return;
    }

    let mut buf_infos = Vec::new();
    let mut channel_types = Vec::new();

    if metadata.input_channels > 0 {
        let in_l = cfg.input_channel_offset.min(metadata.input_channels - 1);
        buf_infos.push(AsioBufferInfo {
            is_input: 1,
            channel_num: in_l as i32,
            buffers: [std::ptr::null_mut(); 2],
        });
        let ty_l = drv
            .channel_info(in_l, true)
            .map(|i| AsioSampleType::from_raw(i.sample_type))
            .unwrap_or(AsioSampleType::Int32Lsb);
        channel_types.push((true, ty_l));

        if in_l + 1 < metadata.input_channels {
            buf_infos.push(AsioBufferInfo {
                is_input: 1,
                channel_num: (in_l + 1) as i32,
                buffers: [std::ptr::null_mut(); 2],
            });
            let ty_r = drv
                .channel_info(in_l + 1, true)
                .map(|i| AsioSampleType::from_raw(i.sample_type))
                .unwrap_or(ty_l);
            channel_types.push((true, ty_r));
        }
    }

    let out_l = cfg.output_channel_offset.min(metadata.output_channels - 1);
    buf_infos.push(AsioBufferInfo {
        is_input: 0,
        channel_num: out_l as i32,
        buffers: [std::ptr::null_mut(); 2],
    });
    let ty_out_l = drv
        .channel_info(out_l, false)
        .map(|i| AsioSampleType::from_raw(i.sample_type))
        .unwrap_or(AsioSampleType::Int32Lsb);
    channel_types.push((false, ty_out_l));

    if out_l + 1 < metadata.output_channels {
        buf_infos.push(AsioBufferInfo {
            is_input: 0,
            channel_num: (out_l + 1) as i32,
            buffers: [std::ptr::null_mut(); 2],
        });
        let ty_out_r = drv
            .channel_info(out_l + 1, false)
            .map(|i| AsioSampleType::from_raw(i.sample_type))
            .unwrap_or(ty_out_l);
        channel_types.push((false, ty_out_r));
    }

    for (_, ty) in &channel_types {
        if !ty.is_supported() {
            let _ = ready_tx.send(Err(AudioError::AsioDriver {
                driver_name: entry.display_name,
                detail: format!("unsupported ASIO sample format: {}", ty.label()),
            }));
            return;
        }
    }

    let callbacks = AsioCallbacks {
        buffer_switch: asio_buffer_switch,
        sample_rate_did_change: asio_sample_rate_did_change,
        asio_message,
        buffer_switch_time_info: asio_buffer_switch_time_info,
    };

    let mut active_buf = chosen_buf;
    if let Err(err) = drv.create_buffers(&mut buf_infos, active_buf, &callbacks) {
        if active_buf != metadata.preferred_buffer_size {
            warn!(
                "ASIO createBuffers({active_buf}) failed, retrying preferred {}: {err}",
                metadata.preferred_buffer_size
            );
            active_buf = metadata.preferred_buffer_size;
            for info in &mut buf_infos {
                info.buffers = [std::ptr::null_mut(); 2];
            }
            if let Err(err2) = drv.create_buffers(&mut buf_infos, active_buf, &callbacks) {
                let _ = ready_tx.send(Err(err2));
                return;
            }
        } else {
            let _ = ready_tx.send(Err(err));
            return;
        }
    }

    let (in_lat, out_lat) = drv.latencies();
    metadata.input_latency_frames = in_lat;
    metadata.output_latency_frames = out_lat;

    let mut in_channels = Vec::new();
    let mut out_channels = Vec::new();
    for (info, &(is_input, sample_type)) in buf_infos.iter().zip(channel_types.iter()) {
        let entry_buf = ActiveChannelBuffers {
            is_input,
            sample_type,
            buf_ptrs: info.buffers,
        };
        if entry_buf.is_input {
            in_channels.push(entry_buf);
        } else {
            out_channels.push(entry_buf);
        }
    }

    let max_block = (active_buf as usize).max(2048);
    let mut graph = AudioGraphProcessor::new(active_sr as f64, max_block);
    graph.attach_retired_tx(cfg.retired_tx);
    graph.attach_retired_aux_tx(cfg.retired_slot_tx, cfg.retired_params_tx, cfg.retired_cfg_tx);
    graph.attach_tuner_tx(cfg.tuner_tx);

    let out_ready_fn = unsafe { drv.vtbl().output_ready };
    let runtime = Box::new(AsioRuntimeContext {
        graph,
        cmd_rx: cfg.cmd_rx,
        telemetry_tx: cfg.telemetry_tx,
        rack_meters_tx: Some(cfg.rack_meters_tx),
        decimator: crate::engine::TelemetryDecimator::new(active_sr),
        buffer_frames: active_buf as usize,
        in_channels,
        out_channels,
        in_l: vec![0.0; max_block],
        in_r: vec![0.0; max_block],
        out_l: vec![0.0; max_block],
        out_r: vec![0.0; max_block],
        output_ready_fn: Some((out_ready_fn, drv.ptr)),
        reset_requested: AtomicBool::new(false),
        mmcss_applied: AtomicBool::new(false),
    });

    let ctx_ptr = Box::into_raw(runtime);
    ACTIVE_ASIO_CTX.store(ctx_ptr, Ordering::Release);

    if let Err(err) = drv.start() {
        ACTIVE_ASIO_CTX.store(std::ptr::null_mut(), Ordering::Release);
        wait_callbacks_drained();
        unsafe {
            drop(Box::from_raw(ctx_ptr));
        }
        drv.dispose_buffers();
        let _ = ready_tx.send(Err(err));
        return;
    }

    info!(
        "Started ASIO driver '{}' @ {} Hz, {} frames (in ch {}, out ch {})",
        entry.display_name, active_sr, active_buf, metadata.input_channels, metadata.output_channels
    );
    let _ = ready_tx.send(Ok((active_sr, active_buf, metadata)));

    loop {
        pump_win32_messages();
        let ctx_ref = unsafe { &*ctx_ptr };
        if ctx_ref.reset_requested.swap(false, Ordering::AcqRel) {
            reset_flag.store(true, Ordering::Release);
        }

        match ctrl_rx.recv_timeout(Duration::from_millis(80)) {
            Ok(AsioCtrlCmd::OpenControlPanel) => {
                if let Err(err) = drv.control_panel() {
                    warn!("ASIO controlPanel returned error: {err}");
                }
            }
            Ok(AsioCtrlCmd::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    drv.stop();
    ACTIVE_ASIO_CTX.store(std::ptr::null_mut(), Ordering::Release);
    wait_callbacks_drained();
    drv.dispose_buffers();
    unsafe {
        drop(Box::from_raw(ctx_ptr));
    }
    info!("Stopped ASIO driver '{}'", entry.display_name);
}

fn wait_callbacks_drained() {
    let mut spins = 0u32;
    while CALLBACK_DEPTH.load(Ordering::Acquire) > 0 {
        if spins < 16 {
            std::thread::yield_now();
            spins += 1;
        } else {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_conversions_roundtrip_int16_int24_int32_float32() {
        let src = [-1.0f32, -0.5, 0.0, 0.5, 0.95];
        let mut dst = [0.0f32; 5];

        let mut i16_buf = [0u8; 10];
        unsafe {
            write_asio_channel(i16_buf.as_mut_ptr() as *mut c_void, AsioSampleType::Int16Lsb, &src, 5);
            read_asio_channel(i16_buf.as_ptr() as *const c_void, AsioSampleType::Int16Lsb, &mut dst, 5);
        }
        for (a, b) in src.iter().zip(dst.iter()) {
            assert!((a - b).abs() < 1e-3, "Int16 mismatch {a} vs {b}");
        }

        let mut i24_buf = [0u8; 15];
        unsafe {
            write_asio_channel(i24_buf.as_mut_ptr() as *mut c_void, AsioSampleType::Int24Lsb, &src, 5);
            read_asio_channel(i24_buf.as_ptr() as *const c_void, AsioSampleType::Int24Lsb, &mut dst, 5);
        }
        for (a, b) in src.iter().zip(dst.iter()) {
            assert!((a - b).abs() < 1e-5, "Int24 mismatch {a} vs {b}");
        }

        let mut i32_buf = [0u8; 20];
        unsafe {
            write_asio_channel(i32_buf.as_mut_ptr() as *mut c_void, AsioSampleType::Int32Lsb, &src, 5);
            read_asio_channel(i32_buf.as_ptr() as *const c_void, AsioSampleType::Int32Lsb, &mut dst, 5);
        }
        for (a, b) in src.iter().zip(dst.iter()) {
            assert!((a - b).abs() < 1e-6, "Int32 mismatch {a} vs {b}");
        }

        let mut f32_buf = [0.0f32; 5];
        unsafe {
            write_asio_channel(f32_buf.as_mut_ptr() as *mut c_void, AsioSampleType::Float32Lsb, &src, 5);
            read_asio_channel(f32_buf.as_ptr() as *const c_void, AsioSampleType::Float32Lsb, &mut dst, 5);
        }
        assert_eq!(dst, src);
    }

    #[test]
    fn choose_buffer_size_respects_granularity_and_bounds() {
        assert_eq!(choose_asio_buffer_size(Some(512), 64, 2048, 256, -1), 512);
        assert_eq!(choose_asio_buffer_size(Some(300), 64, 2048, 256, -1), 256);
        assert_eq!(choose_asio_buffer_size(Some(500), 64, 2048, 256, 64), 448);
        assert_eq!(choose_asio_buffer_size(Some(512), 256, 256, 256, 0), 256);
    }

    #[test]
    fn enumerate_asio_registry_does_not_panic() {
        let _drivers = enumerate_asio_drivers();
    }
}

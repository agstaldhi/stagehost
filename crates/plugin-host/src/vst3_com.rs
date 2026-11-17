use std::collections::HashMap;
use std::ffi::{c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use libloading::Library;
use vst3::Steinberg::Vst::IAttributeList_::AttrID;
use vst3::Steinberg::Vst::{
    IAttributeList, IAttributeListTrait, IComponentHandler, IComponentHandlerTrait,
    IHostApplication, IHostApplicationTrait, IMessage, IMessageTrait, ParamID, ParamValue, TChar,
};
use vst3::Steinberg::{
    char16, int32, int64, kInvalidArgument, kNoInterface, kOutOfMemory, kResultFalse, kResultOk,
    tresult, uint32, FUnknown, IBStream, IBStreamTrait, IPlugFrame, IPlugFrameTrait, IPlugView,
    IPlugViewTrait, IPluginFactory, ViewRect, TUID,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};

use crate::PluginError;

type GetPluginFactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;
type InitDllFn = unsafe extern "system" fn() -> bool;
type ExitDllFn = unsafe extern "system" fn() -> bool;

struct Vst3DllEntry {
    lib: Library,
    exit_fn: Option<ExitDllFn>,
    canonical_path: PathBuf,
}

impl Drop for Vst3DllEntry {
    fn drop(&mut self) {
        if let Some(exit) = self.exit_fn {
            unsafe {
                let _ = exit();
            }
        }
        if let Ok(mut map) = module_cache().lock() {
            if let Some(weak) = map.get(&self.canonical_path) {
                if weak.strong_count() == 0 {
                    map.remove(&self.canonical_path);
                }
            }
        }
    }
}

fn module_cache() -> &'static Mutex<HashMap<PathBuf, Weak<Vst3DllEntry>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Weak<Vst3DllEntry>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Refcounted handle to a loaded VST3 module so `InitDll`/`ExitDll` stay balanced across slots.
pub(crate) struct Vst3ModuleGuard {
    _entry: Arc<Vst3DllEntry>,
}

fn normalized_module_path(path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    if let Some(s) = canonical.to_str() {
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    canonical
}

impl Vst3ModuleGuard {
    pub(crate) fn load_factory(path: &Path) -> Result<(Self, ComPtr<IPluginFactory>), PluginError> {
        let canonical = normalized_module_path(path);
        let entry = {
            let mut cache = module_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(existing) = cache.get(&canonical).and_then(Weak::upgrade) {
                existing
            } else {
                let lib = unsafe { Library::new(&canonical) }.map_err(|e| PluginError::BinaryLoad {
                    path: path.to_path_buf(),
                    detail: e.to_string(),
                })?;

                if let Ok(init_fn) = unsafe { lib.get::<InitDllFn>(b"InitDll\0") } {
                    if !unsafe { init_fn() } {
                        return Err(PluginError::InitFailed {
                            context: "InitDll",
                            detail: format!("VST3 InitDll returned false for '{}'", path.display()),
                        });
                    }
                }
                let exit_fn = unsafe { lib.get::<ExitDllFn>(b"ExitDll\0") }
                    .ok()
                    .map(|s| *s);

                let created = Arc::new(Vst3DllEntry {
                    lib,
                    exit_fn,
                    canonical_path: canonical.clone(),
                });
                cache.insert(canonical, Arc::downgrade(&created));
                created
            }
        };

        let get_factory: libloading::Symbol<GetPluginFactoryFn> =
            unsafe { entry.lib.get(b"GetPluginFactory\0") }.map_err(|e| {
                PluginError::BinaryLoad {
                    path: path.to_path_buf(),
                    detail: format!("missing GetPluginFactory: {e}"),
                }
            })?;

        let raw_factory = unsafe { get_factory() };
        let factory =
            unsafe { ComPtr::from_raw(raw_factory) }.ok_or_else(|| PluginError::BinaryLoad {
                path: path.to_path_buf(),
                detail: "GetPluginFactory returned null".to_string(),
            })?;

        Ok((Self { _entry: entry }, factory))
    }
}

pub(crate) struct HostApplication;

impl Class for HostApplication {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut [char16; 128]) -> tresult {
        if let Some(buf) = unsafe { name.as_mut() } {
            let label: Vec<u16> = "StageHost\0".encode_utf16().collect();
            let count = label.len().min(buf.len());
            buf[..count].copy_from_slice(&label[..count]);
        }
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        let cid_bytes = tuid_ptr_to_bytes(cid);
        let iid_bytes = tuid_ptr_to_bytes(iid);
        unsafe { *obj = ptr::null_mut() };

        if cid_bytes == IMessage::IID {
            let msg = ComWrapper::new(HostMessage::new());
            if iid_bytes == IMessage::IID {
                if let Some(p) = msg.to_com_ptr::<IMessage>() {
                    unsafe { *obj = p.into_raw().cast() };
                    return kResultOk;
                }
            } else if iid_bytes == FUnknown::IID {
                if let Some(p) = msg.to_com_ptr::<FUnknown>() {
                    unsafe { *obj = p.into_raw().cast() };
                    return kResultOk;
                }
            }
            return kNoInterface;
        }

        if cid_bytes == IAttributeList::IID {
            let attrs = ComWrapper::new(HostAttributeList::default());
            if iid_bytes == IAttributeList::IID {
                if let Some(p) = attrs.to_com_ptr::<IAttributeList>() {
                    unsafe { *obj = p.into_raw().cast() };
                    return kResultOk;
                }
            } else if iid_bytes == FUnknown::IID {
                if let Some(p) = attrs.to_com_ptr::<FUnknown>() {
                    unsafe { *obj = p.into_raw().cast() };
                    return kResultOk;
                }
            }
            return kNoInterface;
        }

        kNoInterface
    }
}

fn tuid_ptr_to_bytes(ptr: *const TUID) -> [u8; 16] {
    let raw = unsafe { &*ptr };
    let mut out = [0u8; 16];
    for (dst, &src) in out.iter_mut().zip(raw.iter()) {
        *dst = src as u8;
    }
    out
}

pub(crate) struct HostComponentHandler;

impl Class for HostComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for HostComponentHandler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, _id: ParamID, _value_normalized: ParamValue) -> tresult {
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
        kResultOk
    }
}

pub(crate) struct HostPlugFrame {
    requested_size: Arc<Mutex<Option<(u32, u32)>>>,
}

impl HostPlugFrame {
    pub(crate) fn new(requested_size: Arc<Mutex<Option<(u32, u32)>>>) -> Self {
        Self { requested_size }
    }
}

impl Class for HostPlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for HostPlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if new_size.is_null() {
            return kInvalidArgument;
        }
        let rect = unsafe { &*new_size };
        let w = (rect.right - rect.left).clamp(160, 3840) as u32;
        let h = (rect.bottom - rect.top).clamp(120, 2160) as u32;
        if let Ok(mut slot) = self.requested_size.lock() {
            *slot = Some((w, h));
        }
        if let Some(view_ref) = unsafe { ComRef::from_raw(view) } {
            unsafe {
                let _ = view_ref.onSize(new_size);
            }
        }
        kResultOk
    }
}

struct MessageState {
    id: CString,
    id_history: Vec<CString>,
}

impl Default for MessageState {
    fn default() -> Self {
        Self {
            id: CString::new("").unwrap_or_default(),
            id_history: Vec::new(),
        }
    }
}

pub(crate) struct HostMessage {
    state: Mutex<MessageState>,
    _attrs_owner: ComWrapper<HostAttributeList>,
    attrs_ptr: ComPtr<IAttributeList>,
}

impl HostMessage {
    fn new() -> Self {
        let attrs_owner = ComWrapper::new(HostAttributeList::default());
        let attrs_ptr = attrs_owner
            .to_com_ptr::<IAttributeList>()
            .expect("HostAttributeList implements IAttributeList");
        Self {
            state: Mutex::new(MessageState::default()),
            _attrs_owner: attrs_owner,
            attrs_ptr,
        }
    }
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> vst3::Steinberg::FIDString {
        let guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        guard.id.as_ptr()
    }

    unsafe fn setMessageID(&self, id: vst3::Steinberg::FIDString) {
        if id.is_null() {
            return;
        }
        let cstr = unsafe { CStr::from_ptr(id) }.to_owned();
        let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let old = std::mem::replace(&mut guard.id, cstr);
        guard.id_history.push(old);
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attrs_ptr.as_ptr()
    }
}

#[derive(Default)]
struct AttributeStore {
    ints: HashMap<String, int64>,
    floats: HashMap<String, f64>,
    strings: HashMap<String, Vec<u16>>,
    binaries: HashMap<String, Box<[u8]>>,
}

#[derive(Default)]
pub(crate) struct HostAttributeList {
    store: Mutex<AttributeStore>,
}

impl Class for HostAttributeList {
    type Interfaces = (IAttributeList,);
}

fn attr_key(id: AttrID) -> Option<String> {
    if id.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned())
}

impl IAttributeListTrait for HostAttributeList {
    unsafe fn setInt(&self, id: AttrID, value: int64) -> tresult {
        let Some(key) = attr_key(id) else {
            return kInvalidArgument;
        };
        self.store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .ints
            .insert(key, value);
        kResultOk
    }

    unsafe fn getInt(&self, id: AttrID, value: *mut int64) -> tresult {
        let Some(key) = attr_key(id) else {
            return kInvalidArgument;
        };
        if value.is_null() {
            return kInvalidArgument;
        }
        let guard = self.store.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(&v) = guard.ints.get(&key) {
            unsafe { *value = v };
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        let Some(key) = attr_key(id) else {
            return kInvalidArgument;
        };
        self.store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .floats
            .insert(key, value);
        kResultOk
    }

    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        let Some(key) = attr_key(id) else {
            return kInvalidArgument;
        };
        if value.is_null() {
            return kInvalidArgument;
        }
        let guard = self.store.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(&v) = guard.floats.get(&key) {
            unsafe { *value = v };
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn setString(&self, id: AttrID, string: *const TChar) -> tresult {
        let Some(key) = attr_key(id) else {
            return kInvalidArgument;
        };
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut len = 0usize;
        while unsafe { *string.add(len) } != 0 {
            len += 1;
        }
        let slice = unsafe { std::slice::from_raw_parts(string, len + 1) };
        self.store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .strings
            .insert(key, slice.to_vec());
        kResultOk
    }

    unsafe fn getString(&self, id: AttrID, string: *mut TChar, size_in_bytes: uint32) -> tresult {
        let Some(key) = attr_key(id) else {
            return kInvalidArgument;
        };
        if string.is_null() {
            return kInvalidArgument;
        }
        let guard = self.store.lock().unwrap_or_else(|p| p.into_inner());
        let Some(stored) = guard.strings.get(&key) else {
            return kResultFalse;
        };
        let max_chars = (size_in_bytes as usize) / std::mem::size_of::<TChar>();
        if max_chars == 0 {
            return kOutOfMemory;
        }
        let copy_len = stored.len().min(max_chars);
        unsafe {
            ptr::copy_nonoverlapping(stored.as_ptr(), string, copy_len);
            if copy_len == max_chars && max_chars < stored.len() {
                *string.add(max_chars - 1) = 0;
            }
        }
        kResultOk
    }

    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size_in_bytes: uint32) -> tresult {
        let Some(key) = attr_key(id) else {
            return kInvalidArgument;
        };
        let len = size_in_bytes as usize;
        let bytes = if len == 0 || data.is_null() {
            Box::default()
        } else {
            unsafe { std::slice::from_raw_parts(data as *const u8, len) }.into()
        };
        self.store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .binaries
            .insert(key, bytes);
        kResultOk
    }

    unsafe fn getBinary(
        &self,
        id: AttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut uint32,
    ) -> tresult {
        let Some(key) = attr_key(id) else {
            return kInvalidArgument;
        };
        if data.is_null() || size_in_bytes.is_null() {
            return kInvalidArgument;
        }
        let guard = self.store.lock().unwrap_or_else(|p| p.into_inner());
        let Some(stored) = guard.binaries.get(&key) else {
            return kResultFalse;
        };
        unsafe {
            *data = stored.as_ptr().cast();
            *size_in_bytes = stored.len() as uint32;
        }
        kResultOk
    }
}

#[derive(Default)]
struct StreamInner {
    data: Vec<u8>,
    pos: usize,
}

#[derive(Default)]
pub(crate) struct MemoryBStream {
    inner: Mutex<StreamInner>,
}

impl MemoryBStream {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            inner: Mutex::new(StreamInner {
                data: bytes.to_vec(),
                pos: 0,
            }),
        }
    }

    pub(crate) fn take_bytes(&self) -> Vec<u8> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .data
            .clone()
    }
}

impl Class for MemoryBStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemoryBStream {
    unsafe fn read(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_read: *mut int32,
    ) -> tresult {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let remaining = inner.data.len().saturating_sub(inner.pos);
        let to_read = remaining.min(num_bytes.max(0) as usize);
        if to_read > 0 && !buffer.is_null() {
            unsafe {
                ptr::copy_nonoverlapping(inner.data[inner.pos..].as_ptr(), buffer as *mut u8, to_read);
            }
            inner.pos += to_read;
        }
        if !num_bytes_read.is_null() {
            unsafe { *num_bytes_read = to_read as int32 };
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_written: *mut int32,
    ) -> tresult {
        let len = num_bytes.max(0) as usize;
        if len > 0 && !buffer.is_null() {
            let slice = unsafe { std::slice::from_raw_parts(buffer as *const u8, len) };
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let end_pos = inner.pos + len;
            if end_pos > inner.data.len() {
                inner.data.resize(end_pos, 0);
            }
            let start = inner.pos;
            inner.data[start..end_pos].copy_from_slice(slice);
            inner.pos = end_pos;
        }
        if !num_bytes_written.is_null() {
            unsafe { *num_bytes_written = len as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos_offset: int64, mode: int32, result: *mut int64) -> tresult {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let len = inner.data.len() as int64;
        let base = match mode {
            0 => 0,
            1 => inner.pos as int64,
            2 => len,
            _ => return kInvalidArgument,
        };
        let next = (base + pos_offset).clamp(0, len) as usize;
        inner.pos = next;
        if !result.is_null() {
            unsafe { *result = next as int64 };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos_out: *mut int64) -> tresult {
        if !pos_out.is_null() {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            unsafe { *pos_out = inner.pos as int64 };
        }
        kResultOk
    }
}

#[cfg(test)]
mod tests {
    use vst3::Steinberg::Vst::{IAttributeListTrait, IHostApplication, IHostApplicationTrait, IMessage, IMessageTrait};
    use vst3::Steinberg::{kResultOk, IBStream, IBStreamTrait, TUID};
    use vst3::{ComPtr, ComWrapper, Interface};

    use super::{HostApplication, MemoryBStream};

    #[test]
    fn host_application_creates_imessage_with_attributes() {
        let host = ComWrapper::new(HostApplication);
        let host_ptr = host.to_com_ptr::<IHostApplication>().expect("IHostApplication");
        let mut msg_cid: TUID = [0; 16];
        let mut msg_iid: TUID = [0; 16];
        for (dst, &b) in msg_cid.iter_mut().zip(IMessage::IID.iter()) {
            *dst = b as i8;
        }
        msg_iid.copy_from_slice(&msg_cid);

        let mut raw_msg: *mut std::ffi::c_void = std::ptr::null_mut();
        let rc = unsafe { host_ptr.createInstance(&mut msg_cid, &mut msg_iid, &mut raw_msg) };
        assert_eq!(rc, kResultOk);
        assert!(!raw_msg.is_null());

        let msg = unsafe { ComPtr::from_raw(raw_msg as *mut IMessage) }.expect("IMessage");
        unsafe {
            msg.setMessageID(c"StageHostInit".as_ptr());
            let attrs = msg.getAttributes();
            assert!(!attrs.is_null());
            let attrs_ref = vst3::ComRef::from_raw(attrs).expect("attrs");
            assert_eq!(attrs_ref.setInt(c"kTest".as_ptr(), 42), kResultOk);
            let mut read_back = 0i64;
            assert_eq!(attrs_ref.getInt(c"kTest".as_ptr(), &mut read_back), kResultOk);
            assert_eq!(read_back, 42);
        }
    }

    #[test]
    fn memory_bstream_roundtrips_write_seek_read() {
        let stream = ComWrapper::new(MemoryBStream::default());
        let s = stream.to_com_ptr::<IBStream>().expect("IBStream");
        let mut payload = *b"VST3_STATE_OK";
        let mut written = 0;
        unsafe {
            assert_eq!(
                s.write(payload.as_mut_ptr().cast(), payload.len() as i32, &mut written),
                kResultOk
            );
            assert_eq!(written, payload.len() as i32);
            let mut pos = -1;
            assert_eq!(s.seek(0, 0, &mut pos), kResultOk);
            assert_eq!(pos, 0);
            let mut buf = [0u8; 16];
            let mut read_n = 0;
            assert_eq!(s.read(buf.as_mut_ptr().cast(), buf.len() as i32, &mut read_n), kResultOk);
            assert_eq!(&buf[..read_n as usize], &payload);
        }
    }
}

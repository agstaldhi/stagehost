//! Win32 child/popup window manager for hosting external VST3 and CLAP plugin GUIs.

use std::collections::HashMap;
use uuid::Uuid;

/// Tracks open native OS windows hosting external VST3/CLAP plugin UIs.
#[derive(Default)]
pub struct PluginEditorWindows {
    #[cfg(windows)]
    imp: win32_imp::Win32EditorHost,
}

impl PluginEditorWindows {
    /// Creates (or focuses) a native OS host window for `(rack_id, slot_id)` and returns
    /// `(hwnd, is_newly_created)`.
    pub fn open_or_focus(
        &mut self,
        rack_id: Uuid,
        slot_id: Uuid,
        title: &str,
    ) -> Result<(isize, bool), String> {
        #[cfg(windows)]
        {
            self.imp.open_or_focus(rack_id, slot_id, title)
        }
        #[cfg(not(windows))]
        {
            let _ = (rack_id, slot_id, title);
            Err("Native plugin editor windows are only implemented on Windows".to_string())
        }
    }

    /// Closes a single open editor window when rollback is needed.
    pub fn close_one(&mut self, rack_id: Uuid, slot_id: Uuid) {
        #[cfg(windows)]
        self.imp.close_one(rack_id, slot_id);
        #[cfg(not(windows))]
        let _ = (rack_id, slot_id);
    }

    /// Pumps Win32 window messages and returns any `(rack_id, slot_id)` windows closed by the user.
    pub fn poll_closed(&mut self) -> Vec<(Uuid, Uuid)> {
        #[cfg(windows)]
        {
            self.imp.poll_closed()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }

    /// Resizes an open editor host window so its client area matches the plugin's requested size.
    pub fn resize_client_area(&mut self, rack_id: Uuid, slot_id: Uuid, width: u32, height: u32) {
        #[cfg(windows)]
        self.imp.resize_client_area(rack_id, slot_id, width, height);
        #[cfg(not(windows))]
        let _ = (rack_id, slot_id, width, height);
    }

    /// Detects user-dragged editor window resizes so the hosted plugin GUI can reflow.
    pub fn poll_resized(&mut self) -> Vec<(Uuid, Uuid, u32, u32)> {
        #[cfg(windows)]
        {
            self.imp.poll_resized()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }

    /// Closes all open editor windows cleanly.
    pub fn close_all(&mut self) {
        #[cfg(windows)]
        self.imp.close_all();
    }
}

#[cfg(windows)]
mod win32_imp {
    use super::HashMap;
    use std::sync::Once;
    use uuid::Uuid;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect,
        IsWindow, RegisterClassW, SetForegroundWindow, SetWindowPos, ShowWindow, CS_HREDRAW,
        CS_VREDRAW, CW_USEDEFAULT, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOW, WM_CLOSE,
        WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    const CLASS_NAME: PCWSTR = w!("StageHostPluginEditorWindow");
    static REGISTER_CLASS: Once = Once::new();

    #[derive(Default)]
    pub struct Win32EditorHost {
        pub handles: HashMap<(Uuid, Uuid), (HWND, u32, u32)>,
    }

    impl Win32EditorHost {
        pub fn open_or_focus(
            &mut self,
            rack_id: Uuid,
            slot_id: Uuid,
            title: &str,
        ) -> Result<(isize, bool), String> {
            ensure_window_class();
            let key = (rack_id, slot_id);
            if let Some(&(hwnd, _, _)) = self.handles.get(&key) {
                if unsafe { IsWindow(Some(hwnd)).as_bool() } {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_SHOW);
                        let _ = SetForegroundWindow(hwnd);
                    }
                    return Ok((hwnd.0 as isize, false));
                }
                self.handles.remove(&key);
            }

            let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let hinstance = unsafe {
                GetModuleHandleW(None)
                    .map_err(|e| format!("GetModuleHandleW failed: {e}"))?
            };
            let hwnd = unsafe {
                CreateWindowExW(
                    Default::default(),
                    CLASS_NAME,
                    PCWSTR(title_wide.as_ptr()),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    900,
                    620,
                    None,
                    None,
                    Some(HINSTANCE(hinstance.0)),
                    None,
                )
            }
            .map_err(|e| format!("CreateWindowExW failed: {e}"))?;

            let (cw, ch) = read_client_size(hwnd).unwrap_or((900, 620));
            self.handles.insert(key, (hwnd, cw, ch));
            Ok((hwnd.0 as isize, true))
        }

        pub fn resize_client_area(&mut self, rack_id: Uuid, slot_id: Uuid, width: u32, height: u32) {
            let Some((hwnd, cw, ch)) = self.handles.get_mut(&(rack_id, slot_id)) else {
                return;
            };
            if unsafe { !IsWindow(Some(*hwnd)).as_bool() } {
                return;
            }
            let w = width.clamp(160, 3840);
            let h = height.clamp(120, 2160);
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            };
            unsafe {
                let _ = AdjustWindowRectEx(
                    &mut rect,
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    false,
                    Default::default(),
                );
                let _ = SetWindowPos(
                    *hwnd,
                    None,
                    0,
                    0,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            *cw = w;
            *ch = h;
        }

        pub fn poll_resized(&mut self) -> Vec<(Uuid, Uuid, u32, u32)> {
            let mut changed = Vec::new();
            for (&(rack_id, slot_id), (hwnd, prev_w, prev_h)) in &mut self.handles {
                if let Some((w, h)) = read_client_size(*hwnd) {
                    if w > 0 && h > 0 && (w != *prev_w || h != *prev_h) {
                        *prev_w = w;
                        *prev_h = h;
                        changed.push((rack_id, slot_id, w, h));
                    }
                }
            }
            changed
        }

        pub fn close_one(&mut self, rack_id: Uuid, slot_id: Uuid) {
            if let Some((hwnd, _, _)) = self.handles.remove(&(rack_id, slot_id)) {
                if unsafe { IsWindow(Some(hwnd)).as_bool() } {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
            }
        }

        pub fn poll_closed(&mut self) -> Vec<(Uuid, Uuid)> {
            // winit pumps Win32 messages for all HWNDs created on the UI thread; draining
            // PeekMessageW(None) inside RedrawRequested steals mouse/key events from eframe.
            let mut closed = Vec::new();
            self.handles.retain(|&key, &mut (hwnd, _, _)| {
                let alive = unsafe { IsWindow(Some(hwnd)).as_bool() };
                if !alive {
                    closed.push(key);
                }
                alive
            });
            closed
        }

        pub fn close_all(&mut self) {
            for (_, (hwnd, _, _)) in self.handles.drain() {
                if unsafe { IsWindow(Some(hwnd)).as_bool() } {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
            }
        }
    }

    fn read_client_size(hwnd: HWND) -> Option<(u32, u32)> {
        let mut rect = RECT::default();
        if unsafe { GetClientRect(hwnd, &mut rect) }.is_ok() {
            let w = (rect.right - rect.left).max(0) as u32;
            let h = (rect.bottom - rect.top).max(0) as u32;
            Some((w, h))
        } else {
            None
        }
    }

    impl Drop for Win32EditorHost {
        fn drop(&mut self) {
            self.close_all();
        }
    }

    fn ensure_window_class() {
        REGISTER_CLASS.call_once(|| {
            let hinstance = unsafe { GetModuleHandleW(None).unwrap_or_default() };
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(editor_wnd_proc),
                hInstance: HINSTANCE(hinstance.0),
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            unsafe {
                RegisterClassW(&wc);
            }
        });
    }

    unsafe extern "system" fn editor_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_CLOSE {
            let _ = unsafe { DestroyWindow(hwnd) };
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
}

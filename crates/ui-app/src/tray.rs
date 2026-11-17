//! Native Windows System Tray integration via `Shell_NotifyIconW`.
//!
//! Runs a dedicated helper message window on a background thread so StageHost RS
//! stays alive in the notification area with active audio & global hotkeys when closed.

use std::sync::mpsc::Receiver;

use crate::wake::UiWakeSignal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ShowWindow,
    ExitApp,
}

pub struct SystemTrayHandle {
    rx: Receiver<TrayAction>,
    #[cfg(windows)]
    imp: win32_imp::Win32TrayHandle,
}

impl SystemTrayHandle {
    pub fn start(wake: UiWakeSignal) -> Option<Self> {
        #[cfg(windows)]
        {
            win32_imp::start(wake).map(|(rx, imp)| Self { rx, imp })
        }
        #[cfg(not(windows))]
        {
            let _ = wake;
            None
        }
    }

    pub fn poll_actions(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        while let Ok(action) = self.rx.try_recv() {
            actions.push(action);
        }
        actions
    }
}

#[cfg(windows)]
impl Drop for SystemTrayHandle {
    fn drop(&mut self) {
        self.imp.stop();
    }
}

pub(crate) fn encode_utf16_fixed<const N: usize>(text: &str) -> [u16; N] {
    let mut buf = [0u16; N];
    for (dst, ch) in buf.iter_mut().take(N.saturating_sub(1)).zip(text.encode_utf16()) {
        *dst = ch;
    }
    buf
}

#[cfg(windows)]
mod win32_imp {
    use std::cell::RefCell;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::thread::{self, JoinHandle};

    use tracing::warn;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
        DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, LoadIconW,
        PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
        SetTimer, TrackPopupMenu, TranslateMessage, CW_USEDEFAULT, HICON, IDI_APPLICATION,
        MF_SEPARATOR, MF_STRING, MSG, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP,
        WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP,
        WM_TIMER, WNDCLASSW,
    };

    use super::{encode_utf16_fixed, TrayAction, UiWakeSignal};

    const TRAY_CLASS_NAME: PCWSTR = w!("StageHostSystemTrayWindow");
    const WM_TRAY_CALLBACK: u32 = WM_APP + 42;
    const TRAY_ICON_ID: u32 = 1;
    const TRAY_RETRY_TIMER_ID: usize = 1;
    const TRAY_RETRY_INTERVAL_MS: u32 = 1000;
    const MAX_TRAY_REGISTER_RETRIES: u32 = 30;
    const MENU_CMD_SHOW: usize = 1001;
    const MENU_CMD_EXIT: usize = 1002;

    struct TrayTlsState {
        tx: Sender<TrayAction>,
        wake: UiWakeSignal,
        nid: NOTIFYICONDATAW,
        custom_hicon: Option<HICON>,
        taskbar_created_msg: u32,
        icon_added: bool,
        retry_attempts: u32,
    }

    thread_local! {
        static TRAY_STATE: RefCell<Option<TrayTlsState>> = const { RefCell::new(None) };
    }

    pub struct Win32TrayHandle {
        hwnd_raw: isize,
        join: Option<JoinHandle<()>>,
    }

    impl Win32TrayHandle {
        pub fn stop(&mut self) {
            if self.hwnd_raw != 0 {
                let hwnd = HWND(self.hwnd_raw as *mut _);
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                self.hwnd_raw = 0;
            }
            if let Some(handle) = self.join.take() {
                let _ = handle.join();
            }
        }
    }

    pub fn start(wake: UiWakeSignal) -> Option<(Receiver<TrayAction>, Win32TrayHandle)> {
        let (tx, rx) = mpsc::channel::<TrayAction>();
        let (hwnd_tx, hwnd_rx) = mpsc::channel::<isize>();

        let join = thread::Builder::new()
            .name("stagehost-tray".to_string())
            .spawn(move || unsafe {
                let hmod = GetModuleHandleW(None).ok();
                let hinstance: HINSTANCE = hmod.map(Into::into).unwrap_or_default();

                let wc = WNDCLASSW {
                    lpfnWndProc: Some(tray_wnd_proc),
                    hInstance: hinstance,
                    lpszClassName: TRAY_CLASS_NAME,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);

                let hwnd = match CreateWindowExW(
                    Default::default(),
                    TRAY_CLASS_NAME,
                    w!("StageHost RS Tray"),
                    Default::default(),
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    0,
                    0,
                    None,
                    None,
                    Some(hinstance),
                    None,
                ) {
                    Ok(h) => h,
                    Err(err) => {
                        warn!("Failed to create StageHost tray helper window: {err}");
                        let _ = hwnd_tx.send(0);
                        return;
                    }
                };

                let custom_hicon = crate::icon::create_tray_hicon();
                let hicon = custom_hicon
                    .unwrap_or_else(|| LoadIconW(None, IDI_APPLICATION).unwrap_or_default());
                let nid = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd,
                    uID: TRAY_ICON_ID,
                    uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                    uCallbackMessage: WM_TRAY_CALLBACK,
                    hIcon: hicon,
                    szTip: encode_utf16_fixed::<128>("StageHost RS (Running)"),
                    ..Default::default()
                };

                let taskbar_created_msg = RegisterWindowMessageW(w!("TaskbarCreated"));
                let icon_added = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
                if !icon_added {
                    warn!("Initial system tray registration failed (shell may still be starting); scheduling retry");
                    let _ = SetTimer(Some(hwnd), TRAY_RETRY_TIMER_ID, TRAY_RETRY_INTERVAL_MS, None);
                }

                TRAY_STATE.with(|cell| {
                    *cell.borrow_mut() = Some(TrayTlsState {
                        tx,
                        wake,
                        nid,
                        custom_hicon,
                        taskbar_created_msg,
                        icon_added,
                        retry_attempts: 0,
                    });
                });

                let _ = hwnd_tx.send(hwnd.0 as isize);

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                TRAY_STATE.with(|cell| {
                    if let Some(st) = cell.borrow_mut().take() {
                        if st.icon_added {
                            let _ = Shell_NotifyIconW(NIM_DELETE, &st.nid);
                        }
                        if let Some(icon) = st.custom_hicon {
                            let _ = DestroyIcon(icon);
                        }
                    }
                });
            })
            .ok()?;

        let hwnd_raw = hwnd_rx.recv().ok().unwrap_or(0);
        if hwnd_raw == 0 {
            let _ = join.join();
            return None;
        }
        Some((
            rx,
            Win32TrayHandle {
                hwnd_raw,
                join: Some(join),
            },
        ))
    }

    fn emit_tray_action(action: TrayAction) {
        TRAY_STATE.with(|cell| {
            if let Some(ref st) = *cell.borrow() {
                if st.tx.send(action).is_ok() {
                    st.wake.wake();
                }
            }
        });
    }

    unsafe fn show_tray_context_menu(hwnd: HWND) {
        let Ok(hmenu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        unsafe {
            let _ = AppendMenuW(hmenu, MF_STRING, MENU_CMD_SHOW, w!("Open StageHost RS"));
            let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(hmenu, MF_STRING, MENU_CMD_EXIT, w!("Exit StageHost RS"));

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(
                hmenu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                None,
                hwnd,
                None,
            );
            let _ = DestroyMenu(hmenu);
            match cmd.0 as usize {
                MENU_CMD_SHOW => emit_tray_action(TrayAction::ShowWindow),
                MENU_CMD_EXIT => emit_tray_action(TrayAction::ExitApp),
                _ => {}
            }
        }
    }

    unsafe fn try_register_tray_icon(hwnd: HWND, from_timer: bool) {
        TRAY_STATE.with(|cell| {
            if let Some(ref mut st) = *cell.borrow_mut() {
                if unsafe { Shell_NotifyIconW(NIM_ADD, &st.nid) }.as_bool() {
                    st.icon_added = true;
                    let _ = unsafe { KillTimer(Some(hwnd), TRAY_RETRY_TIMER_ID) };
                    return;
                }
                if from_timer {
                    st.retry_attempts = st.retry_attempts.saturating_add(1);
                    if st.retry_attempts >= MAX_TRAY_REGISTER_RETRIES {
                        warn!("Giving up periodic tray icon retry after {MAX_TRAY_REGISTER_RETRIES} attempts; waiting for TaskbarCreated");
                        let _ = unsafe { KillTimer(Some(hwnd), TRAY_RETRY_TIMER_ID) };
                    }
                }
            }
        });
    }

    unsafe extern "system" fn tray_wnd_proc(
        hwnd: HWND,
        msg: u32,
        w_param: WPARAM,
        l_param: LPARAM,
    ) -> LRESULT {
        let is_taskbar_created = TRAY_STATE.with(|cell| {
            cell.borrow()
                .as_ref()
                .is_some_and(|st| st.taskbar_created_msg != 0 && msg == st.taskbar_created_msg)
        });
        if is_taskbar_created {
            unsafe { try_register_tray_icon(hwnd, false) };
            return LRESULT(0);
        }

        match msg {
            WM_TIMER if w_param.0 == TRAY_RETRY_TIMER_ID => {
                unsafe { try_register_tray_icon(hwnd, true) };
                LRESULT(0)
            }
            WM_TRAY_CALLBACK => {
                let event = (l_param.0 as u32) & 0xFFFF;
                match event {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                        emit_tray_action(TrayAction::ShowWindow);
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => unsafe {
                        show_tray_context_menu(hwnd);
                    },
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = unsafe { DestroyWindow(hwnd) };
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::encode_utf16_fixed;

    #[test]
    fn encodes_null_terminated_utf16_tooltip() {
        let tip = encode_utf16_fixed::<16>("StageHost RS");
        assert_eq!(tip[0], b'S' as u16);
        assert_eq!(tip[11], b'S' as u16);
        assert_eq!(tip[12], 0);
        assert_eq!(tip[15], 0);
    }
}

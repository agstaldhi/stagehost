//! System-wide Windows low-level keyboard listener for global StageHost keybindings.
//!
//! Uses `SetWindowsHookExW(WH_KEYBOARD_LL, ...)` on a dedicated message-loop thread
//! without swallowing key events from other foreground applications.

use std::sync::mpsc::Receiver;

use session_core::KeyComboSpec;

use crate::wake::UiWakeSignal;

pub struct GlobalHotkeyListener {
    rx: Receiver<KeyComboSpec>,
    #[cfg(windows)]
    imp: win32_imp::Win32HookHandle,
}

impl GlobalHotkeyListener {
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

    pub fn poll_pressed(&self) -> Vec<KeyComboSpec> {
        let mut out = Vec::new();
        while let Ok(combo) = self.rx.try_recv() {
            out.push(combo);
        }
        out
    }
}

#[cfg(windows)]
impl Drop for GlobalHotkeyListener {
    fn drop(&mut self) {
        self.imp.stop();
    }
}

pub(crate) fn vk_to_key_name(vk: u32) -> Option<&'static str> {
    match vk {
        0x70 => Some("F1"),
        0x71 => Some("F2"),
        0x72 => Some("F3"),
        0x73 => Some("F4"),
        0x74 => Some("F5"),
        0x75 => Some("F6"),
        0x76 => Some("F7"),
        0x77 => Some("F8"),
        0x78 => Some("F9"),
        0x79 => Some("F10"),
        0x7A => Some("F11"),
        0x7B => Some("F12"),
        0x41 => Some("A"),
        0x42 => Some("B"),
        0x43 => Some("C"),
        0x44 => Some("D"),
        0x45 => Some("E"),
        0x46 => Some("F"),
        0x47 => Some("G"),
        0x48 => Some("H"),
        0x49 => Some("I"),
        0x4A => Some("J"),
        0x4B => Some("K"),
        0x4C => Some("L"),
        0x4D => Some("M"),
        0x4E => Some("N"),
        0x4F => Some("O"),
        0x50 => Some("P"),
        0x51 => Some("Q"),
        0x52 => Some("R"),
        0x53 => Some("S"),
        0x54 => Some("T"),
        0x55 => Some("U"),
        0x56 => Some("V"),
        0x57 => Some("W"),
        0x58 => Some("X"),
        0x59 => Some("Y"),
        0x5A => Some("Z"),
        0x30 => Some("0"),
        0x31 => Some("1"),
        0x32 => Some("2"),
        0x33 => Some("3"),
        0x34 => Some("4"),
        0x35 => Some("5"),
        0x36 => Some("6"),
        0x37 => Some("7"),
        0x38 => Some("8"),
        0x39 => Some("9"),
        0x60 => Some("0"),
        0x61 => Some("1"),
        0x62 => Some("2"),
        0x63 => Some("3"),
        0x64 => Some("4"),
        0x65 => Some("5"),
        0x66 => Some("6"),
        0x67 => Some("7"),
        0x68 => Some("8"),
        0x69 => Some("9"),
        0x6B => Some("+"),
        0x6D => Some("-"),
        0x6E => Some("."),
        0x6F => Some("/"),
        0x20 => Some("Space"),
        0x0D => Some("Enter"),
        0x09 => Some("Tab"),
        0x2D => Some("Insert"),
        0x2E => Some("Delete"),
        0x24 => Some("Home"),
        0x23 => Some("End"),
        0x26 => Some("Up"),
        0x28 => Some("Down"),
        0x25 => Some("Left"),
        0x27 => Some("Right"),
        0xBD => Some("-"),
        0xBB => Some("+"),
        0xDB => Some("["),
        0xDD => Some("]"),
        0xBA => Some(";"),
        0xDE => Some("'"),
        0xBC => Some(","),
        0xBE => Some("."),
        0xBF => Some("/"),
        0xC0 => Some("`"),
        _ => None,
    }
}

#[cfg(windows)]
mod win32_imp {
    use std::cell::RefCell;
    use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use session_core::KeyComboSpec;
    use tracing::warn;
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
        TranslateMessage, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN,
        WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    use super::{vk_to_key_name, UiWakeSignal};

    const STALE_KEY_TIMEOUT: Duration = Duration::from_millis(1500);
    const HOOK_QUEUE_CAPACITY: usize = 64;

    pub(super) struct PressedKeyTracker {
        slots: [Option<Instant>; 256],
    }

    impl PressedKeyTracker {
        pub(super) const fn new() -> Self {
            Self { slots: [None; 256] }
        }

        pub(super) fn record_press(&mut self, vk: u32, now: Instant) -> bool {
            let Ok(idx) = usize::try_from(vk) else {
                return true;
            };
            let Some(slot) = self.slots.get_mut(idx) else {
                return true;
            };
            let prev = slot.replace(now);
            match prev {
                None => true,
                Some(ts) => now.duration_since(ts) >= STALE_KEY_TIMEOUT,
            }
        }

        pub(super) fn record_release(&mut self, vk: u32) {
            if let Ok(idx) = usize::try_from(vk) {
                if let Some(slot) = self.slots.get_mut(idx) {
                    *slot = None;
                }
            }
        }

        /// Clears other tracked keys whose `WM_KEYUP` was missed (for example, when crossing
        /// a Windows UIPI boundary such as clicking Task Manager and returning).
        pub(super) fn prune_released_with<F>(&mut self, active_vk: u32, mut is_still_down: F)
        where
            F: FnMut(u32) -> bool,
        {
            for (idx, slot) in self.slots.iter_mut().enumerate() {
                let vk = idx as u32;
                if slot.is_some() && vk != active_vk && !is_still_down(vk) {
                    *slot = None;
                }
            }
        }
    }

    struct HookTlsState {
        raw_tx: SyncSender<KeyComboSpec>,
        tracker: PressedKeyTracker,
    }

    thread_local! {
        static HOOK_STATE: RefCell<Option<HookTlsState>> = const { RefCell::new(None) };
    }

    pub struct Win32HookHandle {
        thread_id: u32,
        hook_join: Option<JoinHandle<()>>,
        wake_join: Option<JoinHandle<()>>,
    }

    impl Win32HookHandle {
        pub fn stop(&mut self) {
            if self.thread_id != 0 {
                unsafe {
                    let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
                }
                self.thread_id = 0;
            }
            if let Some(handle) = self.hook_join.take() {
                let _ = handle.join();
            }
            if let Some(handle) = self.wake_join.take() {
                let _ = handle.join();
            }
        }
    }

    pub fn start(wake: UiWakeSignal) -> Option<(Receiver<KeyComboSpec>, Win32HookHandle)> {
        let (ui_tx, ui_rx): (Sender<KeyComboSpec>, Receiver<KeyComboSpec>) = mpsc::channel();
        let (raw_tx, raw_rx) = mpsc::sync_channel::<KeyComboSpec>(HOOK_QUEUE_CAPACITY);
        let (tid_tx, tid_rx) = mpsc::channel::<u32>();

        // Keep `UiWakeSignal::wake()` (which locks `egui::Context`) off the OS `WH_KEYBOARD_LL`
        // callback thread so the hook always calls `CallNextHookEx` immediately without timeout risk.
        let wake_join = thread::Builder::new()
            .name("stagehost-hotkey-wake".to_string())
            .spawn(move || {
                while let Ok(combo) = raw_rx.recv() {
                    if ui_tx.send(combo).is_ok() {
                        wake.wake();
                    }
                }
            })
            .ok()?;

        let hook_join = match thread::Builder::new()
            .name("stagehost-global-hotkey".to_string())
            .spawn(move || {
                let tid = unsafe { GetCurrentThreadId() };
                HOOK_STATE.with(|cell| {
                    *cell.borrow_mut() = Some(HookTlsState {
                        raw_tx,
                        tracker: PressedKeyTracker::new(),
                    });
                });

                let hmod = unsafe { GetModuleHandleW(None).ok().map(HINSTANCE::from) };
                let hook = match unsafe {
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_keyboard_proc), hmod, 0)
                } {
                    Ok(h) => h,
                    Err(err) => {
                        warn!("Failed to install global WH_KEYBOARD_LL hook: {err}");
                        let _ = tid_tx.send(0);
                        HOOK_STATE.with(|cell| *cell.borrow_mut() = None);
                        return;
                    }
                };
                let _ = tid_tx.send(tid);

                let mut msg = MSG::default();
                unsafe {
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    let _ = UnhookWindowsHookEx(hook);
                }
                HOOK_STATE.with(|cell| *cell.borrow_mut() = None);
            }) {
            Ok(h) => h,
            Err(_) => {
                let _ = wake_join.join();
                return None;
            }
        };

        let thread_id = tid_rx.recv().ok().unwrap_or(0);
        if thread_id == 0 {
            let _ = hook_join.join();
            let _ = wake_join.join();
            return None;
        }
        Some((
            ui_rx,
            Win32HookHandle {
                thread_id,
                hook_join: Some(hook_join),
                wake_join: Some(wake_join),
            },
        ))
    }

    #[inline]
    unsafe fn is_vk_down(vk: u32) -> bool {
        let Ok(vk_i32) = i32::try_from(vk) else {
            return false;
        };
        (unsafe { GetAsyncKeyState(vk_i32) } as u16 & 0x8000) != 0
    }

    unsafe extern "system" fn low_level_keyboard_proc(
        n_code: i32,
        w_param: WPARAM,
        l_param: LPARAM,
    ) -> LRESULT {
        if n_code >= 0 && l_param.0 != 0 {
            let msg = w_param.0 as u32;
            let kb = unsafe { &*(l_param.0 as *const KBDLLHOOKSTRUCT) };
            let vk = kb.vkCode;

            match msg {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    if let Some(key_name) = vk_to_key_name(vk) {
                        let (ctrl, alt, shift) = unsafe {
                            (
                                is_vk_down(u32::from(VK_CONTROL.0)),
                                is_vk_down(u32::from(VK_MENU.0)),
                                is_vk_down(u32::from(VK_SHIFT.0)),
                            )
                        };
                        HOOK_STATE.with(|cell| {
                            if let Some(ref mut st) = *cell.borrow_mut() {
                                st.tracker
                                    .prune_released_with(vk, |other_vk| unsafe { is_vk_down(other_vk) });
                                let is_fresh_press = st.tracker.record_press(vk, Instant::now());
                                if is_fresh_press {
                                    let combo = KeyComboSpec {
                                        ctrl,
                                        alt,
                                        shift,
                                        key: key_name.to_string(),
                                    };
                                    // Non-blocking send so the hook never stalls waiting on UI/mutexes.
                                    let _ = st.raw_tx.try_send(combo);
                                }
                            }
                        });
                    }
                }
                WM_KEYUP | WM_SYSKEYUP => {
                    HOOK_STATE.with(|cell| {
                        if let Some(ref mut st) = *cell.borrow_mut() {
                            st.tracker.record_release(vk);
                        }
                    });
                }
                _ => {}
            }
        }
        unsafe { CallNextHookEx(None, n_code, w_param, l_param) }
    }
}

#[cfg(test)]
mod tests {
    use super::vk_to_key_name;

    #[test]
    fn maps_virtual_keys_to_expected_keybind_labels() {
        assert_eq!(vk_to_key_name(0x70), Some("F1"));
        assert_eq!(vk_to_key_name(0x77), Some("F8"));
        assert_eq!(vk_to_key_name(0x41), Some("A"));
        assert_eq!(vk_to_key_name(0x31), Some("1"));
        assert_eq!(vk_to_key_name(0x61), Some("1"));
        assert_eq!(vk_to_key_name(0x6B), Some("+"));
        assert_eq!(vk_to_key_name(0x20), Some("Space"));
        assert_eq!(vk_to_key_name(0x11), None); // Bare Ctrl modifier is not a trigger key
    }

    #[cfg(windows)]
    #[test]
    fn pressed_key_tracker_dedups_repeat_and_prunes_missed_keyup() {
        use std::time::{Duration, Instant};

        let mut tracker = super::win32_imp::PressedKeyTracker::new();
        let t0 = Instant::now();
        assert!(tracker.record_press(0x70, t0));
        assert!(!tracker.record_press(0x70, t0 + Duration::from_millis(50)));

        // Simulate missed WM_KEYUP for F1 (0x70) when another key F2 (0x71) is pressed.
        tracker.prune_released_with(0x71, |_| false);
        assert!(tracker.record_press(0x70, t0 + Duration::from_millis(100)));

        tracker.record_release(0x70);
        assert!(tracker.record_press(0x70, t0 + Duration::from_millis(120)));
    }
}

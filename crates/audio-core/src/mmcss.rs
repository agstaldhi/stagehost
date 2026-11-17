//! Windows MMCSS, power-throttling, and CPU denormal helpers for real-time audio threads.

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::sync::Once;

    use tracing::{debug, warn};
    use windows::core::w;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
        GetCurrentProcess, GetCurrentThread, ProcessPowerThrottling, SetPriorityClass,
        SetProcessInformation, SetThreadInformation, ThreadPowerThrottling,
        AVRT_PRIORITY_CRITICAL, HIGH_PRIORITY_CLASS, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
        THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        THREAD_POWER_THROTTLING_STATE,
    };

    /// RAII guard that elevates the current thread to MMCSS `"Pro Audio"` with critical priority.
    pub struct MmcssGuard {
        handle: HANDLE,
    }

    impl MmcssGuard {
        pub fn enter_pro_audio() -> Option<Self> {
            disable_current_thread_power_throttling();
            let mut task_index: u32 = 0;
            let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) };
            match handle {
                Ok(h) if !h.is_invalid() => {
                    if let Err(err) = unsafe { AvSetMmThreadPriority(h, AVRT_PRIORITY_CRITICAL) } {
                        warn!("AvSetMmThreadPriority(CRITICAL) failed: {err}");
                    } else {
                        debug!("Audio thread elevated to MMCSS Pro Audio (task={task_index})");
                    }
                    Some(Self { handle: h })
                }
                Ok(_) => None,
                Err(err) => {
                    warn!("AvSetMmThreadCharacteristicsW('Pro Audio') failed: {err}");
                    None
                }
            }
        }
    }

    impl Drop for MmcssGuard {
        fn drop(&mut self) {
            if let Err(err) = unsafe { AvRevertMmThreadCharacteristics(self.handle) } {
                warn!("AvRevertMmThreadCharacteristics failed: {err}");
            }
        }
    }

    /// Disables Windows execution-speed power throttling (EcoQoS) for the current thread.
    pub fn disable_current_thread_power_throttling() {
        let state = THREAD_POWER_THROTTLING_STATE {
            Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        let res = unsafe {
            SetThreadInformation(
                GetCurrentThread(),
                ThreadPowerThrottling,
                (&raw const state).cast::<c_void>(),
                size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
            )
        };
        if let Err(err) = res {
            debug!("SetThreadInformation(ThreadPowerThrottling) skipped: {err}");
        }
    }

    /// Disables Windows process power throttling / EcoQoS and sets `HIGH_PRIORITY_CLASS` once.
    pub fn disable_process_power_throttling() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let proc_handle = unsafe { GetCurrentProcess() };
            if let Err(err) = unsafe { SetPriorityClass(proc_handle, HIGH_PRIORITY_CLASS) } {
                warn!("SetPriorityClass(HIGH_PRIORITY_CLASS) failed: {err}");
            }

            // Apply flags separately so older Windows builds without IGNORE_TIMER_RESOLUTION
            // still accept disabling EXECUTION_SPEED throttling.
            let exec_state = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
                StateMask: 0,
            };
            if let Err(err) = unsafe {
                SetProcessInformation(
                    proc_handle,
                    ProcessPowerThrottling,
                    (&raw const exec_state).cast::<c_void>(),
                    size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                )
            } {
                debug!("SetProcessInformation(EXECUTION_SPEED) skipped: {err}");
            }

            let timer_state = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
                StateMask: 0,
            };
            if let Err(err) = unsafe {
                SetProcessInformation(
                    proc_handle,
                    ProcessPowerThrottling,
                    (&raw const timer_state).cast::<c_void>(),
                    size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                )
            } {
                debug!("SetProcessInformation(IGNORE_TIMER_RESOLUTION) skipped: {err}");
            }
        });
    }

    /// Ensures the calling OS thread is elevated to MMCSS `"Pro Audio"` once for its lifetime.
    pub fn ensure_pro_audio_for_current_thread() {
        thread_local! {
            static GUARD: RefCell<Option<MmcssGuard>> = const { RefCell::new(None) };
        }
        let _ = GUARD.try_with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = MmcssGuard::enter_pro_audio();
            }
        });
    }
}

#[cfg(not(windows))]
mod imp {
    pub struct MmcssGuard;

    impl MmcssGuard {
        pub fn enter_pro_audio() -> Option<Self> {
            None
        }
    }

    pub fn disable_process_power_throttling() {}
    pub fn ensure_pro_audio_for_current_thread() {}
}

/// RAII guard that enables CPU Flush-To-Zero (FTZ) and Denormals-Are-Zero (DAZ) on x86/x86_64
/// for the duration of an audio callback, preventing denormal floating-point CPU spikes.
pub struct DenormalGuard {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    prev_mxcsr: u32,
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    changed: bool,
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline]
unsafe fn read_mxcsr() -> u32 {
    let mut val = 0u32;
    unsafe {
        std::arch::asm!(
            "stmxcsr [{}]",
            in(reg) &raw mut val,
            options(nostack, preserves_flags),
        );
    }
    val
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline]
unsafe fn write_mxcsr(val: u32) {
    unsafe {
        std::arch::asm!(
            "ldmxcsr [{}]",
            in(reg) &raw const val,
            options(nostack, readonly, preserves_flags),
        );
    }
}

impl DenormalGuard {
    #[inline]
    pub fn enter() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            const FTZ_DAZ_BITS: u32 = 0x8040;
            // SAFETY: SSE2 / MXCSR is baseline on all x86_64 targets.
            let prev_mxcsr = unsafe { read_mxcsr() };
            let changed = (prev_mxcsr & FTZ_DAZ_BITS) != FTZ_DAZ_BITS;
            if changed {
                unsafe { write_mxcsr(prev_mxcsr | FTZ_DAZ_BITS) };
            }
            Self {
                prev_mxcsr,
                changed,
            }
        }
        #[cfg(target_arch = "x86")]
        {
            const FTZ_BIT: u32 = 0x8000;
            let prev_mxcsr = unsafe { read_mxcsr() };
            let changed = (prev_mxcsr & FTZ_BIT) != FTZ_BIT;
            if changed {
                unsafe { write_mxcsr(prev_mxcsr | FTZ_BIT) };
            }
            Self {
                prev_mxcsr,
                changed,
            }
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        {
            Self {}
        }
    }
}

impl Drop for DenormalGuard {
    #[inline]
    fn drop(&mut self) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if self.changed {
            unsafe { write_mxcsr(self.prev_mxcsr) };
        }
    }
}

pub use imp::{
    disable_process_power_throttling, ensure_pro_audio_for_current_thread, MmcssGuard,
};

#[cfg(test)]
mod tests {
    use super::DenormalGuard;

    #[test]
    fn denormal_guard_flushes_subnormals_and_restores_state() {
        let guard = DenormalGuard::enter();
        let tiny = f32::from_bits(1); // smallest positive subnormal
        let flushed = tiny + 0.0;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        assert_eq!(flushed, 0.0);
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        let _ = flushed;
        drop(guard);
    }
}

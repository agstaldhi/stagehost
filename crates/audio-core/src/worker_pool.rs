//! Persistent low-latency worker pool for parallel rack DSP execution.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle, Thread};

use crate::mmcss::{ensure_pro_audio_for_current_thread, DenormalGuard};

#[derive(Clone, Copy)]
struct WaveJob {
    ctx: *const (),
    func: fn(*const (), usize),
}

// SAFETY: `WaveJob` pointers are only read by workers while the coordinator thread
// is blocked inside `dispatch_and_wait` waiting for `active_workers == 0`.
unsafe impl Send for WaveJob {}
unsafe impl Sync for WaveJob {}

struct SharedPoolState {
    next_task: AtomicUsize,
    task_count: AtomicUsize,
    active_workers: AtomicUsize,
    job: UnsafeCell<Option<WaveJob>>,
}

// SAFETY: Access to `job` is strictly synchronized by `active_workers` and `dispatch_gen`
// acquire/release fences.
unsafe impl Sync for SharedPoolState {}

struct WorkerControl {
    dispatch_gen: AtomicUsize,
    shutdown: AtomicBool,
    thread: Thread,
}

struct WorkerEntry {
    ctrl: Arc<WorkerControl>,
    join_handle: Option<JoinHandle<()>>,
}

struct CompletionGuard<'a> {
    active_workers: &'a AtomicUsize,
}

impl Drop for CompletionGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.active_workers.fetch_sub(1, Ordering::Release);
    }
}

/// Persistent real-time worker pool elevated to MMCSS `"Pro Audio"` for parallel racks.
pub(crate) struct RackWorkerPool {
    shared: Arc<SharedPoolState>,
    workers: Vec<WorkerEntry>,
}

// SAFETY: All interior pointers in `SharedPoolState` are cleared before `dispatch_and_wait` returns.
unsafe impl Send for RackWorkerPool {}

impl RackWorkerPool {
    pub(crate) fn desired_worker_count() -> usize {
        thread::available_parallelism()
            .map_or(1, |n| n.get().saturating_sub(1))
            .clamp(1, 7)
    }

    pub(crate) fn new() -> Self {
        let count = Self::desired_worker_count();
        let shared = Arc::new(SharedPoolState {
            next_task: AtomicUsize::new(0),
            task_count: AtomicUsize::new(0),
            active_workers: AtomicUsize::new(0),
            job: UnsafeCell::new(None),
        });

        let mut workers = Vec::with_capacity(count);
        for idx in 0..count {
            let shared_clone = Arc::clone(&shared);
            let (ctrl_tx, ctrl_rx) = std::sync::mpsc::channel::<Arc<WorkerControl>>();
            let builder = thread::Builder::new().name(format!("stagehost-rack-{idx}"));
            match builder.spawn(move || {
                let ctrl = Arc::new(WorkerControl {
                    dispatch_gen: AtomicUsize::new(0),
                    shutdown: AtomicBool::new(false),
                    thread: thread::current(),
                });
                let _ = ctrl_tx.send(Arc::clone(&ctrl));
                worker_loop(&shared_clone, &ctrl);
            }) {
                Ok(join_handle) => {
                    if let Ok(ctrl) = ctrl_rx.recv() {
                        workers.push(WorkerEntry {
                            ctrl,
                            join_handle: Some(join_handle),
                        });
                    } else {
                        let _ = join_handle.join();
                    }
                }
                Err(_) => break,
            }
        }

        Self { shared, workers }
    }

    /// Dispatches `task_count` independent work items across the audio thread and worker pool,
    /// blocking until every item completes.
    pub(crate) fn dispatch_and_wait(
        &mut self,
        task_count: usize,
        ctx: *const (),
        func: fn(*const (), usize),
    ) {
        if task_count == 0 {
            return;
        }
        if task_count == 1 || self.workers.is_empty() {
            for idx in 0..task_count {
                func(ctx, idx);
            }
            return;
        }

        let num_workers = (task_count - 1).min(self.workers.len());
        // SAFETY: `active_workers` is currently 0, so no worker is reading `job`.
        unsafe {
            *self.shared.job.get() = Some(WaveJob { ctx, func });
        }
        self.shared.task_count.store(task_count, Ordering::Relaxed);
        self.shared.next_task.store(0, Ordering::Release);
        self.shared
            .active_workers
            .store(num_workers, Ordering::Release);

        for worker in self.workers.iter().take(num_workers) {
            worker.ctrl.dispatch_gen.fetch_add(1, Ordering::Release);
            worker.ctrl.thread.unpark();
        }

        execute_claimed_tasks(&self.shared);

        let mut spins = 0u32;
        while self.shared.active_workers.load(Ordering::Acquire) > 0 {
            if spins < 4096 {
                std::hint::spin_loop();
                spins += 1;
            } else {
                thread::yield_now();
            }
        }

        // SAFETY: All workers have decremented `active_workers` to 0 and stopped reading `job`.
        unsafe {
            *self.shared.job.get() = None;
        }
    }
}

impl Drop for RackWorkerPool {
    fn drop(&mut self) {
        for worker in &self.workers {
            worker.ctrl.shutdown.store(true, Ordering::Release);
            worker.ctrl.dispatch_gen.fetch_add(1, Ordering::Release);
            worker.ctrl.thread.unpark();
        }
        for worker in &mut self.workers {
            if let Some(handle) = worker.join_handle.take() {
                let _ = handle.join();
            }
        }
    }
}

#[inline]
fn execute_claimed_tasks(shared: &SharedPoolState) {
    let total = shared.task_count.load(Ordering::Relaxed);
    // SAFETY: `job` was initialized with a Release fence on `dispatch_gen` before this runs
    // and remains valid until `active_workers` reaches 0.
    let Some(job) = (unsafe { *shared.job.get() }) else {
        return;
    };
    loop {
        let idx = shared.next_task.fetch_add(1, Ordering::Relaxed);
        if idx >= total {
            break;
        }
        (job.func)(job.ctx, idx);
    }
}

fn worker_loop(shared: &SharedPoolState, ctrl: &WorkerControl) {
    ensure_pro_audio_for_current_thread();
    let mut seen_gen = 0usize;

    loop {
        let current_gen = ctrl.dispatch_gen.load(Ordering::Acquire);
        if current_gen != seen_gen {
            seen_gen = current_gen;
            if ctrl.shutdown.load(Ordering::Acquire) {
                break;
            }
            {
                let _completion = CompletionGuard {
                    active_workers: &shared.active_workers,
                };
                let _denormal = DenormalGuard::enter();
                execute_claimed_tasks(shared);
            }
            continue;
        }

        if ctrl.shutdown.load(Ordering::Acquire) {
            break;
        }
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn worker_pool_executes_all_tasks_once() {
        let mut pool = RackWorkerPool::new();
        let counters: Vec<AtomicUsize> = (0..12).map(|_| AtomicUsize::new(0)).collect();

        fn run(ctx: *const (), idx: usize) {
            let slice = unsafe { &*(ctx as *const Vec<AtomicUsize>) };
            slice[idx].fetch_add(1, Ordering::Relaxed);
        }

        pool.dispatch_and_wait(
            counters.len(),
            (&raw const counters).cast::<()>(),
            run,
        );
        for c in &counters {
            assert_eq!(c.load(Ordering::Relaxed), 1);
        }
    }
}

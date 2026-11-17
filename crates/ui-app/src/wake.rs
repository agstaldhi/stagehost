//! Thread-safe wake signal bridging tray/hotkey/scanner threads with both active
//! `egui::Context` frames and the sleeping system-tray wait loop.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;

#[derive(Clone)]
pub struct UiWakeSignal {
    ctx: Arc<Mutex<Option<egui::Context>>>,
    tx: SyncSender<()>,
}

pub struct UiWakeReceiver {
    rx: Receiver<()>,
}

impl UiWakeSignal {
    pub fn channel() -> (Self, UiWakeReceiver) {
        let (tx, rx) = mpsc::sync_channel(1);
        (
            Self {
                ctx: Arc::new(Mutex::new(None)),
                tx,
            },
            UiWakeReceiver { rx },
        )
    }

    pub fn set_egui_ctx(&self, new_ctx: Option<egui::Context>) {
        if let Ok(mut guard) = self.ctx.lock() {
            *guard = new_ctx;
        }
    }

    pub fn wake(&self) {
        if let Ok(guard) = self.ctx.lock() {
            if let Some(ref ctx) = *guard {
                ctx.request_repaint();
            }
        }
        let _ = self.tx.try_send(());
    }
}

impl UiWakeReceiver {
    pub fn wait_timeout(&self, timeout: Duration) {
        match self.rx.recv_timeout(timeout) {
            Ok(()) => {
                // Drain any coalesced extra wake tokens so the next wait blocks cleanly.
                while self.rx.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {}
        }
    }

    pub fn drain(&self) {
        while self.rx.try_recv().is_ok() {}
    }
}

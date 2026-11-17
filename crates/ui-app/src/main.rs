#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod autostart;
mod file_dialog;
mod global_hotkey;
mod icon;
mod mixer_view;
mod panels;
mod plugin_window;
mod theme;
mod tray;
mod wake;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;

use eframe::egui;

struct StageHostWindowAdapter(Rc<RefCell<Option<app::StageHostApp>>>);

impl eframe::App for StageHostWindowAdapter {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(app_state) = self.0.borrow_mut().as_mut() {
            app_state.ui(ui, frame);
        }
    }
}

#[cfg(windows)]
fn trim_windows_working_set() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32EmptyWorkingSet(hprocess: isize) -> i32;
        fn SetProcessWorkingSetSizeEx(
            hprocess: isize,
            dwminimumworkingsetsize: usize,
            dwmaximumworkingsetsize: usize,
            flags: u32,
        ) -> i32;
    }
    unsafe {
        let proc = GetCurrentProcess();
        let trimmed = K32EmptyWorkingSet(proc);
        if trimmed == 0 {
            let _ = SetProcessWorkingSetSizeEx(proc, usize::MAX, usize::MAX, 0);
        }
    }
}

#[cfg(not(windows))]
fn trim_windows_working_set() {}

fn build_native_options(app_icon: Option<&Arc<egui::IconData>>) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("StageHost RS - Live Audio & Plugin Host")
        .with_inner_size([1280.0, 760.0])
        .with_min_inner_size([920.0, 560.0]);
    if let Some(icon_arc) = app_icon {
        viewport = viewport.with_icon(Arc::clone(icon_arc));
    }

    eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        run_and_return: true,
        depth_buffer: 0,
        stencil_buffer: 0,
        multisampling: 0,
        ..Default::default()
    }
}

fn should_start_in_tray_for_args(args: &[String], autostart_to_tray_pref: bool) -> bool {
    if args.iter().any(|arg| arg == "--tray" || arg == "--minimized") {
        return true;
    }
    args.iter().any(|arg| arg == "--autostart") && autostart_to_tray_pref
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    // Crash-isolated worker mode spawned by `plugin_host::scan_plugins_safe`.
    if args.len() >= 3 && args[1] == "--scan-plugin" {
        return run_isolated_plugin_scan(&PathBuf::from(&args[2]));
    }

    // When launched via Windows Startup, CWD defaults to System32; keep paths relative to the app folder.
    if args.iter().any(|arg| arg == "--autostart") {
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent() {
                let _ = std::env::set_current_dir(parent);
            }
        }
    }

    audio_core::disable_process_power_throttling();

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let start_in_tray = should_start_in_tray_for_args(&args, app::should_autostart_to_tray());
    let app_icon = icon::load_window_icon().map(Arc::new);
    let shared_app: Rc<RefCell<Option<app::StageHostApp>>> = Rc::new(RefCell::new(None));

    if start_in_tray {
        let mut headless_app = app::StageHostApp::new_headless(true);
        if headless_app.has_system_tray() {
            trim_windows_working_set();
            if !headless_app.wait_in_system_tray() {
                drop(headless_app);
                return ExitCode::SUCCESS;
            }
            *shared_app.borrow_mut() = Some(headless_app);
        } else {
            // Fall back to visible UI if the tray window could not be created.
            headless_app.hidden_to_tray = false;
            *shared_app.borrow_mut() = Some(headless_app);
        }
    }

    loop {
        let native_options = build_native_options(app_icon.as_ref());
        let shared_for_creator = Rc::clone(&shared_app);

        let run_result = eframe::run_native(
            "StageHost RS",
            native_options,
            Box::new(move |cc| {
                let needs_init = shared_for_creator.borrow().is_none();
                if needs_init {
                    let initialized = app::StageHostApp::new(cc);
                    *shared_for_creator.borrow_mut() = Some(initialized);
                } else if let Some(app_state) = shared_for_creator.borrow_mut().as_mut() {
                    theme::configure_studio_theme(&cc.egui_ctx);
                    app_state.attach_ui_context(cc.egui_ctx.clone());
                }
                Ok(Box::new(StageHostWindowAdapter(Rc::clone(&shared_for_creator))))
            }),
        );

        if let Err(err) = run_result {
            eprintln!("StageHost UI startup failed: {err}");
            return ExitCode::FAILURE;
        }

        let Some(mut app_state) = shared_app.borrow_mut().take() else {
            break;
        };

        if app_state.force_exit || !app_state.hidden_to_tray {
            drop(app_state);
            break;
        }

        // Release OpenGL/egui allocations and trim cold working-set pages while idling in tray.
        trim_windows_working_set();
        if !app_state.wait_in_system_tray() {
            drop(app_state);
            break;
        }
        *shared_app.borrow_mut() = Some(app_state);
    }

    ExitCode::SUCCESS
}

fn run_isolated_plugin_scan(path: &Path) -> ExitCode {
    #[cfg(windows)]
    struct ComStaGuard(bool);
    #[cfg(windows)]
    impl Drop for ComStaGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe {
                    windows::Win32::System::Com::CoUninitialize();
                }
            }
        }
    }

    #[cfg(windows)]
    let _com_guard = {
        let hr = unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
        };
        ComStaGuard(hr.is_ok())
    };

    match plugin_host::scan_plugin_file_in_process(path) {
        Ok(descriptors) => match serde_json::to_string(&descriptors) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("Failed to serialize plugin descriptors: {err}");
                ExitCode::FAILURE
            }
        },
        Err(err) => {
            eprintln!("Plugin scan failed for {}: {err}", path.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::should_start_in_tray_for_args;

    #[test]
    fn startup_tray_flag_respects_cli_and_preference() {
        let normal = vec!["stagehost.exe".to_string()];
        assert!(!should_start_in_tray_for_args(&normal, true));
        assert!(!should_start_in_tray_for_args(&normal, false));

        let autostart = vec!["stagehost.exe".to_string(), "--autostart".to_string()];
        assert!(should_start_in_tray_for_args(&autostart, true));
        assert!(!should_start_in_tray_for_args(&autostart, false));

        let explicit_tray = vec!["stagehost.exe".to_string(), "--tray".to_string()];
        assert!(should_start_in_tray_for_args(&explicit_tray, false));

        let explicit_min = vec!["stagehost.exe".to_string(), "--minimized".to_string()];
        assert!(should_start_in_tray_for_args(&explicit_min, false));
    }
}

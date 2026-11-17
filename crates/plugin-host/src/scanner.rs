use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tracing::warn;

use crate::builtin::builtin_descriptors;
use crate::clap_host;
use crate::vst2_host::{self, Vst2DllCheck};
use crate::vst3_host;
use crate::{PluginDescriptor, PluginError};

const SCAN_WORKER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginScanIssue {
    pub path: PathBuf,
    pub reason: String,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PluginScanReport {
    pub descriptors: Vec<PluginDescriptor>,
    pub issues: Vec<PluginScanIssue>,
    pub candidate_files: usize,
    pub succeeded_files: usize,
}

/// Returns standard Windows search paths for VST3, VST2, and CLAP plugins.
pub fn default_plugin_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(common_files) = std::env::var("CommonProgramFiles") {
        paths.push(PathBuf::from(&common_files).join("VST3"));
        paths.push(PathBuf::from(&common_files).join("CLAP"));
        let common_vst2 = PathBuf::from(&common_files).join("VST2");
        if common_vst2.is_dir() {
            paths.push(common_vst2);
        }
    }
    if let Ok(program_files) = std::env::var("ProgramFiles") {
        for sub in ["VSTPlugins", "Steinberg\\VSTPlugins"] {
            let dir = PathBuf::from(&program_files).join(sub);
            if dir.is_dir() {
                paths.push(dir);
            }
        }
    }
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        paths.push(
            PathBuf::from(local_app_data)
                .join("Programs")
                .join("Common")
                .join("CLAP"),
        );
    }
    paths
}

/// Scans built-in plugins plus external `.clap`, `.vst3`, and 64-bit VST2 `.dll` files
/// using timeout-guarded out-of-process isolation.
pub fn scan_plugins_safe(search_dirs: &[PathBuf]) -> Vec<PluginDescriptor> {
    scan_plugins_detailed(search_dirs).descriptors
}

/// Scans enabled plugin folders and returns indexed descriptors together with any per-file failures.
pub fn scan_plugins_detailed(search_dirs: &[PathBuf]) -> PluginScanReport {
    let mut report = PluginScanReport {
        descriptors: builtin_descriptors(),
        issues: Vec::new(),
        candidate_files: 0,
        succeeded_files: 0,
    };
    let mut seen_paths = HashSet::new();
    let mut candidates = Vec::new();
    for dir in search_dirs {
        collect_plugin_files(dir, &mut seen_paths, &mut candidates, &mut report.issues);
    }
    report.candidate_files = candidates.len();
    if candidates.is_empty() {
        return report;
    }

    let worker_count = std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 4))
        .unwrap_or(2)
        .min(candidates.len());

    let next_idx = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::with_capacity(candidates.len()));

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| loop {
                let idx = next_idx.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(path) = candidates.get(idx) else {
                    break;
                };
                let res = scan_in_child_process(path);
                if let Ok(mut guard) = results.lock() {
                    guard.push((path.clone(), res));
                }
            });
        }
    });

    if let Ok(list) = results.into_inner() {
        for (path, res) in list {
            match res {
                Ok(mut found) => {
                    if found.is_empty() {
                        report.issues.push(PluginScanIssue {
                            path,
                            reason: "Plugin binary exposed 0 audio plugin descriptors".to_string(),
                            timed_out: false,
                        });
                    } else {
                        report.succeeded_files += 1;
                        report.descriptors.append(&mut found);
                    }
                }
                Err(issue) => {
                    warn!("Skipping plugin '{}': {}", issue.path.display(), issue.reason);
                    report.issues.push(issue);
                }
            }
        }
    }
    report
}

/// Directly inspects a single `.clap`, `.vst3`, or 64-bit VST2 `.dll` binary in the current process.
/// Intended to be invoked by the isolated `--scan-plugin <path>` child worker.
pub fn scan_plugin_file_in_process(path: &Path) -> Result<Vec<PluginDescriptor>, PluginError> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "clap" => clap_host::scan_clap_file(path),
        "vst3" => vst3_host::scan_vst3_path(path),
        "dll" => vst2_host::scan_vst2_file(path),
        _ => Err(PluginError::ScanFailed {
            path: path.to_path_buf(),
            detail: format!("unsupported plugin extension '.{ext}'"),
        }),
    }
}

fn scan_in_child_process(path: &Path) -> Result<Vec<PluginDescriptor>, PluginScanIssue> {
    let exe = std::env::current_exe().map_err(|e| PluginScanIssue {
        path: path.to_path_buf(),
        reason: format!("failed to resolve current_exe: {e}"),
        timed_out: false,
    })?;

    let mut child = Command::new(exe)
        .arg("--scan-plugin")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PluginScanIssue {
            path: path.to_path_buf(),
            reason: format!("failed to spawn scanner worker: {e}"),
            timed_out: false,
        })?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || read_pipe_string(stdout_pipe));
    let stderr_thread = std::thread::spawn(move || read_pipe_string(stderr_pipe));

    let started = Instant::now();
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= SCAN_WORKER_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(PluginScanIssue {
                    path: path.to_path_buf(),
                    reason: format!("scanner worker wait error: {err}"),
                    timed_out: false,
                });
            }
        }
    };

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();

    let Some(status) = exit_status else {
        return Err(PluginScanIssue {
            path: path.to_path_buf(),
            reason: format!(
                "timed out after {}s while initializing plugin",
                SCAN_WORKER_TIMEOUT.as_secs()
            ),
            timed_out: true,
        });
    };

    if !status.success() {
        let err_detail = stderr.trim();
        let reason = if err_detail.is_empty() {
            format!("scanner worker exited with {status}")
        } else {
            err_detail.to_string()
        };
        return Err(PluginScanIssue {
            path: path.to_path_buf(),
            reason,
            timed_out: false,
        });
    }

    serde_json::from_str::<Vec<PluginDescriptor>>(stdout.trim()).map_err(|e| PluginScanIssue {
        path: path.to_path_buf(),
        reason: format!("invalid JSON from scanner worker: {e}"),
        timed_out: false,
    })
}

fn read_pipe_string(pipe: Option<impl Read>) -> String {
    let Some(mut reader) = pipe else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn collect_plugin_files(
    dir: &Path,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
    issues: &mut Vec<PluginScanIssue>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());

        if matches!(ext.as_deref(), Some("clap") | Some("vst3")) {
            // Note: on Windows a .vst3 can be either a DLL file or a bundle directory.
            if seen.insert(path.clone()) {
                out.push(path);
            }
        } else if ext.as_deref() == Some("dll") && path.is_file() {
            match vst2_host::inspect_dll_vst2_candidate(&path) {
                Ok(Vst2DllCheck::Supported64Bit) => {
                    if seen.insert(path.clone()) {
                        out.push(path);
                    }
                }
                Ok(Vst2DllCheck::Unsupported32Bit) => {
                    if seen.insert(path.clone()) {
                        issues.push(PluginScanIssue {
                            path,
                            reason: "32-bit x86 VST2 DLL not supported in 64-bit host".to_string(),
                            timed_out: false,
                        });
                    }
                }
                Ok(Vst2DllCheck::NotVst2Plugin) | Err(_) => {
                    // Companion/support library DLL without VSTPluginMain/main entry point.
                }
            }
        } else if path.is_dir() {
            collect_plugin_files(&path, seen, out, issues);
        }
    }
}

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use audio_core::{
    AudioDeviceInfo, AudioEngine, AudioTelemetry, DriverNotes, EngineCommand, LiveRack,
    LiveSessionParams, LiveSlot, StageTuner,
};
use eframe::egui;
use plugin_host::{
    create_plugin_instance, default_plugin_search_paths, scan_plugins_detailed, PluginDescriptor,
    PluginParamInfo, PluginScanIssue, PluginScanReport, PluginUiHandle,
};
use session_core::{
    decode_base64, default_app_config_dir, encode_base64, load_host_preferences, load_json_file,
    load_session, load_set_list, save_host_preferences, save_json_file, save_session,
    save_set_list, BuiltInEffectKind, HostPreferences, InputChannelMode, KeyBindingAction,
    KeyBindingConfig, KeyComboSpec, PluginFolderEntry, PluginKind, PluginSlotConfig, SessionConfig,
    SessionHistory, SetList, SetListEntry,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use crate::autostart;
use crate::global_hotkey::GlobalHotkeyListener;
use crate::mixer_view;
use crate::panels;
use crate::plugin_window::PluginEditorWindows;
use crate::theme;
use crate::tray::{SystemTrayHandle, TrayAction};
use crate::wake::{UiWakeReceiver, UiWakeSignal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginFilterFormat {
    All,
    BuiltIn,
    Vst3,
    Vst2,
    Clap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginFilterRole {
    All,
    Effects,
    Instruments,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsCategory {
    AudioEngine,
    Plugins,
    General,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindDraftAction {
    ToggleSlotBypass,
    ToggleSlotEnabled,
    OpenSlotEditor,
    SetSlotParam,
    ToggleRackMute,
    ToggleRackSolo,
    ToggleRackEnabled,
    ToggleRackSend,
    ToggleMultiRackMute,
    ToggleMultiRackSolo,
    ToggleMultiRackEnabled,
    LoadSessionFile,
}

impl KeybindDraftAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::ToggleSlotBypass => "Toggle Plugin Bypass",
            Self::ToggleSlotEnabled => "Toggle Plugin Enable",
            Self::OpenSlotEditor => "Open Plugin GUI",
            Self::SetSlotParam => "Set Plugin Parameter",
            Self::ToggleRackMute => "Toggle Rack Mute",
            Self::ToggleRackSolo => "Toggle Rack Solo",
            Self::ToggleRackEnabled => "Toggle Rack Enable",
            Self::ToggleRackSend => "Toggle Rack Send",
            Self::ToggleMultiRackMute => "Toggle Selected Racks Mute",
            Self::ToggleMultiRackSolo => "Toggle Selected Racks Solo",
            Self::ToggleMultiRackEnabled => "Toggle Selected Racks Enable",
            Self::LoadSessionFile => "Switch Session / Settings File",
        }
    }

    pub fn is_slot_action(self) -> bool {
        matches!(
            self,
            Self::ToggleSlotBypass
                | Self::ToggleSlotEnabled
                | Self::OpenSlotEditor
                | Self::SetSlotParam
        )
    }

    pub fn is_send_action(self) -> bool {
        matches!(self, Self::ToggleRackSend)
    }

    pub fn is_multi_rack_action(self) -> bool {
        matches!(
            self,
            Self::ToggleMultiRackMute | Self::ToggleMultiRackSolo | Self::ToggleMultiRackEnabled
        )
    }

    pub fn is_session_file_action(self) -> bool {
        matches!(self, Self::LoadSessionFile)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettingsBundle {
    pub schema_version: u32,
    pub preferences: HostPreferences,
    #[serde(default)]
    pub scanned_plugins: Vec<PluginDescriptor>,
    pub active_session: SessionConfig,
    #[serde(default)]
    pub set_list: SetList,
}

#[derive(Debug, Clone)]
pub enum PendingSessionAction {
    NewSession,
    LoadSession(PathBuf),
    LoadSetListEntry(Uuid),
}

pub struct StageHostApp {
    pub audio_engine: AudioEngine,
    pub session: SessionConfig,
    pub history: SessionHistory,
    pub set_list: SetList,
    pub active_setlist_entry_id: Option<Uuid>,
    pub session_path_input: String,
    pub setlist_path_input: String,

    pub available_audio_devices: Vec<AudioDeviceInfo>,
    pub selected_audio_device: Option<String>,
    pub available_input_devices: Vec<AudioDeviceInfo>,
    pub selected_input_device: Option<String>,
    pub asio_input_channel: usize,
    pub asio_output_channel: usize,
    pub show_driver_notes: bool,
    pub cached_driver_notes: Option<DriverNotes>,
    pub driver_notes_error: Option<String>,
    pub input_enabled: bool,
    pub input_gain_db: f32,
    pub input_mode: InputChannelMode,
    pub output_muted: bool,
    pub test_tone_enabled: bool,
    pub tuner_enabled: bool,
    pub auto_save_on_song_switch: bool,
    pub selected_sample_rate: u32,
    pub selected_buffer_frames: u32,
    pub show_left_pane: bool,
    pub show_io_strip: bool,

    pub plugin_folders: Vec<PluginFolderEntry>,
    pub plugin_search_paths: Vec<PathBuf>,
    pub new_plugin_dir_input: String,
    pub rescan_plugins_on_startup: bool,
    pub start_output_muted: bool,
    pub reopen_last_session: bool,
    pub close_to_tray: bool,
    pub start_in_tray_on_autostart: bool,
    pub hidden_to_tray: bool,
    pub(crate) force_exit: bool,
    pub auto_run_on_startup: bool,
    pub auto_run_registered_cmd: Option<String>,
    pub scanned_plugins: Vec<PluginDescriptor>,
    pub last_plugin_scan_issues: Vec<PluginScanIssue>,
    plugin_scan_rx: Option<Receiver<PluginScanReport>>,
    pub plugin_filter_query: String,
    pub plugin_filter_format: PluginFilterFormat,
    pub plugin_filter_role: PluginFilterRole,
    pub plugin_filter_vendor: Option<String>,
    pub selected_browser_plugin_key: Option<String>,
    pub focus_plugin_search_box: bool,

    pub telemetry: AudioTelemetry,
    pub rack_meters: HashMap<Uuid, (f32, f32)>,
    pub editor_windows: PluginEditorWindows,
    pub slot_ui_handles: HashMap<Uuid, Box<dyn PluginUiHandle>>,
    pub slot_latencies: HashMap<Uuid, u32>,
    pub slot_sidechain_caps: HashMap<Uuid, bool>,

    pub show_settings: bool,
    pub settings_category: SettingsCategory,
    pub selected_plugin_folder_idx: Option<usize>,
    pub show_custom_plugin_path_input: bool,
    pub show_keybindings: bool,
    pub keybind_learn_target_id: Option<Uuid>,
    pub keybind_learn_draft: bool,
    pub keybind_draft_rack_id: Option<Uuid>,
    pub keybind_draft_rack_ids: Vec<Uuid>,
    pub keybind_draft_slot_id: Option<Uuid>,
    pub keybind_draft_send_target_id: Option<Uuid>,
    pub keybind_draft_session_entry_id: Option<Uuid>,
    pub keybind_draft_action: KeybindDraftAction,
    pub keybind_draft_param_id: u32,
    pub keybind_draft_param_value: f32,
    pub keybind_draft_combo: KeyComboSpec,
    pub pending_session_action: Option<PendingSessionAction>,
    pub add_slot_target_rack_id: Option<Uuid>,
    pub selected_rack_id: Option<Uuid>,
    pub selected_rack_ids: Vec<Uuid>,
    pub renaming_rack_id: Option<Uuid>,
    pub cable_drag_source: Option<Uuid>,
    pub rack_drag_source: Option<Uuid>,
    pub slot_drag_source: Option<(Uuid, usize)>,
    pub new_sub_session_name: String,
    pub new_rack_name: String,

    pub slot_param_catalog: HashMap<Uuid, Vec<PluginParamInfo>>,
    pub expanded_param_slots: HashSet<Uuid>,
    pub expanded_sidechain_slots: HashSet<Uuid>,
    pub status_banner: Option<(String, bool)>,
    saved_session_baseline: SessionConfig,
    pub startup_config_path: Option<PathBuf>,
    pub last_saved_session_path: Option<PathBuf>,
    pub last_saved_setlist_path: Option<PathBuf>,
    global_hotkey: Option<GlobalHotkeyListener>,
    system_tray: Option<SystemTrayHandle>,
    recent_keybind_triggers: Vec<(KeyComboSpec, Instant)>,
    tuner: StageTuner,
    ui_ctx: Option<egui::Context>,
    wake_signal: Option<UiWakeSignal>,
    tray_wake_rx: Option<UiWakeReceiver>,
    ui_visible_sent: bool,
    input_capture_sent: bool,
    autosave_pending_since: Option<Instant>,
    pub(crate) browser_catalog_dirty: bool,
    pub(crate) browser_catalog_cache: Vec<panels::PluginBrowserItem>,
    pub(crate) browser_vendors_cache: Vec<(String, usize)>,
}

fn load_startup_preferences() -> (HostPreferences, Option<AppSettingsBundle>, bool) {
    let prefs_file = default_app_config_dir().join("preferences.json");
    let portable_bundle = portable_settings_path()
        .filter(|p| !prefs_file.exists() && p.is_file())
        .and_then(|p| load_json_file::<AppSettingsBundle>(&p).ok());
    let has_saved_prefs = prefs_file.is_file() || portable_bundle.is_some();
    let prefs = if let Some(ref bundle) = portable_bundle {
        bundle.preferences.clone()
    } else {
        load_host_preferences(&prefs_file).unwrap_or_default()
    };
    (prefs, portable_bundle, has_saved_prefs)
}

/// Returns whether launching via Windows Startup (`--autostart`) should stay in the system tray.
pub fn should_autostart_to_tray() -> bool {
    load_startup_preferences().0.start_in_tray_on_autostart
}

impl StageHostApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::new_with_ui(Some(cc), false)
    }

    /// Initializes the full audio/session/tray host without creating an `egui` viewport yet.
    pub fn new_headless(start_in_tray: bool) -> Self {
        Self::new_with_ui(None, start_in_tray)
    }

    fn new_with_ui(cc: Option<&eframe::CreationContext<'_>>, start_in_tray: bool) -> Self {
        if let Some(cc) = cc {
            theme::configure_studio_theme(&cc.egui_ctx);
        }

        let (prefs, portable_bundle, has_saved_prefs) = load_startup_preferences();
        let plugin_folders: Vec<PluginFolderEntry> = if !prefs.plugin_folders.is_empty() {
            prefs.plugin_folders.clone()
        } else if !prefs.plugin_search_paths.is_empty() {
            prefs
                .plugin_search_paths
                .iter()
                .cloned()
                .map(|path| PluginFolderEntry { path, enabled: true })
                .collect()
        } else {
            default_plugin_search_paths()
                .into_iter()
                .map(|path| PluginFolderEntry { path, enabled: true })
                .collect()
        };
        let search_paths: Vec<PathBuf> = plugin_folders
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.path.clone())
            .collect();

        let cache_file = default_app_config_dir().join("plugin_cache.json");
        let cached_plugins = if let Some(ref bundle) = portable_bundle {
            bundle.scanned_plugins.clone()
        } else {
            load_json_file::<Vec<PluginDescriptor>>(&cache_file).unwrap_or_default()
        };
        let auto_run_registered_cmd = autostart::registered_auto_run_command().unwrap_or(None);

        let ui_ctx = cc.map(|c| c.egui_ctx.clone());
        let (wake_signal, tray_wake_rx) = UiWakeSignal::channel();
        wake_signal.set_egui_ctx(ui_ctx.clone());

        let mut app = Self {
            audio_engine: AudioEngine::new(),
            session: SessionConfig::default(),
            history: SessionHistory::default(),
            set_list: SetList::default(),
            active_setlist_entry_id: None,
            session_path_input: prefs
                .last_session_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "session.stagehost.json".to_string()),
            setlist_path_input: prefs
                .last_setlist_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "setlist.stagelist.json".to_string()),
            available_audio_devices: Vec::new(),
            selected_audio_device: prefs.output_device.clone(),
            available_input_devices: Vec::new(),
            selected_input_device: prefs.input_device.clone(),
            asio_input_channel: prefs.asio_input_channel,
            asio_output_channel: prefs.asio_output_channel,
            show_driver_notes: false,
            cached_driver_notes: None,
            driver_notes_error: None,
            input_enabled: prefs.input_enabled,
            input_gain_db: prefs.input_gain_db,
            input_mode: prefs.input_mode,
            output_muted: prefs.start_output_muted,
            test_tone_enabled: false,
            tuner_enabled: prefs.tuner_enabled,
            auto_save_on_song_switch: prefs.auto_save_on_song_switch,
            selected_sample_rate: prefs.sample_rate.max(8_000),
            selected_buffer_frames: prefs.buffer_frames.max(64),
            show_left_pane: prefs.show_left_pane,
            show_io_strip: prefs.show_io_strip,
            selected_plugin_folder_idx: if plugin_folders.is_empty() {
                None
            } else {
                Some(0)
            },
            plugin_folders,
            plugin_search_paths: search_paths,
            new_plugin_dir_input: String::new(),
            rescan_plugins_on_startup: prefs.rescan_plugins_on_startup,
            start_output_muted: prefs.start_output_muted,
            reopen_last_session: prefs.reopen_last_session,
            close_to_tray: prefs.close_to_tray,
            start_in_tray_on_autostart: prefs.start_in_tray_on_autostart,
            hidden_to_tray: start_in_tray,
            force_exit: false,
            auto_run_on_startup: auto_run_registered_cmd.is_some(),
            auto_run_registered_cmd,
            scanned_plugins: cached_plugins,
            last_plugin_scan_issues: Vec::new(),
            plugin_scan_rx: None,
            plugin_filter_query: String::new(),
            plugin_filter_format: PluginFilterFormat::All,
            plugin_filter_role: PluginFilterRole::All,
            plugin_filter_vendor: None,
            selected_browser_plugin_key: None,
            focus_plugin_search_box: false,
            telemetry: AudioTelemetry::default(),
            rack_meters: HashMap::new(),
            editor_windows: PluginEditorWindows::default(),
            slot_ui_handles: HashMap::new(),
            slot_latencies: HashMap::new(),
            slot_sidechain_caps: HashMap::new(),
            show_settings: false,
            settings_category: SettingsCategory::AudioEngine,
            show_custom_plugin_path_input: false,
            show_keybindings: false,
            keybind_learn_target_id: None,
            keybind_learn_draft: false,
            keybind_draft_rack_id: None,
            keybind_draft_rack_ids: Vec::new(),
            keybind_draft_slot_id: None,
            keybind_draft_send_target_id: None,
            keybind_draft_session_entry_id: None,
            keybind_draft_action: KeybindDraftAction::ToggleSlotBypass,
            keybind_draft_param_id: 0,
            keybind_draft_param_value: 0.5,
            keybind_draft_combo: KeyComboSpec {
                ctrl: false,
                alt: false,
                shift: false,
                key: String::new(),
            },
            pending_session_action: None,
            add_slot_target_rack_id: None,
            selected_rack_id: prefs.selected_rack_id,
            selected_rack_ids: prefs.selected_rack_id.into_iter().collect(),
            renaming_rack_id: None,
            cable_drag_source: None,
            rack_drag_source: None,
            slot_drag_source: None,
            new_sub_session_name: String::new(),
            new_rack_name: String::new(),
            slot_param_catalog: HashMap::new(),
            expanded_param_slots: HashSet::new(),
            expanded_sidechain_slots: HashSet::new(),
            status_banner: None,
            saved_session_baseline: SessionConfig::default(),
            startup_config_path: prefs.startup_config_path.clone(),
            last_saved_session_path: prefs.last_session_path.clone(),
            last_saved_setlist_path: prefs.last_setlist_path.clone(),
            global_hotkey: GlobalHotkeyListener::start(wake_signal.clone()),
            system_tray: SystemTrayHandle::start(wake_signal.clone()),
            recent_keybind_triggers: Vec::new(),
            tuner: StageTuner::new(prefs.sample_rate.max(8_000) as f64),
            ui_ctx,
            wake_signal: Some(wake_signal),
            tray_wake_rx: Some(tray_wake_rx),
            ui_visible_sent: !start_in_tray,
            input_capture_sent: true,
            autosave_pending_since: None,
            browser_catalog_dirty: true,
            browser_catalog_cache: Vec::new(),
            browser_vendors_cache: Vec::new(),
        };

        if let Some(bundle) = portable_bundle {
            app.set_list = bundle.set_list;
            app.session = bundle.active_session;
        } else {
            // Always load persisted Session List entries regardless of `reopen_last_session`,
            // preferring the newer autosave list over an older manual list path.
            let autosave_sl = default_app_config_dir().join("autosave_setlist.stagelist.json");
            let use_autosave_sl =
                is_first_path_newer_or_only(&autosave_sl, prefs.last_setlist_path.as_ref());
            let mut loaded_setlist = false;
            if use_autosave_sl && autosave_sl.is_file() {
                if let Ok(loaded) = load_set_list(&autosave_sl) {
                    app.set_list = loaded;
                    loaded_setlist = true;
                }
            }
            if !loaded_setlist {
                if let Some(ref sl_path) = prefs.last_setlist_path {
                    if sl_path.is_file() {
                        if let Ok(loaded) = load_set_list(sl_path) {
                            app.set_list = loaded;
                            loaded_setlist = true;
                        }
                    }
                }
            }
            if !loaded_setlist && autosave_sl.is_file() {
                if let Ok(loaded) = load_set_list(&autosave_sl) {
                    app.set_list = loaded;
                }
            }

            let autosave_sess = default_app_config_dir().join("autosave_session.stagehost.json");
            let loaded_startup_override = app.load_startup_config_from_preferences(&autosave_sess);

            if !loaded_startup_override {
                if prefs.reopen_last_session {
                    let use_autosave_over_last = is_first_path_newer_or_only(
                        &autosave_sess,
                        prefs.last_session_path.as_ref(),
                    );

                    let mut loaded_session = false;
                    if use_autosave_over_last && autosave_sess.is_file() {
                        if let Ok(loaded) = load_session(&autosave_sess) {
                            app.session = loaded;
                            loaded_session = true;
                        }
                    }
                    if !loaded_session {
                        if let Some(ref s_path) = prefs.last_session_path {
                            if s_path.is_file() {
                                if let Ok(loaded) = load_session(s_path) {
                                    app.session = loaded;
                                    loaded_session = true;
                                } else if let Ok(bundle) = load_json_file::<AppSettingsBundle>(s_path) {
                                    app.session = bundle.active_session;
                                    loaded_session = true;
                                }
                            }
                        }
                    }
                    if !loaded_session && autosave_sess.is_file() {
                        if let Ok(loaded) = load_session(&autosave_sess) {
                            app.session = loaded;
                        }
                    }
                } else if autosave_sess.is_file() {
                    if let Ok(saved_sess) = load_session(&autosave_sess) {
                        let switch_binds: Vec<KeyBindingConfig> = saved_sess
                            .key_bindings
                            .into_iter()
                            .filter(|b| matches!(b.action, KeyBindingAction::LoadSessionFile { .. }))
                            .collect();
                        app.preserve_session_switch_key_bindings(switch_binds);
                    }
                }
            }
        }

        app.prune_missing_session_list_entries();
        app.normalize_session_list_titles();
        if let Some(matched_id) = app
            .set_list
            .entries
            .iter()
            .find(|e| {
                app.last_saved_session_path
                    .as_ref()
                    .is_some_and(|p| &e.session_path == p)
            })
            .map(|e| e.id)
        {
            app.active_setlist_entry_id = Some(matched_id);
        } else if let Some(ref last_path) = prefs.last_session_path {
            if app.startup_config_path.is_none() {
                app.session_path_input = last_path.display().to_string();
                if let Some(matched) = app
                    .set_list
                    .entries
                    .iter()
                    .find(|e| &e.session_path == last_path)
                {
                    app.active_setlist_entry_id = Some(matched.id);
                }
            }
        }
        app.saved_session_baseline = app.session.clone();
        app.refresh_devices_with_defaults(!has_saved_prefs);
        app.start_audio();
        if app.rescan_plugins_on_startup {
            app.start_plugin_scan();
        }
        if start_in_tray {
            app.shrink_ui_caches_for_tray();
        }
        app
    }

    pub fn is_session_dirty(&self) -> bool {
        self.session != self.saved_session_baseline
    }

    pub fn build_host_preferences(&self) -> HostPreferences {
        let enabled_paths: Vec<PathBuf> = self
            .plugin_folders
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.path.clone())
            .collect();
        HostPreferences {
            schema_version: 1,
            output_device: self.selected_audio_device.clone(),
            input_device: self.selected_input_device.clone(),
            asio_input_channel: self.asio_input_channel,
            asio_output_channel: self.asio_output_channel,
            input_enabled: self.input_enabled,
            input_gain_db: self.input_gain_db,
            input_mode: self.input_mode,
            sample_rate: self.selected_sample_rate,
            buffer_frames: self.selected_buffer_frames,
            plugin_search_paths: enabled_paths,
            plugin_folders: self.plugin_folders.clone(),
            rescan_plugins_on_startup: self.rescan_plugins_on_startup,
            start_output_muted: self.start_output_muted,
            reopen_last_session: self.reopen_last_session,
            close_to_tray: self.close_to_tray,
            start_in_tray_on_autostart: self.start_in_tray_on_autostart,
            show_left_pane: self.show_left_pane,
            show_io_strip: self.show_io_strip,
            auto_save_on_song_switch: self.auto_save_on_song_switch,
            tuner_enabled: self.tuner_enabled,
            selected_rack_id: self.selected_rack_id,
            startup_config_path: self.startup_config_path.clone(),
            last_session_path: self.last_saved_session_path.clone(),
            last_setlist_path: self.last_saved_setlist_path.clone(),
        }
    }

    pub fn set_startup_config_path(&mut self, path: Option<PathBuf>) {
        self.startup_config_path = path;
        if !cfg!(test) {
            self.save_preferences();
        }
    }

    pub fn load_startup_config_from_preferences(&mut self, autosave_sess_path: &Path) -> bool {
        let Some(startup_path) = self.startup_config_path.clone() else {
            return false;
        };
        if !startup_path.is_file() {
            warn!(
                "Configured startup config file does not exist: {}",
                startup_path.display()
            );
            return false;
        }

        let loaded_session = if is_settings_bundle_path(&startup_path) {
            load_json_file::<AppSettingsBundle>(&startup_path)
                .ok()
                .map(|b| b.active_session)
                .or_else(|| load_session(&startup_path).ok())
        } else {
            load_session(&startup_path).ok().or_else(|| {
                load_json_file::<AppSettingsBundle>(&startup_path)
                    .ok()
                    .map(|b| b.active_session)
            })
        };

        let Some(sess) = loaded_session else {
            warn!(
                "Failed to parse startup config file: {}",
                startup_path.display()
            );
            return false;
        };

        self.session = sess;
        if self
            .selected_rack_id
            .is_none_or(|rid| !self.session.racks.iter().any(|r| r.id == rid))
        {
            self.selected_rack_id = self.session.racks.first().map(|r| r.id);
        }
        self.selected_rack_ids = self.selected_rack_id.into_iter().collect();
        self.session_path_input = startup_path.display().to_string();
        self.last_saved_session_path = Some(startup_path.clone());

        if autosave_sess_path.is_file() {
            if let Ok(saved_sess) = load_session(autosave_sess_path) {
                let switch_binds: Vec<KeyBindingConfig> = saved_sess
                    .key_bindings
                    .into_iter()
                    .filter(|b| matches!(b.action, KeyBindingAction::LoadSessionFile { .. }))
                    .collect();
                self.preserve_session_switch_key_bindings(switch_binds);
            }
        }
        true
    }

    pub fn save_preferences(&self) {
        let prefs = self.build_host_preferences();
        let path = default_app_config_dir().join("preferences.json");
        if let Err(err) = save_host_preferences(&path, &prefs) {
            warn!("Failed to save host preferences: {err}");
        }
    }

    pub fn save_autosave_workspace(&self) {
        let cfg_dir = default_app_config_dir();
        if let Err(err) = save_session(&cfg_dir.join("autosave_session.stagehost.json"), &self.session) {
            warn!("Failed to autosave session: {err}");
        }
        if let Err(err) = save_set_list(&cfg_dir.join("autosave_setlist.stagelist.json"), &self.set_list) {
            warn!("Failed to autosave set list: {err}");
        }
    }

    pub fn export_all_settings_file(&mut self, path: &Path) {
        self.capture_live_plugin_states();
        let bundle = AppSettingsBundle {
            schema_version: 1,
            preferences: self.build_host_preferences(),
            scanned_plugins: self.scanned_plugins.clone(),
            active_session: self.session.clone(),
            set_list: self.set_list.clone(),
        };
        match save_json_file(path, &bundle) {
            Ok(()) => {
                self.save_preferences();
                self.save_autosave_workspace();
                self.set_status(format!("Saved all app settings to {}", path.display()), false);
            }
            Err(err) => self.set_status(format!("Failed to save settings file: {err}"), true),
        }
    }

    pub fn import_all_settings_file(&mut self, path: &Path) {
        let bundle = match load_json_file::<AppSettingsBundle>(path) {
            Ok(b) => b,
            Err(err) => {
                self.set_status(format!("Failed to load settings file: {err}"), true);
                return;
            }
        };
        self.apply_settings_bundle(bundle, false, true);
        self.set_status(format!("Loaded all app settings from {}", path.display()), false);
    }

    pub fn apply_settings_bundle(
        &mut self,
        bundle: AppSettingsBundle,
        preserve_session_list: bool,
        apply_global_preferences: bool,
    ) {
        let preserved_switch_binds = self.take_session_switch_key_bindings();
        let p = &bundle.preferences;
        let audio_cfg_changed = apply_global_preferences
            && (self.selected_audio_device != p.output_device
                || self.selected_input_device != p.input_device
                || self.asio_input_channel != p.asio_input_channel
                || self.asio_output_channel != p.asio_output_channel
                || self.selected_sample_rate != p.sample_rate.max(8_000)
                || self.selected_buffer_frames != p.buffer_frames.max(64));

        if apply_global_preferences {
            self.selected_audio_device = p.output_device.clone();
            self.selected_input_device = p.input_device.clone();
            self.asio_input_channel = p.asio_input_channel;
            self.asio_output_channel = p.asio_output_channel;
            self.input_enabled = p.input_enabled;
            self.input_gain_db = p.input_gain_db;
            self.input_mode = p.input_mode;
            self.selected_sample_rate = p.sample_rate.max(8_000);
            self.selected_buffer_frames = p.buffer_frames.max(64);
            self.plugin_folders = if !p.plugin_folders.is_empty() {
                p.plugin_folders.clone()
            } else {
                p.plugin_search_paths
                    .iter()
                    .cloned()
                    .map(|path| PluginFolderEntry { path, enabled: true })
                    .collect()
            };
            self.plugin_search_paths = self
                .plugin_folders
                .iter()
                .filter(|e| e.enabled)
                .map(|e| e.path.clone())
                .collect();
            self.rescan_plugins_on_startup = p.rescan_plugins_on_startup;
            self.start_output_muted = p.start_output_muted;
            self.reopen_last_session = p.reopen_last_session;
            self.close_to_tray = p.close_to_tray;
            self.start_in_tray_on_autostart = p.start_in_tray_on_autostart;
            self.show_left_pane = p.show_left_pane;
            self.show_io_strip = p.show_io_strip;
            self.auto_save_on_song_switch = p.auto_save_on_song_switch;
            self.tuner_enabled = p.tuner_enabled;
            self.selected_rack_id = p.selected_rack_id;
            self.startup_config_path = p.startup_config_path.clone();
            self.last_saved_session_path = p.last_session_path.clone();
            self.last_saved_setlist_path = p.last_setlist_path.clone();

            if !bundle.scanned_plugins.is_empty() {
                self.scanned_plugins = bundle.scanned_plugins;
                self.browser_catalog_dirty = true;
                if !cfg!(test) {
                    let cache_file = default_app_config_dir().join("plugin_cache.json");
                    let _ = save_json_file(&cache_file, &self.scanned_plugins);
                }
            }
        }
        self.close_all_editors();
        self.slot_ui_handles.clear();
        self.session = bundle.active_session;
        if self
            .selected_rack_id
            .is_none_or(|rid| !self.session.racks.iter().any(|r| r.id == rid))
        {
            self.selected_rack_id = self.session.racks.first().map(|r| r.id);
        }
        self.selected_rack_ids = self.selected_rack_id.into_iter().collect();
        self.preserve_session_switch_key_bindings(preserved_switch_binds);
        if !preserve_session_list {
            self.set_list = bundle.set_list;
            self.normalize_session_list_titles();
        }
        self.saved_session_baseline = self.session.clone();
        if !cfg!(test) {
            self.save_preferences();
            self.save_autosave_workspace();
        }
        if audio_cfg_changed {
            self.restart_audio();
        } else {
            self.rebuild_and_sync_racks();
            self.sync_master_gain_to_engine();
            self.sync_input_config_to_engine();
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>, is_error: bool) {
        let text = msg.into();
        if is_error {
            warn!("{text}");
        } else {
            info!("{text}");
        }
        self.status_banner = Some((text, is_error));
    }

    pub fn refresh_devices(&mut self) {
        self.refresh_devices_with_defaults(false);
    }

    fn refresh_devices_with_defaults(&mut self, pick_defaults: bool) {
        match AudioEngine::list_output_devices() {
            Ok(devs) => {
                if let Some(ref mut sel) = self.selected_audio_device {
                    if !devs.iter().any(|d| &d.id_string == sel) {
                        if let Some(matched) = devs.iter().find(|d| &d.name == sel) {
                            *sel = matched.id_string.clone();
                        }
                    }
                }
                if pick_defaults {
                    pick_default_device(&devs, &mut self.selected_audio_device);
                }
                self.available_audio_devices = devs;
            }
            Err(err) => {
                self.set_status(format!("Failed to enumerate audio drivers: {err}"), true);
            }
        }
        match AudioEngine::list_input_devices() {
            Ok(devs) => {
                if pick_defaults {
                    pick_default_device(&devs, &mut self.selected_input_device);
                }
                self.available_input_devices = devs;
            }
            Err(err) => {
                warn!("Failed to enumerate capture devices: {err}");
            }
        }
    }

    pub fn refresh_selected_driver_notes(&mut self) {
        match self
            .audio_engine
            .query_driver_notes(self.selected_audio_device.as_deref())
        {
            Ok(notes) => {
                if notes.input_channels > 0 && self.asio_input_channel >= notes.input_channels {
                    self.asio_input_channel = 0;
                }
                if notes.output_channels > 0 && self.asio_output_channel >= notes.output_channels {
                    self.asio_output_channel = 0;
                }
                self.cached_driver_notes = Some(notes);
                self.driver_notes_error = None;
            }
            Err(err) => {
                self.cached_driver_notes = None;
                self.driver_notes_error = Some(err.to_string());
            }
        }
    }

    pub fn close_all_editors(&mut self) {
        for handle in self.slot_ui_handles.values_mut() {
            handle.close_editor();
        }
        self.editor_windows.close_all();
    }

    pub fn start_audio(&mut self) {
        self.close_all_editors();

        let result = self.audio_engine.start_with_channels(
            self.selected_audio_device.as_deref(),
            self.selected_input_device.as_deref(),
            Some(self.selected_sample_rate),
            Some(self.selected_buffer_frames),
            self.asio_input_channel,
            self.asio_output_channel,
        );
        match result {
            Ok(()) => {
                self.cached_driver_notes = self.audio_engine.active_driver_notes().cloned();
                self.driver_notes_error = None;
                self.test_tone_enabled = false;
                self.status_banner = None;
                self.tuner.set_sample_rate(self.audio_engine.active_sample_rate().max(8_000) as f64);
                self.ui_visible_sent = !self.hidden_to_tray;
                let _ = self
                    .audio_engine
                    .send_command(EngineCommand::SetUiVisible(self.ui_visible_sent));
                self.rebuild_and_sync_racks();
                self.sync_master_gain_to_engine();
                self.sync_input_config_to_engine();
                let _ = self
                    .audio_engine
                    .send_command(EngineCommand::SetOutputMuted(self.output_muted));
                if self.tuner_enabled {
                    let _ = self.audio_engine.send_command(EngineCommand::SetTunerEnabled(true));
                }
            }
            Err(err) => {
                self.set_status(format!("Audio start failed: {err}"), true);
            }
        }
    }

    pub fn stop_audio(&mut self) {
        self.close_all_editors();
        self.test_tone_enabled = false;
        self.audio_engine.stop();
        self.slot_ui_handles.clear();
        self.slot_latencies.clear();
        self.slot_sidechain_caps.clear();
        self.set_status("Audio engine stopped", false);
    }

    pub fn restart_audio(&mut self) {
        if self.audio_engine.is_running() {
            self.capture_live_plugin_states();
            self.stop_audio();
        }
        self.refresh_devices();
        self.start_audio();
    }

    /// Captures binary `state_base64` and current parameter values from live plugin UI handles
    /// back into `self.session` before saving or rebuilding racks.
    pub fn capture_live_plugin_states(&mut self) {
        for rack in &mut self.session.racks {
            for slot in &mut rack.slots {
                let Some(handle) = self.slot_ui_handles.get_mut(&slot.id) else {
                    continue;
                };
                match handle.save_state() {
                    Ok(bytes) if !bytes.is_empty() => {
                        slot.state_base64 = Some(encode_base64(&bytes));
                    }
                    Ok(_) => {}
                    Err(err) => warn!("Skipping state capture for '{}': {err}", slot.name),
                }
                if self.expanded_param_slots.contains(&slot.id)
                    || !self.slot_param_catalog.contains_key(&slot.id)
                {
                    let params = handle.parameters();
                    for p in &params {
                        slot.params.insert(p.id, p.value);
                    }
                    self.slot_param_catalog.insert(slot.id, params);
                }
            }
        }
    }

    pub fn rebuild_and_sync_racks(&mut self) {
        if !self.audio_engine.is_running() {
            return;
        }
        self.close_all_editors();
        self.capture_live_plugin_states();

        let sample_rate = self.audio_engine.active_sample_rate().max(8_000) as f64;
        let max_block = (self.audio_engine.active_buffer_frames() as usize).max(2048);

        let mut live_racks = Vec::with_capacity(self.session.racks.len());
        let mut load_warnings = Vec::new();
        let mut param_catalog = HashMap::new();
        let mut new_ui_handles = HashMap::new();
        let mut new_slot_latencies = HashMap::new();
        let mut new_slot_sidechain_caps = HashMap::new();

        for rack_cfg in &self.session.racks {
            let mut live_slots = Vec::with_capacity(rack_cfg.slots.len());
            for slot_cfg in &rack_cfg.slots {
                match self.instantiate_slot(slot_cfg, sample_rate, max_block) {
                    Ok(built) => {
                        if let Some(w) = built.state_warning {
                            load_warnings.push(w);
                        }
                        param_catalog.insert(slot_cfg.id, built.params);
                        new_slot_latencies.insert(slot_cfg.id, built.live_slot.latency_samples);
                        new_slot_sidechain_caps.insert(slot_cfg.id, built.supports_sidechain);
                        if let Some(ui) = built.ui_handle {
                            new_ui_handles.insert(slot_cfg.id, ui);
                        }
                        live_slots.push(built.live_slot);
                    }
                    Err(err) => load_warnings.push(format!("{}: {err}", slot_cfg.name)),
                }
            }
            live_racks.push(LiveRack::new(rack_cfg.clone(), live_slots, max_block));
        }
        self.slot_param_catalog = param_catalog;
        self.slot_ui_handles = new_ui_handles;
        self.slot_latencies = new_slot_latencies;
        self.slot_sidechain_caps = new_slot_sidechain_caps;

        if let Err(err) = self.audio_engine.send_command(EngineCommand::ReplaceRacks {
            racks: live_racks,
            master_gain_db: self.session.master_gain_db,
        }) {
            self.set_status(format!("Failed to upload racks to audio engine: {err}"), true);
            return;
        }

        if !load_warnings.is_empty() {
            self.set_status(
                format!("Loaded with plugin warnings: {}", load_warnings.join(" | ")),
                true,
            );
        }
    }

    fn instantiate_slot(
        &self,
        cfg: &PluginSlotConfig,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<InstantiatedSlot, String> {
        let (mut instance, ui_handle) =
            create_plugin_instance(&cfg.kind).map_err(|e| format!("create failed: {e}"))?;
        instance
            .prepare(sample_rate, max_block)
            .map_err(|e| format!("prepare failed: {e}"))?;

        let mut state_warn = None;
        if let Some(ref b64) = cfg.state_base64 {
            match decode_base64(b64) {
                Some(bytes) => {
                    if let Err(err) = instance.load_state(&bytes) {
                        state_warn = Some(format!("{}: state restore warning: {err}", cfg.name));
                    }
                }
                None => {
                    state_warn = Some(format!("{}: corrupt base64 plugin state", cfg.name));
                }
            }
        }

        for (&param_id, &value) in &cfg.params {
            instance.set_parameter(param_id, value);
        }
        let params = ui_handle
            .as_ref()
            .map(|h| h.parameters())
            .unwrap_or_else(|| instance.parameters());
        let latency_samples = instance.latency_samples();
        let supports_sidechain = instance.supports_sidechain();
        Ok(InstantiatedSlot {
            live_slot: LiveSlot::new(
                cfg.id,
                cfg.enabled,
                cfg.bypassed,
                cfg.gain_db,
                cfg.pan,
                cfg.wet_mix,
                latency_samples,
                cfg.sidechain.clone(),
                instance,
            ),
            params,
            ui_handle,
            supports_sidechain,
            state_warning: state_warn,
        })
    }

    pub fn mark_autosave_dirty(&mut self) {
        if !cfg!(test) && self.autosave_pending_since.is_none() {
            self.autosave_pending_since = Some(Instant::now());
        }
    }

    pub fn flush_debounced_autosave(&mut self, force: bool) {
        if cfg!(test) {
            return;
        }
        let Some(since) = self.autosave_pending_since else {
            return;
        };
        if force || since.elapsed() >= Duration::from_millis(1500) {
            self.autosave_pending_since = None;
            self.save_autosave_workspace();
        }
    }

    pub fn has_needed_live_input(&self) -> bool {
        if self.input_enabled {
            return true;
        }
        self.session.racks.iter().any(|r| {
            r.enabled
                && !r.muted
                && r.slots.iter().any(|s| {
                    s.enabled
                        && !s.bypassed
                        && s.sidechain.as_ref().is_some_and(|sc| {
                            sc.enabled
                                && matches!(sc.source, session_core::SidechainSourceKind::LiveInput { .. })
                        })
                })
        })
    }

    pub fn sync_input_capture_state(&mut self) {
        if !self.audio_engine.is_running() {
            return;
        }
        let needed = self.has_needed_live_input();
        if needed != self.input_capture_sent {
            self.input_capture_sent = needed;
            let _ = self
                .audio_engine
                .send_command(EngineCommand::SetInputCaptureActive(needed));
        }
    }

    pub fn sync_params_to_engine(&mut self) {
        self.sync_params_to_engine_impl(false);
    }

    pub fn sync_params_to_engine_with_slot_params(&mut self) {
        self.sync_params_to_engine_impl(true);
    }

    fn sync_params_to_engine_impl(&mut self, include_slot_params: bool) {
        if !self.audio_engine.is_running() {
            return;
        }
        self.sync_input_capture_state();
        let cmd = EngineCommand::SyncLiveParams {
            params: Box::new(LiveSessionParams::from_session(&self.session, include_slot_params)),
        };
        if let Err(err) = self.audio_engine.send_command(cmd) {
            self.set_status(format!("Failed to sync parameters: {err}"), true);
        }
    }

    pub fn sync_master_gain_to_engine(&mut self) {
        if !self.audio_engine.is_running() {
            return;
        }
        let _ = self
            .audio_engine
            .send_command(EngineCommand::SetMasterGainDb(self.session.master_gain_db));
    }

    pub fn resolve_slot_sidechain_support(
        slot_caps: &HashMap<Uuid, bool>,
        scanned_plugins: &[PluginDescriptor],
        slot: &PluginSlotConfig,
    ) -> Option<bool> {
        if let Some(&cap) = slot_caps.get(&slot.id) {
            return Some(cap);
        }
        match &slot.kind {
            PluginKind::BuiltInEffect(BuiltInEffectKind::DynamicsGateComp) => Some(true),
            PluginKind::BuiltInEffect(_) => Some(false),
            PluginKind::Clap { plugin_id, .. } => scanned_plugins
                .iter()
                .find(|d| d.id == *plugin_id)
                .map(|d| d.has_sidechain),
            PluginKind::Vst3 { class_id, .. } => scanned_plugins
                .iter()
                .find(|d| d.id == *class_id && d.has_sidechain)
                .map(|_| true),
            PluginKind::Vst2 { unique_id, path } => {
                let target_id = format!("vst2:{:08X}", *unique_id as u32);
                scanned_plugins
                    .iter()
                    .find(|d| {
                        (d.id == target_id || d.path.as_ref() == Some(path)) && d.has_sidechain
                    })
                    .map(|_| true)
            }
        }
    }

    pub fn sync_input_config_to_engine(&mut self) {
        if !self.audio_engine.is_running() {
            return;
        }
        let needed = self.has_needed_live_input();
        self.input_capture_sent = needed;
        let _ = self
            .audio_engine
            .send_command(EngineCommand::SetInputCaptureActive(needed));
        let _ = self.audio_engine.send_command(EngineCommand::SetInputConfig {
            enabled: self.input_enabled,
            gain_db: self.input_gain_db,
            mode: self.input_mode,
        });
    }

    pub fn set_output_muted(&mut self, muted: bool) {
        self.output_muted = muted;
        if !self.audio_engine.is_running() {
            return;
        }
        if let Err(err) = self
            .audio_engine
            .send_command(EngineCommand::SetOutputMuted(muted))
        {
            self.set_status(format!("Monitor mute failed: {err}"), true);
        } else {
            let msg = if muted {
                "Monitor output muted (input & rack meters remain active)"
            } else {
                "Monitor output active"
            };
            self.set_status(msg, false);
        }
    }

    pub fn set_test_tone(&mut self, enabled: bool) {
        self.test_tone_enabled = enabled;
        if !self.audio_engine.is_running() {
            return;
        }
        match self.audio_engine.send_command(EngineCommand::SetTestTone(enabled)) {
            Ok(()) => {
                let msg = if enabled {
                    "Test Tone active: 440 Hz @ -18 dBFS"
                } else {
                    "Test Tone stopped"
                };
                self.set_status(msg, false);
            }
            Err(err) => self.set_status(format!("Test Tone failed: {err}"), true),
        }
    }

    pub fn set_tuner_enabled(&mut self, enabled: bool) {
        self.tuner_enabled = enabled;
        self.save_preferences();
        if !enabled {
            self.telemetry.tuner = Default::default();
        }
        if !self.audio_engine.is_running() {
            return;
        }
        if let Err(err) = self
            .audio_engine
            .send_command(EngineCommand::SetTunerEnabled(enabled))
        {
            self.set_status(format!("Tuner failed: {err}"), true);
        }
    }

    pub fn activate_sub_session(&mut self, id: Uuid) {
        if let Err(err) = self.session.activate_sub_session(id) {
            self.set_status(format!("Sub-Session switch failed: {err}"), true);
            return;
        }
        self.sync_params_to_engine_with_slot_params();
        self.sync_master_gain_to_engine();
    }

    pub fn refresh_auto_run_state(&mut self) {
        match autostart::registered_auto_run_command() {
            Ok(cmd) => {
                self.auto_run_on_startup = cmd.is_some();
                self.auto_run_registered_cmd = cmd;
            }
            Err(err) => {
                self.set_status(format!("Windows startup status check failed: {err}"), true);
            }
        }
    }

    pub fn set_auto_run_on_startup(&mut self, enabled: bool) {
        match autostart::set_auto_run(enabled) {
            Ok(()) => {
                self.refresh_auto_run_state();
                let msg = if enabled {
                    "Enabled Auto-Run when Windows starts"
                } else {
                    "Disabled Auto-Run when Windows starts"
                };
                self.set_status(msg, false);
            }
            Err(err) => {
                self.refresh_auto_run_state();
                self.set_status(err, true);
            }
        }
    }

    pub fn is_auto_run_path_current(&self) -> bool {
        let Some(ref cmd) = self.auto_run_registered_cmd else {
            return true;
        };
        std::env::current_exe()
            .map(|exe| autostart::command_matches_exe(cmd, &exe))
            .unwrap_or(true)
    }

    pub fn sync_enabled_plugin_search_paths(&mut self) {
        self.plugin_search_paths = self
            .plugin_folders
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.path.clone())
            .collect();
    }

    pub fn add_plugin_folder(&mut self, path: PathBuf) {
        let trimmed = path.to_string_lossy().trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        let candidate = PathBuf::from(&trimmed);
        let duplicate_idx = self.plugin_folders.iter().position(|entry| {
            entry.path.to_string_lossy().eq_ignore_ascii_case(&trimmed)
        });
        if let Some(idx) = duplicate_idx {
            self.plugin_folders[idx].enabled = true;
            self.selected_plugin_folder_idx = Some(idx);
            self.sync_enabled_plugin_search_paths();
            self.save_preferences();
            self.set_status(format!("Enabled existing plugin folder: {trimmed}"), false);
            return;
        }
        self.plugin_folders.push(PluginFolderEntry {
            path: candidate,
            enabled: true,
        });
        self.selected_plugin_folder_idx = Some(self.plugin_folders.len() - 1);
        self.sync_enabled_plugin_search_paths();
        self.save_preferences();
        self.set_status(format!("Added plugin folder: {trimmed}"), false);
    }

    pub fn remove_selected_plugin_folder(&mut self) {
        let Some(idx) = self.selected_plugin_folder_idx else {
            return;
        };
        if idx >= self.plugin_folders.len() {
            self.selected_plugin_folder_idx = None;
            return;
        }
        let removed = self.plugin_folders.remove(idx);
        self.selected_plugin_folder_idx = if self.plugin_folders.is_empty() {
            None
        } else {
            Some(idx.min(self.plugin_folders.len() - 1))
        };
        self.sync_enabled_plugin_search_paths();
        self.save_preferences();
        self.set_status(
            format!("Removed plugin folder: {}", removed.path.display()),
            false,
        );
    }

    pub fn move_selected_plugin_folder(&mut self, delta: isize) {
        let Some(idx) = self.selected_plugin_folder_idx else {
            return;
        };
        if idx >= self.plugin_folders.len() {
            return;
        }
        let target = idx as isize + delta;
        if target < 0 || target >= self.plugin_folders.len() as isize {
            return;
        }
        let target = target as usize;
        self.plugin_folders.swap(idx, target);
        self.selected_plugin_folder_idx = Some(target);
        self.sync_enabled_plugin_search_paths();
        self.save_preferences();
    }

    pub fn restore_default_plugin_folders(&mut self) {
        let mut added = 0usize;
        for default_path in default_plugin_search_paths() {
            let exists = self.plugin_folders.iter().any(|entry| {
                entry
                    .path
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&default_path.to_string_lossy())
            });
            if !exists {
                self.plugin_folders.push(PluginFolderEntry {
                    path: default_path,
                    enabled: true,
                });
                added += 1;
            }
        }
        if self.selected_plugin_folder_idx.is_none() && !self.plugin_folders.is_empty() {
            self.selected_plugin_folder_idx = Some(0);
        }
        self.sync_enabled_plugin_search_paths();
        self.save_preferences();
        self.set_status(
            format!("Restored default plugin folders ({added} added)"),
            false,
        );
    }

    /// Scans enabled VST3 and CLAP folders in a background thread using isolated child processes.
    pub fn start_plugin_scan(&mut self) {
        if self.plugin_scan_rx.is_some() {
            return;
        }
        let dirs: Vec<PathBuf> = self
            .plugin_folders
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.path.clone())
            .collect();
        if dirs.is_empty() {
            self.set_status("No enabled plugin folders configured in Options", true);
            return;
        }
        self.sync_enabled_plugin_search_paths();
        let (tx, rx) = mpsc::channel();
        self.plugin_scan_rx = Some(rx);
        let wake_sig = self.wake_signal.clone();
        std::thread::spawn(move || {
            let report = scan_plugins_detailed(&dirs);
            let _ = tx.send(report);
            if let Some(sig) = wake_sig {
                sig.wake();
            }
        });
        self.set_status("Scanning VST3, VST2, and CLAP plugins out-of-process...", false);
    }

    pub fn panic_audio(&mut self) {
        self.test_tone_enabled = false;
        if !self.audio_engine.is_running() {
            return;
        }
        match self.audio_engine.send_command(EngineCommand::PanicReset) {
            Ok(()) => self.set_status("Audio Panic: reset DSP & plugin buffers", false),
            Err(err) => self.set_status(format!("Panic failed: {err}"), true),
        }
    }

    pub fn new_session(&mut self) {
        self.push_undo_checkpoint();
        let preserved_switch_binds = self.take_session_switch_key_bindings();
        self.close_all_editors();
        self.slot_ui_handles.clear();
        self.session = SessionConfig::default();
        self.preserve_session_switch_key_bindings(preserved_switch_binds);
        self.saved_session_baseline = self.session.clone();
        self.active_setlist_entry_id = None;
        self.rebuild_and_sync_racks();
        self.sync_master_gain_to_engine();
        self.set_status("Started new session", false);
    }

    pub fn request_new_session(&mut self) {
        if self.is_session_dirty() {
            self.pending_session_action = Some(PendingSessionAction::NewSession);
        } else {
            self.new_session();
        }
    }

    pub fn request_load_session(&mut self, path: PathBuf) {
        if self.is_session_dirty() {
            self.pending_session_action = Some(PendingSessionAction::LoadSession(path));
        } else {
            self.load_session_from_path(&path);
        }
    }

    pub fn request_load_setlist_entry(&mut self, entry_id: Uuid) {
        if self.is_session_dirty() {
            if self.auto_save_on_song_switch && !self.session_path_input.trim().is_empty() {
                self.save_session_to_disk();
                if self.is_session_dirty() {
                    return;
                }
                self.load_setlist_entry(entry_id);
            } else {
                self.pending_session_action = Some(PendingSessionAction::LoadSetListEntry(entry_id));
            }
        } else {
            self.load_setlist_entry(entry_id);
        }
    }

    pub fn resolve_pending_session_action(&mut self, save_first: bool) {
        if save_first {
            self.save_session_to_disk();
            if self.is_session_dirty() {
                // Save failed; keep pending dialog open or abort transition safely.
                return;
            }
        }
        let Some(action) = self.pending_session_action.take() else {
            return;
        };
        match action {
            PendingSessionAction::NewSession => self.new_session(),
            PendingSessionAction::LoadSession(path) => self.load_session_from_path(&path),
            PendingSessionAction::LoadSetListEntry(id) => self.load_setlist_entry(id),
        }
    }

    pub fn is_scanning_plugins(&self) -> bool {
        self.plugin_scan_rx.is_some()
    }

    pub fn add_slot_to_rack(&mut self, rack_id: Uuid, name: String, kind: PluginKind) {
        self.push_undo_checkpoint();
        let slot_cfg = PluginSlotConfig::new(name.clone(), kind);
        let slot_id = slot_cfg.id;
        let Some(rack) = self.session.racks.iter_mut().find(|r| r.id == rack_id) else {
            return;
        };
        rack.slots.push(slot_cfg.clone());
        let slot_count_idx = rack.slots.len() - 1;

        if self.audio_engine.is_running() {
            let sample_rate = self.audio_engine.active_sample_rate().max(8_000) as f64;
            let max_block = (self.audio_engine.active_buffer_frames() as usize).max(2048);
            match self.instantiate_slot(&slot_cfg, sample_rate, max_block) {
                Ok(built) => {
                    self.slot_param_catalog.insert(slot_id, built.params);
                    self.slot_latencies.insert(slot_id, built.live_slot.latency_samples);
                    self.slot_sidechain_caps.insert(slot_id, built.supports_sidechain);
                    if let Some(ui) = built.ui_handle {
                        self.slot_ui_handles.insert(slot_id, ui);
                    }
                    if let Err(err) = self.audio_engine.send_command(EngineCommand::AddSlot {
                        rack_id,
                        index: Some(slot_count_idx),
                        slot: Box::new(built.live_slot),
                    }) {
                        self.set_status(format!("Failed to upload slot to audio engine: {err}"), true);
                        return;
                    }
                    if let Some(warn_msg) = built.state_warning {
                        self.set_status(warn_msg, true);
                        return;
                    }
                }
                Err(err) => {
                    self.set_status(format!("{}: {err}", slot_cfg.name), true);
                    return;
                }
            }
            if let Some(&latency_smp) = self.slot_latencies.get(&slot_id) {
                let sr = self.audio_engine.active_sample_rate().max(8_000) as f32;
                let ms = (latency_smp as f32 * 1000.0) / sr;
                let msg = if latency_smp > 0 {
                    format!("Added '{name}' (plugin delay: {latency_smp} smp / {ms:.1} ms)")
                } else {
                    format!("Added '{name}' (0 ms plugin delay)")
                };
                self.set_status(msg, false);
            }
        }
    }

    pub fn add_rack_incremental(&mut self, rack: session_core::RackConfig) {
        if !self.audio_engine.is_running() {
            self.session.racks.push(rack);
            return;
        }
        let sample_rate = self.audio_engine.active_sample_rate().max(8_000) as f64;
        let max_block = (self.audio_engine.active_buffer_frames() as usize).max(2048);
        let mut live_slots = Vec::with_capacity(rack.slots.len());
        let mut load_warnings = Vec::new();
        for slot_cfg in &rack.slots {
            match self.instantiate_slot(slot_cfg, sample_rate, max_block) {
                Ok(built) => {
                    if let Some(w) = built.state_warning {
                        load_warnings.push(w);
                    }
                    self.slot_param_catalog.insert(slot_cfg.id, built.params);
                    self.slot_latencies
                        .insert(slot_cfg.id, built.live_slot.latency_samples);
                    self.slot_sidechain_caps
                        .insert(slot_cfg.id, built.supports_sidechain);
                    if let Some(ui) = built.ui_handle {
                        self.slot_ui_handles.insert(slot_cfg.id, ui);
                    }
                    live_slots.push(built.live_slot);
                }
                Err(err) => load_warnings.push(format!("{}: {err}", slot_cfg.name)),
            }
        }
        let live_rack = LiveRack::new(rack.clone(), live_slots, max_block);
        self.session.racks.push(rack);
        if let Err(err) = self
            .audio_engine
            .send_command(EngineCommand::AddRack {
                rack: Box::new(live_rack),
            })
        {
            self.set_status(format!("Failed to add rack to audio engine: {err}"), true);
            return;
        }
        if !load_warnings.is_empty() {
            self.set_status(
                format!("Loaded with plugin warnings: {}", load_warnings.join(" | ")),
                true,
            );
        }
    }

    pub fn sync_rack_order_to_engine(&mut self) {
        if !self.audio_engine.is_running() {
            return;
        }
        let order: Vec<Uuid> = self.session.racks.iter().map(|r| r.id).collect();
        let _ = self
            .audio_engine
            .send_command(EngineCommand::ReorderRacks { order });
    }

    pub fn remove_rack_incremental(&mut self, rack_id: Uuid) {
        if let Some(rack) = self.session.racks.iter().find(|r| r.id == rack_id) {
            for slot in &rack.slots {
                if let Some(mut handle) = self.slot_ui_handles.remove(&slot.id) {
                    handle.close_editor();
                }
                self.editor_windows.close_one(rack_id, slot.id);
                self.slot_param_catalog.remove(&slot.id);
                self.slot_latencies.remove(&slot.id);
                self.slot_sidechain_caps.remove(&slot.id);
            }
        }
        self.session.racks.retain(|r| r.id != rack_id);
        for rack in &mut self.session.racks {
            rack.sends.retain(|s| s.target_rack_id != rack_id);
            for slot in &mut rack.slots {
                if matches!(
                    slot.sidechain,
                    Some(ref sc) if matches!(sc.source, session_core::SidechainSourceKind::Rack { rack_id: sid, .. } if sid == rack_id)
                ) {
                    slot.sidechain = None;
                }
            }
        }
        if self.selected_rack_id == Some(rack_id) {
            self.selected_rack_id = None;
        }
        self.selected_rack_ids.retain(|rid| *rid != rack_id);
        if self.audio_engine.is_running() {
            let _ = self
                .audio_engine
                .send_command(EngineCommand::RemoveRack { rack_id });
            self.sync_params_to_engine();
        }
    }

    pub fn remove_slot_incremental(&mut self, rack_id: Uuid, slot_idx: usize) {
        let Some(rack) = self.session.racks.iter_mut().find(|r| r.id == rack_id) else {
            return;
        };
        if slot_idx >= rack.slots.len() {
            return;
        }
        let removed = rack.slots.remove(slot_idx);
        if let Some(mut handle) = self.slot_ui_handles.remove(&removed.id) {
            handle.close_editor();
        }
        self.editor_windows.close_one(rack_id, removed.id);
        self.slot_param_catalog.remove(&removed.id);
        self.slot_latencies.remove(&removed.id);
        self.slot_sidechain_caps.remove(&removed.id);
        if self.audio_engine.is_running() {
            let _ = self.audio_engine.send_command(EngineCommand::RemoveSlot {
                rack_id,
                slot_id: removed.id,
            });
            self.sync_input_capture_state();
        }
    }

    pub fn sync_slot_order_to_engine(&mut self, rack_id: Uuid) {
        if !self.audio_engine.is_running() {
            return;
        }
        if let Some(rack) = self.session.racks.iter().find(|r| r.id == rack_id) {
            let order: Vec<Uuid> = rack.slots.iter().map(|s| s.id).collect();
            let _ = self
                .audio_engine
                .send_command(EngineCommand::ReorderSlots { rack_id, order });
        }
    }

    pub fn push_undo_checkpoint(&mut self) {
        self.history.push(self.session.clone());
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.history.undo(self.session.clone()) {
            self.close_all_editors();
            self.slot_ui_handles.clear();
            self.session = prev;
            self.rebuild_and_sync_racks();
            self.sync_master_gain_to_engine();
            self.set_status("Undo applied", false);
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.history.redo(self.session.clone()) {
            self.close_all_editors();
            self.slot_ui_handles.clear();
            self.session = next;
            self.rebuild_and_sync_racks();
            self.sync_master_gain_to_engine();
            self.set_status("Redo applied", false);
        }
    }

    pub fn set_slot_parameter(&mut self, rack_id: Uuid, slot_id: Uuid, param_id: u32, value: f32) {
        if !self.audio_engine.is_running() {
            return;
        }
        let _ = self
            .audio_engine
            .send_command(EngineCommand::SetSlotParameter {
                rack_id,
                slot_id,
                param_id,
                value,
            });
    }

    pub fn save_session_to_disk(&mut self) {
        let path = PathBuf::from(self.session_path_input.trim());
        if is_settings_bundle_path(&path) {
            self.export_all_settings_file(&path);
            return;
        }
        self.capture_live_plugin_states();
        match save_session(&path, &self.session) {
            Ok(()) => {
                self.saved_session_baseline = self.session.clone();
                self.last_saved_session_path = Some(path.clone());
                if !cfg!(test) {
                    self.save_preferences();
                }
                self.set_status(format!("Session saved to {}", path.display()), false);
            }
            Err(err) => self.set_status(format!("Save failed: {err}"), true),
        }
    }

    pub fn load_session_from_path(&mut self, path: &Path) {
        self.load_session_or_settings_preset(path, true);
    }

    fn load_session_or_settings_preset(&mut self, path: &Path, preserve_session_list: bool) {
        if is_settings_bundle_path(path) {
            match load_json_file::<AppSettingsBundle>(path) {
                Ok(bundle) => {
                    self.session_path_input = path.display().to_string();
                    self.apply_settings_bundle(bundle, preserve_session_list, false);
                    self.set_status(format!("Loaded settings preset {}", path.display()), false);
                }
                Err(err) => self.set_status(format!("Load failed: {err}"), true),
            }
            return;
        }

        match load_session(path) {
            Ok(loaded) => {
                let preserved_switch_binds = self.take_session_switch_key_bindings();
                self.close_all_editors();
                self.slot_ui_handles.clear();
                self.session = loaded;
                self.preserve_session_switch_key_bindings(preserved_switch_binds);
                self.saved_session_baseline = self.session.clone();
                self.session_path_input = path.display().to_string();
                self.last_saved_session_path = Some(path.to_path_buf());
                if !cfg!(test) {
                    self.save_preferences();
                    self.save_autosave_workspace();
                }
                self.rebuild_and_sync_racks();
                self.sync_master_gain_to_engine();
                self.set_status(format!("Loaded session {}", path.display()), false);
            }
            Err(session_err) => {
                if let Ok(bundle) = load_json_file::<AppSettingsBundle>(path) {
                    self.session_path_input = path.display().to_string();
                    self.apply_settings_bundle(bundle, preserve_session_list, false);
                    self.set_status(format!("Loaded settings preset {}", path.display()), false);
                } else {
                    self.set_status(format!("Load failed: {session_err}"), true);
                }
            }
        }
    }

    fn take_session_switch_key_bindings(&self) -> Vec<KeyBindingConfig> {
        self.session
            .key_bindings
            .iter()
            .filter(|b| matches!(b.action, KeyBindingAction::LoadSessionFile { .. }))
            .cloned()
            .collect()
    }

    fn preserve_session_switch_key_bindings(&mut self, preserved: Vec<KeyBindingConfig>) {
        for binding in preserved {
            self.session
                .key_bindings
                .retain(|existing| existing.action != binding.action && existing.combo != binding.combo);
            self.session.key_bindings.push(binding);
        }
    }

    pub fn normalize_session_list_titles(&mut self) {
        for entry in &mut self.set_list.entries {
            if let Some(file_title) = derive_session_title_from_file(&entry.session_path) {
                entry.title = file_title;
            }
        }
    }

    pub fn active_session_config_filename(&self) -> String {
        if let Some(active_id) = self.active_setlist_entry_id {
            if let Some(entry) = self.set_list.entries.iter().find(|e| e.id == active_id) {
                if let Some(name) = entry.session_path.file_name().and_then(|n| n.to_str()) {
                    if !name.trim().is_empty() {
                        return name.to_string();
                    }
                }
                if !entry.title.trim().is_empty() {
                    return entry.title.clone();
                }
            }
        }
        let path_trimmed = self.session_path_input.trim();
        if !path_trimmed.is_empty()
            && (Path::new(path_trimmed).exists() || path_trimmed != "session.stagehost.json")
        {
            if let Some(name) = Path::new(path_trimmed).file_name().and_then(|n| n.to_str()) {
                if !name.trim().is_empty() {
                    return name.to_string();
                }
            }
        }
        let sess_name = self.session.name.trim();
        if sess_name.is_empty() {
            "Untitled Session".to_string()
        } else {
            sess_name.to_string()
        }
    }

    pub fn save_setlist_to_disk(&mut self) {
        let path = PathBuf::from(self.setlist_path_input.trim());
        match save_set_list(&path, &self.set_list) {
            Ok(()) => {
                self.last_saved_setlist_path = Some(path.clone());
                self.save_preferences();
                self.set_status(format!("Session list saved to {}", path.display()), false);
            }
            Err(err) => self.set_status(format!("Session list save failed: {err}"), true),
        }
    }

    pub fn load_setlist_from_disk(&mut self) {
        let path = PathBuf::from(self.setlist_path_input.trim());
        match load_set_list(&path) {
            Ok(loaded) => {
                self.set_list = loaded;
                self.prune_missing_session_list_entries();
                self.normalize_session_list_titles();
                self.active_setlist_entry_id = self.set_list.entries.first().map(|e| e.id);
                self.last_saved_setlist_path = Some(path.clone());
                self.save_preferences();
                self.save_autosave_workspace();
                self.set_status(format!("Session list loaded from {}", path.display()), false);
            }
            Err(err) => self.set_status(format!("Session list load failed: {err}"), true),
        }
    }

    pub fn prune_missing_session_list_entries(&mut self) -> bool {
        let before = self.set_list.entries.len();
        self.set_list.entries.retain(|e| e.session_path.is_file());
        let removed_any = self.set_list.entries.len() != before;
        if removed_any {
            if let Some(active_id) = self.active_setlist_entry_id {
                if !self.set_list.entries.iter().any(|e| e.id == active_id) {
                    self.active_setlist_entry_id = None;
                }
            }
            let valid_ids: HashSet<Uuid> = self.set_list.entries.iter().map(|e| e.id).collect();
            self.session.key_bindings.retain(|b| {
                !matches!(&b.action, KeyBindingAction::LoadSessionFile { entry_id, .. } if !valid_ids.contains(entry_id))
            });
            if !cfg!(test) {
                self.save_autosave_workspace();
            }
        }
        removed_any
    }

    pub fn remove_session_list_entry(&mut self, entry_id: Uuid) {
        self.set_list.entries.retain(|e| e.id != entry_id);
        if self.active_setlist_entry_id == Some(entry_id) {
            self.active_setlist_entry_id = None;
        }
        self.session.key_bindings.retain(|b| {
            !matches!(&b.action, KeyBindingAction::LoadSessionFile { entry_id: eid, .. } if *eid == entry_id)
        });
        if !cfg!(test) {
            self.save_autosave_workspace();
        }
    }

    pub fn load_setlist_entry(&mut self, entry_id: Uuid) {
        let Some(entry) = self.set_list.entries.iter().find(|e| e.id == entry_id).cloned() else {
            return;
        };
        if !entry.session_path.is_file() {
            let title = entry.title.clone();
            self.remove_session_list_entry(entry.id);
            self.set_status(
                format!("Session file '{title}' was moved or deleted; removed from Session List"),
                true,
            );
            return;
        }
        self.active_setlist_entry_id = Some(entry.id);
        self.load_session_or_settings_preset(&entry.session_path, true);
    }

    pub fn step_setlist(&mut self, delta: isize) {
        let candidate_ids: Vec<Uuid> = {
            let enabled: Vec<Uuid> = self
                .set_list
                .entries
                .iter()
                .filter(|e| e.enabled)
                .map(|e| e.id)
                .collect();
            if enabled.is_empty() {
                self.set_list.entries.iter().map(|e| e.id).collect()
            } else {
                enabled
            }
        };
        if candidate_ids.is_empty() {
            return;
        }
        let len = candidate_ids.len() as isize;
        let current_idx = self
            .active_setlist_entry_id
            .and_then(|id| candidate_ids.iter().position(|&cand| cand == id))
            .map(|i| i as isize)
            .unwrap_or(if delta >= 0 { -1 } else { 0 });
        let next_idx = (current_idx + delta).rem_euclid(len) as usize;
        self.request_load_setlist_entry(candidate_ids[next_idx]);
    }

    pub fn add_current_session_to_setlist(&mut self) {
        let trimmed = self.session_path_input.trim();
        let path = if trimmed.is_empty() {
            PathBuf::from("session.stagehost.json")
        } else {
            PathBuf::from(trimmed)
        };
        if !path.exists() {
            self.session_path_input = path.display().to_string();
            self.save_session_to_disk();
        }
        self.add_session_file_to_list(path);
    }

    pub fn add_session_file_to_list(&mut self, path: PathBuf) -> Uuid {
        if let Some(existing) = self
            .set_list
            .entries
            .iter_mut()
            .find(|e| e.session_path == path)
        {
            if let Some(detected) = derive_session_title_from_file(&path) {
                existing.title = detected;
            }
            let id = existing.id;
            self.active_setlist_entry_id = Some(id);
            if !cfg!(test) {
                self.save_autosave_workspace();
            }
            return id;
        }

        let title = derive_session_title_from_file(&path)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| {
                if !self.session.name.trim().is_empty() && path == Path::new(self.session_path_input.trim()) {
                    self.session.name.clone()
                } else {
                    "Untitled Session".to_string()
                }
            });
        let id = Uuid::new_v4();
        self.set_list.entries.push(SetListEntry {
            id,
            title: title.clone(),
            session_path: path,
            notes: String::new(),
            enabled: true,
        });
        self.active_setlist_entry_id = Some(id);
        if !cfg!(test) {
            self.save_autosave_workspace();
        }
        self.set_status(format!("Added '{title}' to Session List"), false);
        id
    }

    pub fn save_preset_and_add_to_session_list(&mut self, path: PathBuf) {
        self.session_path_input = path.display().to_string();
        self.save_session_to_disk();
        self.add_session_file_to_list(path);
    }

    pub fn open_slot_editor(&mut self, rack_id: Uuid, slot_id: Uuid, title: &str) {
        let Some(handle) = self.slot_ui_handles.get_mut(&slot_id) else {
            self.set_status("Plugin UI handle is not active (start audio engine first)", true);
            return;
        };
        if !handle.has_editor() {
            self.set_status(format!("'{title}' does not expose a native GUI editor"), false);
            return;
        }
        match self.editor_windows.open_or_focus(rack_id, slot_id, title) {
            Ok((hwnd, newly_created)) => {
                if newly_created {
                    if let Err(err) = handle.open_editor(hwnd) {
                        self.editor_windows.close_one(rack_id, slot_id);
                        self.set_status(format!("Failed to open plugin editor: {err}"), true);
                    } else if let Some((w, h)) = handle.preferred_size() {
                        self.editor_windows.resize_client_area(rack_id, slot_id, w, h);
                        handle.resize_editor(w, h);
                    }
                }
            }
            Err(err) => self.set_status(format!("Window creation failed: {err}"), true),
        }
    }

    fn poll_background_state(&mut self, ui_visible: bool) {
        if self.audio_engine.take_reset_requested() {
            info!("ASIO driver requested reset; restarting audio engine");
            self.restart_audio();
        }
        self.audio_engine.poll_retired_racks();
        self.flush_debounced_autosave(false);

        if ui_visible {
            if let Some(t) = self.audio_engine.poll_telemetry() {
                self.telemetry = t;
            }
            if self.tuner_enabled {
                if let Some(reading) = self.audio_engine.poll_tuner(&mut self.tuner) {
                    self.telemetry.tuner = reading;
                }
            }
            self.audio_engine.poll_rack_meters(&mut self.rack_meters);

            for (_, slot_id, w, h) in self.editor_windows.poll_resized() {
                if let Some(handle) = self.slot_ui_handles.get_mut(&slot_id) {
                    handle.resize_editor(w, h);
                }
            }

            for (_, slot_id) in self.editor_windows.poll_closed() {
                if let Some(handle) = self.slot_ui_handles.get_mut(&slot_id) {
                    handle.close_editor();
                    if self.expanded_param_slots.contains(&slot_id) {
                        self.slot_param_catalog.insert(slot_id, handle.parameters());
                    }
                }
            }
        }

        if let Some(rx) = &self.plugin_scan_rx {
            if let Ok(report) = rx.try_recv() {
                let count = report.descriptors.len();
                let issue_count = report.issues.len();
                self.scanned_plugins = report.descriptors;
                self.browser_catalog_dirty = true;
                self.last_plugin_scan_issues = report.issues;
                self.plugin_scan_rx = None;
                let cache_path = default_app_config_dir().join("plugin_cache.json");
                if let Err(err) = save_json_file(&cache_path, &self.scanned_plugins) {
                    warn!("Failed to persist plugin cache: {err}");
                }
                if issue_count > 0 {
                    self.set_status(
                        format!("Plugin scan finished: found {count} plugins ({issue_count} skipped)"),
                        false,
                    );
                } else {
                    self.set_status(format!("Plugin scan finished: found {count} plugins"), false);
                }
            }
        }
    }

    pub fn clear_keybind_draft_combo(&mut self) {
        self.keybind_draft_combo = KeyComboSpec {
            ctrl: false,
            alt: false,
            shift: false,
            key: String::new(),
        };
    }

    /// Commits the pending draft keybind into `session.key_bindings` if target and key are valid.
    pub fn commit_keybind_draft(&mut self) -> bool {
        if self.keybind_draft_combo.key.is_empty() {
            return false;
        }
        let action = if self.keybind_draft_action == KeybindDraftAction::LoadSessionFile {
            let Some(entry_id) = self
                .keybind_draft_session_entry_id
                .or_else(|| self.set_list.entries.first().map(|e| e.id))
            else {
                return false;
            };
            let Some(entry) = self.set_list.entries.iter().find(|e| e.id == entry_id) else {
                return false;
            };
            KeyBindingAction::LoadSessionFile {
                entry_id: entry.id,
                path: entry.session_path.clone(),
            }
        } else {
            let Some(rack_id) = self
                .keybind_draft_rack_id
                .or_else(|| self.session.racks.first().map(|r| r.id))
            else {
                return false;
            };

            match self.keybind_draft_action {
                KeybindDraftAction::ToggleSlotBypass => {
                    let Some(slot_id) = self.keybind_draft_slot_id else {
                        return false;
                    };
                    KeyBindingAction::ToggleSlotBypass { rack_id, slot_id }
                }
                KeybindDraftAction::ToggleSlotEnabled => {
                    let Some(slot_id) = self.keybind_draft_slot_id else {
                        return false;
                    };
                    KeyBindingAction::ToggleSlotEnabled { rack_id, slot_id }
                }
                KeybindDraftAction::OpenSlotEditor => {
                    let Some(slot_id) = self.keybind_draft_slot_id else {
                        return false;
                    };
                    KeyBindingAction::OpenSlotEditor { rack_id, slot_id }
                }
                KeybindDraftAction::SetSlotParam => {
                    let Some(slot_id) = self.keybind_draft_slot_id else {
                        return false;
                    };
                    KeyBindingAction::SetSlotParam {
                        rack_id,
                        slot_id,
                        param_id: self.keybind_draft_param_id,
                        value: self.keybind_draft_param_value,
                    }
                }
                KeybindDraftAction::ToggleRackMute => KeyBindingAction::ToggleRackMute { rack_id },
                KeybindDraftAction::ToggleRackSolo => KeyBindingAction::ToggleRackSolo { rack_id },
                KeybindDraftAction::ToggleRackEnabled => {
                    KeyBindingAction::ToggleRackEnabled { rack_id }
                }
                KeybindDraftAction::ToggleRackSend => {
                    let Some(target_rack_id) = self.keybind_draft_send_target_id else {
                        return false;
                    };
                    KeyBindingAction::ToggleRackSend {
                        rack_id,
                        target_rack_id,
                    }
                }
                KeybindDraftAction::ToggleMultiRackMute => {
                    if self.keybind_draft_rack_ids.is_empty() {
                        return false;
                    }
                    KeyBindingAction::ToggleMultiRackMute {
                        rack_ids: self.keybind_draft_rack_ids.clone(),
                    }
                }
                KeybindDraftAction::ToggleMultiRackSolo => {
                    if self.keybind_draft_rack_ids.is_empty() {
                        return false;
                    }
                    KeyBindingAction::ToggleMultiRackSolo {
                        rack_ids: self.keybind_draft_rack_ids.clone(),
                    }
                }
                KeybindDraftAction::ToggleMultiRackEnabled => {
                    if self.keybind_draft_rack_ids.is_empty() {
                        return false;
                    }
                    KeyBindingAction::ToggleMultiRackEnabled {
                        rack_ids: self.keybind_draft_rack_ids.clone(),
                    }
                }
                KeybindDraftAction::LoadSessionFile => unreachable!(),
            }
        };

        let combo = self.keybind_draft_combo.clone();
        if let KeyBindingAction::LoadSessionFile { entry_id, .. } = &action {
            let replacing = self.session.key_bindings.iter().any(|b| {
                matches!(&b.action, KeyBindingAction::LoadSessionFile { entry_id: eid, .. } if eid == entry_id)
                    || b.combo == combo
            });
            if replacing {
                self.push_undo_checkpoint();
                self.session.key_bindings.retain(|b| {
                    !matches!(&b.action, KeyBindingAction::LoadSessionFile { entry_id: eid, .. } if eid == entry_id)
                        && b.combo != combo
                });
            }
        }
        let already_exists = self
            .session
            .key_bindings
            .iter()
            .any(|b| b.combo == combo && b.action == action);
        if !already_exists {
            self.push_undo_checkpoint();
            self.session.key_bindings.push(KeyBindingConfig {
                id: Uuid::new_v4(),
                enabled: true,
                combo: combo.clone(),
                action,
            });
            if !cfg!(test) {
                self.save_autosave_workspace();
            }
            self.set_status(format!("Added keybind {}", combo.display_label()), false);
        }
        self.clear_keybind_draft_combo();
        true
    }

    pub fn open_keybindings_modal(&mut self) {
        if self.keybind_draft_rack_id.is_none() {
            self.keybind_draft_rack_id = self.session.racks.first().map(|r| r.id);
        }
        if self.keybind_draft_slot_id.is_none() {
            self.keybind_draft_slot_id = self
                .keybind_draft_rack_id
                .and_then(|rid| self.session.racks.iter().find(|r| r.id == rid))
                .and_then(|r| r.slots.first().map(|s| s.id));
        }
        if self.keybind_draft_send_target_id.is_none() {
            self.keybind_draft_send_target_id = self
                .keybind_draft_rack_id
                .and_then(|rid| self.session.racks.iter().find(|r| r.id == rid))
                .and_then(|r| r.sends.first().map(|s| s.target_rack_id));
        }
        if self.keybind_draft_session_entry_id.is_none() {
            self.keybind_draft_session_entry_id = self
                .active_setlist_entry_id
                .or_else(|| self.set_list.entries.first().map(|e| e.id));
        }
        self.keybind_learn_target_id = None;
        self.keybind_learn_draft = false;
        self.show_keybindings = true;
    }

    pub fn open_keybindings_for_session_entry(&mut self, entry_id: Uuid) {
        self.keybind_draft_session_entry_id = Some(entry_id);
        self.keybind_draft_action = KeybindDraftAction::LoadSessionFile;
        self.clear_keybind_draft_combo();
        self.keybind_learn_target_id = None;
        self.keybind_learn_draft = true;
        self.show_keybindings = true;
    }

    pub fn open_keybindings_for_rack(&mut self, rack_id: Uuid) {
        self.keybind_draft_rack_id = Some(rack_id);
        self.keybind_draft_slot_id = self
            .session
            .racks
            .iter()
            .find(|r| r.id == rack_id)
            .and_then(|r| r.slots.first().map(|s| s.id));
        self.keybind_draft_send_target_id = self
            .session
            .racks
            .iter()
            .find(|r| r.id == rack_id)
            .and_then(|r| r.sends.first().map(|s| s.target_rack_id));
        self.keybind_draft_action = KeybindDraftAction::ToggleRackMute;
        self.clear_keybind_draft_combo();
        self.keybind_learn_target_id = None;
        self.keybind_learn_draft = true;
        self.show_keybindings = true;
    }

    pub fn open_keybindings_for_multi_rack(&mut self, rack_ids: Vec<Uuid>) {
        self.keybind_draft_rack_ids = rack_ids;
        self.keybind_draft_rack_id = self.keybind_draft_rack_ids.first().copied();
        self.keybind_draft_action = KeybindDraftAction::ToggleMultiRackMute;
        self.clear_keybind_draft_combo();
        self.keybind_learn_target_id = None;
        self.keybind_learn_draft = true;
        self.show_keybindings = true;
    }

    pub fn open_keybindings_for_rack_send(&mut self, rack_id: Uuid, target_rack_id: Uuid) {
        self.keybind_draft_rack_id = Some(rack_id);
        self.keybind_draft_send_target_id = Some(target_rack_id);
        self.keybind_draft_action = KeybindDraftAction::ToggleRackSend;
        self.clear_keybind_draft_combo();
        self.keybind_learn_target_id = None;
        self.keybind_learn_draft = true;
        self.show_keybindings = true;
    }

    pub fn open_keybindings_for_slot(&mut self, rack_id: Uuid, slot_id: Uuid) {
        self.keybind_draft_rack_id = Some(rack_id);
        self.keybind_draft_slot_id = Some(slot_id);
        self.keybind_draft_action = KeybindDraftAction::ToggleSlotBypass;
        if let Some(first_param) = self
            .slot_param_catalog
            .get(&slot_id)
            .and_then(|list| list.first())
        {
            self.keybind_draft_param_id = first_param.id;
            self.keybind_draft_param_value = first_param.value;
        }
        self.clear_keybind_draft_combo();
        self.keybind_learn_target_id = None;
        self.keybind_learn_draft = true;
        self.show_keybindings = true;
    }

    /// Handles Escape cancellation and key capture while Key Bindings learn mode is active.
    pub fn handle_keybind_learn_input(&mut self, ctx: &egui::Context) -> bool {
        let is_learning =
            self.keybind_learn_target_id.is_some() || self.keybind_learn_draft;
        if !is_learning {
            return false;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.keybind_learn_target_id = None;
            self.keybind_learn_draft = false;
            return true;
        }
        if self.show_keybindings {
            if let Some(combo) = capture_pressed_key_combo(ctx) {
                if let Some(binding_id) = self.keybind_learn_target_id.take() {
                    self.push_undo_checkpoint();
                    if let Some(b) = self
                        .session
                        .key_bindings
                        .iter_mut()
                        .find(|b| b.id == binding_id)
                    {
                        b.combo = combo;
                    }
                    if !cfg!(test) {
                        self.save_autosave_workspace();
                    }
                } else if self.keybind_learn_draft {
                    self.keybind_draft_combo = combo;
                    self.keybind_learn_draft = false;
                }
            }
            return true;
        }
        false
    }

    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        let is_learning = self.handle_keybind_learn_input(ctx);

        // Drain system-wide global hotkeys (works even when other apps are focused).
        let global_presses = self
            .global_hotkey
            .as_ref()
            .map(GlobalHotkeyListener::poll_pressed)
            .unwrap_or_default();

        if is_learning {
            return;
        }

        // Only treat the user as typing when the StageHost window is focused and a TextEdit has focus.
        let typing_in_text_edit = ctx.input(|i| i.focused) && ctx.text_edit_focused();
        let mut custom_triggered_builtin = false;
        for pressed in global_presses {
            if typing_in_text_edit && !is_fn_or_mod_combo(&pressed) {
                continue;
            }
            if is_builtin_shortcut(&pressed) && self.has_matching_key_binding(&pressed) {
                custom_triggered_builtin = true;
            }
            self.trigger_matching_key_bindings_deduped(&pressed);
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_settings = false;
            self.show_keybindings = false;
            self.keybind_learn_target_id = None;
            self.keybind_learn_draft = false;
            self.add_slot_target_rack_id = None;
            self.pending_session_action = None;
        }

        if typing_in_text_edit {
            if let Some(pressed) = capture_pressed_key_combo(ctx) {
                if is_fn_or_mod_combo(&pressed) {
                    self.trigger_matching_key_bindings_deduped(&pressed);
                }
            }
            return;
        }

        if custom_triggered_builtin {
            return;
        }

        // Custom user keybindings take precedence over built-in shortcuts when triggered in-window.
        if let Some(pressed) = capture_pressed_key_combo(ctx) {
            if self.has_matching_key_binding(&pressed) {
                self.trigger_matching_key_bindings_deduped(&pressed);
                return;
            }
        }

        let (save_req, undo_req, redo_req) = ctx.input(|i| {
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            let s = ctrl && i.key_pressed(egui::Key::S);
            let z = ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::Z);
            let y = ctrl
                && (i.key_pressed(egui::Key::Y) || (i.modifiers.shift && i.key_pressed(egui::Key::Z)));
            (s, z, y)
        });
        if save_req {
            self.save_session_to_disk();
            return;
        }
        if undo_req {
            self.undo();
            return;
        }
        if redo_req {
            self.redo();
            return;
        }

        if ctx.input(|i| i.key_pressed(egui::Key::PageUp)) {
            self.step_setlist(-1);
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::PageDown)) {
            self.step_setlist(1);
            return;
        }

        let alt_digit = ctx.input(|i| {
            if !i.modifiers.alt {
                return None;
            }
            const NUM_KEYS: [egui::Key; 9] = [
                egui::Key::Num1,
                egui::Key::Num2,
                egui::Key::Num3,
                egui::Key::Num4,
                egui::Key::Num5,
                egui::Key::Num6,
                egui::Key::Num7,
                egui::Key::Num8,
                egui::Key::Num9,
            ];
            NUM_KEYS.iter().position(|&k| i.key_pressed(k))
        });

        if let Some(idx) = alt_digit {
            if let Some(sub) = self.session.sub_sessions.get(idx) {
                let id = sub.id;
                self.activate_sub_session(id);
            }
        }

        // Local capture handled above when matched; keep remaining built-in fallback flow clean.
    }

    fn has_matching_key_binding(&self, pressed: &KeyComboSpec) -> bool {
        self.session
            .key_bindings
            .iter()
            .any(|b| b.enabled && &b.combo == pressed)
    }

    fn trigger_matching_key_bindings_deduped(&mut self, pressed: &KeyComboSpec) {
        const DEDUP_WINDOW: Duration = Duration::from_millis(45);
        let now = Instant::now();
        self.recent_keybind_triggers
            .retain(|(_, ts)| now.duration_since(*ts) < DEDUP_WINDOW);
        if self
            .recent_keybind_triggers
            .iter()
            .any(|(combo, _)| combo == pressed)
        {
            return;
        }
        self.recent_keybind_triggers.push((pressed.clone(), now));
        self.trigger_matching_key_bindings(pressed);
    }

    fn trigger_matching_key_bindings(&mut self, pressed: &KeyComboSpec) {
        let actions: Vec<KeyBindingAction> = self
            .session
            .key_bindings
            .iter()
            .filter(|b| b.enabled && &b.combo == pressed)
            .map(|b| b.action.clone())
            .collect();
        for action in actions {
            self.execute_key_binding_action(&action);
        }
    }

    pub(crate) fn execute_key_binding_action(&mut self, action: &KeyBindingAction) {
        match action {
            KeyBindingAction::ToggleSlotEnabled { rack_id, slot_id } => {
                if let Some(slot) = self.find_slot_mut(*rack_id, *slot_id) {
                    slot.enabled = !slot.enabled;
                    let msg = format!(
                        "Keybind: {} {}",
                        slot.name,
                        if slot.enabled { "Enabled" } else { "Disabled" }
                    );
                    self.sync_params_to_engine();
                    self.set_status(msg, false);
                }
            }
            KeyBindingAction::ToggleSlotBypass { rack_id, slot_id } => {
                if let Some(slot) = self.find_slot_mut(*rack_id, *slot_id) {
                    slot.bypassed = !slot.bypassed;
                    let msg = format!(
                        "Keybind: {} {}",
                        slot.name,
                        if slot.bypassed { "Bypassed" } else { "Active" }
                    );
                    self.sync_params_to_engine();
                    self.set_status(msg, false);
                }
            }
            KeyBindingAction::OpenSlotEditor { rack_id, slot_id } => {
                let title = self
                    .find_slot_mut(*rack_id, *slot_id)
                    .map(|s| s.name.clone());
                if let Some(name) = title {
                    self.open_slot_editor(*rack_id, *slot_id, &name);
                }
            }
            KeyBindingAction::SetSlotParam {
                rack_id,
                slot_id,
                param_id,
                value,
            } => {
                let name = if let Some(slot) = self.find_slot_mut(*rack_id, *slot_id) {
                    slot.params.insert(*param_id, *value);
                    Some(slot.name.clone())
                } else {
                    None
                };
                if let Some(slot_name) = name {
                    self.set_slot_parameter(*rack_id, *slot_id, *param_id, *value);
                    self.set_status(
                        format!("Keybind: {slot_name} param #{param_id} -> {value:.3}"),
                        false,
                    );
                }
            }
            KeyBindingAction::ToggleRackMute { rack_id } => {
                if let Some(rack) = self.session.racks.iter_mut().find(|r| r.id == *rack_id) {
                    rack.muted = !rack.muted;
                    let msg = format!(
                        "Keybind: {} {}",
                        rack.name,
                        if rack.muted { "Muted" } else { "Unmuted" }
                    );
                    self.sync_params_to_engine();
                    self.set_status(msg, false);
                }
            }
            KeyBindingAction::ToggleRackSolo { rack_id } => {
                if let Some(rack) = self.session.racks.iter_mut().find(|r| r.id == *rack_id) {
                    rack.solo = !rack.solo;
                    let msg = format!(
                        "Keybind: {} {}",
                        rack.name,
                        if rack.solo { "Solo ON" } else { "Solo OFF" }
                    );
                    self.sync_params_to_engine();
                    self.set_status(msg, false);
                }
            }
            KeyBindingAction::ToggleRackEnabled { rack_id } => {
                if let Some(rack) = self.session.racks.iter_mut().find(|r| r.id == *rack_id) {
                    rack.enabled = !rack.enabled;
                    let msg = format!(
                        "Keybind: {} {}",
                        rack.name,
                        if rack.enabled { "Enabled" } else { "Disabled" }
                    );
                    self.sync_params_to_engine();
                    self.set_status(msg, false);
                }
            }
            KeyBindingAction::ToggleRackSend {
                rack_id,
                target_rack_id,
            } => {
                let target_name = self
                    .session
                    .racks
                    .iter()
                    .find(|r| r.id == *target_rack_id)
                    .map(|r| r.name.clone())
                    .unwrap_or_else(|| "Target".to_string());
                if let Some(rack) = self.session.racks.iter_mut().find(|r| r.id == *rack_id) {
                    if let Some(send) = rack
                        .sends
                        .iter_mut()
                        .find(|s| s.target_rack_id == *target_rack_id)
                    {
                        send.enabled = !send.enabled;
                        let msg = format!(
                            "Keybind: {} -> {} {}",
                            rack.name,
                            target_name,
                            if send.enabled { "ON" } else { "OFF" }
                        );
                        self.sync_params_to_engine();
                        self.set_status(msg, false);
                    }
                }
            }
            KeyBindingAction::ToggleMultiRackMute { rack_ids } => {
                let mut targets: Vec<_> = self
                    .session
                    .racks
                    .iter_mut()
                    .filter(|r| rack_ids.contains(&r.id))
                    .collect();
                if !targets.is_empty() {
                    let all_muted = targets.iter().all(|r| r.muted);
                    let next = !all_muted;
                    let count = targets.len();
                    for r in &mut targets {
                        r.muted = next;
                    }
                    self.sync_params_to_engine();
                    self.set_status(
                        format!(
                            "Keybind: {count} Racks {}",
                            if next { "Muted" } else { "Unmuted" }
                        ),
                        false,
                    );
                }
            }
            KeyBindingAction::ToggleMultiRackSolo { rack_ids } => {
                let mut targets: Vec<_> = self
                    .session
                    .racks
                    .iter_mut()
                    .filter(|r| rack_ids.contains(&r.id))
                    .collect();
                if !targets.is_empty() {
                    let all_solo = targets.iter().all(|r| r.solo);
                    let next = !all_solo;
                    let count = targets.len();
                    for r in &mut targets {
                        r.solo = next;
                    }
                    self.sync_params_to_engine();
                    self.set_status(
                        format!(
                            "Keybind: {count} Racks Solo {}",
                            if next { "ON" } else { "OFF" }
                        ),
                        false,
                    );
                }
            }
            KeyBindingAction::ToggleMultiRackEnabled { rack_ids } => {
                let mut targets: Vec<_> = self
                    .session
                    .racks
                    .iter_mut()
                    .filter(|r| rack_ids.contains(&r.id))
                    .collect();
                if !targets.is_empty() {
                    let all_enabled = targets.iter().all(|r| r.enabled);
                    let next = !all_enabled;
                    let count = targets.len();
                    for r in &mut targets {
                        r.enabled = next;
                    }
                    self.sync_params_to_engine();
                    self.set_status(
                        format!(
                            "Keybind: {count} Racks {}",
                            if next { "Enabled" } else { "Disabled" }
                        ),
                        false,
                    );
                }
            }
            KeyBindingAction::LoadSessionFile { entry_id, path } => {
                if self.auto_save_on_song_switch && !self.session_path_input.trim().is_empty() {
                    self.save_session_to_disk();
                }
                if let Some(entry) = self
                    .set_list
                    .entries
                    .iter()
                    .find(|e| e.id == *entry_id || (!path.as_os_str().is_empty() && e.session_path == *path))
                    .cloned()
                {
                    let title = entry.title.clone();
                    if !entry.session_path.is_file() {
                        self.remove_session_list_entry(entry.id);
                        self.set_status(
                            format!("Session file '{title}' was moved or deleted; removed from Session List"),
                            true,
                        );
                        return;
                    }
                    self.active_setlist_entry_id = Some(entry.id);
                    self.load_session_or_settings_preset(&entry.session_path, true);
                    if !self.status_banner.as_ref().is_some_and(|(_, is_err)| *is_err) {
                        self.set_status(format!("Keybind: Switched to '{title}'"), false);
                    }
                } else if !path.as_os_str().is_empty() && path.is_file() {
                    self.load_session_or_settings_preset(path, true);
                }
            }
        }
    }

    fn find_slot_mut(&mut self, rack_id: Uuid, slot_id: Uuid) -> Option<&mut PluginSlotConfig> {
        self.session
            .racks
            .iter_mut()
            .find(|r| r.id == rack_id)
            .and_then(|r| r.slots.iter_mut().find(|s| s.id == slot_id))
    }

    pub fn duplicate_rack(&mut self, rack_id: Uuid) {
        self.capture_live_plugin_states();
        let Some(pos) = self.session.racks.iter().position(|r| r.id == rack_id) else {
            return;
        };
        self.push_undo_checkpoint();
        let mut cloned = self.session.racks[pos].clone();
        cloned.id = Uuid::new_v4();
        cloned.name = format!("{} (Copy)", cloned.name);
        for slot in &mut cloned.slots {
            slot.id = Uuid::new_v4();
        }
        let cloned_id = cloned.id;
        let cloned_name = cloned.name.clone();
        self.add_rack_incremental(cloned);
        let added_idx = self.session.racks.len() - 1;
        if pos + 1 < added_idx {
            let item = self.session.racks.remove(added_idx);
            self.session.racks.insert(pos + 1, item);
            self.sync_rack_order_to_engine();
        }
        self.selected_rack_id = Some(cloned_id);
        self.set_status(format!("Duplicated rack '{cloned_name}'"), false);
    }

    pub fn duplicate_slot(&mut self, rack_id: Uuid, slot_idx: usize) {
        self.capture_live_plugin_states();
        let cloned_opt = self
            .session
            .racks
            .iter()
            .find(|r| r.id == rack_id)
            .and_then(|r| r.slots.get(slot_idx).cloned());
        let Some(mut cloned) = cloned_opt else {
            return;
        };
        self.push_undo_checkpoint();
        cloned.id = Uuid::new_v4();
        cloned.name = format!("{} (Copy)", cloned.name);
        let cloned_id = cloned.id;
        let cloned_name = cloned.name.clone();
        if let Some(rack) = self.session.racks.iter_mut().find(|r| r.id == rack_id) {
            rack.slots.insert(slot_idx + 1, cloned.clone());
        }
        if self.audio_engine.is_running() {
            let sample_rate = self.audio_engine.active_sample_rate().max(8_000) as f64;
            let max_block = (self.audio_engine.active_buffer_frames() as usize).max(2048);
            match self.instantiate_slot(&cloned, sample_rate, max_block) {
                Ok(built) => {
                    self.slot_param_catalog.insert(cloned_id, built.params);
                    self.slot_latencies
                        .insert(cloned_id, built.live_slot.latency_samples);
                    self.slot_sidechain_caps
                        .insert(cloned_id, built.supports_sidechain);
                    if let Some(ui) = built.ui_handle {
                        self.slot_ui_handles.insert(cloned_id, ui);
                    }
                    let _ = self.audio_engine.send_command(EngineCommand::AddSlot {
                        rack_id,
                        index: Some(slot_idx + 1),
                        slot: Box::new(built.live_slot),
                    });
                }
                Err(err) => {
                    self.set_status(format!("Duplicate slot failed: {err}"), true);
                    return;
                }
            }
        }
        self.set_status(format!("Duplicated slot '{cloned_name}'"), false);
    }

    pub fn move_rack(&mut self, index: usize, delta: isize) {
        let target = index as isize + delta;
        if target < 0 || (target as usize) >= self.session.racks.len() {
            return;
        }
        self.push_undo_checkpoint();
        self.session.racks.swap(index, target as usize);
        self.sync_rack_order_to_engine();
    }

    pub fn builtin_catalog() -> Vec<(&'static str, &'static str, PluginKind)> {
        vec![
            (
                "3-Band Tone EQ",
                "Zero-latency Low 120Hz, Mid 1kHz, High 4.5kHz stereo shelving/bell EQ",
                PluginKind::BuiltInEffect(BuiltInEffectKind::ThreeBandEq),
            ),
            (
                "Gate & Compressor",
                "Zero-latency stereo noise gate and compressor for live dynamics",
                PluginKind::BuiltInEffect(BuiltInEffectKind::DynamicsGateComp),
            ),
            (
                "Stereo Chorus Delay",
                "Modulated stereo chorus and feedback delay",
                PluginKind::BuiltInEffect(BuiltInEffectKind::StereoChorusDelay),
            ),
            (
                "Stereo Reverb",
                "Algorithmic stereo room and hall reverb",
                PluginKind::BuiltInEffect(BuiltInEffectKind::StereoReverb),
            ),
            (
                "Gain & Safety Limiter",
                "Output gain staging and soft-clip safety limiter",
                PluginKind::BuiltInEffect(BuiltInEffectKind::GainLimiter),
            ),
        ]
    }

    #[cfg(test)]
    pub(crate) fn new_for_ui_test(session: SessionConfig) -> Self {
        let selected_rack_id = session.racks.first().map(|r| r.id);
        Self {
            audio_engine: AudioEngine::new(),
            saved_session_baseline: session.clone(),
            session,
            history: SessionHistory::default(),
            set_list: SetList::default(),
            active_setlist_entry_id: None,
            session_path_input: "session.stagehost.json".to_string(),
            setlist_path_input: "setlist.stagelist.json".to_string(),
            available_audio_devices: Vec::new(),
            selected_audio_device: None,
            available_input_devices: Vec::new(),
            selected_input_device: None,
            asio_input_channel: 0,
            asio_output_channel: 0,
            show_driver_notes: false,
            cached_driver_notes: None,
            driver_notes_error: None,
            input_enabled: true,
            input_gain_db: 0.0,
            input_mode: InputChannelMode::Stereo,
            output_muted: false,
            test_tone_enabled: false,
            tuner_enabled: false,
            auto_save_on_song_switch: false,
            selected_sample_rate: 48_000,
            selected_buffer_frames: 256,
            show_left_pane: true,
            show_io_strip: true,
            plugin_folders: Vec::new(),
            plugin_search_paths: Vec::new(),
            new_plugin_dir_input: String::new(),
            rescan_plugins_on_startup: false,
            start_output_muted: false,
            reopen_last_session: false,
            close_to_tray: true,
            start_in_tray_on_autostart: true,
            hidden_to_tray: false,
            force_exit: false,
            auto_run_on_startup: false,
            auto_run_registered_cmd: None,
            scanned_plugins: Vec::new(),
            last_plugin_scan_issues: Vec::new(),
            plugin_scan_rx: None,
            plugin_filter_query: String::new(),
            plugin_filter_format: PluginFilterFormat::All,
            plugin_filter_role: PluginFilterRole::All,
            plugin_filter_vendor: None,
            selected_browser_plugin_key: None,
            focus_plugin_search_box: false,
            telemetry: AudioTelemetry::default(),
            rack_meters: HashMap::new(),
            editor_windows: PluginEditorWindows::default(),
            slot_ui_handles: HashMap::new(),
            slot_latencies: HashMap::new(),
            slot_sidechain_caps: HashMap::new(),
            show_settings: false,
            settings_category: SettingsCategory::AudioEngine,
            selected_plugin_folder_idx: None,
            show_custom_plugin_path_input: false,
            show_keybindings: false,
            keybind_learn_target_id: None,
            keybind_learn_draft: false,
            keybind_draft_rack_id: None,
            keybind_draft_rack_ids: Vec::new(),
            keybind_draft_slot_id: None,
            keybind_draft_send_target_id: None,
            keybind_draft_session_entry_id: None,
            keybind_draft_action: KeybindDraftAction::ToggleSlotBypass,
            keybind_draft_param_id: 0,
            keybind_draft_param_value: 0.5,
            keybind_draft_combo: KeyComboSpec {
                ctrl: false,
                alt: false,
                shift: false,
                key: String::new(),
            },
            pending_session_action: None,
            add_slot_target_rack_id: None,
            selected_rack_id,
            selected_rack_ids: selected_rack_id.into_iter().collect(),
            renaming_rack_id: None,
            cable_drag_source: None,
            rack_drag_source: None,
            slot_drag_source: None,
            new_sub_session_name: String::new(),
            new_rack_name: String::new(),
            slot_param_catalog: HashMap::new(),
            expanded_param_slots: HashSet::new(),
            expanded_sidechain_slots: HashSet::new(),
            status_banner: None,
            startup_config_path: None,
            last_saved_session_path: None,
            last_saved_setlist_path: None,
            global_hotkey: None,
            system_tray: None,
            recent_keybind_triggers: Vec::new(),
            tuner: StageTuner::new(48_000.0),
            ui_ctx: None,
            wake_signal: None,
            tray_wake_rx: None,
            ui_visible_sent: true,
            input_capture_sent: true,
            autosave_pending_since: None,
            browser_catalog_dirty: true,
            browser_catalog_cache: Vec::new(),
            browser_vendors_cache: Vec::new(),
        }
    }

    fn shrink_ui_caches_for_tray(&mut self) {
        self.rack_meters.clear();
        self.rack_meters.shrink_to_fit();
        self.slot_param_catalog.clear();
        self.slot_param_catalog.shrink_to_fit();
        self.browser_catalog_cache.clear();
        self.browser_catalog_cache.shrink_to_fit();
        self.browser_catalog_dirty = true;
        self.browser_vendors_cache.clear();
        self.browser_vendors_cache.shrink_to_fit();
        self.cached_driver_notes = None;
        self.available_audio_devices.clear();
        self.available_audio_devices.shrink_to_fit();
        self.available_input_devices.clear();
        self.available_input_devices.shrink_to_fit();
        self.history.shrink_to_fit();
    }

    pub fn has_system_tray(&self) -> bool {
        self.system_tray.is_some()
    }

    pub fn attach_ui_context(&mut self, ctx: egui::Context) {
        self.hidden_to_tray = false;
        self.ui_ctx = Some(ctx.clone());
        if let Some(ref wake) = self.wake_signal {
            wake.set_egui_ctx(Some(ctx));
        }
        if !self.ui_visible_sent {
            self.ui_visible_sent = true;
            let _ = self
                .audio_engine
                .send_command(EngineCommand::SetUiVisible(true));
        }
    }

    fn detach_ui_context(&mut self) {
        self.ui_ctx = None;
        if let Some(ref wake) = self.wake_signal {
            wake.set_egui_ctx(None);
        }
    }

    fn enter_tray_sleep(&mut self, ctx: Option<&egui::Context>, send_close_cmd: bool) {
        info!("Closing UI viewport to enter system tray (send_close={send_close_cmd})");
        if !cfg!(test) {
            self.save_preferences();
            self.flush_debounced_autosave(true);
            self.save_autosave_workspace();
        }
        self.close_all_editors();
        self.audio_engine.poll_retired_racks();
        self.show_settings = false;
        self.show_keybindings = false;
        self.add_slot_target_rack_id = None;
        self.hidden_to_tray = true;
        if self.ui_visible_sent {
            self.ui_visible_sent = false;
            let _ = self
                .audio_engine
                .send_command(EngineCommand::SetUiVisible(false));
        }
        self.shrink_ui_caches_for_tray();
        self.detach_ui_context();
        if send_close_cmd {
            if let Some(ctx) = ctx {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    pub fn request_hide_to_tray(&mut self, ctx: &egui::Context) {
        self.enter_tray_sleep(Some(ctx), true);
    }

    pub fn request_restore_from_tray(&mut self, ctx: &egui::Context) {
        self.hidden_to_tray = false;
        if !self.ui_visible_sent {
            self.ui_visible_sent = true;
            let _ = self
                .audio_engine
                .send_command(EngineCommand::SetUiVisible(true));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    pub fn request_exit_app(&mut self, ctx: &egui::Context) {
        self.force_exit = true;
        self.hidden_to_tray = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Runs the low-overhead headless system-tray loop while the UI window/OpenGL context is destroyed.
    /// Returns `true` to reopen the UI window, or `false` to exit StageHost cleanly.
    pub fn wait_in_system_tray(&mut self) -> bool {
        info!("Entered system tray mode; UI and OpenGL context released");
        if let Some(ref rx) = self.tray_wake_rx {
            rx.drain();
        }
        loop {
            if let Some(ref tray) = self.system_tray {
                if let Some(action) = tray.poll_actions().into_iter().last() {
                    match action {
                        TrayAction::ShowWindow => {
                            self.hidden_to_tray = false;
                            return true;
                        }
                        TrayAction::ExitApp => {
                            self.force_exit = true;
                            self.hidden_to_tray = false;
                            return false;
                        }
                    }
                }
            }

            let global_presses = self
                .global_hotkey
                .as_ref()
                .map(GlobalHotkeyListener::poll_pressed)
                .unwrap_or_default();
            for combo in global_presses {
                self.trigger_matching_key_bindings_deduped(&combo);
            }

            self.poll_background_state(false);

            let wait_dur = if self.autosave_pending_since.is_some() || self.is_scanning_plugins() {
                Duration::from_millis(250)
            } else if self.audio_engine.is_running() {
                Duration::from_secs(2)
            } else {
                Duration::from_secs(30)
            };

            if let Some(ref rx) = self.tray_wake_rx {
                rx.wait_timeout(wait_dur);
            } else {
                std::thread::sleep(wait_dur);
            }
        }
    }

    fn handle_tray_and_close(&mut self, ctx: &egui::Context) {
        let tray_actions = self
            .system_tray
            .as_ref()
            .map(SystemTrayHandle::poll_actions)
            .unwrap_or_default();
        for action in tray_actions {
            match action {
                TrayAction::ShowWindow => self.request_restore_from_tray(ctx),
                TrayAction::ExitApp => self.request_exit_app(ctx),
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) && self.close_to_tray && !self.force_exit
        {
            // Allow the OS close to shut down the eframe/OpenGL window while keeping runtime state alive.
            self.enter_tray_sleep(Some(ctx), false);
        }
    }
}

impl eframe::App for StageHostApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.ui_ctx.is_none() {
            self.ui_ctx = Some(ctx.clone());
        }

        self.handle_tray_and_close(&ctx);

        let is_minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let ui_visible = !self.hidden_to_tray && !is_minimized;
        if ui_visible != self.ui_visible_sent && self.audio_engine.is_running() {
            self.ui_visible_sent = ui_visible;
            let _ = self
                .audio_engine
                .send_command(EngineCommand::SetUiVisible(ui_visible));
        }

        self.poll_background_state(ui_visible);
        self.handle_global_shortcuts(&ctx);

        if !ui_visible {
            // Sleep completely in tray/minimized; tray events, global hotkeys, and scan completion wake UI on demand.
            return;
        }

        if self.audio_engine.is_running() {
            let user_active = ctx.input(|i| {
                i.pointer.any_down() || i.pointer.is_moving() || !i.events.is_empty()
            });
            let has_signal = self.test_tone_enabled
                || self.telemetry.master_peak_l > 1e-4
                || self.telemetry.master_peak_r > 1e-4
                || self.telemetry.input_peak_l > 1e-4
                || self.telemetry.input_peak_r > 1e-4
                || self
                    .rack_meters
                    .values()
                    .any(|&(l, r)| l > 1e-4 || r > 1e-4);
            let interval_ms = if user_active || (self.tuner_enabled && has_signal) {
                16
            } else if has_signal || self.is_scanning_plugins() {
                33
            } else {
                200
            };
            ctx.request_repaint_after(Duration::from_millis(interval_ms));
        } else if self.is_scanning_plugins() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }

        egui::Panel::top("top_menu_bar")
            .frame(egui::Frame::new().fill(theme::BG_PANEL).inner_margin(egui::Margin::symmetric(8, 3)))
            .show(ui, |ui| {
                panels::draw_menu_bar(self, ui);
            });

        egui::Panel::top("top_transport")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_CHASSIS)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(8, 6)),
            )
            .show(ui, |ui| {
                panels::draw_top_transport_bar(self, ui);
            });

        egui::Panel::bottom("bottom_status")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_PANEL)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_HAIRLINE))
                    .inner_margin(egui::Margin::symmetric(8, 3)),
            )
            .show(ui, |ui| {
                panels::draw_bottom_status_bar(self, ui);
            });

        if self.show_left_pane {
            egui::Panel::left("left_sidebar")
                .resizable(true)
                .default_size(264.0)
                .size_range(220.0..=380.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::BG_PANEL)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER_HAIRLINE))
                        .inner_margin(egui::Margin::same(8)),
                )
                .show(ui, |ui| {
                    panels::draw_left_sidebar(self, ui);
                });
        }

        if self.show_io_strip {
            egui::Panel::right("right_fx_inspector")
                .resizable(true)
                .default_size(340.0)
                .size_range(290.0..=520.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::FL_INSPECTOR_BG)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER_HAIRLINE))
                        .inner_margin(egui::Margin::same(8)),
                )
                .show(ui, |ui| {
                    mixer_view::draw_right_fx_inspector(self, ui);
                });
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_CHASSIS)
                    .inner_margin(egui::Margin::same(6)),
            )
            .show(ui, |ui| {
                mixer_view::draw_center_mixer_strips(self, ui);
            });

        panels::draw_modals(self, ui.ctx());
    }
}

impl Drop for StageHostApp {
    fn drop(&mut self) {
        self.capture_live_plugin_states();
        self.save_preferences();
        self.autosave_pending_since = None;
        self.save_autosave_workspace();
        self.stop_audio();
    }
}

fn portable_settings_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("stagehost.stagehost-settings.json")))
}

struct InstantiatedSlot {
    live_slot: LiveSlot,
    params: Vec<PluginParamInfo>,
    ui_handle: Option<Box<dyn PluginUiHandle>>,
    supports_sidechain: bool,
    state_warning: Option<String>,
}

fn pick_default_device(devs: &[AudioDeviceInfo], target: &mut Option<String>) {
    if target.is_none() {
        *target = devs
            .iter()
            .find(|d| d.is_default)
            .or_else(|| devs.first())
            .map(|d| d.id_string.clone());
    }
}

const BINDABLE_KEYS: &[(egui::Key, &str)] = &[
    (egui::Key::F1, "F1"),
    (egui::Key::F2, "F2"),
    (egui::Key::F3, "F3"),
    (egui::Key::F4, "F4"),
    (egui::Key::F5, "F5"),
    (egui::Key::F6, "F6"),
    (egui::Key::F7, "F7"),
    (egui::Key::F8, "F8"),
    (egui::Key::F9, "F9"),
    (egui::Key::F10, "F10"),
    (egui::Key::F11, "F11"),
    (egui::Key::F12, "F12"),
    (egui::Key::A, "A"),
    (egui::Key::B, "B"),
    (egui::Key::C, "C"),
    (egui::Key::D, "D"),
    (egui::Key::E, "E"),
    (egui::Key::F, "F"),
    (egui::Key::G, "G"),
    (egui::Key::H, "H"),
    (egui::Key::I, "I"),
    (egui::Key::J, "J"),
    (egui::Key::K, "K"),
    (egui::Key::L, "L"),
    (egui::Key::M, "M"),
    (egui::Key::N, "N"),
    (egui::Key::O, "O"),
    (egui::Key::P, "P"),
    (egui::Key::Q, "Q"),
    (egui::Key::R, "R"),
    (egui::Key::S, "S"),
    (egui::Key::T, "T"),
    (egui::Key::U, "U"),
    (egui::Key::V, "V"),
    (egui::Key::W, "W"),
    (egui::Key::X, "X"),
    (egui::Key::Y, "Y"),
    (egui::Key::Z, "Z"),
    (egui::Key::Num0, "0"),
    (egui::Key::Num1, "1"),
    (egui::Key::Num2, "2"),
    (egui::Key::Num3, "3"),
    (egui::Key::Num4, "4"),
    (egui::Key::Num5, "5"),
    (egui::Key::Num6, "6"),
    (egui::Key::Num7, "7"),
    (egui::Key::Num8, "8"),
    (egui::Key::Num9, "9"),
    (egui::Key::Space, "Space"),
    (egui::Key::Enter, "Enter"),
    (egui::Key::Tab, "Tab"),
    (egui::Key::Insert, "Insert"),
    (egui::Key::Delete, "Delete"),
    (egui::Key::Home, "Home"),
    (egui::Key::End, "End"),
    (egui::Key::ArrowUp, "Up"),
    (egui::Key::ArrowDown, "Down"),
    (egui::Key::ArrowLeft, "Left"),
    (egui::Key::ArrowRight, "Right"),
    (egui::Key::Minus, "-"),
    (egui::Key::Plus, "+"),
    (egui::Key::OpenBracket, "["),
    (egui::Key::CloseBracket, "]"),
    (egui::Key::Semicolon, ";"),
    (egui::Key::Quote, "'"),
    (egui::Key::Comma, ","),
    (egui::Key::Period, "."),
    (egui::Key::Slash, "/"),
    (egui::Key::Backtick, "`"),
];

pub fn capture_pressed_key_combo(ctx: &egui::Context) -> Option<KeyComboSpec> {
    ctx.input(|i| {
        let ctrl = i.modifiers.ctrl || i.modifiers.command;
        let alt = i.modifiers.alt;
        let shift = i.modifiers.shift;
        for &(k, name) in BINDABLE_KEYS {
            if i.key_pressed(k) {
                return Some(KeyComboSpec {
                    ctrl,
                    alt,
                    shift,
                    key: name.to_string(),
                });
            }
        }
        None
    })
}

pub fn is_builtin_shortcut(combo: &KeyComboSpec) -> bool {
    if combo.key.is_empty() {
        return false;
    }
    if combo.ctrl && !combo.alt && matches!(combo.key.as_str(), "S" | "Z" | "Y") {
        return true;
    }
    if !combo.ctrl && !combo.alt && !combo.shift && matches!(combo.key.as_str(), "PageUp" | "PageDown") {
        return true;
    }
    combo.alt
        && !combo.ctrl
        && !combo.shift
        && matches!(
            combo.key.as_str(),
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
        )
}

fn is_fn_or_mod_combo(combo: &KeyComboSpec) -> bool {
    combo.ctrl || combo.alt || (combo.key.starts_with('F') && combo.key.len() >= 2)
}

fn is_first_path_newer_or_only(first: &Path, second: Option<&PathBuf>) -> bool {
    match (
        first.metadata().and_then(|m| m.modified()).ok(),
        second.and_then(|p| p.metadata().and_then(|m| m.modified()).ok()),
    ) {
        (Some(t1), Some(t2)) => t1 >= t2,
        (Some(_), None) => true,
        _ => false,
    }
}

pub fn is_settings_bundle_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".stagehost-settings.json"))
}

fn strip_session_extension(stem: &str) -> &str {
    for suffix in [".stagehost-settings.json", ".stagehost.json", ".json"] {
        if let Some(stripped) = stem.strip_suffix(suffix) {
            return stripped;
        }
        let lower = stem.to_ascii_lowercase();
        if lower.ends_with(suffix) && stem.len() > suffix.len() {
            return &stem[..stem.len() - suffix.len()];
        }
    }
    stem
}

fn derive_session_title_from_file(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(strip_session_extension)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduped_keybind_trigger_prevents_same_press_double_toggle() {
        let mut session = SessionConfig::default();
        let rack_id = session.racks[0].id;
        let combo = KeyComboSpec {
            ctrl: false,
            alt: false,
            shift: false,
            key: "F1".to_string(),
        };
        session.key_bindings.push(KeyBindingConfig {
            id: Uuid::new_v4(),
            combo: combo.clone(),
            action: KeyBindingAction::ToggleRackMute { rack_id },
            enabled: true,
        });

        let mut app = StageHostApp::new_for_ui_test(session);
        assert!(!app.session.racks[0].muted);

        // Dual arrival from both global Win32 hook and local egui pass for the same press.
        app.trigger_matching_key_bindings_deduped(&combo);
        app.trigger_matching_key_bindings_deduped(&combo);
        assert!(app.session.racks[0].muted);
    }

    #[test]
    fn load_session_file_keybind_switches_settings_preset_and_persists_across_switches() {
        let dir = std::env::temp_dir().join(format!("stagehost_session_list_kb_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let preset_a_path = dir.join("clean.stagehost-settings.json");
        let preset_b_path = dir.join("lead.stagehost.json");

        let session_a = SessionConfig {
            name: "Clean Preset".to_string(),
            master_gain_db: -3.0,
            ..Default::default()
        };
        let bundle_a = AppSettingsBundle {
            schema_version: 1,
            preferences: HostPreferences::default(),
            scanned_plugins: Vec::new(),
            active_session: session_a,
            set_list: SetList::default(),
        };
        save_json_file(&preset_a_path, &bundle_a).expect("write settings bundle A");

        let session_b = SessionConfig {
            name: "Lead Session".to_string(),
            master_gain_db: 2.5,
            ..Default::default()
        };
        save_session(&preset_b_path, &session_b).expect("write session B");

        let mut app = StageHostApp::new_for_ui_test(SessionConfig::default());
        app.selected_audio_device = Some("ASIO - Focusrite USB ASIO".to_string());
        app.selected_input_device = Some("Line In (USB)".to_string());
        app.asio_input_channel = 2;
        app.asio_output_channel = 2;
        app.selected_sample_rate = 96_000;
        app.selected_buffer_frames = 128;

        let entry_a = app.add_session_file_to_list(preset_a_path.clone());
        let entry_b = app.add_session_file_to_list(preset_b_path.clone());
        assert_eq!(app.set_list.entries[0].title, "clean");
        assert_eq!(app.set_list.entries[1].title, "lead");

        let key_f1 = KeyComboSpec {
            ctrl: false,
            alt: false,
            shift: false,
            key: "F1".to_string(),
        };
        let key_f2 = KeyComboSpec {
            ctrl: false,
            alt: false,
            shift: false,
            key: "F2".to_string(),
        };
        app.open_keybindings_for_session_entry(entry_a);
        app.keybind_draft_combo = key_f1.clone();
        assert!(app.commit_keybind_draft());

        app.open_keybindings_for_session_entry(entry_b);
        app.keybind_draft_combo = key_f2.clone();
        assert!(app.commit_keybind_draft());

        // Trigger F1 -> loads .stagehost-settings.json while preserving Session List, keybinds, and Audio Engine config.
        app.trigger_matching_key_bindings_deduped(&key_f1);
        assert_eq!(app.session.name, "Clean Preset");
        assert_eq!(app.active_session_config_filename(), "clean.stagehost-settings.json");
        assert_eq!(app.active_setlist_entry_id, Some(entry_a));
        assert_eq!(app.set_list.entries.len(), 2);
        assert_eq!(
            app.selected_audio_device.as_deref(),
            Some("ASIO - Focusrite USB ASIO")
        );
        assert_eq!(app.selected_sample_rate, 96_000);
        assert_eq!(app.selected_buffer_frames, 128);
        assert_eq!(app.asio_input_channel, 2);
        assert_eq!(app.asio_output_channel, 2);

        // Trigger F2 -> switches to .stagehost.json, F1 binding must still remain active!
        app.trigger_matching_key_bindings_deduped(&key_f2);
        assert_eq!(app.session.name, "Lead Session");
        assert_eq!(app.active_session_config_filename(), "lead.stagehost.json");
        assert_eq!(app.active_setlist_entry_id, Some(entry_b));

        // Wait past the dual-source dedup window before pressing F1 again.
        std::thread::sleep(Duration::from_millis(55));
        // Switch back with F1 to confirm LoadSessionFile bindings survived switch.
        app.trigger_matching_key_bindings_deduped(&key_f1);
        assert_eq!(app.session.name, "Clean Preset");
        assert_eq!(app.active_session_config_filename(), "clean.stagehost-settings.json");
        assert_eq!(app.active_setlist_entry_id, Some(entry_a));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_missing_and_manual_remove_cleans_entries_and_keybinds() {
        let dir = std::env::temp_dir().join(format!("stagehost_prune_sl_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let file_a = dir.join("stay.stagehost.json");
        let file_b = dir.join("moved.stagehost.json");
        save_session(&file_a, &SessionConfig::default()).expect("write stay");
        save_session(&file_b, &SessionConfig::default()).expect("write moved");

        let mut app = StageHostApp::new_for_ui_test(SessionConfig::default());
        let id_a = app.add_session_file_to_list(file_a.clone());
        let id_b = app.add_session_file_to_list(file_b.clone());

        app.open_keybindings_for_session_entry(id_a);
        app.keybind_draft_combo = KeyComboSpec {
            ctrl: false,
            alt: false,
            shift: false,
            key: "F3".to_string(),
        };
        assert!(app.commit_keybind_draft());
        app.open_keybindings_for_session_entry(id_b);
        app.keybind_draft_combo = KeyComboSpec {
            ctrl: false,
            alt: false,
            shift: false,
            key: "F4".to_string(),
        };
        assert!(app.commit_keybind_draft());

        // Delete/move file B from disk -> pruning removes entry B & F4, keeps stay & F3.
        std::fs::remove_file(&file_b).expect("remove file_b");
        assert!(app.prune_missing_session_list_entries());
        assert_eq!(app.set_list.entries.len(), 1);
        assert_eq!(app.set_list.entries[0].id, id_a);
        assert_eq!(app.session.key_bindings.len(), 1);

        // Manual removal removes entry A & F3 cleanly.
        app.remove_session_list_entry(id_a);
        assert!(app.set_list.entries.is_empty());
        assert!(app.session.key_bindings.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_config_path_autoloads_chosen_preset_and_keeps_audio_and_keybinds() {
        let dir = std::env::temp_dir().join(format!("stagehost_startup_cfg_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let preset_path = dir.join("live_rig.stagehost-settings.json");
        let autosave_path = dir.join("autosave_session.stagehost.json");

        let startup_sess = SessionConfig {
            name: "Live Rig".to_string(),
            master_gain_db: -4.5,
            ..Default::default()
        };
        let bundle = AppSettingsBundle {
            schema_version: 1,
            preferences: HostPreferences {
                output_device: Some("WASAPI - Old Device".to_string()),
                ..HostPreferences::default()
            },
            scanned_plugins: Vec::new(),
            active_session: startup_sess,
            set_list: SetList::default(),
        };
        save_json_file(&preset_path, &bundle).expect("save startup preset");

        let mut autosave_sess = SessionConfig::default();
        let switch_entry_id = Uuid::new_v4();
        autosave_sess.key_bindings.push(KeyBindingConfig {
            id: Uuid::new_v4(),
            enabled: true,
            combo: KeyComboSpec {
                ctrl: false,
                alt: false,
                shift: false,
                key: "F8".to_string(),
            },
            action: KeyBindingAction::LoadSessionFile {
                entry_id: switch_entry_id,
                path: preset_path.clone(),
            },
        });
        save_session(&autosave_path, &autosave_sess).expect("save autosave");

        let mut app = StageHostApp::new_for_ui_test(SessionConfig::default());
        app.selected_audio_device = Some("ASIO - Voicemeeter Insert Virtual ASIO".to_string());
        app.set_startup_config_path(Some(preset_path.clone()));

        assert!(app.load_startup_config_from_preferences(&autosave_path));
        assert_eq!(app.session.name, "Live Rig");
        assert!((app.session.master_gain_db - (-4.5)).abs() < 1e-4);
        assert_eq!(
            app.selected_audio_device.as_deref(),
            Some("ASIO - Voicemeeter Insert Virtual ASIO")
        );
        assert_eq!(app.last_saved_session_path.as_ref(), Some(&preset_path));
        assert!(app.session.key_bindings.iter().any(|b| {
            matches!(&b.action, KeyBindingAction::LoadSessionFile { entry_id, .. } if *entry_id == switch_entry_id)
        }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_to_tray_hides_restores_and_exits_cleanly() {
        let mut app = StageHostApp::new_for_ui_test(SessionConfig::default());
        assert!(app.close_to_tray);
        assert!(!app.hidden_to_tray);
        assert!(!app.force_exit);

        let ctx = egui::Context::default();
        app.request_hide_to_tray(&ctx);
        assert!(app.hidden_to_tray);
        assert!(!app.force_exit);

        app.request_restore_from_tray(&ctx);
        assert!(!app.hidden_to_tray);

        app.request_exit_app(&ctx);
        assert!(app.force_exit);
        assert!(!app.hidden_to_tray);
    }
}

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::SessionError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltInEffectKind {
    StereoChorusDelay,
    GainLimiter,
    ThreeBandEq,
    DynamicsGateComp,
    StereoReverb,
}

fn default_true() -> bool {
    true
}

fn default_one() -> f32 {
    1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RackSendConfig {
    pub target_rack_id: Uuid,
    pub level_db: f32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputChannelMode {
    #[default]
    Stereo,
    MonoLeft,
    MonoRight,
    MonoSum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SidechainSourceKind {
    Rack {
        rack_id: Uuid,
        #[serde(default)]
        pre_fader: bool,
    },
    LiveInput {
        #[serde(default)]
        mode: InputChannelMode,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SidechainConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub source: SidechainSourceKind,
    #[serde(default)]
    pub gain_db: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginKind {
    BuiltInEffect(BuiltInEffectKind),
    Clap { path: PathBuf, plugin_id: String },
    Vst3 { path: PathBuf, class_id: String },
    Vst2 { path: PathBuf, unique_id: i32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginSlotConfig {
    pub id: Uuid,
    pub name: String,
    pub kind: PluginKind,
    pub enabled: bool,
    pub bypassed: bool,
    pub gain_db: f32,
    pub pan: f32,
    /// `0.0` = 100% dry, `1.0` = 100% wet.
    pub wet_mix: f32,
    pub preset_name: String,
    pub state_base64: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<u32, f32>,
    #[serde(default)]
    pub sidechain: Option<SidechainConfig>,
}

impl PluginSlotConfig {
    pub fn new(name: impl Into<String>, kind: PluginKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind,
            enabled: true,
            bypassed: false,
            gain_db: 0.0,
            pan: 0.0,
            wet_mix: 1.0,
            preset_name: "Default".to_string(),
            state_base64: None,
            params: BTreeMap::new(),
            sidechain: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RackConfig {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub muted: bool,
    pub solo: bool,
    pub gain_db: f32,
    pub pan: f32,
    #[serde(default = "default_one")]
    pub width: f32,
    #[serde(default = "default_true")]
    pub receive_input: bool,
    #[serde(default = "default_true")]
    pub route_to_master: bool,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub group_id: Option<Uuid>,
    #[serde(default)]
    pub sends: Vec<RackSendConfig>,
    pub slots: Vec<PluginSlotConfig>,
}

impl RackConfig {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            enabled: true,
            muted: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
            width: 1.0,
            receive_input: true,
            route_to_master: true,
            color: None,
            group_id: None,
            sends: Vec::new(),
            slots: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotSnapshot {
    pub enabled: bool,
    pub bypassed: bool,
    pub gain_db: f32,
    pub pan: f32,
    pub wet_mix: f32,
    pub preset_name: String,
    #[serde(default)]
    pub params: BTreeMap<u32, f32>,
    #[serde(default)]
    pub sidechain: Option<SidechainConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RackSnapshot {
    pub enabled: bool,
    pub muted: bool,
    pub solo: bool,
    pub gain_db: f32,
    pub pan: f32,
    #[serde(default = "default_one")]
    pub width: f32,
    #[serde(default = "default_true")]
    pub receive_input: bool,
    #[serde(default = "default_true")]
    pub route_to_master: bool,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub group_id: Option<Uuid>,
    #[serde(default)]
    pub sends: Vec<RackSendConfig>,
    pub slots: BTreeMap<Uuid, SlotSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubSessionSnapshot {
    pub id: Uuid,
    pub name: String,
    pub racks: BTreeMap<Uuid, RackSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyComboSpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Normalized key identifier (e.g. "F1", "B", "Space", "Num1").
    pub key: String,
}

impl KeyComboSpec {
    pub fn display_label(&self) -> String {
        if self.key.is_empty() {
            return "<Unassigned>".to_string();
        }
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(self.key.as_str());
        parts.join("+")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum KeyBindingAction {
    ToggleSlotEnabled {
        rack_id: Uuid,
        slot_id: Uuid,
    },
    ToggleSlotBypass {
        rack_id: Uuid,
        slot_id: Uuid,
    },
    OpenSlotEditor {
        rack_id: Uuid,
        slot_id: Uuid,
    },
    SetSlotParam {
        rack_id: Uuid,
        slot_id: Uuid,
        param_id: u32,
        value: f32,
    },
    ToggleRackMute {
        rack_id: Uuid,
    },
    ToggleRackSolo {
        rack_id: Uuid,
    },
    ToggleRackEnabled {
        rack_id: Uuid,
    },
    ToggleRackSend {
        rack_id: Uuid,
        target_rack_id: Uuid,
    },
    ToggleMultiRackMute {
        rack_ids: Vec<Uuid>,
    },
    ToggleMultiRackSolo {
        rack_ids: Vec<Uuid>,
    },
    ToggleMultiRackEnabled {
        rack_ids: Vec<Uuid>,
    },
    LoadSessionFile {
        entry_id: Uuid,
        #[serde(default)]
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyBindingConfig {
    pub id: Uuid,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub combo: KeyComboSpec,
    pub action: KeyBindingAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub schema_version: u32,
    pub name: String,
    pub master_gain_db: f32,
    pub racks: Vec<RackConfig>,
    pub sub_sessions: Vec<SubSessionSnapshot>,
    pub active_sub_session: Option<Uuid>,
    #[serde(default)]
    pub key_bindings: Vec<KeyBindingConfig>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        let mut main_rack = RackConfig::new("Main FX");
        main_rack.slots.push(PluginSlotConfig::new(
            "Stereo Chorus Delay",
            PluginKind::BuiltInEffect(BuiltInEffectKind::StereoChorusDelay),
        ));
        main_rack.slots.push(PluginSlotConfig::new(
            "Gain & Safety Limiter",
            PluginKind::BuiltInEffect(BuiltInEffectKind::GainLimiter),
        ));

        let mut session = Self {
            schema_version: 2,
            name: "Untitled Session".to_string(),
            master_gain_db: 0.0,
            racks: vec![main_rack],
            sub_sessions: Vec::new(),
            active_sub_session: None,
            key_bindings: Vec::new(),
        };
        let default_snap = session.capture_sub_session("Default");
        session.active_sub_session = Some(default_snap.id);
        session.sub_sessions.push(default_snap);
        session
    }
}

impl SessionConfig {
    pub fn capture_sub_session(&self, name: impl Into<String>) -> SubSessionSnapshot {
        let mut racks = BTreeMap::new();
        for rack in &self.racks {
            let mut slots = BTreeMap::new();
            for slot in &rack.slots {
                slots.insert(
                    slot.id,
                    SlotSnapshot {
                        enabled: slot.enabled,
                        bypassed: slot.bypassed,
                        gain_db: slot.gain_db,
                        pan: slot.pan,
                        wet_mix: slot.wet_mix,
                        preset_name: slot.preset_name.clone(),
                        params: slot.params.clone(),
                        sidechain: slot.sidechain.clone(),
                    },
                );
            }
            racks.insert(
                rack.id,
                RackSnapshot {
                    enabled: rack.enabled,
                    muted: rack.muted,
                    solo: rack.solo,
                    gain_db: rack.gain_db,
                    pan: rack.pan,
                    width: rack.width,
                    receive_input: rack.receive_input,
                    route_to_master: rack.route_to_master,
                    color: rack.color,
                    group_id: rack.group_id,
                    sends: rack.sends.clone(),
                    slots,
                },
            );
        }
        SubSessionSnapshot {
            id: Uuid::new_v4(),
            name: name.into(),
            racks,
        }
    }

    pub fn update_sub_session(&mut self, sub_session_id: Uuid) -> Result<(), SessionError> {
        let captured = self.capture_sub_session("");
        let target = self
            .sub_sessions
            .iter_mut()
            .find(|s| s.id == sub_session_id)
            .ok_or_else(|| SessionError::SubSessionNotFound {
                id: sub_session_id,
                session_name: self.name.clone(),
            })?;
        target.racks = captured.racks;
        Ok(())
    }

    /// Applies a stored Sub-Session snapshot without reloading plugin instances.
    pub fn activate_sub_session(&mut self, sub_session_id: Uuid) -> Result<(), SessionError> {
        let snapshot = self
            .sub_sessions
            .iter()
            .find(|s| s.id == sub_session_id)
            .cloned()
            .ok_or_else(|| SessionError::SubSessionNotFound {
                id: sub_session_id,
                session_name: self.name.clone(),
            })?;

        for rack in &mut self.racks {
            if let Some(rack_snap) = snapshot.racks.get(&rack.id) {
                rack.enabled = rack_snap.enabled;
                rack.muted = rack_snap.muted;
                rack.solo = rack_snap.solo;
                rack.gain_db = rack_snap.gain_db;
                rack.pan = rack_snap.pan;
                rack.width = rack_snap.width;
                rack.receive_input = rack_snap.receive_input;
                rack.route_to_master = rack_snap.route_to_master;
                rack.color = rack_snap.color;
                rack.group_id = rack_snap.group_id;
                rack.sends = rack_snap.sends.clone();
                for slot in &mut rack.slots {
                    if let Some(slot_snap) = rack_snap.slots.get(&slot.id) {
                        slot.enabled = slot_snap.enabled;
                        slot.bypassed = slot_snap.bypassed;
                        slot.gain_db = slot_snap.gain_db;
                        slot.pan = slot_snap.pan;
                        slot.wet_mix = slot_snap.wet_mix;
                        slot.preset_name = slot_snap.preset_name.clone();
                        slot.params = slot_snap.params.clone();
                        slot.sidechain = slot_snap.sidechain.clone();
                    }
                }
            }
        }
        self.active_sub_session = Some(sub_session_id);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetListEntry {
    pub id: Uuid,
    pub title: String,
    pub session_path: PathBuf,
    pub notes: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetList {
    pub schema_version: u32,
    pub name: String,
    pub entries: Vec<SetListEntry>,
}

impl Default for SetList {
    fn default() -> Self {
        Self {
            schema_version: 1,
            name: "Live Set List".to_string(),
            entries: Vec::new(),
        }
    }
}

/// A plugin search directory with an individual enable/disable flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginFolderEntry {
    pub path: PathBuf,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Persistent host audio/scanner preferences saved across application restarts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostPreferences {
    pub schema_version: u32,
    pub output_device: Option<String>,
    pub input_device: Option<String>,
    #[serde(default)]
    pub asio_input_channel: usize,
    #[serde(default)]
    pub asio_output_channel: usize,
    pub input_enabled: bool,
    pub input_gain_db: f32,
    #[serde(default)]
    pub input_mode: InputChannelMode,
    pub sample_rate: u32,
    pub buffer_frames: u32,
    #[serde(default)]
    pub plugin_search_paths: Vec<PathBuf>,
    #[serde(default)]
    pub plugin_folders: Vec<PluginFolderEntry>,
    #[serde(default)]
    pub rescan_plugins_on_startup: bool,
    #[serde(default)]
    pub start_output_muted: bool,
    #[serde(default = "default_true")]
    pub reopen_last_session: bool,
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    #[serde(default = "default_true")]
    pub start_in_tray_on_autostart: bool,
    #[serde(default = "default_true")]
    pub show_left_pane: bool,
    #[serde(default = "default_true")]
    pub show_io_strip: bool,
    #[serde(default)]
    pub auto_save_on_song_switch: bool,
    #[serde(default)]
    pub tuner_enabled: bool,
    #[serde(default)]
    pub selected_rack_id: Option<Uuid>,
    #[serde(default)]
    pub startup_config_path: Option<PathBuf>,
    pub last_session_path: Option<PathBuf>,
    pub last_setlist_path: Option<PathBuf>,
}

impl Default for HostPreferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            output_device: None,
            input_device: None,
            asio_input_channel: 0,
            asio_output_channel: 0,
            input_enabled: true,
            input_gain_db: 0.0,
            input_mode: InputChannelMode::Stereo,
            sample_rate: 48_000,
            buffer_frames: 256,
            plugin_search_paths: Vec::new(),
            plugin_folders: Vec::new(),
            rescan_plugins_on_startup: false,
            start_output_muted: false,
            reopen_last_session: true,
            close_to_tray: true,
            start_in_tray_on_autostart: true,
            show_left_pane: true,
            show_io_strip: true,
            auto_save_on_song_switch: false,
            tuner_enabled: false,
            selected_rack_id: None,
            startup_config_path: None,
            last_session_path: None,
            last_setlist_path: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_session_switching_restores_rack_and_slot_state() {
        let mut session = SessionConfig::default();
        let intro_id = session.active_sub_session.expect("default sub-session");

        session.racks[0].gain_db = -6.0;
        session.racks[0].slots[1].bypassed = true;
        session.racks[0].slots[0].params.insert(0, 0.25);
        let chorus = session.capture_sub_session("Chorus");
        let chorus_id = chorus.id;
        session.sub_sessions.push(chorus);

        session.activate_sub_session(intro_id).expect("activate intro");
        assert_eq!(session.racks[0].gain_db, 0.0);
        assert!(!session.racks[0].slots[1].bypassed);
        assert!(session.racks[0].slots[0].params.is_empty());

        session.activate_sub_session(chorus_id).expect("activate chorus");
        assert_eq!(session.racks[0].gain_db, -6.0);
        assert!(session.racks[0].slots[1].bypassed);
        assert_eq!(session.racks[0].slots[0].params.get(&0).copied(), Some(0.25));
    }

    #[test]
    fn key_bindings_roundtrip_and_display_label() {
        let mut session = SessionConfig::default();
        let rack_id = session.racks[0].id;
        let slot_id = session.racks[0].slots[0].id;
        let binding = KeyBindingConfig {
            id: Uuid::new_v4(),
            enabled: true,
            combo: KeyComboSpec {
                ctrl: true,
                alt: false,
                shift: true,
                key: "F5".to_string(),
            },
            action: KeyBindingAction::ToggleSlotBypass { rack_id, slot_id },
        };
        assert_eq!(binding.combo.display_label(), "Ctrl+Shift+F5");
        session.key_bindings.push(binding.clone());

        let json = serde_json::to_string(&session).expect("serialize");
        let loaded: SessionConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(loaded.key_bindings, vec![binding]);

        let target_rack_id = Uuid::new_v4();
        let rack_bindings = vec![
            KeyBindingConfig {
                id: Uuid::new_v4(),
                enabled: true,
                combo: KeyComboSpec {
                    ctrl: false,
                    alt: false,
                    shift: false,
                    key: "F6".to_string(),
                },
                action: KeyBindingAction::ToggleRackEnabled { rack_id },
            },
            KeyBindingConfig {
                id: Uuid::new_v4(),
                enabled: true,
                combo: KeyComboSpec {
                    ctrl: false,
                    alt: false,
                    shift: false,
                    key: "F7".to_string(),
                },
                action: KeyBindingAction::ToggleRackSend {
                    rack_id,
                    target_rack_id,
                },
            },
        ];
        session.key_bindings.extend(rack_bindings.clone());
        let json2 = serde_json::to_string(&session).expect("serialize rack bindings");
        let loaded2: SessionConfig = serde_json::from_str(&json2).expect("deserialize rack bindings");
        assert_eq!(loaded2.key_bindings.len(), 3);
        assert_eq!(loaded2.key_bindings[1..], rack_bindings[..]);
    }

    #[test]
    fn host_preferences_persist_plugin_folders_and_startup_flags() {
        let prefs = HostPreferences {
            plugin_folders: vec![
                PluginFolderEntry {
                    path: PathBuf::from(r"C:\Program Files\Common Files\VST3"),
                    enabled: true,
                },
                PluginFolderEntry {
                    path: PathBuf::from(r"S:\VST"),
                    enabled: false,
                },
            ],
            rescan_plugins_on_startup: true,
            start_output_muted: true,
            reopen_last_session: false,
            close_to_tray: false,
            start_in_tray_on_autostart: false,
            startup_config_path: Some(PathBuf::from(r"F:\agimic.stagehost.json")),
            ..HostPreferences::default()
        };
        let json = serde_json::to_string(&prefs).expect("serialize prefs");
        let loaded: HostPreferences = serde_json::from_str(&json).expect("deserialize prefs");
        assert_eq!(loaded.plugin_folders.len(), 2);
        assert!(loaded.plugin_folders[0].enabled);
        assert!(!loaded.plugin_folders[1].enabled);
        assert!(loaded.rescan_plugins_on_startup);
        assert!(loaded.start_output_muted);
        assert!(!loaded.reopen_last_session);
        assert!(!loaded.close_to_tray);
        assert!(HostPreferences::default().close_to_tray);
        assert!(!loaded.start_in_tray_on_autostart);
        assert!(HostPreferences::default().start_in_tray_on_autostart);
        assert_eq!(
            loaded.startup_config_path,
            Some(PathBuf::from(r"F:\agimic.stagehost.json"))
        );
    }

    #[test]
    fn sidechain_config_roundtrips_json_and_sub_sessions() {
        let mut session = SessionConfig::default();
        let default_id = session.active_sub_session.expect("default sub-session");
        let key_rack = RackConfig::new("Key Source");
        let key_rack_id = key_rack.id;
        session.racks.push(key_rack);

        session.racks[0].slots[0].sidechain = Some(SidechainConfig {
            enabled: true,
            source: SidechainSourceKind::Rack {
                rack_id: key_rack_id,
                pre_fader: true,
            },
            gain_db: 3.5,
        });
        let duck_snap = session.capture_sub_session("Ducking");
        let duck_id = duck_snap.id;
        session.sub_sessions.push(duck_snap);

        session.activate_sub_session(default_id).expect("activate default");
        assert!(session.racks[0].slots[0].sidechain.is_none());

        session.activate_sub_session(duck_id).expect("activate ducking");
        let sc = session.racks[0].slots[0]
            .sidechain
            .as_ref()
            .expect("restored sidechain");
        assert!(sc.enabled);
        assert_eq!(sc.gain_db, 3.5);
        assert_eq!(
            sc.source,
            SidechainSourceKind::Rack {
                rack_id: key_rack_id,
                pre_fader: true,
            }
        );

        let json = serde_json::to_string(&session).expect("serialize");
        let decoded: SessionConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, session);
    }

    #[test]
    fn rack_route_to_master_and_color_roundtrip_sub_sessions() {
        let mut session = SessionConfig::default();
        let default_id = session.active_sub_session.expect("default sub-session");
        session.racks[0].route_to_master = false;
        session.racks[0].color = Some([84, 132, 210]);
        let bus_snap = session.capture_sub_session("Bus Only");
        let bus_id = bus_snap.id;
        session.sub_sessions.push(bus_snap);

        session.activate_sub_session(default_id).expect("activate default");
        assert!(session.racks[0].route_to_master);
        assert_eq!(session.racks[0].color, None);

        session.activate_sub_session(bus_id).expect("activate bus");
        assert!(!session.racks[0].route_to_master);
        assert_eq!(session.racks[0].color, Some([84, 132, 210]));
    }

    #[test]
    fn multi_rack_key_bindings_roundtrip_json() {
        let mut session = SessionConfig::default();
        let r1 = session.racks[0].id;
        let r2 = Uuid::new_v4();
        let binding = KeyBindingConfig {
            id: Uuid::new_v4(),
            enabled: true,
            combo: KeyComboSpec {
                ctrl: false,
                alt: false,
                shift: false,
                key: "F9".to_string(),
            },
            action: KeyBindingAction::ToggleMultiRackMute {
                rack_ids: vec![r1, r2],
            },
        };
        session.key_bindings.push(binding.clone());
        let json = serde_json::to_string(&session).expect("serialize");
        let decoded: SessionConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.key_bindings, vec![binding]);
    }

    #[test]
    fn load_session_file_key_binding_roundtrips_json() {
        let mut session = SessionConfig::default();
        let entry_id = Uuid::new_v4();
        let binding = KeyBindingConfig {
            id: Uuid::new_v4(),
            enabled: true,
            combo: KeyComboSpec {
                ctrl: false,
                alt: false,
                shift: false,
                key: "F10".to_string(),
            },
            action: KeyBindingAction::LoadSessionFile {
                entry_id,
                path: PathBuf::from("presets/vocal.stagehost-settings.json"),
            },
        };
        session.key_bindings.push(binding.clone());
        let json = serde_json::to_string(&session).expect("serialize");
        let decoded: SessionConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.key_bindings, vec![binding]);
    }
}

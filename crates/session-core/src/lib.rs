//! Session hierarchy: Racks, Plugin Slots, Sub-Sessions, and Set Lists.

pub mod error;
pub mod history;
pub mod model;
pub mod persistence;

pub use error::SessionError;
pub use history::SessionHistory;
pub use model::{
    BuiltInEffectKind, HostPreferences, InputChannelMode, KeyBindingAction, KeyBindingConfig,
    KeyComboSpec, PluginFolderEntry, PluginKind, PluginSlotConfig, RackConfig, RackSendConfig,
    RackSnapshot, SessionConfig, SetList, SetListEntry, SidechainConfig, SidechainSourceKind,
    SlotSnapshot, SubSessionSnapshot,
};
pub use persistence::{
    decode_base64, default_app_config_dir, encode_base64, load_host_preferences, load_json_file,
    load_session, load_set_list, save_host_preferences, save_json_file, save_session,
    save_set_list,
};

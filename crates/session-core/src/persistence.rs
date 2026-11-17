use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{HostPreferences, SessionConfig, SessionError, SetList};

pub fn default_app_config_dir() -> PathBuf {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("StageHostRS");
    }
    PathBuf::from(".stagehost")
}

pub fn save_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), SessionError> {
    write_json_atomic(path, value)
}

pub fn load_json_file<T: DeserializeOwned>(path: &Path) -> Result<T, SessionError> {
    read_json(path)
}

pub fn save_host_preferences(path: &Path, prefs: &HostPreferences) -> Result<(), SessionError> {
    write_json_atomic(path, prefs)
}

pub fn load_host_preferences(path: &Path) -> Result<HostPreferences, SessionError> {
    read_json(path)
}

pub fn save_session(path: &Path, session: &SessionConfig) -> Result<(), SessionError> {
    write_json_atomic(path, session)
}

pub fn load_session(path: &Path) -> Result<SessionConfig, SessionError> {
    read_json(path)
}

pub fn save_set_list(path: &Path, set_list: &SetList) -> Result<(), SessionError> {
    write_json_atomic(path, set_list)
}

pub fn load_set_list(path: &Path) -> Result<SetList, SessionError> {
    read_json(path)
}

const B64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes opaque plugin binary chunks to standard RFC 4648 base64 text.
pub fn encode_base64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(B64_ALPHABET[(triple >> 18) & 0x3F] as char);
        out.push(B64_ALPHABET[(triple >> 12) & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            B64_ALPHABET[(triple >> 6) & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_ALPHABET[triple & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes RFC 4648 base64 text back into plugin binary bytes.
pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let clean: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if !clean.len().is_multiple_of(4) {
        return None;
    }

    let mut out = Vec::with_capacity((clean.len() / 4) * 3);
    for quartet in clean.as_chunks::<4>().0 {
        let mut vals = [0u8; 4];
        let mut pad = 0usize;
        for (i, &ch) in quartet.iter().enumerate() {
            if ch == b'=' {
                pad += 1;
                vals[i] = 0;
            } else {
                vals[i] = decode_b64_char(ch)?;
            }
        }
        let triple = ((vals[0] as u32) << 18)
            | ((vals[1] as u32) << 12)
            | ((vals[2] as u32) << 6)
            | (vals[3] as u32);
        out.push(((triple >> 16) & 0xFF) as u8);
        if pad < 2 {
            out.push(((triple >> 8) & 0xFF) as u8);
        }
        if pad == 0 {
            out.push((triple & 0xFF) as u8);
        }
    }
    Some(out)
}

fn decode_b64_char(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), SessionError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| SessionError::IoWrite {
                path: path.to_path_buf(),
                source,
            })?;
        }
    }

    let json = serde_json::to_string_pretty(value).map_err(|source| SessionError::JsonSerialize {
        path: path.to_path_buf(),
        source,
    })?;

    // Write via temp file before rename so partial writes cannot corrupt saved sessions.
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, json.as_bytes()).map_err(|source| SessionError::IoWrite {
        path: tmp_path.clone(),
        source,
    })?;

    fs::rename(&tmp_path, path).map_err(|source| SessionError::IoWrite {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, SessionError> {
    let raw = fs::read_to_string(path).map_err(|source| SessionError::IoRead {
        path: path.to_path_buf(),
        source,
    })?;

    serde_json::from_str(&raw).map_err(|source| SessionError::JsonParse {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_session_json() {
        let dir = std::env::temp_dir().join("stagehost_session_test");
        let file = dir.join("test_session.stagehost.json");
        let session = SessionConfig::default();

        save_session(&file, &session).expect("save");
        let loaded = load_session(&file).expect("load");
        assert_eq!(loaded, session);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn roundtrips_binary_state_base64() {
        let payload: Vec<u8> = (0..=255).collect();
        let encoded = encode_base64(&payload);
        let decoded = decode_base64(&encoded).expect("valid base64");
        assert_eq!(decoded, payload);
        assert!(decode_base64("not_valid!").is_none());
    }

    #[test]
    fn roundtrips_host_preferences_json() {
        let dir = std::env::temp_dir().join("stagehost_prefs_test");
        let file = dir.join("prefs.json");
        let prefs = HostPreferences {
            output_device: Some("Studio Out".to_string()),
            input_device: Some("Studio In".to_string()),
            input_enabled: false,
            input_gain_db: -3.5,
            sample_rate: 96_000,
            buffer_frames: 128,
            plugin_search_paths: vec![PathBuf::from("C:\\Plugins\\VST3")],
            last_session_path: Some(PathBuf::from("song.stagehost.json")),
            last_setlist_path: None,
            ..HostPreferences::default()
        };
        save_host_preferences(&file, &prefs).expect("save prefs");
        let loaded = load_host_preferences(&file).expect("load prefs");
        assert_eq!(loaded, prefs);
        let _ = fs::remove_dir_all(&dir);
    }
}

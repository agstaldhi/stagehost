use std::path::Path;

pub const RUN_VALUE_NAME: &str = "StageHost";

/// Builds the startup command line written to the OS run entry.
pub fn format_autostart_command(exe_path: &Path) -> String {
    format!("\"{}\" --autostart", exe_path.display())
}

/// Checks whether a registered Run command points to the same executable path.
pub fn command_matches_exe(command: &str, exe_path: &Path) -> bool {
    let trimmed = command.trim();
    let exe_str = exe_path.to_string_lossy();
    if let Some(after_quote) = trimmed.strip_prefix('"') {
        if let Some((quoted_path, _)) = after_quote.split_once('"') {
            return quoted_path.eq_ignore_ascii_case(exe_str.as_ref());
        }
    }
    let first_token = trimmed.split_whitespace().next().unwrap_or("");
    first_token.eq_ignore_ascii_case(exe_str.as_ref())
}

#[cfg(windows)]
mod win {
    use std::path::Path;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SAM_FLAGS, REG_SZ,
        REG_VALUE_TYPE,
    };

    use super::{format_autostart_command, RUN_VALUE_NAME};

    const RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

    struct RegKeyGuard(HKEY);

    impl Drop for RegKeyGuard {
        fn drop(&mut self) {
            // SAFETY: Handle came from a successful RegOpenKeyExW call.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn to_wide_null(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn open_run_key(access: REG_SAM_FLAGS) -> Result<RegKeyGuard, windows::Win32::Foundation::WIN32_ERROR> {
        let subkey_w = to_wide_null(RUN_SUBKEY);
        let mut hkey = HKEY::default();
        // SAFETY: subkey_w is NUL-terminated UTF-16 and hkey receives an owned registry handle.
        let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(subkey_w.as_ptr()), None, access, &mut hkey) };
        if status == ERROR_SUCCESS {
            Ok(RegKeyGuard(hkey))
        } else {
            Err(status)
        }
    }

    pub fn registered_auto_run_command() -> Result<Option<String>, String> {
        let key = match open_run_key(KEY_READ) {
            Ok(guard) => guard,
            Err(err) if err == ERROR_FILE_NOT_FOUND => return Ok(None),
            Err(err) => {
                return Err(format!(
                    "Failed to open Windows Startup registry key (code {})",
                    err.0
                ))
            }
        };

        let val_w = to_wide_null(RUN_VALUE_NAME);
        let mut val_type = REG_VALUE_TYPE::default();
        let mut byte_len: u32 = 0;
        // SAFETY: Queries required byte size first with NULL buffer.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(val_w.as_ptr()),
                None,
                Some(&mut val_type),
                None,
                Some(&mut byte_len),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
            return Err(format!(
                "Failed to query Windows Startup registry entry (code {})",
                status.0
            ));
        }
        if val_type != REG_SZ && val_type != REG_EXPAND_SZ {
            return Ok(None);
        }
        if byte_len == 0 {
            return Ok(Some(String::new()));
        }

        let word_len = (byte_len as usize).div_ceil(2);
        let mut buf = vec![0u16; word_len];
        let mut actual_bytes = (buf.len() * std::mem::size_of::<u16>()) as u32;
        // SAFETY: buf is sized to at least byte_len bytes aligned as u16.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(val_w.as_ptr()),
                None,
                Some(&mut val_type),
                Some(buf.as_mut_ptr().cast::<u8>()),
                Some(&mut actual_bytes),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!(
                "Failed to read Windows Startup registry value (code {})",
                status.0
            ));
        }

        let actual_words = (actual_bytes as usize) / std::mem::size_of::<u16>();
        let slice = &buf[..actual_words.min(buf.len())];
        let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
        Ok(Some(String::from_utf16_lossy(&slice[..end])))
    }

    pub fn set_auto_run_for_exe(enabled: bool, exe_path: &Path) -> Result<(), String> {
        let key = open_run_key(KEY_SET_VALUE).map_err(|err| {
            format!(
                "Failed to open Windows Startup registry key for writing (code {})",
                err.0
            )
        })?;
        let val_w = to_wide_null(RUN_VALUE_NAME);

        if !enabled {
            // SAFETY: key.0 is open with KEY_SET_VALUE and val_w is NUL-terminated.
            let status = unsafe { RegDeleteValueW(key.0, PCWSTR(val_w.as_ptr())) };
            if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            return Err(format!(
                "Failed to remove Windows Startup entry (code {})",
                status.0
            ));
        }

        let cmd = format_autostart_command(exe_path);
        let cmd_w = to_wide_null(&cmd);
        // SAFETY: cmd_w is NUL-terminated UTF-16 passed as a contiguous byte slice.
        let bytes = unsafe {
            std::slice::from_raw_parts(cmd_w.as_ptr().cast::<u8>(), std::mem::size_of_val(&*cmd_w))
        };
        let status = unsafe {
            RegSetValueExW(key.0, PCWSTR(val_w.as_ptr()), None, REG_SZ, Some(bytes))
        };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!(
                "Failed to save Windows Startup entry (code {})",
                status.0
            ))
        }
    }
}

#[cfg(not(windows))]
mod win {
    use std::path::Path;

    pub fn registered_auto_run_command() -> Result<Option<String>, String> {
        Ok(None)
    }

    pub fn set_auto_run_for_exe(_enabled: bool, _exe_path: &Path) -> Result<(), String> {
        Err("Auto-run on startup is currently supported on Windows only".to_string())
    }
}

/// Returns the currently registered startup command if present in the Windows Run key.
pub fn registered_auto_run_command() -> Result<Option<String>, String> {
    win::registered_auto_run_command()
}

/// Registers or removes StageHost from per-user Windows Startup (`HKCU\...\Run`).
pub fn set_auto_run(enabled: bool) -> Result<(), String> {
    let exe_path = std::env::current_exe()
        .map_err(|err| format!("Failed to resolve StageHost executable path: {err}"))?;
    win::set_auto_run_for_exe(enabled, &exe_path)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{command_matches_exe, format_autostart_command};

    #[test]
    fn formats_quoted_autostart_command_with_flag() {
        let exe = PathBuf::from(r"C:\Program Files\StageHost\stagehost.exe");
        assert_eq!(
            format_autostart_command(&exe),
            r#""C:\Program Files\StageHost\stagehost.exe" --autostart"#
        );
    }

    #[test]
    fn matches_quoted_and_unquoted_exe_paths_case_insensitively() {
        let exe = PathBuf::from(r"D:\Audio\StageHost\stagehost.exe");
        assert!(command_matches_exe(
            r#""d:\audio\stagehost\stagehost.exe" --autostart"#,
            &exe
        ));
        assert!(command_matches_exe(
            r"D:\Audio\StageHost\stagehost.exe",
            &exe
        ));
        assert!(!command_matches_exe(
            r#""D:\Other\stagehost.exe" --autostart"#,
            &exe
        ));
    }
}

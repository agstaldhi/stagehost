use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ico_path = manifest_dir.join("assets").join("stagehost.ico");
    println!("cargo:rerun-if-changed={}", ico_path.display());

    let ico_bytes = fs::read(&ico_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", ico_path.display()));
    let res_bytes = build_icon_res_from_ico(&ico_bytes)
        .unwrap_or_else(|e| panic!("Invalid ICO file {}: {e}", ico_path.display()));

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let res_path = out_dir.join("stagehost_icon.res");
    fs::write(&res_path, &res_bytes)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", res_path.display()));

    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "msvc" {
        println!("cargo:rustc-link-arg-bins={}", res_path.display());
    } else if let Some(windres) = find_gnu_windres() {
        let rc_path = out_dir.join("stagehost_icon.rc");
        let obj_path = out_dir.join("stagehost_icon.o");
        let normalized_ico = ico_path.to_string_lossy().replace('\\', "/");
        fs::write(&rc_path, format!("1 ICON \"{normalized_ico}\"\n")).ok();
        let status = std::process::Command::new(windres)
            .args([
                "-i",
                &rc_path.to_string_lossy(),
                "-o",
                &obj_path.to_string_lossy(),
            ])
            .status();
        if status.is_ok_and(|s| s.success()) {
            println!("cargo:rustc-link-arg-bins={}", obj_path.display());
        }
    }
}

fn find_gnu_windres() -> Option<PathBuf> {
    which_in_path("x86_64-w64-mingw32-windres").or_else(|| which_in_path("windres"))
}

fn which_in_path(exe: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    for dir in env::split_paths(&paths) {
        let cand = dir.join(format!("{exe}.exe"));
        if cand.is_file() {
            return Some(cand);
        }
        let cand_raw = dir.join(exe);
        if cand_raw.is_file() {
            return Some(cand_raw);
        }
    }
    None
}

fn read_u16_le(bytes: &[u8], off: usize) -> Result<u16, &'static str> {
    let slice: [u8; 2] = bytes
        .get(off..off + 2)
        .ok_or("unexpected EOF reading u16")?
        .try_into()
        .map_err(|_| "slice mismatch")?;
    Ok(u16::from_le_bytes(slice))
}

fn read_u32_le(bytes: &[u8], off: usize) -> Result<u32, &'static str> {
    let slice: [u8; 4] = bytes
        .get(off..off + 4)
        .ok_or("unexpected EOF reading u32")?
        .try_into()
        .map_err(|_| "slice mismatch")?;
    Ok(u32::from_le_bytes(slice))
}

fn push_res_record(out: &mut Vec<u8>, res_type: u16, res_id: u16, data: &[u8]) {
    // 32-byte Win32 RES header when both Type and Name are ordinal WORDs.
    let header_size: u32 = 32;
    let data_size = data.len() as u32;
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(&header_size.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&res_type.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&res_id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    out.extend_from_slice(&0x1030u16.to_le_bytes()); // MemoryFlags: MOVEABLE | PURE | DISCARDABLE
    out.extend_from_slice(&0x0409u16.to_le_bytes()); // LanguageId: en-US
    out.extend_from_slice(&0u32.to_le_bytes()); // Version
    out.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// Encodes a standard Win32 `.res` resource file containing `RT_ICON` entries and `RT_GROUP_ICON` #1.
fn build_icon_res_from_ico(ico: &[u8]) -> Result<Vec<u8>, &'static str> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;

    if ico.len() < 6 {
        return Err("file too short");
    }
    let reserved = read_u16_le(ico, 0)?;
    let ico_type = read_u16_le(ico, 2)?;
    let count = read_u16_le(ico, 4)? as usize;
    if reserved != 0 || ico_type != 1 || count == 0 {
        return Err("invalid ICO header");
    }
    let dir_end = 6 + count * 16;
    if ico.len() < dir_end {
        return Err("truncated ICONDIRENTRY table");
    }

    let mut out = Vec::with_capacity(ico.len() + 256);
    // Empty resource prefix header required by Win32 .res format.
    push_res_record(&mut out, 0, 0, &[]);

    let mut group = Vec::with_capacity(6 + count * 14);
    group.extend_from_slice(&0u16.to_le_bytes()); // idReserved
    group.extend_from_slice(&1u16.to_le_bytes()); // idType = 1 (Icon)
    group.extend_from_slice(&(count as u16).to_le_bytes()); // idCount

    for idx in 0..count {
        let entry_off = 6 + idx * 16;
        let width = ico[entry_off];
        let height = ico[entry_off + 1];
        let color_count = ico[entry_off + 2];
        let reserved_b = ico[entry_off + 3];
        let planes = read_u16_le(ico, entry_off + 4)?;
        let bit_count = read_u16_le(ico, entry_off + 6)?;
        let bytes_in_res = read_u32_le(ico, entry_off + 8)?;
        let img_off = read_u32_le(ico, entry_off + 12)? as usize;
        let img_end = img_off
            .checked_add(bytes_in_res as usize)
            .ok_or("icon image offset overflow")?;
        let img_data = ico.get(img_off..img_end).ok_or("truncated icon image data")?;

        let icon_id = (idx + 1) as u16;
        push_res_record(&mut out, RT_ICON, icon_id, img_data);

        // GRPICONDIRENTRY is 14 bytes (2-byte nID replaces 4-byte dwImageOffset).
        group.push(width);
        group.push(height);
        group.push(color_count);
        group.push(reserved_b);
        group.extend_from_slice(&planes.to_le_bytes());
        group.extend_from_slice(&bit_count.to_le_bytes());
        group.extend_from_slice(&bytes_in_res.to_le_bytes());
        group.extend_from_slice(&icon_id.to_le_bytes());
    }

    push_res_record(&mut out, RT_GROUP_ICON, 1, &group);
    Ok(out)
}

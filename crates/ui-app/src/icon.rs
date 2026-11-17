//! Application and Windows system tray icon loader derived from the bundled `icon.png`.

use std::sync::OnceLock;

use eframe::egui;

const ICON_PNG_BYTES: &[u8] = include_bytes!("../../../icon.png");

struct CachedIcons {
    window_icon: egui::IconData,
    tray_rgba_32: Vec<u8>,
}

fn load_cached_icons() -> Option<&'static CachedIcons> {
    static CACHE: OnceLock<Option<CachedIcons>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let decoded = eframe::icon_data::from_png_bytes(ICON_PNG_BYTES).ok()?;
            let window_rgba = downsample_rgba(&decoded.rgba, decoded.width, decoded.height, 64, 64);
            let tray_rgba_32 = downsample_rgba(&window_rgba, 64, 64, 32, 32);
            Some(CachedIcons {
                window_icon: egui::IconData {
                    rgba: window_rgba,
                    width: 64,
                    height: 64,
                },
                tray_rgba_32,
            })
        })
        .as_ref()
}

/// Returns a compact 64x64 RGBA icon for the native window and taskbar.
pub fn load_window_icon() -> Option<egui::IconData> {
    load_cached_icons().map(|c| c.window_icon.clone())
}

fn downsample_rgba(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    if src_w == dst_w && src_h == dst_h && src.len() >= (src_w * src_h * 4) as usize {
        return src[..(src_w * src_h * 4) as usize].to_vec();
    }
    let mut dst = vec![0u8; (dst_w * dst_h * 4) as usize];
    for dy in 0..dst_h {
        let y0 = (dy as u64 * src_h as u64 / dst_h as u64) as u32;
        let y1 = (((dy + 1) as u64 * src_h as u64 / dst_h as u64) as u32)
            .max(y0 + 1)
            .min(src_h);
        for dx in 0..dst_w {
            let x0 = (dx as u64 * src_w as u64 / dst_w as u64) as u32;
            let x1 = (((dx + 1) as u64 * src_w as u64 / dst_w as u64) as u32)
                .max(x0 + 1)
                .min(src_w);

            let mut sum = [0u64; 4];
            let mut count = 0u64;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let idx = ((sy * src_w + sx) * 4) as usize;
                    if idx + 3 < src.len() {
                        sum[0] += u64::from(src[idx]);
                        sum[1] += u64::from(src[idx + 1]);
                        sum[2] += u64::from(src[idx + 2]);
                        sum[3] += u64::from(src[idx + 3]);
                        count += 1;
                    }
                }
            }
            let out_idx = ((dy * dst_w + dx) * 4) as usize;
            if let Some(d) = std::num::NonZeroU64::new(count) {
                let d = d.get();
                dst[out_idx] = (sum[0] / d) as u8;
                dst[out_idx + 1] = (sum[1] / d) as u8;
                dst[out_idx + 2] = (sum[2] / d) as u8;
                dst[out_idx + 3] = (sum[3] / d) as u8;
            }
        }
    }
    dst
}

#[cfg(windows)]
pub fn create_tray_hicon() -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    let cached = load_cached_icons()?;
    const SIZE: usize = 32;
    if cached.tray_rgba_32.len() < SIZE * SIZE * 4 {
        return None;
    }

    let mut bgra_xor = [0u8; SIZE * SIZE * 4];
    let mut and_mask = [0u8; (SIZE / 8) * SIZE];

    for y in 0..SIZE {
        for x in 0..SIZE {
            let i = (y * SIZE + x) * 4;
            let r = cached.tray_rgba_32[i];
            let g = cached.tray_rgba_32[i + 1];
            let b = cached.tray_rgba_32[i + 2];
            let a = cached.tray_rgba_32[i + 3];
            bgra_xor[i] = b;
            bgra_xor[i + 1] = g;
            bgra_xor[i + 2] = r;
            bgra_xor[i + 3] = a;

            if a < 16 {
                let mask_idx = y * (SIZE / 8) + (x / 8);
                and_mask[mask_idx] |= 1 << (7 - (x % 8));
            }
        }
    }

    unsafe {
        windows::Win32::UI::WindowsAndMessaging::CreateIcon(
            None,
            SIZE as i32,
            SIZE as i32,
            1,
            32,
            and_mask.as_ptr(),
            bgra_xor.as_ptr(),
        )
        .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_downsamples_embedded_window_icon() {
        let icon = load_window_icon().expect("embedded icon.png should decode cleanly");
        assert_eq!(icon.width, 64);
        assert_eq!(icon.height, 64);
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);
    }
}

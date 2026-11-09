//! 图标提取：系统 Shell 图标缓存 → 32bpp BMP data URL。
//!
//! **不随包分发任何第三方图标**：图标全部由 `SHGetFileInfoW` 在用户本机运行时取得，
//! 取不到就返回 `None`，前端回落到自绘 SVG 线条图标。
//!
//! 为什么必须带超时：Shell 图标提取会打到外壳扩展 / 网络重定向路径，旧仓实测过
//! 单个 `.lnk` 卡住数秒。合并后 dock 与 island 同进程，dock 卡住 = 岛也卡住，
//! 所以整个提取跑在独立短命线程，800ms 硬截止（卡面 D5 / 硬约束 14）。
//! 超时线程不会被强杀（强杀线程在 Windows 上会泄漏 COM 状态），但它持有的
//! `Sender` 一旦被外层丢弃，结果就再无人消费，等效降级。

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

use base64::Engine as _;
use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS, RGBQUAD,
};
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, DrawIcon, HICON};

use super::config::IconData;

const ICON_PX: i32 = 32;
const EXTRACT_DEADLINE: Duration = Duration::from_millis(800);

/// 带硬截止的图标提取。超时 / 失败返回 `None`（前端用通用图标）。
pub fn extract_bounded(path: &str) -> Option<IconData> {
    if path.trim().is_empty() {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let owned = path.to_string();
    let spawned = std::thread::Builder::new()
        .name("glasssuite-icon".into())
        .spawn(move || {
            let _ = tx.send(extract(&owned));
        })
        .is_ok();
    if !spawned {
        return None;
    }
    match rx.recv_timeout(EXTRACT_DEADLINE) {
        Ok(v) => v,
        Err(_) => {
            winplat::log::log("dock: icon extraction timeout, degraded count=1");
            None
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn extract(path: &str) -> Option<IconData> {
    let w = wide(path);
    let mut info = SHFILEINFOW::default();
    let cb = std::mem::size_of::<SHFILEINFOW>() as u32;

    // 先按真实文件取图标；文件不存在（例如迁移来的旧快捷方式）再退到按扩展名取通用图标。
    unsafe {
        let _ = SHGetFileInfoW(
            PCWSTR(w.as_ptr()),
            Default::default(),
            Some(&mut info),
            cb,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if info.hIcon.0.is_null() {
            let _ = SHGetFileInfoW(
                PCWSTR(w.as_ptr()),
                Default::default(),
                Some(&mut info),
                cb,
                SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
            );
        }
    }
    if info.hIcon.0.is_null() {
        return None;
    }

    let data = hicon_to_data(info.hIcon);
    unsafe {
        let _ = DestroyIcon(info.hIcon);
    }
    data
}

/// HICON → DIBSection → 自下而上的 32bpp BMP data URL。
///
/// 用 `CreateDIBSection` 返回的指针直接读像素，省掉一次 `GetDIBits` 拷贝；
/// `biHeight` 取正数，因此 BMP 像素行要按「最后一行在前」写入才能正着显示。
fn hicon_to_data(icon: HICON) -> Option<IconData> {
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let result = (|| {
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: ICON_PX,
                    biHeight: ICON_PX,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0, // BI_RGB
                    ..Default::default()
                },
                bmiColors: [RGBQUAD::default(); 1],
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            if bits.is_null() {
                let _ = DeleteObject(bmp.into());
                return None;
            }
            let old = SelectObject(dc, bmp.into());
            let drew = DrawIcon(dc, 0, 0, icon).is_ok();
            SelectObject(dc, old);

            let len = (ICON_PX * ICON_PX * 4) as usize;
            let raw = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
            let _ = DeleteObject(bmp.into());
            if !drew {
                return None;
            }

            let mut px = raw;
            // GDI 的 DrawIcon 在部分图标上不写 alpha 通道；颜色非零即视为不透明，
            // 否则浏览器会把整枚图标渲染成全透明。
            for i in (0..px.len()).step_by(4) {
                if px[i + 3] == 0 && (px[i] | px[i + 1] | px[i + 2]) != 0 {
                    px[i + 3] = 0xff;
                }
            }
            let url = bmp_data_url(&px, ICON_PX as u32, ICON_PX as u32);
            Some(IconData {
                w: ICON_PX as u32,
                h: ICON_PX as u32,
                data_url: url,
                legacy_rgba: None,
            })
        })();
        let _ = DeleteDC(dc);
        result
    }
}

/// 把 BGRA 像素（自上而下）编成标准 BMP 的 data URL。纯函数，单测覆盖头部。
fn bmp_data_url(top_down_bgra: &[u8], w: u32, h: u32) -> String {
    let row = (w * 4) as usize;
    let pixel_len = row * h as usize;
    let mut out = Vec::with_capacity(54 + pixel_len);

    // BITMAPFILEHEADER
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + pixel_len) as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    debug_assert_eq!(out.len(), 54);

    // BMP 正高度 = 自下而上，倒序写行。
    for y in (0..h as usize).rev() {
        let start = y * row;
        out.extend_from_slice(&top_down_bgra[start..start + row]);
    }
    format!(
        "data:image/bmp;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(out)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_header_is_well_formed_and_rows_are_flipped() {
        // 1x2：第一行红、第二行蓝（BGRA）。BMP 里应当先出现蓝色行。
        let px = vec![
            0, 0, 255, 255, // 红 (B,G,R,A)
            255, 0, 0, 255, // 蓝
        ];
        let url = bmp_data_url(&px, 1, 2);
        let b64 = url.strip_prefix("data:image/bmp;base64,").unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        assert_eq!(&bytes[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(bytes[2..6].try_into().unwrap()), 54 + 8);
        assert_eq!(u32::from_le_bytes(bytes[10..14].try_into().unwrap()), 54);
        assert_eq!(i32::from_le_bytes(bytes[18..22].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(bytes[22..26].try_into().unwrap()), 2);
        assert_eq!(
            &bytes[54..58],
            &[255, 0, 0, 255],
            "BMP 首行应是原图最后一行"
        );
        assert_eq!(&bytes[58..62], &[0, 0, 255, 255]);
    }

    #[test]
    fn empty_path_is_rejected_without_spawning_a_thread() {
        assert!(extract_bounded("  ").is_none());
    }
}

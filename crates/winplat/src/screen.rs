//! 屏幕几何：显示器枚举与主屏矩形，**物理像素**。
//!
//! 前置条件：进程已通过 [`crate::dpi::init_per_monitor_v2`] 声明 Per-Monitor-Aware V2。
//! 否则 `GetMonitorInfoW` 返回的会是虚拟化后的坐标，与任务栏矩形（物理）对不上。

use crate::Rect;

/// 一台显示器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// 显示器工作区（不含任务栏）。
    pub work: Rect,
    /// 显示器整屏矩形。
    pub bounds: Rect,
    /// 是否主显示器。
    pub primary: bool,
}

/// 主屏整屏矩形。取不到时返回 1920×1080 的兜底值（并应在上层记日志）。
pub fn primary_screen() -> Rect {
    imp::primary_screen()
}

/// 枚举所有显示器（顺序不保证稳定，用 `primary` 字段判定主屏）。
pub fn monitors() -> Vec<Monitor> {
    imp::monitors()
}

/// 包含指定点的显示器（物理像素坐标）。用于把鼠标位置映射到具体屏。
pub fn monitor_from_point(x: i32, y: i32) -> Option<Monitor> {
    imp::monitor_from_point(x, y)
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::{LPARAM, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    };

    /// `MONITORINFO.dwFlags` 的主屏位（winuser.h：MONITORINFOF_PRIMARY = 0x1）。
    const MONITORINFOF_PRIMARY: u32 = 0x1;

    fn rect_from(r: &RECT) -> Rect {
        Rect::new(r.left, r.top, r.right, r.bottom)
    }

    fn info_from(hmon: HMONITOR) -> Option<Monitor> {
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: mi 是本地栈上正确初始化的结构（cbSize 已填），hmon 来自系统调用。
        if !unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
            return None;
        }
        Some(Monitor {
            work: rect_from(&mi.rcWork),
            bounds: rect_from(&mi.rcMonitor),
            primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
        })
    }

    pub fn primary_screen() -> Rect {
        let pt = POINT { x: 0, y: 0 };
        // SAFETY: 纯查询调用，参数都是值类型。
        let hmon = unsafe {
            MonitorFromPoint(pt, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY)
        };
        info_from(hmon)
            .map(|m| m.bounds)
            .unwrap_or(Rect::new(0, 0, 1920, 1080))
    }

    pub fn monitors() -> Vec<Monitor> {
        let mut out: Vec<Monitor> = Vec::new();
        let ptr = &mut out as *mut Vec<Monitor> as isize;
        // SAFETY: 回调在本次调用内同步执行，指针只在调用期间被解引用。
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(enum_cb), LPARAM(ptr));
        }
        out
    }

    unsafe extern "system" fn enum_cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _clip: *mut RECT,
        data: LPARAM,
    ) -> windows::core::BOOL {
        // SAFETY: data 是调用方传入的 Vec<Monitor> 指针，调用期间有效。
        let out = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
        if let Some(m) = info_from(hmon) {
            out.push(m);
        }
        windows::core::BOOL(1)
    }

    pub fn monitor_from_point(x: i32, y: i32) -> Option<Monitor> {
        let pt = POINT { x, y };
        // SAFETY: 纯查询调用。
        let hmon = unsafe {
            MonitorFromPoint(pt, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST)
        };
        info_from(hmon)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn primary_screen() -> Rect {
        Rect::new(0, 0, 1920, 1080)
    }

    pub fn monitors() -> Vec<Monitor> {
        Vec::new()
    }

    pub fn monitor_from_point(_x: i32, _y: i32) -> Option<Monitor> {
        None
    }
}

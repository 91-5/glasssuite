//! 全屏检测：唯一判定实现。
//!
//! **为什么必须唯一**：旧 dock 用纯几何判定（窗口矩形覆盖屏幕 ≥99.5%），旧岛用另一套；
//! 同一个前台窗口在两边得出不同结论，于是「岛藏起来了但 dock 还杵着」。合并后只保留这一份。
//!
//! **判定口径**：
//! 1. 取前台窗口；窗口不可见 / 最小化 → 非全屏。
//! 2. 类名命中 shell 排除名单（桌面、任务栏、开始菜单宿主等）→ 非全屏。
//! 3. 窗口矩形优先用 DWM 的 `DWMWA_EXTENDED_FRAME_BOUNDS`（去掉 Win10 之后不可见的
//!    边框阴影，纯 `GetWindowRect` 会多出约 8px，导致 100% 窗口被判成 99.4% 而漏判）。
//! 4. 与**所在显示器**的整屏矩形比较（不是主屏），四边容差 `TOLERANCE_PX`。
//!
//! 已知局限：独占全屏（D3D exclusive）时窗口矩形可能与显示器模式而非桌面矩形比较，
//! 这类应用在切换时会改变显示模式，需要监测 `WM_DISPLAYCHANGE`；本模块只回答
//! 「当前前台窗口是否铺满它所在的那块屏」。

use crate::{screen, Rect};

/// 四边容差（物理像素）。边框/圆角/DPI 舍入导致的 1~2px 差不应改变结论。
pub const TOLERANCE_PX: i32 = 2;

/// shell 自身窗口类名：它们铺满屏幕也不代表「有应用在全屏」。
pub const SHELL_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "ForegroundStaging",
    "MultitaskingViewFrame",
    "XamlExplorerHostIslandWindow",
    "TaskManagerWindow",
];

/// 前台窗口的观测结果（调试 / 日志用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    pub class: String,
    /// DWM 扩展边框矩形（物理像素）。
    pub rect: Rect,
    /// 所在显示器整屏矩形。
    pub monitor: Rect,
    pub fullscreen: bool,
}

/// 当前是否有一个普通应用在全屏（dock / island 应让位）。
pub fn foreground_is_fullscreen() -> bool {
    foreground().map(|f| f.fullscreen).unwrap_or(false)
}

/// 观测前台窗口。没有前台窗口时返回 `None`。
pub fn foreground() -> Option<Foreground> {
    imp::foreground()
}

/// 纯逻辑：给定窗口矩形与显示器矩形，是否可视为铺满。
pub fn covers_monitor(window: Rect, monitor: Rect) -> bool {
    window.left <= monitor.left + TOLERANCE_PX
        && window.top <= monitor.top + TOLERANCE_PX
        && window.right >= monitor.right - TOLERANCE_PX
        && window.bottom >= monitor.bottom - TOLERANCE_PX
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowRect, IsIconic, IsWindowVisible,
    };

    pub fn foreground() -> Option<Foreground> {
        // SAFETY: 纯查询。
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() {
            return None;
        }
        // SAFETY: 均为纯查询，参数有效。
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return None;
            }
        }

        let class = class_of(hwnd);
        let rect = frame_bounds(hwnd).or_else(|| window_rect(hwnd))?;
        let cx = rect.left + rect.width() / 2;
        let cy = rect.top + rect.height() / 2;
        let monitor = screen::monitor_from_point(cx, cy)
            .map(|m| m.bounds)
            .unwrap_or_else(screen::primary_screen);

        let excluded = SHELL_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(&class));
        let fullscreen = !excluded && covers_monitor(rect, monitor);

        Some(Foreground {
            class,
            rect,
            monitor,
            fullscreen,
        })
    }

    fn class_of(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        // SAFETY: buf 足够大。
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    /// DWM 扩展边框：排除 Win10+ 不可见的窗口阴影区域。
    fn frame_bounds(hwnd: HWND) -> Option<Rect> {
        let mut r = RECT::default();
        // SAFETY: r 是本地栈结构，大小已正确传入。
        unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut r as *mut RECT as *mut core::ffi::c_void,
                std::mem::size_of::<RECT>() as u32,
            )
        }
        .ok()?;
        Some(Rect::new(r.left, r.top, r.right, r.bottom))
    }

    fn window_rect(hwnd: HWND) -> Option<Rect> {
        let mut r = RECT::default();
        // SAFETY: 同上。
        unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
        Some(Rect::new(r.left, r.top, r.right, r.bottom))
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn foreground() -> Option<Foreground> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_cover_is_fullscreen() {
        assert!(covers_monitor(
            Rect::new(0, 0, 1920, 1080),
            Rect::new(0, 0, 1920, 1080)
        ));
    }

    #[test]
    fn borderless_maximised_with_shadow_is_fullscreen() {
        // GetWindowRect 会多出不可见边框（-8..+8），容差内仍应判全屏。
        assert!(covers_monitor(
            Rect::new(-8, -8, 1928, 1088),
            Rect::new(0, 0, 1920, 1080)
        ));
    }

    #[test]
    fn windowed_is_not_fullscreen() {
        assert!(!covers_monitor(
            Rect::new(100, 100, 1200, 900),
            Rect::new(0, 0, 1920, 1080)
        ));
    }

    #[test]
    fn fullscreen_on_secondary_monitor() {
        let secondary = Rect::new(1920, 0, 3840, 1080);
        assert!(covers_monitor(Rect::new(1920, 0, 3840, 1080), secondary));
        // 主屏尺寸的窗口放在副屏 → 不算铺满。
        assert!(!covers_monitor(Rect::new(1920, 0, 3440, 1080), secondary));
    }

    #[test]
    fn shell_classes_are_excluded_listed() {
        for c in ["Progman", "WorkerW", "Shell_TrayWnd"] {
            assert!(SHELL_CLASSES.iter().any(|x| x.eq_ignore_ascii_case(c)));
        }
    }
}

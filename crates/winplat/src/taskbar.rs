//! 任务栏几何：唯一真相源。
//!
//! 旧两仓里 dock 有一套、岛又自己去 `GetSystemMetrics` 算一套，坐标系还不统一。
//! 合并后所有窗口都必须从这里取，禁止在窗口模块里再调 `FindWindow("Shell_TrayWnd")`。
//!
//! **两处历史坑（已固化为用例）**：
//! 1. 自动隐藏的任务栏被全屏应用唤出过一次后，`GetWindowRect` 会停在可见位置而
//!    `IsWindowVisible` 为 false（幻影矩形，见 `Rect::looks_like_hidden_dock`）。
//! 2. 多显示器时副屏任务栏的类名是 `Shell_SecondaryTrayWnd`，只找 `Shell_TrayWnd`
//!    会漏掉，副屏上的 dock 就没法停靠。

use crate::{screen, Rect};

/// 一条任务栏。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taskbar {
    /// 是否当前可见（自动隐藏时为 false）。
    pub visible: bool,
    /// 是否位于主显示器。
    pub primary: bool,
    /// 停泊矩形（物理像素）。隐藏时通常越过屏幕底边，见说明。
    pub rect: Rect,
    /// 所在显示器的整屏矩形。
    pub monitor: Rect,
}

/// 主任务栏的停泊矩形（**已做幻影修正**），供 dock 定锚。
///
/// 语义：
/// - 任务栏可见 → 返回可见矩形（dock 会贴在它上方）；
/// - 任务栏隐藏 → 返回隐藏停泊矩形 `(left, screen_bottom - 2, right, screen_bottom + 46)`。
///
/// 返回的矩形可能超出屏幕底边，这是正常的——调用方用 [`Rect::bottom_anchored_y`] 或直接
/// 取 `bottom`，不要假设它一定在屏幕内。
pub fn primary_taskbar_rect() -> Option<Taskbar> {
    let bars = taskbars();
    bars.into_iter().find(|b| b.primary)
}

/// 枚举所有任务栏（主 + 副屏）。
pub fn taskbars() -> Vec<Taskbar> {
    imp::taskbars()
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::{HWND, RECT};

    const PRIMARY_CLASS: &str = "Shell_TrayWnd";
    const SECONDARY_CLASS: &str = "Shell_SecondaryTrayWnd";

    pub fn taskbars() -> Vec<Taskbar> {
        let mut out = Vec::new();

        if let Some(hwnd) = find_window(PRIMARY_CLASS) {
            if let Some(t) = build(hwnd, true) {
                out.push(t);
            }
        }

        // 副屏任务栏：每块副屏一个窗口，类名固定，必须枚举。
        let mut hwnds: Vec<HWND> = Vec::new();
        let ptr = &mut hwnds as *mut Vec<HWND> as isize;
        // SAFETY: 回调同步执行，指针在调用期间有效。
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::EnumWindows(
                Some(enum_secondary),
                windows::Win32::Foundation::LPARAM(ptr),
            );
        }
        for hwnd in hwnds {
            if let Some(t) = build(hwnd, false) {
                out.push(t);
            }
        }
        out
    }

    unsafe extern "system" fn enum_secondary(
        hwnd: HWND,
        data: windows::Win32::Foundation::LPARAM,
    ) -> windows::core::BOOL {
        // SAFETY: data 是调用方传入的 Vec<HWND> 指针。
        let out = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
        if window_class(hwnd) == SECONDARY_CLASS {
            out.push(hwnd);
        }
        windows::core::BOOL(1)
    }

    fn find_window(class: &str) -> Option<HWND> {
        use windows::core::HSTRING;
        use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
        let cls = HSTRING::from(class);
        // SAFETY: 纯查询；类名不存在时返回 HWND(0)。
        let hwnd = unsafe { FindWindowW(&cls, None) }.unwrap_or_default();
        (!hwnd.is_invalid()).then_some(hwnd)
    }

    fn window_class(hwnd: HWND) -> String {
        use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
        let mut buf = [0u16; 128];
        // SAFETY: buf 足够大，hwnd 由枚举提供。
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    fn build(hwnd: HWND, primary: bool) -> Option<Taskbar> {
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindowVisible};

        // SAFETY: 纯查询，参数是有效 hwnd。
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        let mut raw = RECT::default();
        // SAFETY: 同上；raw 是本地栈结构。
        unsafe { GetWindowRect(hwnd, &mut raw) }.ok()?;
        let mut rect = Rect::new(raw.left, raw.top, raw.right, raw.bottom);

        // 用任务栏自身中心定位所在显示器，避免任务栏跨屏时选错。
        let cx = rect.left + rect.width() / 2;
        let cy = rect.top + rect.height() / 2;
        let monitor = screen::monitor_from_point(cx, cy)
            .map(|m| m.bounds)
            .unwrap_or_else(screen::primary_screen);

        if visible {
            if !rect.is_plausible_taskbar(monitor.bottom) {
                return None;
            }
        } else if !rect.looks_like_hidden_dock(monitor.bottom) {
            // 幻影矩形：位置停在可见态。按屏底重建隐藏停泊矩形。
            rect = Rect::new(
                monitor.left,
                monitor.bottom - 2,
                monitor.right,
                monitor.bottom + 46,
            );
        }

        Some(Taskbar {
            visible,
            primary,
            rect,
            monitor,
        })
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn taskbars() -> Vec<Taskbar> {
        Vec::new()
    }
}

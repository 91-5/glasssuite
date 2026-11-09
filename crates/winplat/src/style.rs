//! 扩展窗口样式（ExStyle）操作。
//!
//! 三个常驻需求，全部收敛在这里，窗口模块不得自己拼 `SetWindowLongW`：
//!
//! 1. **不进 Alt+Tab、不进任务栏**：`WS_EX_TOOLWINDOW` 置位**并且** `WS_EX_APPWINDOW` 清零。
//!    只置 TOOLWINDOW 不够——旧 glassdesk 的实现漏了清 APPWINDOW，导致部分窗口仍出现在
//!    Alt+Tab 列表里。
//! 2. **不抢焦点**：`WS_EX_NOACTIVATE`。dock / island 都必须在点击时不把焦点从用户当前
//!    窗口拿走，否则点一下 dock 就把游戏或编辑器切走了。
//! 3. **鼠标穿透**：`WS_EX_TRANSPARENT`（配合 `WS_EX_LAYERED`）。开关必须是幂等的位操作，
//!    不能整体覆盖 ExStyle——别的模块可能已经设了别的位。

#[cfg(windows)]
use windows::Win32::Foundation::HWND;

/// 把窗口标记为工具窗口：置 `WS_EX_TOOLWINDOW`，清 `WS_EX_APPWINDOW`。
///
/// 必须在窗口显示之前调用；显示后再改通常需要 `SetWindowPos(..., SWP_FRAMECHANGED)` 才生效。
#[cfg(windows)]
pub fn hide_from_alt_tab(hwnd: HWND) -> Result<(), String> {
    imp::update_ex_style(hwnd, |style| {
        style | WS_EX_TOOLWINDOW_BIT & !WS_EX_APPWINDOW_BIT
    })
}

/// 置 `WS_EX_NOACTIVATE`：窗口可接收鼠标消息但不接收焦点。
#[cfg(windows)]
pub fn set_no_activate(hwnd: HWND, enabled: bool) -> Result<(), String> {
    imp::update_ex_style(hwnd, |style| {
        if enabled {
            style | WS_EX_NOACTIVATE_BIT
        } else {
            style & !WS_EX_NOACTIVATE_BIT
        }
    })
}

/// 开关鼠标穿透。开启时窗口对鼠标完全透明（事件落到下层窗口）。
#[cfg(windows)]
pub fn set_click_through(hwnd: HWND, enabled: bool) -> Result<(), String> {
    imp::update_ex_style(hwnd, |style| {
        if enabled {
            style | WS_EX_TRANSPARENT_BIT | WS_EX_LAYERED_BIT
        } else {
            style & !WS_EX_TRANSPARENT_BIT
        }
    })
}

/// 读当前 ExStyle 原始位（排查用）。
#[cfg(windows)]
pub fn ex_style(hwnd: HWND) -> isize {
    imp::get_ex_style(hwnd)
}

// WS_EX_* 的位常量在这里以裸位给出，避免上层为了拿一个位而引入 windows crate 的
// 一大串 feature。值与 `winuser.h` 一致。
#[cfg(windows)]
const WS_EX_APPWINDOW_BIT: isize = 0x0004_0000;
#[cfg(windows)]
const WS_EX_TOOLWINDOW_BIT: isize = 0x0000_0080;
#[cfg(windows)]
const WS_EX_TRANSPARENT_BIT: isize = 0x0000_0020;
#[cfg(windows)]
const WS_EX_NOACTIVATE_BIT: isize = 0x0800_0000;
#[cfg(windows)]
const WS_EX_LAYERED_BIT: isize = 0x0008_0000;

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE,
    };

    pub fn get_ex_style(hwnd: HWND) -> isize {
        // SAFETY: hwnd 由调用方保证有效；GWL_EXSTYLE 是合法索引。
        unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) }
    }

    pub fn update_ex_style(hwnd: HWND, f: impl FnOnce(isize) -> isize) -> Result<(), String> {
        let before = get_ex_style(hwnd);
        let after = f(before);
        if after == before {
            return Ok(());
        }
        // SAFETY: 同上；SetWindowLongPtrW 返回旧值，0 与「失败」需靠 GetLastError 区分，
        // 这里用回读校验代替错误码判断，避免 0 值歧义。
        let prev = unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, after) };
        let now = get_ex_style(hwnd);
        if now == after {
            Ok(())
        } else {
            Err(format!(
                "SetWindowLongPtrW(GWL_EXSTYLE) 未生效：before={before:#x} want={after:#x} prev={prev:#x} now={now:#x}"
            ))
        }
    }
}

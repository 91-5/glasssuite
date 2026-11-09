//! 扩展窗口样式（ExStyle）操作。
//!
//! 三个常驻需求，全部收敛在这里，窗口模块不得自己拼 `SetWindowLongW`：
//!
//! 1. **不进 Alt+Tab、不进任务栏**：`WS_EX_TOOLWINDOW` 置位**并且** `WS_EX_APPWINDOW` 清零。
//! 2. **不抢焦点**：`WS_EX_NOACTIVATE`。dock / island 都必须在点击时不把焦点从用户当前
//!    窗口拿走，否则点一下 dock 就把游戏或编辑器切走了。
//! 3. **鼠标穿透**：`WS_EX_TRANSPARENT`（配合 `WS_EX_LAYERED`）。开关必须是幂等的位操作，
//!    不能整体覆盖 ExStyle——别的模块可能已经设了别的位。
//!
//! **接口用裸 `isize` 而不是 `HWND`**：宿主（Tauri）可能链接另一个 `windows` crate 版本，
//! 传 `HWND` 会因类型来自不同 crate 而无法编译。`isize` 就是 `HWND.0` 的整数值，
//! 调用方写 `hwnd.0 as isize`。
//!
//! **位运算放在纯函数里并单测**：这里出过一个真实 bug——`style | TOOLWINDOW & !APPWINDOW`
//! 在 Rust 里等价于 `style | (TOOLWINDOW & !APPWINDOW)`（`&` 优先级高于 `|`），于是
//! APPWINDOW 从来没被清掉，窗口照样出现在 Alt+Tab 里。旧 glassdesk 也是同样的错。

/// `WS_EX_TOOLWINDOW`：不出现在任务栏与 Alt+Tab。
pub const TOOLWINDOW: isize = 0x0000_0080;
/// `WS_EX_APPWINDOW`：强制出现在任务栏。与 TOOLWINDOW 互斥，必须**两者一起处理**。
pub const APPWINDOW: isize = 0x0004_0000;
/// `WS_EX_NOACTIVATE`：可接收鼠标输入但不获得焦点。
pub const NOACTIVATE: isize = 0x0800_0000;
/// `WS_EX_TRANSPARENT`：鼠标事件穿透到下层窗口。
pub const TRANSPARENT: isize = 0x0000_0020;
/// `WS_EX_LAYERED`：分层窗口，`WS_EX_TRANSPARENT` 生效的前提。
pub const LAYERED: isize = 0x0008_0000;

/// 置 TOOLWINDOW、清 APPWINDOW，其余位原样保留。
pub fn tool_window_bits(ex: isize) -> isize {
    (ex | TOOLWINDOW) & !APPWINDOW
}

/// 开关 NOACTIVATE，其余位原样保留。
pub fn no_activate_bits(ex: isize, enabled: bool) -> isize {
    if enabled {
        ex | NOACTIVATE
    } else {
        ex & !NOACTIVATE
    }
}

/// 开关鼠标穿透（开时同时需要 LAYERED），其余位原样保留。
pub fn click_through_bits(ex: isize, enabled: bool) -> isize {
    if enabled {
        ex | TRANSPARENT | LAYERED
    } else {
        ex & !TRANSPARENT
    }
}

/// 把窗口标记为工具窗口：置 `WS_EX_TOOLWINDOW`，清 `WS_EX_APPWINDOW`。
///
/// 必须在窗口显示之前调用；显示后再改通常需要 `SetWindowPos(..., SWP_FRAMECHANGED)` 才生效。
/// Tauri/tao 在 `show()` 之后可能重设窗口属性，因此显示后再套一次是必要的（幂等）。
#[cfg(windows)]
pub fn hide_from_alt_tab(raw_hwnd: isize) -> Result<(), String> {
    imp::update_ex_style(raw_hwnd, tool_window_bits)
}

/// 置 `WS_EX_NOACTIVATE`：窗口可接收鼠标消息但不接收焦点。
#[cfg(windows)]
pub fn set_no_activate(raw_hwnd: isize, enabled: bool) -> Result<(), String> {
    imp::update_ex_style(raw_hwnd, |ex| no_activate_bits(ex, enabled))
}

/// 开关鼠标穿透。开启时窗口对鼠标完全透明（事件落到下层窗口）。
#[cfg(windows)]
pub fn set_click_through(raw_hwnd: isize, enabled: bool) -> Result<(), String> {
    imp::update_ex_style(raw_hwnd, |ex| click_through_bits(ex, enabled))
}

/// 读当前 ExStyle 原始位（排查用）。
#[cfg(windows)]
pub fn ex_style(raw_hwnd: isize) -> isize {
    imp::get_ex_style(raw_hwnd)
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE,
    };

    fn hwnd(raw: isize) -> HWND {
        HWND(raw as *mut core::ffi::c_void)
    }

    pub fn get_ex_style(raw_hwnd: isize) -> isize {
        // SAFETY: 调用方保证 raw_hwnd 是有效窗口句柄；GWL_EXSTYLE 是合法索引。
        unsafe { GetWindowLongPtrW(hwnd(raw_hwnd), GWL_EXSTYLE) }
    }

    pub fn update_ex_style(raw_hwnd: isize, f: impl FnOnce(isize) -> isize) -> Result<(), String> {
        let before = get_ex_style(raw_hwnd);
        let after = f(before);
        if after == before {
            return Ok(());
        }
        // SAFETY: 同上。
        let prev = unsafe { SetWindowLongPtrW(hwnd(raw_hwnd), GWL_EXSTYLE, after) };
        let now = get_ex_style(raw_hwnd);
        if now == after {
            Ok(())
        } else {
            Err(format!(
                "SetWindowLongPtrW(GWL_EXSTYLE) 未生效：before={before:#x} want={after:#x} prev={prev:#x} now={now:#x}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：`style | TOOLWINDOW & !APPWINDOW` 是错的（`&` 先算），APPWINDOW 必须真被清掉。
    #[test]
    fn tool_window_really_clears_app_window() {
        let before = 0x0004_0118; // APPWINDOW | WINDOWEDGE | TOPMOST | ACCEPTFILES
        let after = tool_window_bits(before);
        assert_eq!(after & APPWINDOW, 0, "APPWINDOW 必须清零");
        assert_eq!(after & TOOLWINDOW, TOOLWINDOW, "TOOLWINDOW 必须置位");
        // 其余位不得被抹掉。
        assert_eq!(after & 0x118, 0x118);
    }

    #[test]
    fn no_activate_toggles_both_ways() {
        let off = 0x0004_0118;
        let on = no_activate_bits(off, true);
        assert_eq!(on & NOACTIVATE, NOACTIVATE);
        assert_eq!(no_activate_bits(on, false), off);
    }

    #[test]
    fn click_through_requires_layered() {
        let on = click_through_bits(0, true);
        assert_eq!(on & TRANSPARENT, TRANSPARENT);
        assert_eq!(on & LAYERED, LAYERED);
        assert_eq!(click_through_bits(on, false) & TRANSPARENT, 0);
        // 关掉穿透不该顺手关掉 LAYERED（其它模块可能在用）。
        assert_eq!(click_through_bits(on, false) & LAYERED, LAYERED);
    }
}

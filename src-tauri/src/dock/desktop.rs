//! 桌面几何工具（Win32 查询）。
//!
//! 历史：本模块前身是 `overlay.rs`（任务栏玻璃覆盖层）。任务栏玻璃功能已于 2026-10-30
//! 由 sir 裁定**整体删除**（ADR-0001 D6）——「改的是系统底部任务栏，没什么用」。
//! 覆盖层机制（colorkey 窗口 / DWM 模糊层 / 着色层 / worker 线程 / 浓度档位）连同
//! `glass.rs` 一并移除，仅保留 dock 定位与全屏判定所需的下面这些查询函数。

use std::ffi::c_void;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmEnableBlurBehindWindow, DwmGetWindowAttribute, DWMWA_CLOAKED, DWM_BB_BLURREGION,
    DWM_BB_ENABLE, DWM_BLURBEHIND,
};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, CreateRoundRectRgn, DeleteObject, GetMonitorInfoW, MonitorFromWindow, HMONITOR,
    MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, IsZoomed,
};

/// 给任意窗口（含 dock 的 tao 窗口）开遗留 DWM 玻璃模糊。
/// `round_radius` 为圆角半径（物理像素），None = 整窗矩形。
///
/// 这是 dock 药丸自身玻璃质感的来源，与已删除的任务栏覆盖层无关。
pub fn enable_glass(hwnd: HWND, round_radius: Option<i32>) -> bool {
    unsafe {
        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        let hrgn = match round_radius {
            Some(r) => CreateRoundRectRgn(0, 0, w, h, r, r),
            None => CreateRectRgn(0, 0, w, h),
        };
        let bb = DWM_BLURBEHIND {
            dwFlags: DWM_BB_ENABLE | DWM_BB_BLURREGION,
            fEnable: true.into(),
            hRgnBlur: hrgn,
            fTransitionOnMaximized: false.into(),
        };
        let ok = DwmEnableBlurBehindWindow(hwnd, &bb).is_ok();
        let _ = DeleteObject(hrgn.into());
        ok
    }
}

/// 枚举全部**视觉可见**的任务栏窗口（主屏在前）：`(hwnd, rect)`。
/// 只认 IsWindowVisible + 非 DWM 遮罩——副屏任务栏在部分状态下「窗口存在但不渲染」。
pub fn taskbars() -> Vec<(HWND, RECT)> {
    let mut out = Vec::new();
    unsafe {
        if let Ok(h) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) {
            if !h.0.is_null() && visually_shown(h) {
                if let Some(r) = window_rect(h) {
                    out.push((h, r));
                }
            }
        }
        let mut after = HWND::default();
        loop {
            let h = match FindWindowExW(
                None,
                Some(after),
                w!("Shell_SecondaryTrayWnd"),
                PCWSTR::null(),
            ) {
                Ok(h) if !h.0.is_null() => h,
                _ => break,
            };
            if visually_shown(h) {
                if let Some(r) = window_rect(h) {
                    out.push((h, r));
                }
            }
            after = h;
        }
    }
    out
}

/// 视觉真实状态：IsWindowVisible 且未被 DWM 遮罩。
fn visually_shown(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        );
        cloaked == 0
    }
}

/// 主屏任务栏矩形（dock 定位用）。
/// 2026-11-04 热修（sir 指令「彻底隐藏任务栏」后 dock 消失）：外部守护进程会把
/// Shell_TrayWnd / Shell_SecondaryTrayWnd SW_HIDE，taskbars() 只认可见窗口 →
/// 本函数返回 None → dock 轮询进「找不到任务栏 → 永久隐藏」分支。找不到可见
/// 任务栏时回退读隐藏任务栏窗口自身的 rect（auto-hide 停泊位，几何与隐藏前一致）。
pub fn primary_taskbar_rect() -> Option<RECT> {
    unsafe {
        let (sw, sh) = primary_screen();
        let h = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok()?;
        let primary_raw = if h.0.is_null() {
            None
        } else {
            window_rect(h).map(|r| (r, IsWindowVisible(h).as_bool()))
        };
        let visible: Vec<RECT> = taskbars().into_iter().map(|(_, r)| r).collect();
        resolve_primary_taskbar_rect(primary_raw, &visible, sw, sh)
    }
}

/// 纯逻辑：从「主屏任务栏本体」与「可见任务栏列表」推导主屏任务栏停泊矩形。
///
/// 拆成纯函数是为了能单测——Win32 查询本身没法在测试里复现。
///
/// # 2026-11-16 事故复盘（dock 整体不可见，实测坐标 (360,1478)-(1560,1592)）
///
/// 旧实现先取 `taskbars().into_iter().next()`——那是「任意一条**可见**任务栏」。
/// 外部守卫 tb_guard.ps1 把**主屏** Shell_TrayWnd 藏起来（vis=false），
/// 而副屏 Shell_SecondaryTrayWnd 仍是可见的 → 列表里只剩副屏那条 →
/// dock 用**副屏**任务栏的 top 定位。调用方（dock/mod.rs）却用
/// `primary_screen()` 的**主屏**宽做水平居中 → x 来自主屏、y 来自副屏，
/// 窗口被摆到两块显示器之间的无人区（主屏 x 带内、副屏 y 带内），
/// 那里没有任何显示表面 → dock 完全不渲染（PrintWindow 却能截到内容，
/// 因为 DComp 表面本身是好的）。
///
/// 旧守卫每 250ms 硬藏**两条**任务栏 → 可见列表恒空 → 一直走「隐藏态重建」
/// 分支贴主屏，所以这个潜伏缺陷一直没暴露；新守卫改成「仅状态切换才动作」
/// 后 explorer 把副屏任务栏弹了回来，才第一次踩中。
///
/// 正确语义：这里要的是**主屏**任务栏的停泊矩形，与它当前可见与否无关。
///
/// - `primary_raw` = Shell_TrayWnd 本体的 rect 与其可见状态；
/// - `visible` = 当前视觉可见的全部任务栏 rect（仅在主屏任务栏窗口不存在时兜底）；
/// - 返回值已按主屏边界裁剪，`top` 即 dock 的贴靠基准。
pub(crate) fn resolve_primary_taskbar_rect(
    primary_raw: Option<(RECT, bool)>,
    visible: &[RECT],
    sw: i32,
    sh: i32,
) -> Option<RECT> {
    if let Some((mut r, is_visible)) = primary_raw {
        if !is_visible {
            // 2026-11-07 热修（sir 实测：剪映全屏 → 任务栏弹出 → 退出全屏后 dock 上移 ~46px）：
            // 全屏应用唤出过自动隐藏任务栏后，Shell_TrayWnd 的 GetWindowRect 会停在
            // 「可见位置」rect（top=1032, bottom=1080），而 IsWindowVisible=false（已收回）。
            // 旧热修信任该 rect → dock 定位在 962，比贴底位置高一个任务栏高度（46px）。
            // 判定：任务栏隐藏时停泊矩形 bottom 必然超出屏幕（如 top≈1078, bottom=1126）；
            // bottom ≤ sh 说明 rect 是失效的可见位置 → 按屏幕底边重建停泊矩形。
            let bar_h = r.bottom - r.top;
            if r.bottom <= sh {
                r.top = sh - 2;
                r.bottom = r.top + bar_h;
            }
        }
        // 隐藏任务栏的停泊矩形 bottom 在屏幕外（如 (0,1078,1920,1126)，屏高 1080）。
        // 直接返回会让 dock.rs 的贴边唤出阈值（tr.bottom - REVEAL_EDGE_PX）
        // 超出光标物理上限（1079），dock 永远唤不出。按主屏边界裁剪。
        r.left = r.left.max(0);
        r.top = r.top.max(0);
        r.right = r.right.min(sw);
        r.bottom = r.bottom.min(sh);
        if r.right > r.left && r.bottom > r.top {
            return Some(r);
        }
    }
    // 极端情况：主屏任务栏窗口不存在（explorer 重启窗口期）。
    // 此时才退回可见列表，但必须挑**与主屏相交**的那条，不能拿副屏的。
    visible
        .iter()
        .copied()
        .find(|r| r.left < sw && r.right > 0 && r.bottom > 0 && r.top < sh)
}

fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut r) }.is_ok() && r.right > r.left && r.bottom > r.top {
        Some(r)
    } else {
        None
    }
}

/// 前台窗口是否全屏覆盖某个显示器（dock 全屏让位判定）。
/// 注意：LockApp 锁屏时可能返回 true——锁屏时 dock 隐藏属预期。
///
/// v0.2.x 修复（sir 实测 bug：全屏应用下鼠标移到底边 dock 仍弹出）：
/// 旧判定要求窗口矩形**完全**覆盖 monitor rect，但真实全屏窗口（Chromium F11、
/// 无边框游戏、DPI 缩放偏移）常与 monitor 差 1~8px → 判 false → dock 弹出。
/// 新判定：覆盖 monitor 面积 ≥99.5% 即算全屏（容差吸收边框/圆角/DPI 偏差）。
/// 反例校验：最大化窗口 bottom 只到 1040/1080（覆盖率 96.3%）→ 不误判为全屏。
///
/// v0.5 修复（sir 实测 bug：开始菜单面板点桌面空白收回后 dock 几十秒不唤出）：
/// 点击桌面空白 → 前台 = Progman（桌面），其矩形 (0,0)-(4480,1600) 覆盖主屏 100%
/// → 被旧判定误判「全屏应用」→ dock 全屏让位，**桌面当前台期间 dock 永不唤出**
/// （探针 dockreveal 修复前实测：15s 超时不出现，foreground=Progman）。
/// shell 桌面/任务栏类窗口（与 running.rs is_skipped_class 同名单）不是全屏应用。
pub fn is_foreground_fullscreen() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        // 排除本进程（dock 自己不会全屏，避免自遮）
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == std::process::id() {
            return false;
        }
        // 排除 shell 桌面/任务栏类（Progman 双屏矩形覆盖主屏 100%，恒误判）
        let mut cls_buf = [0u16; 64];
        let n = windows::Win32::UI::WindowsAndMessaging::GetClassNameW(fg, &mut cls_buf).max(0)
            as usize;
        let cls = String::from_utf16_lossy(&cls_buf[..n]);
        if matches!(
            cls.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let hmon: HMONITOR = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut info).as_bool() {
            return false;
        }
        let m = info.rcMonitor;
        let mw = (m.right - m.left).max(1) as f64;
        let mh = (m.bottom - m.top).max(1) as f64;
        // 前台窗与 monitor 的交集面积 / monitor 面积
        let ix = (r.right.min(m.right) - r.left.max(m.left)).max(0) as f64;
        let iy = (r.bottom.min(m.bottom) - r.top.max(m.top)).max(0) as f64;
        (ix * iy) / (mw * mh) >= 0.995
    }
}

/// 前台窗口是否处于最大化状态（非本进程、非 shell 桌面类）。
///
/// 用途：dock 显示逻辑的「最大化让位」——v0.7.2 sir 反馈「dock 一直在
/// 最上面」盖住最大化窗口，当时的修法是显示后 SetWindowPos(HWND_BOTTOM)
/// 把 dock 压到同层底部；但 HWND_BOTTOM 会把 dock 压到桌面图标视图
/// （SHELLDLL_DefView）之下，导致 dock 的鼠标点击全被桌面层吃掉
/// （sir 反馈「图标点了没反应」，2026-11-13）。改为：dock 保持同层顶部
/// （SW_SHOWNA 不激活、不压底），前台最大化时 dock 直接不显示。
pub fn is_foreground_maximized() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == std::process::id() {
            return false;
        }
        let mut cls_buf = [0u16; 64];
        let n = windows::Win32::UI::WindowsAndMessaging::GetClassNameW(fg, &mut cls_buf).max(0)
            as usize;
        let cls = String::from_utf16_lossy(&cls_buf[..n]);
        if matches!(
            cls.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return false;
        }
        IsZoomed(fg).as_bool()
    }
}

/// 主屏尺寸（物理像素，dock 定位用）。
///
/// 用 `winplat::screen::primary_screen()`（MonitorFromPoint + GetMonitorInfoW）而不是
/// `GetSystemMetrics(SM_CXSCREEN/SM_CYSCREEN)`：后者返回的是**系统 DPI 上下文**下的
/// 主屏尺寸，跨屏不同缩放时不是主屏的真实物理像素。2026-11-16 dock 不可见事故里，
/// 主屏 1920×1080@96dpi 与副屏 2560×1600@144dpi 并存，逻辑坐标与物理坐标一旦混用
/// 就会把窗口摆到无人区。
pub fn primary_screen() -> (i32, i32) {
    let r = winplat::screen::primary_screen();
    (r.width(), r.height())
}

#[cfg(test)]
#[path = "desktop_taskbar_tests.rs"]
mod taskbar_tests;

//! UIA 兜底：轮询通知窗口 + 子树遍历取标题。
//!
//! 覆盖少数不走通知中心、也不发 WinRT 事件的来源。旧仓实测该路径会与 db 路径**重复上报**
//! 且依赖 Toast 窗口 5~7s 的存续，因此本仓默认关闭 —— 由
//! `settings.notifications.uiaFallback` 显式打开；打开时由汇聚层去重。
//!
//! 两条纪律：
//! - 只轮询白名单窗口类 + 尺寸闸门，不订阅整个桌面树（性能）；
//! - 只取收集到的**第一段文本**作标题，其余正文不进入数据结构、不进日志。

use std::collections::HashSet;
use std::sync::{mpsc::Sender, Mutex};
use std::time::Duration;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Descendants,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowRect, IsWindowVisible,
};

use super::{now_unix_ms, RawNotification};

/// 通知窗口类**前缀**白名单。
///
/// 实测 Win11 上真实宿主类带 `_WASDK` 后缀（旧版精确相等匹配导致通知全漏）。
/// `ApplicationFrameWindow` / `DesktopWindowContentBridge` 是所有 UWP 窗口通用类，
/// 会造成启动时误捕假通知，**不得加入**。
const ALLOWED_CLASS_PREFIXES: &[&str] = &[
    "XamlExplorerHostIslandWindow",
    "Xaml_WindowedPopupClass",
    "Windows.UI.Core.CoreWindow",
    "Windows.UI.Input.InputSite.WindowClass",
];

const MAX_TOAST_W: i32 = 700;
const MAX_TOAST_H: i32 = 450;
const MAX_CHILDREN: i32 = 40;

struct PollCtx<'a> {
    automation: &'a IUIAutomation,
    tx: &'a Sender<RawNotification>,
    seen: &'a Mutex<HashSet<String>>,
}

/// Toast 是小窗口；超大窗口（应用主窗）直接跳过，防误捕。
fn toast_size_ok(w: i32, h: i32) -> bool {
    w > 0 && h > 0 && w <= MAX_TOAST_W && h <= MAX_TOAST_H
}

pub fn spawn(tx: Sender<RawNotification>, enabled: bool) -> bool {
    if !enabled {
        winplat::log::log("notifications: uia source disabled by config count=0");
        return false;
    }
    std::thread::Builder::new()
        .name("glasssuite-notification-uia".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let automation: windows::core::Result<IUIAutomation> =
                unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) };
            match automation {
                Ok(a) => {
                    winplat::log::log("notifications: uia source started count=1");
                    poll(a, tx);
                }
                Err(_) => {
                    winplat::log::log("notifications: uia source unavailable, degraded count=1")
                }
            }
        })
        .is_ok()
}

fn poll(automation: IUIAutomation, tx: Sender<RawNotification>) {
    let seen = Mutex::new(HashSet::<String>::new());
    loop {
        let ctx = PollCtx {
            automation: &automation,
            tx: &tx,
            seen: &seen,
        };
        let ctx_ptr = &ctx as *const PollCtx as isize;
        unsafe {
            let _ = EnumWindows(Some(enum_window), LPARAM(ctx_ptr));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// 遍历元素子树，只返回第一段非空文本。
fn first_text(automation: &IUIAutomation, element: &IUIAutomationElement) -> Option<String> {
    let name = unsafe { element.CurrentName() };
    if let Ok(name) = name {
        let s = name.to_string();
        if !s.trim().is_empty() {
            return Some(s.trim().to_string());
        }
    }
    let condition = unsafe { automation.CreateTrueCondition() };
    let Ok(condition) = condition else {
        return None;
    };
    let array = unsafe { element.FindAll(TreeScope_Descendants, &condition) };
    let Ok(array) = array else {
        return None;
    };
    let length = unsafe { array.Length() }.unwrap_or(0).min(MAX_CHILDREN);
    for i in 0..length {
        let child = unsafe { array.GetElement(i) };
        let Ok(child) = child else {
            continue;
        };
        let name = unsafe { child.CurrentName() };
        if let Ok(name) = name {
            let s = name.to_string();
            if !s.trim().is_empty() {
                return Some(s.trim().to_string());
            }
        }
    }
    None
}

unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    let mut class = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd, &mut class) };
    if length == 0 {
        return BOOL(1);
    }
    let class_name = String::from_utf16_lossy(&class[..length as usize]);

    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return BOOL(1);
    }
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;

    if !ALLOWED_CLASS_PREFIXES
        .iter()
        .any(|p| class_name.starts_with(*p))
        || !toast_size_ok(w, h)
    {
        return BOOL(1);
    }

    // 回调上下文在 EnumWindows 调用期间有效。
    let ctx = &*(lparam.0 as *const PollCtx);
    let element = unsafe { ctx.automation.ElementFromHandle(hwnd) };
    let Ok(element) = element else {
        return BOOL(1);
    };
    let Some(title) = first_text(ctx.automation, &element) else {
        return BOOL(1);
    };

    // 去重键刻意不含窗口类名：日志与去重都不应保留来源窗口细节。
    let key = format!("{title}|{}", now_unix_ms() / 3_000);
    {
        let mut seen = ctx.seen.lock().unwrap();
        if !seen.insert(key) {
            return BOOL(1);
        }
        if seen.len() > 500 {
            seen.clear();
        }
    }

    let _ = ctx.tx.send(RawNotification {
        app: "Windows 通知".to_string(),
        title,
        arrived_at: now_unix_ms(),
        source: "uia-fallback",
    });
    BOOL(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_uses_prefix_not_exact_match() {
        // 旧版踩过的坑：真实类名带 _WASDK 后缀。
        let real = "XamlExplorerHostIslandWindow_WASDK";
        assert!(ALLOWED_CLASS_PREFIXES.iter().any(|p| real.starts_with(*p)));
        assert!(!ALLOWED_CLASS_PREFIXES
            .iter()
            .any(|p| "ApplicationFrameWindow".starts_with(*p)));
    }

    #[test]
    fn size_gate_rejects_main_windows() {
        assert!(toast_size_ok(400, 200));
        assert!(!toast_size_ok(1920, 1080), "应用主窗不应被当成 Toast");
        assert!(!toast_size_ok(0, 100));
    }
}

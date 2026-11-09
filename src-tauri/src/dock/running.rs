//! 运行中应用枚举（dock 的 transient 图标来源）。
//!
//! 参考 `glassdesk/src-tauri/src/running.rs` 的 key 体系（`exe:<规范化路径>`），
//! 但收敛为**按需扫描 + 硬超时**：
//! - 只枚举有可见顶层窗口的进程（通常 10~30 个），不做全进程表遍历；
//! - `EnumWindows` 回调只收集 `(pid, hwnd)`，进程路径随后逐个查，回调不阻塞；
//! - 整轮扫描放在独立短命线程，外层 2s `recv_timeout` 硬截止。单进程下
//!   「dock 卡 = 岛也卡」，这条不是可选项（卡面 D5 / 硬约束 14）。
//!
//! 不用 500ms 无条件轮询推前端：内容指纹比对在 `mod.rs` 的 watcher 里做，
//! 集合没变就不 emit，前端再做一次指纹比对（v0.8.1 防跳修复 #1）。
//!
//! 日志只记事件类型与数量，进程路径不进日志（硬约束 13）。

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindowVisible,
    GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

const SCAN_DEADLINE: Duration = Duration::from_secs(2);

/// 一个可见应用窗口对应的进程记录。`key` 是 dock 排序/去重的稳定标识。
#[derive(Debug, Clone, PartialEq)]
pub struct RunningApp {
    pub key: String,
    pub path: String,
    /// 原生窗口句柄（地址），DWM 悬停预览要用；0 = 不可用。
    pub hwnd: isize,
}

/// 路径归一化：小写、正斜杠改反斜杠、去掉 `\\?\` 前缀。
pub fn normalize_path(p: &str) -> String {
    p.trim()
        .trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .to_lowercase()
}

pub fn key_for_path(p: &str) -> String {
    format!("exe:{}", normalize_path(p))
}

fn is_shell_class(name: &str) -> bool {
    winplat::fullscreen::SHELL_CLASSES
        .iter()
        .any(|c| name.eq_ignore_ascii_case(c))
        || name.starts_with("Windows.UI.Core.CoreWindow")
}

/// 带硬截止的扫描。`None` = 超时/无法启动线程，**不是「一个程序都没有」**：
/// 两者必须区分，否则一次卡住会把 dock 上的运行图标全部清空再重建（视觉闪跳）。
pub fn scan_apps_bounded() -> Option<Vec<RunningApp>> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("glasssuite-running-scan".into())
        .spawn(move || {
            let _ = tx.send(scan_apps());
        })
        .is_ok();
    if !spawned {
        return None;
    }
    match rx.recv_timeout(SCAN_DEADLINE) {
        Ok(v) => Some(v),
        Err(_) => {
            winplat::log::log("dock: running scan timeout, degraded count=1");
            None
        }
    }
}

/// 只取 key 列表（供单测和指纹计算，避免带上窗口句柄）。
pub fn scan_bounded() -> Vec<String> {
    scan_apps_bounded()
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.key)
        .collect()
}

fn scan_apps() -> Vec<RunningApp> {
    let mut seen: Vec<(u32, isize)> = Vec::new();
    let ptr = &mut seen as *mut Vec<(u32, isize)> as isize;
    unsafe {
        let _ = EnumWindows(Some(enum_window), LPARAM(ptr));
    }

    // 同一进程多个窗口只留一个图标；优先保留第一个可见主窗口的 hwnd 供预览。
    let mut by_key: HashMap<String, RunningApp> = HashMap::new();
    for (pid, hwnd) in seen {
        let Some(path) = process_path(pid) else {
            continue;
        };
        let key = key_for_path(&path);
        by_key
            .entry(key.clone())
            .or_insert(RunningApp { key, path, hwnd });
    }
    let mut out: Vec<RunningApp> = by_key.into_values().collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if !ok || len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    let mut class = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut class) };
    if n > 0 {
        let name = String::from_utf16_lossy(&class[..n as usize]);
        if is_shell_class(&name) {
            return BOOL(1);
        }
    }
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    if ex & (WS_EX_TOOLWINDOW.0 as isize) != 0 {
        return BOOL(1); // 我们自己的两窗 / 其它工具窗
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 || pid == std::process::id() {
        return BOOL(1);
    }
    let list = &mut *(lparam.0 as *mut Vec<(u32, isize)>);
    list.push((pid, hwnd.0 as isize));
    BOOL(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(
            normalize_path(r"\\?\C:\Program Files\App\App.EXE"),
            r"c:\program files\app\app.exe"
        );
        // 用通用盘符而非本机用户目录：保持 A5 宽松 grep 只有 log.rs 那一处夹具命中。
        assert_eq!(normalize_path("D:/Apps/App.EXE"), r"d:\apps\app.exe");
    }

    #[test]
    fn key_has_stable_prefix() {
        assert_eq!(key_for_path(r"C:\App\A.exe"), r"exe:c:\app\a.exe");
        assert_eq!(key_for_path("C:/App/A.exe"), key_for_path(r"C:\App\A.exe"));
    }

    #[test]
    fn shell_classes_are_filtered() {
        assert!(is_shell_class("Shell_TrayWnd"));
        assert!(is_shell_class("Progman"));
        assert!(is_shell_class("Windows.UI.Core.CoreWindow_ABC"));
        assert!(!is_shell_class("Notepad"));
    }
}

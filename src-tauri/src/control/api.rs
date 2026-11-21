//! 控制中心四开关 IPC（OC-20261118-002）。
//!
//! 每个命令：改 settings.control 内存态 → persist 写盘 → 生效（热开关/写守卫意图）。
//! 岛/Dock 是热开关（不销毁窗口），图标/任务栏只写意图文件（守卫执行）。

use tauri::{AppHandle, Manager, State};

use crate::settings::{self, ControlSettings, Settings};

/// 读取四个开关的当前状态（前端首次加载时调用）。
#[tauri::command]
pub fn control_get_state(store: State<'_, std::sync::Mutex<Settings>>) -> ControlSettings {
    store.lock().unwrap().control.clone()
}

/// OC-20261118-003 / OC-20261121-003 §2.1：逃生热键（多候选其一）是否注册成功。
/// 控制页首次加载查询；false 时显式告警"逃生舱不可用，请勿同时隐藏任务栏和桌面图标"。
#[tauri::command]
pub fn control_hotkey_status() -> bool {
    crate::control::state::hotkey_registered()
}

/// OC-20261118-003 §2.3：实况回显——返回"实际"桌面/窗口状态，供控制页对比"意图"告警。
/// 与 control_get_state（意图）分开：守卫没跑/权限不足时两者会背离，控制页据此橙色告警。
// OC-20261121-008：必须 camelCase，否则前端 ControlLiveStatus interface 读
// iconsHidden/islandVisible/dockVisible 全是 undefined → driftMessage 三项恒报
// 「图标仍可见/岛未显示/Dock未显示」误报。与 ControlSettings 的 rename_all 口径一致。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLiveStatus {
    /// 主屏任务栏是否真的可见（IsWindowVisible(Shell_TrayWnd)）。
    pub taskbar_visible: bool,
    /// 桌面原生图标是否被隐藏（注册表 HideIcons==1）。
    pub icons_hidden: bool,
    /// 灵动岛窗口是否可见。
    pub island_visible: bool,
    /// Dock 窗口是否可见。
    pub dock_visible: bool,
}

#[tauri::command]
pub fn control_live_status(app: AppHandle) -> ControlLiveStatus {
    #[cfg(windows)]
    let taskbar_visible = {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsWindowVisible};
        unsafe {
            FindWindowW(w!("Shell_TrayWnd"), windows::core::PCWSTR::null())
                .map(|h| IsWindowVisible(h).as_bool())
                .unwrap_or(true)
        }
    };
    #[cfg(not(windows))]
    let taskbar_visible = true;

    #[cfg(windows)]
    let icons_hidden = {
        use windows::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        };
        use windows::core::w;
        let subkey = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced");
        let value = w!("HideIcons");
        let mut hk = HKEY(std::ptr::null_mut());
        let mut out: u32 = 0;
        let mut found = false;
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, subkey, Some(0), KEY_READ, &mut hk).is_ok() {
                let mut ty = windows::Win32::System::Registry::REG_DWORD;
                let mut len = std::mem::size_of::<u32>() as u32;
                if RegQueryValueExW(
                    hk,
                    value,
                    None,
                    Some(&mut ty),
                    Some(&mut out as *mut u32 as *mut u8),
                    Some(&mut len),
                )
                .is_ok()
                {
                    found = true;
                }
                let _ = RegCloseKey(hk);
            }
        }
        found && out == 1
    };
    #[cfg(not(windows))]
    let icons_hidden = false;

    // OC-20261121-007：实况必须用 Win32 IsWindowVisible(hwnd)，不能用 Tauri
    // WebviewWindow::is_visible()。dock 窗口创建时 show:false，之后靠 dock_logic /
    // lib.rs 的 Win32 ShowWindow 显隐；Tauri 内部可见标志不会同步这个外部调用，
    // 导致 is_visible() 恒返回 false → 控制页误报「Dock 未显示」。island 也会被
    // apply_styles_noactivate 等 Win32 操作改写属性，口径统一走 hwnd 最稳。
    #[cfg(windows)]
    let win_visible = |label: &str| {
        use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
        app.get_webview_window(label)
            .and_then(|w| w.hwnd().ok())
            .map(|h| unsafe { IsWindowVisible(h).as_bool() })
            .unwrap_or(false)
    };
    #[cfg(not(windows))]
    let win_visible = |label: &str| {
        app.get_webview_window(label)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false)
    };

    ControlLiveStatus {
        taskbar_visible,
        icons_hidden,
        island_visible: win_visible("island"),
        dock_visible: win_visible("dock"),
    }
}

/// 通用：更新 control 段的一个字段并持久化 + 生效。
fn update_control<F: FnOnce(&mut ControlSettings)>(
    app: &AppHandle,
    store: &State<'_, std::sync::Mutex<Settings>>,
    change: F,
) -> Result<ControlSettings, String> {
    let next = {
        let mut guard = store.lock().unwrap();
        change(&mut guard.control);
        guard.clone()
    };
    settings::persist(&settings::data_dir(), &next)?;
    apply_control(app, &next.control);
    Ok(next.control)
}

/// 把 control 段应用进运行时。岛/Dock 热开关，图标/任务栏写守卫意图。
fn apply_control(app: &AppHandle, c: &ControlSettings) {
    // 岛：不销毁窗口；关闭时 hide（保留 DOM/热区状态），开启时 show。
    if let Some(w) = app.get_webview_window("island") {
        if c.island_enabled {
            let _ = w.show();
        } else {
            let _ = w.hide();
        }
    }
    // Dock：设置全局标志，dock_logic 循环下一轮即隐藏/恢复显示。
    crate::dock::set_dock_enabled(c.dock_enabled);
    // 桌面图标 + 任务栏：只写意图文件，守卫（每 250ms）读取执行。
    if let Err(e) = crate::control::state::set_flags(c.taskbar_hidden, c.desktop_icons_hidden) {
        winplat::log::log(&format!("control: 写守卫意图失败: {e}"));
    }
    winplat::log::log(&format!(
        "control: 已应用 island={} dock={} icons_hidden={} taskbar_hidden={}",
        c.island_enabled, c.dock_enabled, c.desktop_icons_hidden, c.taskbar_hidden
    ));
}

/// 启动时把持久化的 control 段应用一次（对齐守卫意图 + 岛/Dock 初始可见态）。
pub fn apply_on_boot(app: &AppHandle, c: &ControlSettings) {
    apply_control(app, c);
}

#[tauri::command]
pub fn control_set_island(
    app: AppHandle,
    store: State<'_, std::sync::Mutex<Settings>>,
    enabled: bool,
) -> Result<ControlSettings, String> {
    update_control(&app, &store, |c| c.island_enabled = enabled)
}

#[tauri::command]
pub fn control_set_dock(
    app: AppHandle,
    store: State<'_, std::sync::Mutex<Settings>>,
    enabled: bool,
) -> Result<ControlSettings, String> {
    update_control(&app, &store, |c| c.dock_enabled = enabled)
}

#[tauri::command]
pub fn control_set_icons(
    app: AppHandle,
    store: State<'_, std::sync::Mutex<Settings>>,
    hidden: bool,
) -> Result<ControlSettings, String> {
    update_control(&app, &store, |c| c.desktop_icons_hidden = hidden)
}

#[tauri::command]
pub fn control_set_taskbar(
    app: AppHandle,
    store: State<'_, std::sync::Mutex<Settings>>,
    hidden: bool,
) -> Result<ControlSettings, String> {
    update_control(&app, &store, |c| c.taskbar_hidden = hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_settings_default_has_island_dock_on() {
        let c = ControlSettings::default();
        assert!(c.island_enabled, "灵动岛默认开");
        assert!(c.dock_enabled, "Dock 默认开");
        assert!(c.desktop_icons_hidden, "沉浸态默认：桌面图标隐藏");
        assert!(c.taskbar_hidden, "沉浸态默认：任务栏隐藏");
    }

    #[test]
    fn control_live_status_serializes_camel_case_keys() {
        // OC-20261121-008：前端 interface 读 camelCase；snake_case 会让
        // driftMessage 三项恒报误报。锁死序列化键名，防回归。
        let live = ControlLiveStatus {
            taskbar_visible: false,
            icons_hidden: true,
            island_visible: true,
            dock_visible: true,
        };
        let json = serde_json::to_value(&live).unwrap();
        let obj = json.as_object().unwrap();
        for key in ["taskbarVisible", "iconsHidden", "islandVisible", "dockVisible"] {
            assert!(obj.contains_key(key), "缺少 camelCase 键 {key}: {obj:?}");
        }
        assert!(!obj.contains_key("icons_hidden"), "不得输出 snake_case");
        assert_eq!(obj["iconsHidden"], serde_json::json!(true));
    }
}

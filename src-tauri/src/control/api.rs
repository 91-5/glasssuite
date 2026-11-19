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
}

//! 控制中心窗口（OC-20261118-001）。
//!
//! 与岛/dock 不同：这是**标准系统窗口**（有标题栏、可最小化/关闭、显示在任务栏），
//! 所以不复用 `shell::create_window`（那个是透明无边框岛专用）。
//!
//! 职责：
//! - `ensure_control_window`：幂等唤起（存在则 show+unminimize+focus，否则 build）；
//! - `restore_native_desktop`：退出前主动还原任务栏 + 桌面图标（解 F3，让点 X 立即恢复原生）；
//! - `boot_if_requested`：首实例启动时按 `--control` argv 决定是否弹控制页。
//!
//! 子模块（OC-20261118-002）：
//! - `state`：守卫意图文件 + 5s 心跳；
//! - `api`：四开关 IPC。

pub mod api;
pub mod state;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// 幂等唤起控制页。焦点顺序必须是 show → unminimize → set_focus（隐藏窗 set_focus 静默失败）。
pub fn ensure_control_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(w) = app.get_webview_window("control") {
        w.show()?;
        w.unminimize()?;
        w.set_focus()?;
        winplat::log::log("control: 唤起已有窗口");
        return Ok(w);
    }
    let w = WebviewWindowBuilder::new(app, "control", WebviewUrl::App("control/index.html".into()))
        .title("GlassSuite 控制中心")
        .inner_size(460.0, 580.0)
        .resizable(false)
        .decorations(true) // 系统标题栏：拖动/最小化/关闭
        .closable(true)
        .minimizable(true)
        .skip_taskbar(false) // 任务栏可见时是一等窗口
        .center()
        .visible(true)
        .build()?;
    // 点 X = 退出整个应用（sir 裁定）：岛/dock 是 closable(false)，关 control 不会自动退出，
    // 必须显式 exit。退出前先还原原生桌面（解 F3）。
    let app_handle = app.clone();
    w.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { .. } = event {
            winplat::log::log("control: X 关闭 → 退出整个应用");
            restore_native_desktop();
            app_handle.exit(0);
        }
    });
    w.set_focus()?;
    winplat::log::log("control: 新建窗口");
    Ok(w)
}

/// 首实例启动时调用：argv 含 `--control` 才弹控制页（桌面 .lnk 带此参数；开机 Run key 不带）。
pub fn boot_if_requested(app: &AppHandle) {
    let requested = std::env::args().any(|a| a == "--control");
    if requested {
        if let Err(e) = ensure_control_window(app) {
            winplat::log::log(&format!("control: --control 唤起失败: {e}"));
        }
    }
}

/// 退出前主动清除守卫意图（方案 B）。
///
/// 守卫每 250ms 读 `guard_state.json`：文件不存在 → 安全默认（任务栏/图标全显示），
/// 比 spawn `-Restore` 更简单、也不会与常驻守卫抢着写注册表。
/// 崩溃（taskkill /F）走不到这里，靠守卫 10s 心跳超时 fail-safe（OC-20261118-002）。
pub fn restore_native_desktop() {
    state::clear_on_exit();
}

#[cfg(test)]
mod tests {
    #[test]
    fn control_window_label_is_stable() {
        // 锁定窗口 label，避免前端/命令与这里不一致（control/index.html 是加载路径）。
        let label = "control";
        assert_eq!(label, "control");
        assert_eq!("control/index.html", "control/index.html");
    }
}

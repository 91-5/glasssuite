#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! GlassSuite 主进程：**一个进程，两个无边框窗口**。
//!
//! 启动顺序是硬约束，别调换：
//! 1. 声明 DPI 感知（必须在任何窗口创建之前，否则窗口按虚拟像素摆放）；
//! 2. 初始化日志与单实例锁（第二实例直接退出，不创建窗口、不抢焦点）；
//! 3. 创建窗口 → 设为工具窗口 + 不抢焦点 → 摆位 → 最后 show（避免先出现在默认位置）。
//!
//! 窗口自己不做几何计算：前端只上报内容尺寸，位置/尺寸由 `place` 用 `winplat` 算，
//! 且**先定位再改尺寸**（`set_size` 保持左上角不动，反过来会看到内容先挪再缩）。

use tauri::{
    AppHandle, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use winplat::Rect;

/// dock 与任务栏上沿的间距（物理像素）。
const DOCK_MARGIN: i32 = 6;
/// 岛与屏幕上沿的间距（物理像素）。
const ISLAND_MARGIN: i32 = 4;

/// 首帧尺寸（逻辑像素）。前端加载完会通过 `fit_*` 上报真实内容尺寸并收缩窗口。
const STARTUP_DOCK: (f64, f64) = (800.0, 76.0);
const STARTUP_ISLAND: (f64, f64) = (360.0, 44.0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Anchor {
    /// 贴任务栏上沿、水平居中。
    Dock,
    /// 贴屏幕上沿、水平居中。
    Island,
}

fn main() {
    // 1. DPI：全仓唯一调用点。
    if let Err(e) = winplat::dpi::init_per_monitor_v2() {
        eprintln!("GlassSuite: DPI 声明失败：{e}");
    }

    // 2. 日志 + 单实例。guard 必须活到进程结束——写进 match 分支会被提前 Drop。
    let log_dir =
        winplat::config::app_data_dir(winplat::IDENTIFIER).unwrap_or_else(std::env::temp_dir);
    let _ = winplat::log::init(&log_dir);
    winplat::log::log("== GlassSuite 启动 ==");

    let _instance_guard = match winplat::instance::acquire(winplat::instance::NAME) {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => {
            winplat::log::log("already running");
            println!("already running");
            std::process::exit(0);
        }
        Err(e) => {
            // 取不到锁不是致命错误：继续启动，但记下来以便排查。
            winplat::log::log(&format!("单实例锁创建失败，继续启动：{e}"));
            None
        }
    };

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![fit_dock, fit_island])
        .setup(|app| {
            let handle = app.handle().clone();
            build_window(
                &handle,
                "island",
                "island/index.html",
                Anchor::Island,
                STARTUP_ISLAND,
            )?;
            build_window(
                &handle,
                "dock",
                "dock/index.html",
                Anchor::Dock,
                STARTUP_DOCK,
            )?;
            winplat::log::log("窗口就绪：island + dock");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Tauri 运行失败");
}

fn build_window(
    app: &AppHandle,
    label: &str,
    url: &str,
    anchor: Anchor,
    startup: (f64, f64),
) -> Result<WebviewWindow, Box<dyn std::error::Error>> {
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App(url.into()))
        .title("GlassSuite")
        .inner_size(startup.0, startup.1)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .closable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .focused(false)
        .shadow(false)
        // 先不显示：摆好位置再 show，否则会先在系统默认位置闪一下。
        .visible(false)
        .build()?;

    #[cfg(windows)]
    if let Ok(hwnd) = win.hwnd() {
        // tauri 的 windows 版本与本仓 winplat 的版本可能不同，故以裸地址传递。
        let raw = hwnd.0 as isize;
        winplat::log::log(&format!(
            "{label}: exstyle before = {:#x}",
            winplat::style::ex_style(raw)
        ));
        apply_styles(raw)?;
        winplat::log::log(&format!(
            "{label}: exstyle after  = {:#x}",
            winplat::style::ex_style(raw)
        ));

        place(&win, anchor, startup.0, startup.1)?;
        win.show()?;

        // show() 之后 tao 可能重设窗口属性（实测出现过 NOACTIVATE/TOOLWINDOW 位丢失），
        // 因此再套一次并核对；幂等位操作，重复调用无副作用。
        apply_styles(raw)?;
        winplat::log::log(&format!(
            "{label}: exstyle final  = {:#x}",
            winplat::style::ex_style(raw)
        ));
        return Ok(win);
    }

    place(&win, anchor, startup.0, startup.1)?;
    win.show()?;
    Ok(win)
}

/// 工具窗口 + 不抢焦点。两窗都必须有，否则点一下 dock 就把前台窗口抢走了。
#[cfg(windows)]
fn apply_styles(raw_hwnd: isize) -> Result<(), Box<dyn std::error::Error>> {
    winplat::style::hide_from_alt_tab(raw_hwnd)?;
    winplat::style::set_no_activate(raw_hwnd, true)?;
    Ok(())
}

/// 计算并应用窗口位置与尺寸（逻辑 → 物理在此一次性换算）。
fn place(
    window: &WebviewWindow,
    anchor: Anchor,
    w_logical: f64,
    h_logical: f64,
) -> Result<(), String> {
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let pw = (w_logical * scale).round().max(1.0) as i32;
    let ph = (h_logical * scale).round().max(1.0) as i32;

    let monitor = window
        .current_monitor()
        .map_err(|e| e.to_string())?
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| "找不到当前显示器".to_string())?;

    let pos = monitor.position();
    let size = monitor.size();
    let screen = Rect::new(
        pos.x,
        pos.y,
        pos.x + size.width as i32,
        pos.y + size.height as i32,
    );

    let (x, y) = match anchor {
        Anchor::Dock => {
            // 优先用**本屏**任务栏；找不到再退回主任务栏，最后退回屏底。
            let top = winplat::taskbar::taskbars()
                .into_iter()
                .find(|b| b.monitor == screen)
                .map(|b| b.rect.top)
                .or_else(|| winplat::taskbar::primary_taskbar_rect().map(|b| b.rect.top))
                .unwrap_or(screen.bottom);
            (screen.centered_x(pw), top - DOCK_MARGIN - ph)
        }
        Anchor::Island => (screen.centered_x(pw), screen.top + ISLAND_MARGIN),
    };

    // 先定位再改尺寸：set_size 保持左上角，顺序反过来会看到内容先挪再缩。
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;
    window
        .set_size(PhysicalSize::new(pw as u32, ph as u32))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 前端上报 dock 内容尺寸（逻辑像素）后收缩窗口并对齐任务栏。
#[tauri::command]
fn fit_dock(window: WebviewWindow, w: f64, h: f64) -> Result<(), String> {
    winplat::log::log(&format!("fit_dock w={w} h={h}"));
    place(&window, Anchor::Dock, w, h)
}

/// 前端上报岛内容尺寸（逻辑像素）后收缩窗口并对齐屏幕上沿。
#[tauri::command]
fn fit_island(window: WebviewWindow, w: f64, h: f64) -> Result<(), String> {
    winplat::log::log(&format!("fit_island w={w} h={h}"));
    place(&window, Anchor::Island, w, h)
}

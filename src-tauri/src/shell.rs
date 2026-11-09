//! 窗口层：建窗、摆位、内容自适应尺寸。
//!
//! 窗口自己不做几何计算：前端只上报内容尺寸，位置/尺寸由这里用 `winplat` 算，
//! 且**先定位再改尺寸**（`set_size` 保持左上角不动，反过来会看到内容先挪再缩）。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use winplat::Rect;

use crate::state::{Hotzone, SharedState};

/// dock 与任务栏上沿的间距（物理像素）。
const DOCK_MARGIN: i32 = 6;
/// 岛与屏幕上沿的间距（物理像素）。
const ISLAND_MARGIN: i32 = 4;

/// 首帧尺寸（逻辑像素）。前端加载完会通过 `fit_*` 上报真实内容尺寸并收缩窗口。
pub const STARTUP_DOCK: (f64, f64) = (800.0, 76.0);
pub const STARTUP_ISLAND: (f64, f64) = (360.0, 44.0);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// 贴任务栏上沿、水平居中。
    Dock,
    /// 贴屏幕上沿、水平居中。
    Island,
}

/// 建 island 与 dock 两个窗口。先 island 后 dock：dock 依赖任务栏几何，晚一步更稳。
pub fn create_windows(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    build_window(
        app,
        "island",
        "island/index.html",
        Anchor::Island,
        STARTUP_ISLAND,
    )?;
    build_window(app, "dock", "dock/index.html", Anchor::Dock, STARTUP_DOCK)?;
    Ok(())
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

        // 系统缩放变化（设置里改 DPI、跨屏不同缩放）后按新缩放重摆，
        // 否则窗口还留在旧物理坐标上，看起来「跑偏了」。
        let handle = app.clone();
        let anchor_copy = anchor;
        let size = startup;
        let own_label = label.to_string();
        win.on_window_event(move |event| {
            if let tauri::WindowEvent::ScaleFactorChanged { .. } = event {
                if let Some(w) = handle.get_webview_window(&own_label) {
                    let _ = place(&w, anchor_copy, size.0, size.1);
                }
            }
        });

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

/// 算出「新尺寸下窗口该在哪」——**中心锚定**，不是保持旧左上角。
///
/// 这是 v0.8.1 防跳修复 #4 的几何基础：窗口宽度随图标增减变化时，若让 `set_size`
/// 以旧左上角为锚，内容会整体平移半个增量，前端 FLIP 会把这一帧当成图标位移，
/// 于是整排图标一起跳动。正确做法是先用新宽度算出居中位置并同步 `set_position`。
fn compute_geometry(
    window: &WebviewWindow,
    anchor: Anchor,
    w_logical: f64,
    h_logical: f64,
) -> Result<(PhysicalPosition<i32>, PhysicalSize<u32>), String> {
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
    Ok((
        PhysicalPosition::new(x, y),
        PhysicalSize::new(pw as u32, ph as u32),
    ))
}

/// 计算并应用窗口位置与尺寸（逻辑 → 物理在此一次性换算）。
pub fn place(
    window: &WebviewWindow,
    anchor: Anchor,
    w_logical: f64,
    h_logical: f64,
) -> Result<(), String> {
    let (position, size) = compute_geometry(window, anchor, w_logical, h_logical)?;
    // 先定位再改尺寸：set_size 保持左上角，顺序反过来会看到内容先挪再缩。
    window.set_position(position).map_err(|e| e.to_string())?;
    window.set_size(size).map_err(|e| e.to_string())?;
    Ok(())
}

/// 按绝对屏幕坐标摆放辅助窗口（开始菜单 / 游离图标）。
///
/// 与 `place` 的区别：这两个窗口不是「贴屏幕上沿 / 贴任务栏」，位置由内容决定，
/// 所以不接受 `Anchor`。仍然保持「先定位后改尺寸」的同步顺序。
pub fn place_at(
    window: &WebviewWindow,
    x_logical: f64,
    y_logical: f64,
    w_logical: f64,
    h_logical: f64,
) -> Result<(), String> {
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    window
        .set_position(PhysicalPosition::new(
            (x_logical * scale).round() as i32,
            (y_logical * scale).round() as i32,
        ))
        .map_err(|e| e.to_string())?;
    window
        .set_size(PhysicalSize::new(
            (w_logical * scale).round().max(1.0) as u32,
            (h_logical * scale).round().max(1.0) as u32,
        ))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 开始菜单面板：在 dock 正上方居中摆放（物理像素偏移按当前缩放换算）。
///
/// 不引入固定尺寸热区窗：面板本身尺寸固定，但窗口尺寸就等于面板尺寸，
/// 与 island 展开面板同一套 resize 模型。
pub fn place_above_dock(
    window: &WebviewWindow,
    w_logical: f64,
    h_logical: f64,
    dock_h_logical: f64,
) -> Result<(), String> {
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let (pos, size) = compute_geometry(window, Anchor::Dock, w_logical, h_logical)?;
    let y = pos.y - (dock_h_logical * scale).round() as i32;
    window
        .set_position(PhysicalPosition::new(pos.x, y))
        .map_err(|e| e.to_string())?;
    window.set_size(size).map_err(|e| e.to_string())?;
    Ok(())
}

/// 按需创建辅助窗口（第三个及以后的窗口）。重复调用返回既有窗口，不新建。
///
/// 复用 `dock/index.html` 的 hash 路由，避免为开始菜单 / 游离图标再起一套前端构建。
/// 窗口一律**先隐藏**创建，由调用方摆好位置再 show。
pub fn ensure_window(
    app: &AppHandle,
    label: &str,
    url: &str,
    w: f64,
    h: f64,
) -> Result<WebviewWindow, String> {
    if let Some(win) = app.get_webview_window(label) {
        return Ok(win);
    }
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App(url.into()))
        .title("GlassSuite")
        .inner_size(w, h)
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
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;
    #[cfg(windows)]
    if let Ok(hwnd) = win.hwnd() {
        apply_styles(hwnd.0 as isize).map_err(|e| e.to_string())?;
    }
    Ok(win)
}

/// 记录内容尺寸、缩放与热区。
///
/// 热区直接覆盖整个窗口客户区：窗口尺寸 = 内容尺寸，前端传来的 `w/h` 已含留白，
/// 所以不存在「透明宽窗吃掉桌面点击」的问题。resize 与热区更新在同一次调用里完成，
/// 避免前端 ResizeObserver 未触发时热区滞后。
fn remember(window: &WebviewWindow, state: &SharedState, label: &str, w: f64, h: f64) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let hz = Hotzone {
        x: 0.0,
        y: 0.0,
        w,
        h,
    };
    if label == "dock" {
        *state.dock_hotzone.lock().unwrap() = hz;
        *state.dock_scale.lock().unwrap() = scale;
        *state.dock_size.lock().unwrap() = (w, h);
    } else {
        *state.island_hotzone.lock().unwrap() = hz;
        *state.island_scale.lock().unwrap() = scale;
        *state.island_collapsed_size.lock().unwrap() = (w, h);
    }
}

/// 前端上报 dock 内容尺寸（逻辑像素）后收缩窗口并对齐任务栏。
#[tauri::command]
pub fn fit_dock(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<SharedState>>,
    w: f64,
    h: f64,
) -> Result<(), String> {
    winplat::log::log(&format!("fit_dock w={w} h={h}"));
    remember(&window, state.inner().as_ref(), "dock", w, h);

    // v0.8.1 防跳修复 #4：在**缩放之前**先按「新窗口中心 = 屏幕中心」同步 set_position。
    // 若只调 place()，虽然 place 内部也是先定位后改尺寸，但那一步与 set_size 之间存在
    // 一次 IPC/合成器往返；旧仓在图标连续增减时正是这一帧被 FLIP 读成整排位移。
    // 这里先单独把原点摆到新尺寸的居中位置，place() 再幂等确认一次并改尺寸。
    let (position, _) = compute_geometry(&window, Anchor::Dock, w, h)?;
    window.set_position(position).map_err(|e| e.to_string())?;

    place(&window, Anchor::Dock, w, h)
}

/// 前端上报岛内容尺寸（逻辑像素）后收缩窗口并对齐屏幕上沿。
///
/// 展开态忽略尺寸上报：面板是固定尺寸，ResizeObserver 会把药丸尺寸报回来把它缩掉。
/// 但仍记录折叠尺寸，收起时用它复位。
#[tauri::command]
pub fn fit_island(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<SharedState>>,
    w: f64,
    h: f64,
) -> Result<(), String> {
    winplat::log::log(&format!("fit_island w={w} h={h}"));
    remember(&window, state.inner().as_ref(), "island", w, h);
    if state.island_expanded.load(Ordering::Relaxed) {
        return Ok(());
    }
    place(&window, Anchor::Island, w, h)
}

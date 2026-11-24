//! 窗口层：建窗、摆位、内容自适应尺寸。
//!
//! 窗口自己不做几何计算：前端只上报内容尺寸，位置/尺寸由这里用 `winplat` 算，
//! 且**先定位再改尺寸**（`set_size` 保持左上角不动，反过来会看到内容先挪再缩）。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use winplat::Rect;

use crate::state::{Hotzone, SharedState};

/// dock 与任务栏上沿的间距（物理像素）。
const DOCK_MARGIN: i32 = 6;
/// 岛与屏幕上沿的间距（物理像素）。
const ISLAND_MARGIN: i32 = 10;

/// 首帧尺寸（逻辑像素）。前端加载完会通过 `fit_*` 上报真实内容尺寸并收缩窗口。
pub const STARTUP_DOCK: (f64, f64) = (800.0, 76.0);
/// 岛折叠窗固定尺寸（移植自 glasspill：内容在窗内自由布局，位移靠热区判定）。
pub const ISLAND_W: f64 = 420.0;
pub const ISLAND_H: f64 = 210.0;
pub const STARTUP_ISLAND: (f64, f64) = (ISLAND_W, ISLAND_H);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// 贴任务栏上沿、水平居中。
    Dock,
    /// 贴屏幕上沿、水平居中。
    Island,
}

/// 建一个无边框窗口的规格（参数收进结构体，避免长参数列）。
struct WindowSpec<'a> {
    label: &'a str,
    url: &'a str,
    anchor: Anchor,
    startup: (f64, f64),
    always_on_top: bool,
    no_activate: bool,
    // dock / startmenu 走 glassdesk 的「隐藏→就绪→显示」模型，
    // 由 dock_logic / menu::toggle 决定何时显示，故建窗时传 false。
    show: bool,
}

/// 建 island 窗口。dock / startmenu 由 [`create_dock_windows`] 独立建：
/// 它们沿用 glassdesk 的「隐藏 → 就绪 → 显示」模型（dock_logic 自动隐藏逻辑管显示），
/// 且**不能**设 NOACTIVATE——dock 要能拿到焦点，否则「添加程序」的文件对话框
/// 会弹在当前前台应用后面（glassdesk v0.7.1 修复过这个坑）。
pub fn create_windows(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    build_window(
        app,
        WindowSpec {
            label: "island",
            url: "island/index.html",
            anchor: Anchor::Island,
            startup: STARTUP_ISLAND,
            always_on_top: true,
            no_activate: true,
            show: true,
        },
    )?;
    Ok(())
}

/// 建 dock 与开始菜单窗口（均先隐藏，由各自逻辑决定何时显示）。
///
/// - dock：桌面层（D7 语义）——是否置顶由 dock.json 的 `dock_top_layer` 决定，
///   因此这里建为**非置顶**，`lib.rs` setup 再按配置 `set_always_on_top`。
/// - startmenu：置顶面板，失焦收回，由 `menu::toggle` 摆位与显隐。
pub fn create_dock_windows(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    build_window(
        app,
        WindowSpec {
            label: "dock",
            url: "dock/index.html",
            anchor: Anchor::Dock,
            startup: STARTUP_DOCK,
            always_on_top: false,
            no_activate: false,
            show: false,
        },
    )?;
    build_window(
        app,
        WindowSpec {
            label: "startmenu",
            url: "dock/index.html",
            anchor: Anchor::Dock,
            startup: (crate::dock::menu::PANEL_W, crate::dock::menu::PANEL_H),
            always_on_top: true,
            no_activate: false,
            show: false,
        },
    )?;
    Ok(())
}

fn build_window(
    app: &AppHandle,
    spec: WindowSpec,
) -> Result<WebviewWindow, Box<dyn std::error::Error>> {
    let WindowSpec {
        label,
        url,
        anchor,
        startup,
        always_on_top,
        no_activate,
        show,
    } = spec;
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
        .always_on_top(always_on_top)
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
        if no_activate {
            apply_styles_noactivate(raw)?;
        } else {
            apply_styles(raw)?;
        }
        winplat::log::log(&format!(
            "{label}: exstyle after  = {:#x}",
            winplat::style::ex_style(raw)
        ));

        place(&win, anchor, startup.0, startup.1)?;
        if show {
            win.show()?;
        }

        // show() 之后 tao 可能重设窗口属性（实测出现过 NOACTIVATE/TOOLWINDOW 位丢失），
        // 因此再套一次并核对；幂等位操作，重复调用无副作用。
        if no_activate {
            let _ = apply_styles_noactivate(raw);
        } else {
            let _ = apply_styles(raw);
        }
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
                match scale_changed_action(&own_label) {
                    // startmenu 不能按 Anchor::Dock 摆（会摆到任务栏上方 dock 位）。
                    // 它走 menu::toggle 的自适应几何（含目标屏 scale），这里只清缓存强制重算。
                    ScaleChangedAction::InvalidateMenuAnchor => {
                        crate::dock::menu::invalidate_anchor_cache();
                    }
                    // dock 的**宽度单一真相源是前端 fit_dock**（药丸实际宽度 + padding），
                    // 不是这里的 startup 常量。
                    //
                    // 2026-11-24 修复（OC-20261124-001，sir 报「dock 右半被截断、偶尔发生」）：
                    // 旧实现对 dock 也走 `place(&w, anchor_copy, size.0, size.1)`，
                    // 而 `size` = startup = STARTUP_DOCK = (800, 76)。于是：
                    //   ① 前端 fit_dock 已把窗口撑到内容宽（实测 1076）；
                    //   ② 触发 ScaleFactorChanged（换屏 / 改 DPI）→ place(startup) 把窗口
                    //      **写回 800**；
                    //   ③ 前端 lastFit 去重是「与上次**上报值**比较」，它仍记得自己报过
                    //      1076 → 认为无需变更 → **永远不再上报**；
                    //   ④ 窗口永久卡在 800，内容 1064 被裁掉右侧约 264px（微信等图标
                    //      只显示一半）。
                    // 「偶尔」的成因：只有触发 DPI 事件时才发生（换显示器、跨屏）。
                    //
                    // 现在：dock 只**重定位**不**改尺寸**（尺寸交给前端 fit_dock），
                    // 且重定位后主动通知前端重新测量，避免外部改动后前端不自知。
                    ScaleChangedAction::RepositionOnly => {
                        if let Some(w) = handle.get_webview_window(&own_label) {
                            let _ = reposition_only(&w, anchor_copy);
                            notify_refit(&w);
                        }
                    }
                    ScaleChangedAction::PlaceWithStartupSize => {
                        if let Some(w) = handle.get_webview_window(&own_label) {
                            let _ = place(&w, anchor_copy, size.0, size.1);
                        }
                    }
                }
            }
        });

        return Ok(win);
    }

    place(&win, anchor, startup.0, startup.1)?;
    if show {
        win.show()?;
    }
    Ok(win)
}

/// 工具窗口 + 不抢焦点。两窗都必须有，否则点一下 dock 就把前台窗口抢走了。
#[cfg(windows)]
fn apply_styles(raw_hwnd: isize) -> Result<(), Box<dyn std::error::Error>> {
    winplat::style::hide_from_alt_tab(raw_hwnd)?;
    Ok(())
}

/// 工具窗口 + 不抢焦点（island 专用：岛是常显装饰窗，不需要拿焦点）。
#[cfg(windows)]
fn apply_styles_noactivate(raw_hwnd: isize) -> Result<(), Box<dyn std::error::Error>> {
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
    // 目标显示器：**固定主屏**。
    //
    // 2026-11-16 修复（dock 整体不可见）：旧实现用 `window.current_monitor()`。
    // 新建窗口的初始位置由 Windows 决定（跟随鼠标所在屏），于是光标停在
    // 150% 的副屏上时，dock 从诞生起就挂在副屏的 DPI 上下文里
    // （GetDpiForWindow=144），尺寸被放大 1.5 倍（800 逻辑 → 1200 物理），
    // 而 dock/mod.rs 的水平居中又按**主屏宽**算 → x 与 y 分属两块屏，
    // 窗口被摆到 (360,1478)——两块显示器之间的无人区，没有任何显示表面，
    // 于是 dock 完全不渲染（PrintWindow 仍能截到内容，因为 DComp 表面是好的）。
    //
    // 现在：目标屏 = 主屏，且尺寸换算用**目标屏自己的 scale_factor()**，
    // 不用窗口当前上下文的（那是上一块屏遗留的 DPI）。
    let monitor = window
        .primary_monitor()
        .map_err(|e| e.to_string())?
        .or_else(|| window.current_monitor().ok().flatten())
        .ok_or_else(|| "找不到主显示器".to_string())?;

    let scale = monitor.scale_factor();
    let pw = (w_logical * scale).round().max(1.0) as i32;
    let ph = (h_logical * scale).round().max(1.0) as i32;

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
        Anchor::Island => (
            screen.centered_x(pw),
            screen.top + (ISLAND_MARGIN as f64 * scale).round() as i32,
        ),
    };
    Ok((
        PhysicalPosition::new(x, y),
        PhysicalSize::new(pw as u32, ph as u32),
    ))
}

/// ScaleFactorChanged 时各窗口应采取的动作（OC-20261124-001）。
///
/// 抽成纯函数是为了能单测锁死「dock 绝不能用 startup 尺寸覆盖」这条规则——
/// 它是「dock 右半被截断」那个 bug 的根因，属于必须被测试覆盖的决策层。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ScaleChangedAction {
    /// startmenu：清锚点缓存，由 menu::toggle 按目标屏 scale 重算几何。
    InvalidateMenuAnchor,
    /// dock：只重定位、不改尺寸（宽度真相源在前端 fit_dock）+ 通知前端重测。
    RepositionOnly,
    /// island 等固定尺寸窗口：按启动尺寸重新 place（位置与尺寸都重算）。
    PlaceWithStartupSize,
}

/// 决策：某个窗口在 ScaleFactorChanged 时该怎么做。
pub fn scale_changed_action(label: &str) -> ScaleChangedAction {
    match label {
        "startmenu" => ScaleChangedAction::InvalidateMenuAnchor,
        // dock 必须走 RepositionOnly：用 STARTUP_DOCK(800) 覆盖前端上报的内容宽
        // 会把窗口压回初始宽，右侧图标被裁，且前端因去重不再上报 → 永久卡死。
        "dock" => ScaleChangedAction::RepositionOnly,
        _ => ScaleChangedAction::PlaceWithStartupSize,
    }
}

/// 只重定位、不改尺寸（OC-20261124-001）。
///
/// dock 的宽度由前端 `fit_dock` 上报（药丸实际宽 + padding），是**单一真相源**；
/// Rust 侧若在 DPI 事件里用启动常量（STARTUP_DOCK=800）改尺寸，会把窗口压回
/// 初始宽 → 右侧图标被裁（见 build_window 里的 ScaleFactorChanged 分支注释）。
///
/// 这里复用 `compute_geometry` 的定位算法，但尺寸取**窗口当前实际尺寸**，
/// 因此只移动位置、不改变大小。
fn reposition_only(window: &WebviewWindow, anchor: Anchor) -> Result<(), String> {
    let current = window.outer_size().map_err(|e| e.to_string())?;
    let monitor = window
        .primary_monitor()
        .map_err(|e| e.to_string())?
        .or_else(|| window.current_monitor().ok().flatten())
        .ok_or_else(|| "找不到主显示器".to_string())?;
    let scale = monitor.scale_factor();
    // 当前尺寸是物理像素 → 换算回逻辑像素交给 compute_geometry 定位（它会再乘一次 scale）。
    let w_logical = current.width as f64 / scale;
    let h_logical = current.height as f64 / scale;
    let (position, _size) = compute_geometry(window, anchor, w_logical, h_logical)?;
    window.set_position(position).map_err(|e| e.to_string())
}

/// 通知前端重新测量并上报 dock 尺寸（OC-20261124-001）。
///
/// 场景：窗口尺寸被外部（OS / 其它路径）改动后，前端并不知道，继续沿用旧的
/// `lastFit` 去重基线 → 永远不再上报 → 窗口卡在错误宽度。
/// 这里发一个事件，前端收到后清掉去重基线并强制重新测量。
fn notify_refit(window: &WebviewWindow) {
    let _ = window.emit("dock-refit", ());
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
    // 岛是固定窗 + 热区上报模型：窗口一挪/一缩放，前端必须重报热区（否则穿透区错位）。
    if matches!(anchor, Anchor::Island) {
        let scale = window.scale_factor().unwrap_or(1.0);
        let _ = window.emit(
            "island-placed",
            (position.x as f64 / scale, position.y as f64 / scale, scale),
        );
    }
    Ok(())
}

// 2026-11-16 移除（OC-20261116-002，死代码清理）：
//
// 原 `place_at()` 与 `place_above_dock()` 两个函数已删除。
// 理由（AgnesCode 对 OC-20261116-001 的 merge_condition 2 裁定）：
// ① 全仓零调用点——grep `place_at` / `place_above_dock` 只命中定义本身，
//    是货真价实的死函数；
// ② 二者内部都用 `window.scale_factor()` 做逻辑→物理换算，而窗口的「当前屏」
//    上下文会随窗口被摆到哪块屏而变——这正是 OC-20261116-001 事故的同源风险
//    （dock 摆到副屏 → 继承 144 DPI → 尺寸放大 1.5 倍 → 落进无人区）。
//    留着不补防护 = 埋雷；补防护 = 给没人用的代码写维护成本。
//
// 保留而非补防护的理由：将来若真需要「按绝对坐标摆放辅助窗口」，
// 应重新按 `compute_geometry` 的口径实现（目标屏固定 + 用目标屏自己的 scale），
// 而不是复活这两个用窗口上下文的旧函数。

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

/// 记录内容尺寸、缩放与热区（island 专用：dock 窗口由 glassdesk 移植逻辑 wholly 接管，
/// 不再走 fit_* 上报模型）。
fn remember(window: &WebviewWindow, state: &SharedState, w: f64, h: f64) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let hz = Hotzone {
        x: 0.0,
        y: 0.0,
        w,
        h,
    };
    *state.island_hotzone.lock().unwrap() = hz;
    *state.island_scale.lock().unwrap() = scale;
    *state.island_collapsed_size.lock().unwrap() = (w, h);
}

/// 前端上报岛内容尺寸（逻辑像素）。
///
/// 岛现在是**固定窗**（420×210）：内容在窗内自由布局，宽度不再驱动窗口尺寸，
/// 热区由前端 `set_hotzone` / `set_ball_hotzone` 上报。这里保留成命令只为兼容旧调用，
/// 实际只做一次固定尺寸对账（幂等），并把折叠尺寸记录给收起逻辑。
#[tauri::command]
pub fn fit_island(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<SharedState>>,
    w: f64,
    h: f64,
) -> Result<(), String> {
    winplat::log::log(&format!("fit_island ignored w={w} h={h} (fixed window)"));
    remember(&window, state.inner().as_ref(), w, h);
    if state.island_expanded.load(Ordering::Relaxed) {
        return Ok(());
    }
    place(&window, Anchor::Island, ISLAND_W, ISLAND_H)
}

// ---------------------------------------------------------------------------
// OC-20261124-001：ScaleFactorChanged 的按窗口决策
//
// 背景（sir 报「dock 右半被截断、偶尔发生」）：
//   dock 宽度由前端 fit_dock 上报（药丸实际宽 + padding），是单一真相源；
//   但旧实现在 DPI 事件里对 dock 调 `place(startup)`，而 startup = STARTUP_DOCK
//   = (800, 76) → 把窗口写回初始宽 → 右侧内容（实测 1064px）被裁掉约 264px。
//   前端 lastFit 去重是「与上次**上报值**比较」，它仍记得报过 1076 → 永远不再上报
//   → 窗口永久卡在 800 → 「偶尔发生」（只在换屏/跨屏触发 DPI 事件时）。
//
// 这里的单测把「dock 绝不能用启动尺寸覆盖」这条规则锁死，防止将来重构改回去。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod scale_changed_tests {
    use super::*;

    #[test]
    fn dock_must_not_use_startup_size() {
        // dock 走 RepositionOnly：只挪位置，不改尺寸。
        assert_eq!(
            scale_changed_action("dock"),
            ScaleChangedAction::RepositionOnly
        );
        // 回归防线：dock 绝不能落到 PlaceWithStartupSize 分支。
        assert_ne!(
            scale_changed_action("dock"),
            ScaleChangedAction::PlaceWithStartupSize
        );
    }

    #[test]
    fn startmenu_invalidates_anchor_cache() {
        assert_eq!(
            scale_changed_action("startmenu"),
            ScaleChangedAction::InvalidateMenuAnchor
        );
    }

    #[test]
    fn island_and_unknown_use_startup_size() {
        // island 是固定尺寸窗口，重算位置与尺寸都正确。
        assert_eq!(
            scale_changed_action("island"),
            ScaleChangedAction::PlaceWithStartupSize
        );
        // 未知窗口保持旧行为（按启动尺寸 place），避免行为漂移。
        assert_eq!(
            scale_changed_action("some-future-window"),
            ScaleChangedAction::PlaceWithStartupSize
        );
    }
}

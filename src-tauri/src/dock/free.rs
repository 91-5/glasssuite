//! 游离图标窗口（v0.7 Phase 1 · OC-20261102-002）。
//!
//! 架构（卡面 §1.2，dp 大脑已调研定死，**勿自行推翻**）：**混合模式**——dock 行保持
//! 单 WebView2 不变；每个游离图标是**一个独立顶层 Tauri 透明窗口**（各带一个
//! WebView2）。
//!
//! 候选路的判定（卡面原文）：全屏 overlay + DOM 绝对定位 ❌（DWM 缩略图 destination
//! 必须是顶层窗口，悬浮预览彻底失效）；纯独立 HWND（连 dock 行也拆）⚠️（回归风险
//! 大于收益）；**混合模式** ✅。
//!
//! 卡面 §1.3 的七个硬技术要点在本文件的落点：
//! 1. 拖动走 Windows 原生：`WM_NCHITTEST` → `HTCAPTION` 由系统接管；**不许用前端
//!    pointer capture 拖窗口**（那样窗口不动、光标会跑出窗口，体验直接断掉）。
//! 2. 边界约束在 `WM_MOVING` 里改 `lParam` 指向的 `RECT*`；**不许**在 `WM_MOVE`
//!    收到后再 `SetWindowPos`（那样会抖一帧）。
//! 3. 坐标全程**物理像素虚拟桌面坐标**——副屏在左/上时坐标是负的。
//! 4. 拖出判定阈值在 `dock::drag_out_decision`（前端与探针共用同一套常量）。
//! 5. dock 行补位动画在前端（`App.tsx` + `dock.css`）。
//! 6. 游离窗口内**只渲染极简 DOM**（`public/free.html`：一个 `<img>` + 运行指示条
//!    + 悬停名称），**不加载完整 React bundle**——每个游离图标 = 一个 WebView2，
//!
//!    实例膨胀是真实成本，软上限 30 个。
//! 7. **不能真正贴桌面层**：`SetParent` 到 `WorkerW` 会跑到桌面图标下面，用户点不到。
//!    游离图标只能走顶层窗口路线。
//!
//! 层级语义（卡面 §1.4，与 ADR-0001 D7 一致）：游离窗口**不加 `WS_EX_TOPMOST`**，
//! 保持「可被前台应用盖住」。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::dock::{self, DockConfig, FreeRect, IconMode, Shortcut};
use serde::Serialize;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, EnumWindows, GetSystemMetrics, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextW, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_WNDPROC,
    HTCAPTION, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWNA, WINDOW_LONG_PTR_INDEX, WM_EXITSIZEMOVE, WM_MOVING,
    WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDOWN, WNDPROC,
};

/// 游离图标窗口的逻辑尺寸（CSS px）。上 48 是图标区，下 24 是悬停名称条。
pub const FREE_WIN_W: f64 = 48.0;
pub const FREE_WIN_H: f64 = 72.0;

/// 游离图标数量软上限（卡面 §1.3-6 / 验收 9）。超出时拒绝拖出并提示，不静默失败。
pub const MAX_FREE_ICONS: usize = 30;

/// 当前生效的上限。默认 30；`GLASSDESK_FREE_MAX` 可临时调低用于**测试上限拒绝路径**
/// （避免为了测「第 31 个被拒」而真的拉起 30 个 WebView2——实例膨胀是真实成本）。
pub fn max_free_icons() -> usize {
    std::env::var("GLASSDESK_FREE_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(MAX_FREE_ICONS)
}

/// 一个游离图标的运行时状态。
#[derive(Clone)]
pub struct FreeWin {
    pub id: String,
    pub label: String,
    pub hwnd: isize,
    pub rect: FreeRect,
    /// 上次写给前端的运行指示状态（变化时才 eval，省 IPC）。
    pub running: bool,
    /// 正在被系统拖动（拖动期间 reconciler 不得回写位置）。
    pub dragging: bool,
}

static FREE_WINS: Mutex<Vec<FreeWin>> = Mutex::new(Vec::new());

/// 正在 build 中的游离窗口 label 集合——防同 label 并发 build。
///
/// 为什么需要：Tauri v2 对「同 label 二次 build」是 **panic**（内部 assert，非
/// Err 返回），而 `get_webview_window` 只查**已注册**窗口——build 过程中
/// （WebviewWindowBuilder::build 未返回前）注册表里还没有它，重入会绕过
/// L364 的存在性检查 → 并发同 label build → panic → 进程崩 → dock 成僵尸。
///
/// 实测事故（2026-11-02）：sir 快速拖出「此电脑」，`free_drag_out` 在
/// pointermove 高频触发下重入，产出两个同 label 窗口 + 进程崩溃重启。
static BUILDING: LazyLock<Mutex<std::collections::HashSet<String>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// 子类化登记：hwnd → (原窗口过程, shortcut id)。
/// 原过程必须在 WM_NCDESTROY 之前取到（tao 的 wndproc 在 NCDESTROY 里清理 userdata）。
static SUBCLASS: Mutex<Vec<(isize, WNDPROC, String)>> = Mutex::new(Vec::new());

/// 游离窗口的窗口标题前缀——探针用标题枚举顶层窗口（验收 2：类名/标题可识别）。
pub const FREE_TITLE_PREFIX: &str = "GlassSuite Free";

pub fn label_for(id: &str) -> String {
    format!("free-{id}")
}

/// 虚拟桌面矩形（物理像素）。副屏在左/上时 left/top 为负。
pub fn virtual_desktop_rect() -> RECT {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }
}

/// 窗口矩形（物理像素，虚拟桌面坐标）。
pub fn window_rect(hwnd: HWND) -> Option<FreeRect> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
    Some(FreeRect {
        x: r.left,
        y: r.top,
        w: (r.right - r.left).max(0) as u32,
        h: (r.bottom - r.top).max(0) as u32,
    })
}

/// 游离窗口数量（上限判定用）。
pub fn live_count() -> usize {
    FREE_WINS.lock().map(|v| v.len()).unwrap_or(0)
}

/// 供探针/前端查询的游离窗口清单。
#[derive(Serialize, Clone)]
pub struct FreeStatus {
    pub id: String,
    pub label: String,
    pub hwnd: isize,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub dragging: bool,
}

pub fn status() -> Vec<FreeStatus> {
    FREE_WINS
        .lock()
        .map(|v| {
            v.iter()
                .map(|w| FreeStatus {
                    id: w.id.clone(),
                    label: w.label.clone(),
                    hwnd: w.hwnd,
                    x: w.rect.x,
                    y: w.rect.y,
                    w: w.rect.w,
                    h: w.rect.h,
                    dragging: w.dragging,
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 子类化窗口过程：WM_NCHITTEST / WM_MOVING / WM_EXITSIZEMOVE
// ---------------------------------------------------------------------------

fn lookup_subclass(hwnd: isize) -> Option<(WNDPROC, String)> {
    SUBCLASS.lock().ok().and_then(|v| {
        v.iter()
            .find(|(k, _, _)| *k == hwnd)
            .map(|(_, p, id)| (*p, id.clone()))
    })
}

/// 窗口过程地址（fn 指针 ↔ isize 的 win32 回调地址转换）。
fn proc_addr(p: WNDPROC) -> isize {
    unsafe { std::mem::transmute(p) }
}

/// 装上子类窗口过程（保存原过程以便转发其余消息）。
fn install_subclass(hwnd: HWND, id: &str) {
    unsafe {
        let idx = WINDOW_LONG_PTR_INDEX(GWLP_WNDPROC.0);
        let raw = GetWindowLongPtrW(hwnd, idx);
        if raw == 0 {
            return;
        }
        let old_proc: WNDPROC = std::mem::transmute(raw);
        SetWindowLongPtrW(hwnd, idx, proc_addr(Some(free_wnd_proc)));
        if let Ok(mut v) = SUBCLASS.lock() {
            v.retain(|(k, _, _)| *k != hwnd.0 as isize);
            v.push((hwnd.0 as isize, old_proc, id.to_string()));
        }
    }
}

/// 诊断计数（拖动链路取证）：WM_NCLBUTTONDOWN / WM_MOVING / WM_EXITSIZEMOVE 命中次数。
static DIAG_NCLBUTTONDOWN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIAG_WM_MOVING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIAG_EXITSIZEMOVE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

unsafe extern "system" fn free_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // 卡面 §1.3-1：命中测试一律按标题栏处理 → 系统接管拖动（原生移动循环）。
    if msg == WM_NCHITTEST {
        return LRESULT(HTCAPTION as isize);
    }
    let old = lookup_subclass(hwnd.0 as isize).map(|(p, _)| p);
    match msg {
        // 卡面 §1.3-2：边界约束在 WM_MOVING 里改 RECT*（不在 WM_MOVE 后 SetWindowPos）。
        WM_MOVING => {
            DIAG_WM_MOVING.fetch_add(1, Ordering::Relaxed);
            mark_dragging(hwnd.0 as isize, true);
            clamp_moving_rect(lparam);
        }
        WM_EXITSIZEMOVE => {
            DIAG_EXITSIZEMOVE.fetch_add(1, Ordering::Relaxed);
            mark_dragging(hwnd.0 as isize, false);
            on_drag_end(hwnd);
        }
        WM_NCLBUTTONDOWN => {
            DIAG_NCLBUTTONDOWN.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
    let result = match old {
        Some(p) => CallWindowProcW(p, hwnd, msg, wparam, lparam),
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    if msg == WM_NCDESTROY {
        if let Ok(mut v) = SUBCLASS.lock() {
            v.retain(|(k, _, _)| *k != hwnd.0 as isize);
        }
        if let Ok(mut v) = FREE_WINS.lock() {
            v.retain(|w| w.hwnd != hwnd.0 as isize);
        }
    }
    result
}

/// WM_MOVING 的软边界：只有**整个窗口都跑到虚拟桌面外**时才拉回来（保留 24px 可见）。
/// 不做网格/边缘吸附——那是 Phase 2（卡面 §1.1 明确不在本卡范围）。
unsafe fn clamp_moving_rect(lparam: LPARAM) {
    let ptr = lparam.0 as *mut RECT;
    if ptr.is_null() {
        return;
    }
    let rect = &mut *ptr;
    let vd = virtual_desktop_rect();
    const KEEP: i32 = 24;
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    if rect.right <= vd.left + KEEP {
        rect.left = vd.left + KEEP - w;
        rect.right = rect.left + w;
    } else if rect.left >= vd.right - KEEP {
        rect.left = vd.right - KEEP;
        rect.right = rect.left + w;
    }
    if rect.bottom <= vd.top + KEEP {
        rect.top = vd.top + KEEP - h;
        rect.bottom = rect.top + h;
    } else if rect.top >= vd.bottom - KEEP {
        rect.top = vd.bottom - KEEP;
        rect.bottom = rect.top + h;
    }
}

fn mark_dragging(hwnd: isize, dragging: bool) {
    if let Ok(mut v) = FREE_WINS.lock() {
        for w in v.iter_mut() {
            if w.hwnd == hwnd {
                w.dragging = dragging;
            }
        }
    }
}

/// 原生拖动结束：保存新位置；若落在 dock 区则问前端要落点槽位（拖回 dock）。
fn on_drag_end(hwnd: HWND) {
    let Some((_, id)) = lookup_subclass(hwnd.0 as isize) else {
        return;
    };
    eprintln!(
        "[free] drag end id={id} nclbuttondown={} wm_moving={} exitsizemove={}",
        DIAG_NCLBUTTONDOWN.load(Ordering::Relaxed),
        DIAG_WM_MOVING.load(Ordering::Relaxed),
        DIAG_EXITSIZEMOVE.load(Ordering::Relaxed),
    );
    let Some(rect) = window_rect(hwnd) else {
        return;
    };
    let app = match app_handle() {
        Some(a) => a,
        None => return,
    };
    let id2 = id.clone();
    // 配置写盘 + 落点询问都在工作线程做（不卡窗口过程）。
    std::thread::spawn(move || {
        finish_drag(&app, &id2, rect);
    });
}

/// 全局 AppHandle（游离窗口创建时登记）。
static APP_HANDLE: Mutex<Option<AppHandle>> = Mutex::new(None);

pub fn set_app_handle(app: &AppHandle) {
    if let Ok(mut v) = APP_HANDLE.lock() {
        *v = Some(app.clone());
    }
}

fn app_handle() -> Option<AppHandle> {
    APP_HANDLE.lock().ok().and_then(|v| v.clone())
}

/// 拖动收尾：
/// 1. 先把新位置写进 dock.json（mode 仍是 free）——位置永不丢；
/// 2. 若图标中心落在 dock 窗口内 → 问 dock 前端要落点槽位 → 前端回调
///    `free_drop_result` 完成「收回 dock + 按落点插入」。
fn finish_drag(app: &AppHandle, id: &str, rect: FreeRect) {
    let Some(path) = dock::config_path(app) else {
        return;
    };
    let mut cfg = dock::load_config(&path);
    // 只有仍处于 free 模式才写位置（防止覆盖前端回调里的收回动作）。
    if dock::find_shortcut_by_id(&cfg, id).map(|s| s.mode) == Some(IconMode::Free) {
        dock::set_free_mode(&mut cfg, id, rect);
        let _ = dock::save_config(&path, &cfg);
    }
    // 落点是否在 dock 区？
    let Some(dock) = app.get_webview_window("dock") else {
        return;
    };
    let Ok(dock_hwnd) = dock.hwnd() else {
        return;
    };
    let mut dr = RECT::default();
    unsafe {
        if GetWindowRect(HWND(dock_hwnd.0), &mut dr).is_err() {
            return;
        }
    }
    let cx = rect.center_x();
    let cy = rect.center_y();
    if cx < dr.left || cx > dr.right || cy < dr.top || cy > dr.bottom {
        return; // 不在 dock 区 → 保持游离
    }
    // 物理坐标 → dock 客户区 CSS 坐标（前端拿它按 DOM 几何挑槽位）。
    let scale = dock.scale_factor().unwrap_or(1.0);
    let inner = dock.inner_position().ok();
    let css_x = match inner {
        Some(p) => (cx as f64 - p.x as f64) / scale,
        None => cx as f64,
    };
    let id_json = serde_json::to_string(id).unwrap_or_else(|_| "\"\"".into());
    let script = format!("window.__gdPickDrop && window.__gdPickDrop({id_json}, {css_x:.1})");
    let _ = dock.eval(&script);
}

// ---------------------------------------------------------------------------
// 窗口创建 / 销毁 / 位置同步
// ---------------------------------------------------------------------------

/// 创建游离窗口（必须**在主线程**调用——tao 的窗口创建要求事件循环线程）。
pub fn create(app: &AppHandle, sc: &Shortcut, rect: &FreeRect) -> Result<(), String> {
    let id = sc.id.clone();
    let label = label_for(&id);
    if live_count() >= max_free_icons() {
        return Err(format!(
            "已达游离图标上限（{} 个），无法继续拖出",
            max_free_icons()
        ));
    }
    if app.get_webview_window(&label).is_some() {
        return Ok(());
    }
    // 并发重入防护（见 BUILDING 文档）：build 中的 label 拒绝第二次进入。
    // 否则 Tauri v2 同 label 二次 build 直接 panic 崩进程。
    if !BUILDING
        .lock()
        .map(|mut b| b.insert(label.clone()))
        .unwrap_or(false)
    {
        return Ok(()); // 正在同 label build：视为已受理，静默丢弃重入调用
    }
    // build 结束（无论成败）必须清标记——用 guard 结构保证 panic 路径也不泄漏。
    struct BuildingGuard(String);
    impl Drop for BuildingGuard {
        fn drop(&mut self) {
            if let Ok(mut b) = BUILDING.lock() {
                b.remove(&self.0);
            }
        }
    }
    let _guard = BuildingGuard(label.clone());
    // 页面数据（id / 名称 / 图标 RGBA）用初始化脚本注入——游离窗口是静态页，
    // 不加载 React，也就没有打包产物可用。
    let payload = serde_json::json!({
        "id": sc.id,
        "name": sc.name,
        "w": sc.icon.w,
        "h": sc.icon.h,
        "rgba": sc.icon.rgba,
    });
    let script = format!("window.__GD_FREE__ = {payload};");

    let win = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("dock/free.html".into()))
        .title(format!("{FREE_TITLE_PREFIX} {}", sc.name))
        .inner_size(FREE_WIN_W, FREE_WIN_H)
        .decorations(false)
        .transparent(true)
        .always_on_top(false) // D7 语义：不加 WS_EX_TOPMOST，可被前台应用盖住
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .visible(false)
        .initialization_script(script)
        .build()
        .map_err(|e| format!("创建游离窗口失败: {e}"))?;

    let Ok(hwnd) = win.hwnd() else {
        return Err("游离窗口取不到 HWND".into());
    };
    let hwnd = HWND(hwnd.0);
    // 精确落位（物理像素虚拟桌面坐标）：负坐标副屏同样成立。
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            rect.x,
            rect.y,
            rect.w as i32,
            rect.h as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .map_err(|e| format!("游离窗口定位失败: {e}"))?;
    }
    install_subclass(hwnd, &id);
    if let Ok(mut v) = FREE_WINS.lock() {
        v.retain(|w| w.id != id);
        v.push(FreeWin {
            id: id.clone(),
            label: label.clone(),
            hwnd: hwnd.0 as isize,
            rect: *rect,
            running: false,
            dragging: false,
        });
    }
    // 首帧就绪由前端 `free_ready` 上报后显示；兜底 1200ms 后无论如何显示。
    let app2 = app.clone();
    let label2 = label.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1200));
        show_free(&app2, &label2);
    });
    Ok(())
}

/// 显示游离窗口（SW_SHOWNA：不抢焦点，与 dock 一致）。
pub fn show_free(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        if let Ok(h) = w.hwnd() {
            unsafe {
                let _ = ShowWindow(HWND(h.0), SW_SHOWNA);
            }
        }
    }
}

/// 销毁某个 id 的游离窗口（主线程调用）。
pub fn destroy_by_id(app: &AppHandle, id: &str) {
    let label = label_for(id);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.destroy();
    }
    if let Ok(mut v) = FREE_WINS.lock() {
        v.retain(|w| w.id != id);
    }
}

/// 把某个 id 的窗口挪到指定物理矩形（reconciler 用）。
fn move_to(id: &str, rect: &FreeRect) {
    let target = FREE_WINS
        .lock()
        .ok()
        .and_then(|v| v.iter().find(|w| w.id == id).map(|w| (w.hwnd, w.rect)));
    let Some((hwnd, cur)) = target else {
        return;
    };
    if (cur.x - rect.x).abs() <= 1
        && (cur.y - rect.y).abs() <= 1
        && cur.w == rect.w
        && cur.h == rect.h
    {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            HWND(hwnd as *mut std::ffi::c_void),
            None,
            rect.x,
            rect.y,
            rect.w as i32,
            rect.h as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
    if let Ok(mut v) = FREE_WINS.lock() {
        for w in v.iter_mut() {
            if w.id == id {
                w.rect = *rect;
            }
        }
    }
}

/// 按 dock.json 对齐游离窗口集合（创建缺的、挪错的、删多的）。
pub fn reconcile(app: &AppHandle, cfg: &DockConfig) {
    let desired: Vec<(String, Shortcut, FreeRect)> = cfg
        .shortcuts
        .iter()
        .filter(|s| s.mode == IconMode::Free)
        .filter_map(|s| s.free_rect.map(|r| (s.id.clone(), s.clone(), r)))
        .collect();
    // 多余的（配置已改回 dock / 条目被删）→ 销毁
    let live: Vec<String> = FREE_WINS
        .lock()
        .map(|v| v.iter().map(|w| w.id.clone()).collect())
        .unwrap_or_default();
    for id in live {
        if !desired.iter().any(|(d, _, _)| *d == id) {
            destroy_by_id(app, &id);
        }
    }
    // 缺的 → 创建；对的 → 挪位置（拖动中不动）
    for (id, sc, rect) in desired {
        let exists = FREE_WINS
            .lock()
            .map(|v| v.iter().any(|w| w.id == id))
            .unwrap_or(false);
        if !exists {
            if let Err(e) = create(app, &sc, &rect) {
                eprintln!("[free] 创建 {id} 失败: {e}");
            }
            continue;
        }
        let dragging = FREE_WINS
            .lock()
            .map(|v| {
                v.iter()
                    .find(|w| w.id == id)
                    .map(|w| w.dragging)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !dragging {
            move_to(&id, &rect);
        }
    }
}

/// 运行指示条刷新（复用 running::is_running，**只调用不改**）。
fn refresh_running(app: &AppHandle, cfg: &DockConfig) {
    let snap = crate::dock::running::snapshot();
    let live: Vec<(String, String, bool)> = FREE_WINS
        .lock()
        .map(|v| {
            v.iter()
                .map(|w| (w.id.clone(), w.label.clone(), w.running))
                .collect()
        })
        .unwrap_or_default();
    for (id, label, last) in live {
        let Some(sc) = dock::find_shortcut_by_id(cfg, &id) else {
            continue;
        };
        let now = crate::dock::running::is_running(&snap, &sc.aumid, &sc.path);
        if now == last {
            continue;
        }
        if let Ok(mut v) = FREE_WINS.lock() {
            for w in v.iter_mut() {
                if w.id == id {
                    w.running = now;
                }
            }
        }
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.eval(format!("window.gdSetRun && window.gdSetRun({now})"));
        }
    }
}

/// 配置对账线程：外部改 dock.json（含探针）也能让游离窗口跟随。
pub fn spawn_reconciler(app: AppHandle, shutdown: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut last_sig: Option<(u64, u64)> = None;
        let mut last_run_poll = Instant::now();
        while !shutdown.load(Ordering::Relaxed) {
            let path = dock::config_path(&app);
            if let Some(p) = path {
                let sig = file_sig(&p);
                if sig != last_sig {
                    last_sig = sig;
                    // 外部改了 dock.json（探针/手改）→ bump 版本号，
                    // 前端据此重拉列表（dock 行里的图标进出）。
                    dock::bump_config_version();
                    let cfg = dock::load_config(&p);
                    let app2 = app.clone();
                    let app3 = app2.clone();
                    let _ = app2.run_on_main_thread(move || {
                        reconcile(&app3, &cfg);
                    });
                }
                // 运行指示 800ms 一刷
                if last_run_poll.elapsed() >= Duration::from_millis(800) {
                    last_run_poll = Instant::now();
                    let cfg = dock::load_config(&p);
                    let app2 = app.clone();
                    let app3 = app2.clone();
                    let _ = app2.run_on_main_thread(move || {
                        refresh_running(&app3, &cfg);
                    });
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}

/// 文件指纹（mtime + 长度）——配置被改过就重读。
fn file_sig(path: &std::path::Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

/// 顶层窗口枚举辅助（探针/自检用）：按标题前缀找游离窗口。
///
/// EnumWindows 的回调必须是无捕获闭包（fn 指针），所以结果与前缀都放静态里。
static FREE_ENUM_OUT: Mutex<Vec<(isize, String, RECT)>> = Mutex::new(Vec::new());
static FREE_ENUM_PREFIX: Mutex<String> = Mutex::new(String::new());

pub fn find_free_windows_by_title(prefix: &str) -> Vec<(isize, String, RECT)> {
    if let Ok(mut p) = FREE_ENUM_PREFIX.lock() {
        *p = prefix.to_string();
    }
    if let Ok(mut o) = FREE_ENUM_OUT.lock() {
        o.clear();
    }
    unsafe {
        let _ = EnumWindows(Some(enum_collect_free), LPARAM(0));
    }
    FREE_ENUM_OUT.lock().map(|v| v.clone()).unwrap_or_default()
}

unsafe extern "system" fn enum_collect_free(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    let prefix = FREE_ENUM_PREFIX
        .lock()
        .map(|p| p.clone())
        .unwrap_or_default();
    let mut buf = [0u16; 256];
    let n = GetWindowTextW(hwnd, &mut buf);
    if n > 0 {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
        let title = String::from_utf16_lossy(&buf[..end]);
        if title.starts_with(&prefix) {
            let mut r = RECT::default();
            if GetWindowRect(hwnd, &mut r).is_ok() {
                if let Ok(mut o) = FREE_ENUM_OUT.lock() {
                    o.push((hwnd.0 as isize, title, r));
                }
            }
        }
    }
    BOOL(1)
}

/// 窗口是否可见（探针用）。
pub fn is_visible(hwnd: isize) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    unsafe { IsWindowVisible(HWND(hwnd as *mut std::ffi::c_void)).as_bool() }
}

/// 窗口扩展样式里的 WS_EX_TOPMOST 位（验收 7：必须为 0 —— 与 D7 一致）。
pub fn is_topmost(hwnd: isize) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWL_EXSTYLE};
    unsafe {
        let style = GetWindowLongPtrW(
            HWND(hwnd as *mut std::ffi::c_void),
            WINDOW_LONG_PTR_INDEX(GWL_EXSTYLE.0),
        );
        (style & 0x0000_0008) != 0 // WS_EX_TOPMOST
    }
}

/// 标题里的应用名（探针展示用）。
pub fn title_of(hwnd: isize) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    unsafe {
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(HWND(hwnd as *mut std::ffi::c_void), &mut buf);
        if n == 0 {
            return String::new();
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
        String::from_utf16_lossy(&buf[..end])
    }
}

/// 供命令层用：把某个 dock 条目切成游离并落地窗口。
pub fn drag_out(app: &AppHandle, id: &str, css_x: f64, css_y: f64) -> Result<(), String> {
    let Some(path) = dock::config_path(app) else {
        return Err("无法定位配置目录".into());
    };
    let mut cfg = dock::load_config(&path);
    let Some(sc) = dock::find_shortcut_by_id(&cfg, id).cloned() else {
        return Err("条目不存在".into());
    };
    if live_count() >= max_free_icons() {
        return Err(format!(
            "已达游离图标上限（{} 个），无法继续拖出",
            max_free_icons()
        ));
    }
    // CSS 坐标 → 物理像素虚拟桌面坐标（负坐标副屏同样成立）
    let Some((px, py)) = dock::css_to_physical(app, css_x, css_y) else {
        return Err("取不到 dock 窗口位置".into());
    };
    // 窗口物理尺寸：按当前显示比例算（跨屏 DPI 属 Phase 2）
    let scale = app
        .get_webview_window("dock")
        .and_then(|d| d.scale_factor().ok())
        .unwrap_or(1.0);
    let w = (FREE_WIN_W * scale).round() as u32;
    let h = (FREE_WIN_H * scale).round() as u32;
    let rect = FreeRect {
        x: px - (w as i32) / 2,
        y: py - (h as i32) / 2,
        w,
        h,
    };
    dock::set_free_mode(&mut cfg, id, rect);
    dock::save_config(&path, &cfg)?;
    // 建窗（主线程）
    let app2 = app.clone();
    let sc2 = sc.clone();
    let rect2 = rect;
    let _ = app.run_on_main_thread(move || {
        if let Err(e) = create(&app2, &sc2, &rect2) {
            eprintln!("[free] drag_out 创建失败: {e}");
        }
    });
    Ok(())
}

/// 供命令层用：把游离图标收回 dock（`slot` = 行内插入序号，None = 末尾）。
pub fn return_to_dock(app: &AppHandle, id: &str, slot: Option<usize>) -> Result<(), String> {
    let Some(path) = dock::config_path(app) else {
        return Err("无法定位配置目录".into());
    };
    let mut cfg = dock::load_config(&path);
    if !dock::set_dock_mode(&mut cfg, id, slot) {
        return Err("条目不存在".into());
    }
    dock::save_config(&path, &cfg)?;
    destroy_by_id(app, id);
    Ok(())
}

/// 供命令层用：启动/激活游离图标对应的应用（复用 dock::activate_or_launch）。
pub fn launch(app: &AppHandle, id: &str) -> Result<(), String> {
    let Some(path) = dock::config_path(app) else {
        return Err("无法定位配置目录".into());
    };
    let cfg = dock::load_config(&path);
    let Some(sc) = dock::find_shortcut_by_id(&cfg, id) else {
        return Err("条目不存在".into());
    };
    dock::activate_or_launch(&sc.aumid, &sc.path)
}

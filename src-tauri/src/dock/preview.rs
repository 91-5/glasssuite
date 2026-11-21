//! 悬停预览（OC-20261030-003 任务 B）。
//!
//! 悬停 dock 图标 ≥300ms → 弹出该应用窗口的 DWM 缩略图小窗；移开即销毁。
//!
//! 承载窗选型（bin/preview_poc.rs 2026-10-31 实测，三变体缩略图均注册成功且可见）：
//! **品红 colorkey 全透明 + DwmEnableBlurBehindWindow**（v0.1 玻璃层同款手法，
//! 照抄已删 overlay.rs 的 create_glass_window）——表面全透明只露模糊，
//! 头部图标/文字用不透明 GDI 直绘（PoC 变体 C 证实 GDI 文字在模糊上可读）。
//! 不用 Tauri/WebView2 窗口承载：DWM 缩略图直接合成到窗口表面，与 webview 抢绘制
//! （卡面钉死）。不引入新合成路径（DirectComposition / Accent 禁用，卡面硬约束）。
//!
//! 源窗口定位：复用 running.rs 的枚举/匹配基础设施（key_for_shortcut 同源），
//! EnumWindows **z 序最前**的可见匹配 = 最近激活（EnumWindows 按 z 序枚举）；
//! DWMWA_CLOAKED（虚拟桌面隐藏）跳过不预览。
//!
//! 降级策略（卡面 2.4）：
//! - 最小化窗口：缩略图是陈旧帧/空白 → 降级「图标 + 窗口标题」
//! - DRM 保护内容：缩略图黑块 → 前台窗口中心区近纯黑（mean<3 且 stdev<1.5）
//!   判为疑似保护内容，降级「图标 + 标题」（启发式，局限见交付单）
//! - 应用未运行：无源窗口 → 不弹预览（与任务栏「固定未启动不弹预览」同语义）
//!
//! 生命周期：独立线程 + 消息泵；Show/Hide 走通道；预览窗**销毁制**——
//! 每次 Hide 都 DestroyWindow（卡面「不得残留窗口句柄」），下次 Show 重建。
//! 进程退出线程即亡，OS 回收全部窗口（硬约束 5「退出即还原」）。

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE,
    DWM_TNP_VISIBLE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetDIBits,
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, BI_RGB, DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_SINGLELINE, DT_VCENTER, HGDIOBJ,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow,
    DispatchMessageW, DrawIconEx, GetForegroundWindow, GetWindowRect, GetWindowTextW, IsIconic,
    LoadCursorW, PeekMessageW, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, TranslateMessage, DI_NORMAL, ICONINFO, LWA_COLORKEY, MSG, PM_REMOVE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use tauri::Manager;

use crate::dock::IconData;

/// 品红 colorkey：客户区涂品红 = 表面全透明（v0.1 玻璃层同款）。
const COLORKEY: COLORREF = COLORREF(0x00FF_00FF);
const CLASS_NAME: PCWSTR = w!("GlassDesk_Preview");
const WINDOW_TITLE: PCWSTR = w!("GlassDesk Preview");

// 尺寸基准（96 DPI 物理像素，show 时乘 scale）。220×170 贴近卡面「约 200×150」。
const HEADER_H: f32 = 26.0; // 头部带（应用图标 + 应用名）高
const PAD: f32 = 10.0; // 缩略图区与窗缘间距
const GAP_ABOVE: i32 = 6; // 预览窗与图标上缘间距
const ROUND_R: f32 = 14.0; // 圆角半径（与 dock 药丸 18 略小，同材质语言）
const WATCHDOG_MS: u64 = 10_000; // 兜底：前端失联时预览最长存活

// 007 多窗口预览（返工：sir 裁定**横排**，Win11 任务栏样式）：
// 每格 = 缩略图（或降级条）+ 格内标题条（不透明可点击）；格间 ≥4px 间隙
// 不响应点击；宽度不足换行、总宽 clamp 到显示器工作区（先缩格宽再换行）。
// 格底衬刻意不透明：品红 colorkey 面上的纯色像素会被系统判「点击穿透」，
// 缩略图区的鼠标事件会漏到下层窗口——底衬不透明才接得住 WM_LBUTTONDOWN。
const ROW_H: f32 = 150.0; // 单格高（缩略内容 116 + 标题条 22 + 上下留白 12）
const CELL_W: f32 = 200.0; // 单格宽（96dpi 基准）
const ROW_TITLE: f32 = 22.0; // 格内标题条高（不透明可点击）
const MAX_ROWS: usize = 6; // 格数上限（再多的窗口按 z 序截断，日志注明）
const GUTTER: f32 = 6.0; // 格间距（≥4px 不响应间隙）
const CELL_MIN_W: f32 = 120.0; // 格宽下限（工作区太窄时收缩下限）
const ROW_BG: COLORREF = COLORREF(0x00ED_EDE9); // 格底衬（浅色，与头部带同系）

/// 一行预览 = 一个顶层窗口。降级仅影响显示（`PaintState.degraded`），
/// `src` 保留真实源窗口句柄——降级格点击照样还原/置前。
#[derive(Debug)]
pub struct PreviewWin {
    src: isize,
    title: String,
    degraded: bool,
}

/// Show 请求（show_for / show_for_transient 解析完毕后投递给预览线程）。
pub struct ShowReq {
    /// 应用名（头部带）。
    app_title: String,
    /// 应用图标（RGBA；头部带 + 降级行共用）。
    app_icon: IconData,
    /// 窗口行（z 序，最前在前）。
    windows: Vec<PreviewWin>,
    /// 图标中心 x / 图标上缘 y（物理屏幕坐标，预览窗定位锚点）
    cx: i32,
    top: i32,
    scale: f32,
}

enum Cmd {
    Show(Box<ShowReq>),
    Hide,
}

static SENDER: LazyLock<Mutex<Option<std::sync::mpsc::Sender<Cmd>>>> =
    LazyLock::new(|| Mutex::new(None));
static LOG_PATH: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(|| Mutex::new(None));

/// 启动预览线程。`log_dir` 为应用数据目录（preview.log 落这里）。
pub fn spawn(log_dir: PathBuf) {
    let (tx, rx) = std::sync::mpsc::channel::<Cmd>();
    if let Ok(mut g) = SENDER.lock() {
        *g = Some(tx);
    }
    if let Ok(mut g) = LOG_PATH.lock() {
        *g = Some(log_dir.join("preview.log"));
    }
    let _ = std::thread::Builder::new()
        .name("glassdesk-preview".into())
        .spawn(move || thread_loop(rx));
}

fn send(cmd: Cmd) {
    if let Ok(g) = SENDER.lock() {
        if let Some(s) = g.as_ref() {
            let _ = s.send(cmd);
        }
    }
}

/// 前端请求：立即销毁预览（鼠标移开 / dock 隐藏 / 点击图标）。
pub fn hide() {
    send(Cmd::Hide);
}

/// 单窗口降级判定（卡面 2.4）：最小化 → 图标+标题；DRM 黑块启发式 → 图标+标题。
fn degrade_reason(src: isize, minimized: bool) -> bool {
    minimized || is_protected_black(src)
}

/// 收集该 key 的全部顶层窗口 → 预览行（z 序，最前在前；标题取窗口文本）。
/// 数量超过 `MAX_ROWS` 按 z 序截断（日志注明）。
fn collect_rows(key: &str) -> Vec<PreviewWin> {
    let wins = crate::dock::running::windows_for_key(key);
    let mut rows: Vec<PreviewWin> = Vec::with_capacity(wins.len());
    for (src, minimized) in wins {
        let title = unsafe {
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(HWND(src as *mut c_void), &mut buf);
            String::from_utf16_lossy(&buf[..n as usize])
        };
        let title = if title.trim().is_empty() {
            "(无标题窗口)".to_string()
        } else {
            title
        };
        let degraded = degrade_reason(src, minimized);
        rows.push(PreviewWin { src, title, degraded });
    }
    truncate_rows(rows)
}

/// OC-20261121-005：自身 fixed 图标（GlassSuite）的预览行。
/// 自身窗口被 running.rs:516 排除（不进运行快照），故不能走 collect_rows/windows_for_key。
/// 改从 AppHandle 管理的窗口 label 直接取句柄：只列岛 + 控制中心（sir 拍板范围），
/// dock/startmenu 是悬浮操作的载体，列入会自我套娃。完全不碰 running.rs。
fn collect_self_rows(app: &tauri::AppHandle) -> Vec<PreviewWin> {
    let mut rows: Vec<PreviewWin> = Vec::new();
    for (label, title) in [("island", "灵动岛"), ("control", "控制中心")] {
        let Some(win) = app.get_webview_window(label) else {
            continue;
        };
        if !win.is_visible().unwrap_or(false) {
            continue;
        }
        let Ok(h) = win.hwnd() else {
            continue;
        };
        let src = h.0 as isize;
        let minimized = unsafe { IsIconic(HWND(src as *mut c_void)) }.as_bool();
        rows.push(PreviewWin {
            src,
            title: title.to_string(),
            degraded: degrade_reason(src, minimized),
        });
    }
    rows
}

/// 解析悬停的快捷方式 → **全部**匹配窗口 → 投递 Show（007 多窗口预览）。
/// 由 Tauri 命令调用（lib.rs `preview_show`）。
/// `x/y/w/h` 为图标按钮矩形（CSS px，前端 getBoundingClientRect）。
pub fn show_for(
    app: &tauri::AppHandle,
    id: &str,
    x: f64,
    y: f64,
    w: f64,
    _h: f64,
) -> Result<(), String> {
    let cfg_path = crate::dock::config_path(app).ok_or("无法定位配置目录")?;
    let cfg = crate::dock::load_config(&cfg_path);
    let sc = cfg
        .shortcuts
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("未知快捷方式 id: {id}"))?;

    let dock = app.get_webview_window("dock").ok_or("dock 窗口缺失")?;
    // dock 隐藏中（自动隐藏/全屏让位）不弹预览；开始菜单面板开着也不弹
    // （预览窗 TOPMOST 会盖在面板上，悬停 ⊞ 周边图标时体验混乱）
    if !dock.is_visible().unwrap_or(false) || crate::dock::menu::is_panel_visible(app) {
        return Ok(());
    }
    let scale = dock.scale_factor().unwrap_or(1.0) as f32;
    let pos = dock
        .outer_position()
        .map_err(|e| format!("dock 位置不可得: {e}"))?;
    let cx = pos.x + ((x + w / 2.0) * scale as f64) as i32;
    let top = pos.y + (y * scale as f64) as i32;

    // OC-20261121-005：自身 fixed 图标（GlassSuite）→ 从 AppHandle 管理的窗口取预览行。
    // 自身窗口被 running.rs 排除，走 key/collect_rows 必然空；用 name 判定（大小写不敏感），
    // 用户重命名该条目则自然退化为原行为（同 api.rs:147 的自身判定口径）。
    let is_self = sc.name.eq_ignore_ascii_case("GlassSuite");
    let rows = if is_self {
        collect_self_rows(app)
    } else {
        // 源窗口：全部可见匹配（复用任务 A 的 key 体系），z 序最前在前
        let key = crate::dock::running::key_for_shortcut(&sc.aumid, &sc.path);
        collect_rows(&key)
    };
    if rows.is_empty() {
        log(&format!(
            "show id={id} name='{}' -> no matching window, no preview",
            sc.name
        ));
        return Ok(());
    }

    log(&format!(
        "show id={id} name='{}' windows={} mode={}",
        sc.name,
        rows.len(),
        if rows.iter().all(|r| r.degraded) {
            "degraded"
        } else {
            "thumbnail"
        }
    ));
    send(Cmd::Show(Box::new(ShowReq {
        app_title: sc.name.clone(),
        app_icon: sc.icon.clone(),
        windows: rows,
        cx,
        top,
        scale,
    })));
    Ok(())
}

/// 悬停**运行中未固定**图标（transient）→ 按 key 收集窗口 → 投递 Show。
/// 应用名/图标取自 transient 目录缓存（`transient_entry`）。
pub fn show_for_transient(
    app: &tauri::AppHandle,
    key: &str,
    x: f64,
    y: f64,
    w: f64,
    _h: f64,
) -> Result<(), String> {
    let dock = app.get_webview_window("dock").ok_or("dock 窗口缺失")?;
    if !dock.is_visible().unwrap_or(false) || crate::dock::menu::is_panel_visible(app) {
        return Ok(());
    }
    let scale = dock.scale_factor().unwrap_or(1.0) as f32;
    let pos = dock
        .outer_position()
        .map_err(|e| format!("dock 位置不可得: {e}"))?;
    let cx = pos.x + ((x + w / 2.0) * scale as f64) as i32;
    let top = pos.y + (y * scale as f64) as i32;

    let entry = crate::dock::transient_entry(key);
    let rows = collect_rows(key);
    if rows.is_empty() {
        log(&format!(
            "show-transient key='{key}' -> no matching window, no preview"
        ));
        return Ok(());
    }
    log(&format!(
        "show-transient key='{key}' name='{}' windows={}",
        entry.name,
        rows.len()
    ));
    send(Cmd::Show(Box::new(ShowReq {
        app_title: entry.name,
        app_icon: entry.icon.unwrap_or(IconData {
            w: 1,
            h: 1,
            rgba: String::new(),
        }),
        windows: rows,
        cx,
        top,
        scale,
    })));
    Ok(())
}

// ---------------------------------------------------------------------------
// 预览线程
// ---------------------------------------------------------------------------

struct ThreadState {
    hwnd: HWND,
    /// 每行一个 DWM 缩略图句柄（0 = 该行降级未注册）。
    thumbs: Vec<isize>,
    /// 可点击的窗口行：`(源窗口, minimized)`——WM_LBUTTONDOWN 按行激活。
    rows: Vec<(isize, bool)>,
    /// 悬停行高亮（-1 = 无）。
    hover_row: i32,
    last_cmd: Instant,
}

impl Default for ThreadState {
    fn default() -> Self {
        ThreadState {
            hwnd: HWND::default(),
            thumbs: Vec::new(),
            rows: Vec::new(),
            hover_row: -1,
            last_cmd: Instant::now(),
        }
    }
}

/// WM_PAINT 用（wndproc 与线程同体，锁竞争可忽略）。
struct PaintState {
    app_title: String,
    app_icon: isize,   // HICON raw
    rows: Vec<String>, // 每行窗口标题
    degraded: Vec<bool>,
    scale: f32,
}

static PAINT: Mutex<Option<PaintState>> = Mutex::new(None);
/// 可点击窗口行注册表：show_preview 写入 / destroy_preview 清空 / wndproc 点击读取。
/// `(源窗口, minimized)`，下标 = 行号。
static ROWS: Mutex<Vec<(isize, bool)>> = Mutex::new(Vec::new());
/// 格几何表（007 横排）：show_preview 写入 / destroy 清空 / paint 与 hit_cell 读。
static CELLS: Mutex<Vec<RECT>> = Mutex::new(Vec::new());
/// 悬停行（-1 = 无）。
static HOVER_ROW: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// 预览窗屏幕 rect（007 断桥修复）：show_preview 写入 / destroy 清空，
/// dock 显隐循环经 current_rect() 读取——鼠标在预览窗内视同在 dock 内。
static PREVIEW_RECT: std::sync::RwLock<Option<RECT>> = std::sync::RwLock::new(None);

/// 预览窗当前屏幕 rect（物理 px）。`None` = 预览未显示。
/// dock 显隐循环用它把「鼠标在预览窗内」并入 hover 判定（007 返工补丁）。
pub fn current_rect() -> Option<RECT> {
    PREVIEW_RECT.read().ok().and_then(|g| *g)
}

fn thread_loop(rx: Receiver<Cmd>) {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let mut st = ThreadState::default();
    loop {
        match rx.try_recv() {
            Ok(Cmd::Show(req)) => {
                st.last_cmd = Instant::now();
                unsafe { show_preview(&mut st, *req) };
            }
            Ok(Cmd::Hide) => {
                st.last_cmd = Instant::now();
                unsafe { destroy_preview(&mut st, "hide-cmd") };
            }
            Err(TryRecvError::Disconnected) => {
                unsafe { destroy_preview(&mut st, "shutdown") };
                return;
            }
            Err(TryRecvError::Empty) => {}
        }
        // 兜底：前端失联（webview 崩溃等）时预览不悬留
        if !st.hwnd.0.is_null() && st.last_cmd.elapsed() >= Duration::from_millis(WATCHDOG_MS) {
            unsafe { destroy_preview(&mut st, "watchdog") };
        }
        pump(if st.hwnd.0.is_null() { 80 } else { 15 });
    }
}

fn pump(ms: u64) {
    unsafe {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(ms) {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

unsafe fn show_preview(st: &mut ThreadState, req: ShowReq) {
    destroy_preview(st, "replaced");

    let sc = req.scale;
    let n = req.windows.len().max(1);

    // 定位：图标上方居中，夹到图标所在显示器内。
    // 007 返工（横排）：总宽 clamp 到**工作区**（rcWork）——超出先缩格宽再换行，
    // 禁止横向溢出；窗口尺寸由 CellLayout 给出。
    let hmon = MonitorFromPoint(
        POINT {
            x: req.cx,
            y: req.top,
        },
        MONITOR_DEFAULTTONEAREST,
    );
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(hmon, &mut mi);
    let m = mi.rcMonitor;
    let work_w = mi.rcWork.right - mi.rcWork.left;
    let layout = layout_cells(
        n,
        CELL_W * sc,
        ROW_H * sc,
        GUTTER * sc,
        PAD * sc,
        HEADER_H * sc,
        work_w,
    );
    let w = layout.window_w;
    let h = layout.window_h;
    let x = (req.cx - w / 2).max(m.left + 4).min(m.right - w - 4);
    let y = (req.top - h - GAP_ABOVE).max(m.top + 4);

    let hmodule = match GetModuleHandleW(PCWSTR::null()) {
        Ok(h) => h,
        Err(_) => return,
    };
    let hinst = windows::Win32::Foundation::HINSTANCE(hmodule.0);
    // 类只注册一次；重复注册失败无害（overlay.rs 同款注释）
    let cursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR =
        LoadCursorW(None, windows::Win32::UI::WindowsAndMessaging::IDC_ARROW).unwrap_or_default();
    let wc = WNDCLASSW {
        lpfnWndProc: Some(preview_wndproc),
        hInstance: hinst,
        hCursor: cursor,
        // 类背景刷 = 品红：WM_ERASEBKGND 自动填充，首帧无白闪
        hbrBackground: CreateSolidBrush(COLORKEY),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    let _ = RegisterClassW(&wc);

    let hwnd = match CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
        CLASS_NAME,
        WINDOW_TITLE,
        WS_POPUP,
        x,
        y,
        w,
        h,
        None,
        None,
        Some(hinst),
        None,
    ) {
        Ok(h) => h,
        Err(e) => {
            log(&format!("create window failed: {e}"));
            return;
        }
    };
    // colorkey 全透明 + 玻璃模糊（复用 dock 药丸的 enable_glass 手法）
    let _ = SetLayeredWindowAttributes(hwnd, COLORKEY, 0, LWA_COLORKEY);
    let radius = (ROUND_R * sc) as i32;
    let glass = crate::dock::desktop::enable_glass(hwnd, Some(radius));
    // 窗口圆角裁切：头部带是不透明填充，靠窗口 region 裁出圆角
    let rgn = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(0, 0, w + 1, h + 1, radius, radius);
    if !rgn.0.is_null() {
        let _ = windows::Win32::Graphics::Gdi::SetWindowRgn(hwnd, Some(rgn), true);
    }

    // 头部应用图标：RGBA → HICON（一次/show）
    let icon_raw = icon_from_rgba(&req.app_icon)
        .map(|h| h.0 as isize)
        .unwrap_or(0);
    if let Ok(mut g) = PAINT.lock() {
        *g = Some(PaintState {
            app_title: req.app_title.clone(),
            app_icon: icon_raw,
            rows: req.windows.iter().map(|r| r.title.clone()).collect(),
            degraded: req.windows.iter().map(|r| r.degraded).collect(),
            scale: sc,
        });
    }

    st.hwnd = hwnd;
    st.rows = req.windows.iter().map(|r| (r.src, r.degraded)).collect();
    if let Ok(mut reg) = ROWS.lock() {
        *reg = st.rows.clone();
    }
    if let Ok(mut cells) = CELLS.lock() {
        *cells = layout.cells.clone();
    }
    // 007 断桥修复：登记预览窗屏幕 rect，供 dock 显隐循环并入 hover 判定
    if let Ok(mut pr) = PREVIEW_RECT.write() {
        *pr = Some(RECT { left: x, top: y, right: x + w, bottom: y + h });
    }
    st.hover_row = -1;

    // 每个非降级窗口注册一格 DWM 缩略图：目的地 = 格内容区（去掉格内标题条）。
    let strip = (ROW_TITLE * sc) as i32;
    for (i, r) in req.windows.iter().enumerate() {
        if r.degraded {
            continue; // 降级格：无缩略图（PaintState.degraded[i] 已置位）
        }
        let cr = layout.cells[i];
        let dest_rect = RECT {
            left: cr.left + 4,
            top: cr.top + 4,
            right: cr.right - 4,
            bottom: cr.bottom - strip - 4,
        };
        match DwmRegisterThumbnail(hwnd, HWND(r.src as *mut c_void)) {
            Ok(t) => {
                let dest = letterbox_dest(r.src, dest_rect);
                let mut sr = RECT::default();
                let _ = GetWindowRect(HWND(r.src as *mut c_void), &mut sr);
                let props = DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_RECTDESTINATION
                        | DWM_TNP_RECTSOURCE
                        | DWM_TNP_OPACITY
                        | DWM_TNP_VISIBLE,
                    rcDestination: dest,
                    rcSource: RECT {
                        left: 0,
                        top: 0,
                        right: sr.right - sr.left,
                        bottom: sr.bottom - sr.top,
                    },
                    opacity: 255,
                    fVisible: true.into(),
                    ..Default::default()
                };
                if let Err(e) = DwmUpdateThumbnailProperties(t, &props) {
                    log(&format!("thumb props failed: {e}"));
                }
                st.thumbs.push(t);
            }
            Err(e) => {
                // 注册失败（罕见）：该格降级为图标+标题，不影响其他格
                log(&format!("thumb register failed cell={i}: {e} -> degrade"));
                if let Ok(mut g) = PAINT.lock() {
                    if let Some(p) = g.as_mut() {
                        if let Some(d) = p.degraded.get_mut(i) {
                            *d = true;
                        }
                    }
                }
                st.thumbs.push(0);
            }
        }
    }

    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    let _ = SetWindowPos(
        hwnd,
        Some(windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
    );
    let _ = InvalidateRect(Some(hwnd), None, false);
    let thumb_count = st.thumbs.iter().filter(|&&t| t != 0).count();
    log(&format!(
        "preview open rect=({x},{y}) {w}x{h} glass={glass} rows={} thumbs={thumb_count}",
        st.rows.len(),
    ));
}

/// 横排格布局的纯函数入口（007 返工单测口）。
/// - `count`：格数（≤MAX_ROWS，截断在 collect_rows 已做）；
/// - `cell_w/cell_h/gutter/pad/header`：物理 px；
/// - `work_w`：显示器工作区宽度（总宽必须 clamp 进去）。
///
/// 算法：先按标准格宽算一行能放几格；一格都放不下 → 收缩格宽（下限
/// CELL_MIN_W，仍超就贴工作区宽）；`换行行数 = ceil(count / 每行格数)`；
/// 总宽 = 每行格数 × 格宽 + 间隙 + 左右 pad，永不超 `work_w`。
/// 返回窗口尺寸与每格客户区矩形（行主序：先横向后纵向）。
pub(crate) struct CellLayout {
    pub window_w: i32,
    pub window_h: i32,
    pub cells: Vec<RECT>,
}

fn layout_cells(
    count: usize,
    cell_w: f32,
    cell_h: f32,
    gutter: f32,
    pad: f32,
    header: f32,
    work_w: i32,
) -> CellLayout {
    let count = count.max(1);
    let avail = (work_w as f32 - 2.0 * pad).max(0.0);
    // 单格上限：标准格宽与工作区可容宽取小
    let cw = cell_w.min((avail - gutter).max(CELL_MIN_W));
    // 一行最多几格（含间隙；先按标准格宽，放不下时已收缩过一次）
    let per_row = (((avail + gutter) / (cw + gutter)).floor() as usize).clamp(1, count);
    let rows = count.div_ceil(per_row);
    let used_w = per_row as f32 * cw + (per_row as f32 - 1.0) * gutter + 2.0 * pad;
    let window_w = (used_w.min(work_w as f32)) as i32;
    let window_h = (header + rows as f32 * cell_h) as i32;

    let mut cells = Vec::with_capacity(count);
    for i in 0..count {
        let row = i / per_row;
        let col = i % per_row;
        let x = pad + col as f32 * (cw + gutter);
        let y = header + row as f32 * cell_h;
        let w = cw.min((window_w as f32 - x - pad).max(CELL_MIN_W));
        cells.push(RECT {
            left: x as i32,
            top: y as i32,
            right: (x + w) as i32,
            bottom: (y + cell_h) as i32,
        });
    }
    CellLayout {
        window_w,
        window_h,
        cells,
    }
}

/// 行/格截断（纯函数，单测口）：>MAX_ROWS 按 z 序截断。
fn truncate_rows(rows: Vec<PreviewWin>) -> Vec<PreviewWin> {
    let n = rows.len();
    if n > MAX_ROWS {
        log(&format!("rows truncated {n} -> {MAX_ROWS}"));
        rows.into_iter().take(MAX_ROWS).collect()
    } else {
        rows
    }
}

/// 缩略图目的地：源窗口宽高比等比装入 `dest`（居中 letterbox）。
/// DWM 不自动保比例（rcSource 直接拉伸到 rcDestination），不处理会压扁画面。
/// 缩略图目的地：源窗口宽高比等比装入 `dest`（居中 letterbox）。
/// DWM 不自动保比例（rcSource 直接拉伸到 rcDestination），不处理会压扁画面。
fn letterbox_dest(src: isize, dest: RECT) -> RECT {
    let mut sr = RECT::default();
    unsafe {
        let _ = GetWindowRect(HWND(src as *mut c_void), &mut sr);
    }
    let (sw, sh) = ((sr.right - sr.left).max(1), (sr.bottom - sr.top).max(1));
    let (bw, bh) = (
        (dest.right - dest.left).max(1),
        (dest.bottom - dest.top).max(1),
    );
    let k = (bw as f32 / sw as f32).min(bh as f32 / sh as f32);
    let (dw, dh) = ((sw as f32 * k) as i32, (sh as f32 * k) as i32);
    let dx = dest.left + (bw - dw) / 2;
    let dy = dest.top + (bh - dh) / 2;
    RECT {
        left: dx,
        top: dy,
        right: dx + dw,
        bottom: dy + dh,
    }
}

unsafe fn destroy_preview(st: &mut ThreadState, reason: &str) {
    if st.hwnd.0.is_null() {
        return;
    }
    for t in st.thumbs.drain(..) {
        if t != 0 {
            let _ = DwmUnregisterThumbnail(t);
        }
    }
    let _ = ShowWindow(st.hwnd, SW_HIDE);
    let _ = DestroyWindow(st.hwnd);
    st.hwnd = HWND::default();
    st.rows.clear();
    st.hover_row = -1;
    if let Ok(mut reg) = ROWS.lock() {
        reg.clear();
    }
    if let Ok(mut cells) = CELLS.lock() {
        cells.clear();
    }
    if let Ok(mut pr) = PREVIEW_RECT.write() {
        *pr = None;
    }
    HOVER_ROW.store(-1, Ordering::Relaxed);
    if let Ok(mut g) = PAINT.lock() {
        if let Some(p) = g.take() {
            if p.app_icon != 0 {
                let _ = DestroyIcon(windows::Win32::UI::WindowsAndMessaging::HICON(
                    p.app_icon as *mut c_void,
                ));
            }
        }
    }
    log(&format!("preview destroyed ({reason})"));
}

// ---------------------------------------------------------------------------
// 窗口过程：头部图标 + 标题（不透明 GDI 直绘于 colorkey 透明面之上）
// ---------------------------------------------------------------------------

unsafe extern "system" fn preview_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        windows::Win32::UI::WindowsAndMessaging::WM_PAINT => {
            let mut ps = Default::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            // colorkey 面：整个客户区涂品红 = 全透明，只露 DWM 玻璃模糊
            let brush = CreateSolidBrush(COLORKEY);
            FillRect(hdc, &ps.rcPaint, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));

            if let Ok(g) = PAINT.lock() {
                if let Some(p) = g.as_ref() {
                    let sc = p.scale;
                    let (w, _h) = (ps.rcPaint.right, ps.rcPaint.bottom);
                    // 007 返工横排：头部带（应用图标 + 应用名）+ N 格横排
                    // （每格 = 底衬 + 格内标题条；非降级格的缩略图由 DWM 叠在其上）。
                    // ⚠ 文字必须落在不透明面上：品红 colorkey 面上的 GDI 文字会与
                    // key 色混合渲染成白团（preview_poc 变体 E 实测）。
                    let band_h = (HEADER_H * sc) as i32;
                    let band = CreateSolidBrush(COLORREF(0x00F5_F5F2));
                    let band_rect = RECT {
                        left: 0,
                        top: 0,
                        right: w,
                        bottom: band_h,
                    };
                    FillRect(hdc, &band_rect, band);
                    let _ = DeleteObject(HGDIOBJ(band.0));

                    SetBkMode(hdc, TRANSPARENT);
                    let font = create_ui_font(sc);
                    let old = SelectObject(hdc, font.into());
                    SetTextColor(hdc, COLORREF(0x0018_1818));

                    // 头部：应用图标 + 应用名
                    let ix = (PAD * sc) as i32;
                    let iy = ((HEADER_H - 18.0) / 2.0 * sc) as i32;
                    let isz = (18.0 * sc) as i32;
                    if p.app_icon != 0 {
                        let _ = DrawIconEx(
                            hdc,
                            ix,
                            iy,
                            windows::Win32::UI::WindowsAndMessaging::HICON(
                                p.app_icon as *mut c_void,
                            ),
                            isz,
                            isz,
                            0,
                            None,
                            DI_NORMAL,
                        );
                    }
                    let mut head: Vec<u16> = p.app_title.encode_utf16().collect();
                    let mut hr = RECT {
                        left: ((PAD + 18.0 + 6.0) * sc) as i32,
                        top: 0,
                        right: w - 4,
                        bottom: band_h,
                    };
                    DrawTextW(
                        hdc,
                        &mut head,
                        &mut hr,
                        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                    );

                    // 格（007 返工横排）：底衬（不透明，接住点击）+ 悬停高亮 +
                    // 格内标题条（宽度随格宽）。几何来自 CELLS 静态表。
                    let cells = CELLS.lock().unwrap();
                    for (i, row_title) in p.rows.iter().enumerate() {
                        let Some(rr) = cells.get(i).copied() else {
                            continue;
                        };
                        let hover = i as i32 == HOVER_ROW.load(Ordering::Relaxed);
                        let bg = if hover { COLORREF(0x00E2_F2FC) } else { ROW_BG };
                        let bgb = CreateSolidBrush(bg);
                        FillRect(hdc, &rr, bgb);
                        let _ = DeleteObject(HGDIOBJ(bgb.0));

                        if p.degraded.get(i).copied().unwrap_or(true) {
                            // 降级格：应用图标 + 窗口标题占满格（无缩略图）
                            if p.app_icon != 0 {
                                let _ = DrawIconEx(
                                    hdc,
                                    rr.left + (8.0 * sc) as i32,
                                    rr.top + ((ROW_H - 24.0) / 2.0 * sc) as i32,
                                    windows::Win32::UI::WindowsAndMessaging::HICON(
                                        p.app_icon as *mut c_void,
                                    ),
                                    (24.0 * sc) as i32,
                                    (24.0 * sc) as i32,
                                    0,
                                    None,
                                    DI_NORMAL,
                                );
                            }
                            let mut t: Vec<u16> = row_title.encode_utf16().collect();
                            let mut tr = RECT {
                                left: rr.left + ((8.0 + 24.0 + 4.0) * sc) as i32,
                                top: rr.top,
                                right: rr.right - 8,
                                bottom: rr.bottom,
                            };
                            DrawTextW(
                                hdc,
                                &mut t,
                                &mut tr,
                                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                            );
                        } else {
                            // 格内标题条：不透明浅条（窗口标题），随格宽
                            let strip_y = rr.bottom - (ROW_TITLE * sc) as i32;
                            let strip = CreateSolidBrush(COLORREF(0x00F5_F5F2));
                            let strip_rect = RECT {
                                left: rr.left,
                                top: strip_y,
                                right: rr.right,
                                bottom: rr.bottom,
                            };
                            FillRect(hdc, &strip_rect, strip);
                            let _ = DeleteObject(HGDIOBJ(strip.0));
                            let mut t: Vec<u16> = row_title.encode_utf16().collect();
                            let mut tr = RECT {
                                left: rr.left + (6.0 * sc) as i32,
                                top: strip_y,
                                right: rr.right - 6,
                                bottom: rr.bottom,
                            };
                            DrawTextW(
                                hdc,
                                &mut t,
                                &mut tr,
                                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                            );
                        }
                    }
                    drop(cells);
                    let _ = SelectObject(hdc, old);
                    let _ = DeleteObject(font.into());
                }
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_ERASEBKGND => LRESULT(1),
        windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONDOWN => {
            // 007 点击切换：命中行 → 还原/置前该窗口，销毁预览。
            // 激活调用发生在预览线程（非 UI 线程）；前台锁按进程判定，
            // 用户刚点了 dock/预览（本进程持前台权），置前通常直接成功。
            let x = (lp.0 & 0xFFFF) as i16 as i32;
            let y = (lp.0 >> 16) as i16 as i32;
            let src = preview_src_at(x, y);
            log(&format!(
                "LBUTTONDOWN client=({},{}) src={}",
                x,
                y,
                src.map(|s| format!("0x{:x}", s)).unwrap_or_else(|| "NONE".into())
            ));
            if let Some(src) = src {
                unsafe { activate_window(src) };
                log(&format!("activate called src=0x{:x}", src));
            }
            // 经 Cmd::Hide 让预览线程自行销毁（wndproc 无 ThreadState 上下文）
            send(Cmd::Hide);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_MOUSEMOVE => {
            // 悬停格高亮：变了才重绘（格间隙/头部带 → -1 不高亮）
            let x = (lp.0 & 0xFFFF) as i16 as i32;
            let y = (lp.0 >> 16) as i16 as i32;
            let row = hit_cell(x, y).map(|i| i as i32).unwrap_or(-1);
            if HOVER_ROW.swap(row, Ordering::Relaxed) != row && row >= 0 {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 点 → 格号（纯函数，单测口）：落在格矩形内（半开区间）才命中——
/// 格间 ≥4px 间隙与头部带天然不在任何矩形内 → None。
fn hit_cell_in(client_x: i32, client_y: i32, cells: &[RECT]) -> Option<usize> {
    cells.iter().position(|r| {
        client_x >= r.left && client_x < r.right && client_y >= r.top && client_y < r.bottom
    })
}

/// 运行时壳：读 CELLS 静态表（show_preview 写入的同一几何）。
fn hit_cell(client_x: i32, client_y: i32) -> Option<usize> {
    let cells = CELLS.lock().unwrap();
    hit_cell_in(client_x, client_y, &cells)
}

/// 点击激活目标：格注册表按坐标命中 → 源窗口（降级格保留真实句柄，仍返回 Some）。
fn preview_src_at(client_x: i32, client_y: i32) -> Option<isize> {
    let cell = hit_cell(client_x, client_y)?;
    let rows = ROWS.lock().ok()?;
    let (src, _minimized) = *rows.get(cell)?;
    Some(src)
}

/// 还原最小化 + 置前（preview 行点击共用；前台锁语义见 activate_or_launch）。
unsafe fn activate_window(src: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, SetForegroundWindow, ShowWindow, SwitchToThisWindow, SW_RESTORE,
    };
    let hwnd = HWND(src as *mut c_void);
    let iconic = IsIconic(hwnd).as_bool();
    if iconic {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        log(&format!("src=0x{:x} was minimized → SW_RESTORE", src));
    }
    let set_ok = SetForegroundWindow(hwnd).as_bool();
    let fg1 = GetForegroundWindow();
    log(&format!(
        "SetForegroundWindow -> {} (fg now 0x{:x})",
        set_ok, fg1.0 as isize
    ));
    if !set_ok {
        SwitchToThisWindow(hwnd, true);
        let fg2 = GetForegroundWindow();
        log(&format!(
            "SwitchToThisWindow fallback -> fg now 0x{:x}",
            fg2.0 as isize
        ));
    }
}

/// UI 字体（Segoe UI 9pt × scale）。
unsafe fn create_ui_font(scale: f32) -> windows::Win32::Graphics::Gdi::HFONT {
    use windows::Win32::Graphics::Gdi::{
        CreateFontW, ANTIALIASED_QUALITY, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, FW_NORMAL,
        OUT_DEFAULT_PRECIS,
    };
    let h = (12.0 * scale) as i32;
    let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
    CreateFontW(
        h,
        0,
        0,
        0,
        FW_NORMAL.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
        // ⚠ 不能用 CLEARTYPE_QUALITY：ClearType 亚像素混色会把 LWA_COLORKEY 的品红
        // key 色掺进文字（preview_poc 变体 E 实测：白底品红描边糊字）；ANTIALIASED 正常。
        ANTIALIASED_QUALITY,
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        windows::core::PCWSTR::from_raw(face.as_ptr()),
    )
}

// ---------------------------------------------------------------------------
// 工具：RGBA → HICON / base64 解码 / DRM 启发式 / 日志
// ---------------------------------------------------------------------------

/// dock 的 IconData（32bpp RGBA base64）→ HICON（图标 alpha 为直通 alpha，非预乘）。
fn icon_from_rgba(d: &IconData) -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    let rgba = crate::dock::b64_decode(&d.rgba)?;
    let (w, h) = (d.w as i32, d.h as i32);
    if rgba.len() < (w * h * 4) as usize {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        // 颜色位图：32bpp BGRA top-down
        let bi = windows::Win32::Graphics::Gdi::BITMAPINFO {
            bmiHeader: windows::Win32::Graphics::Gdi::BITMAPINFOHEADER {
                biSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>()
                    as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbm_color =
            CreateDIBSection(Some(screen), &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), bits as *mut u8, rgba.len());
        // BGRA ↔ RGBA 交换（IconData 存 RGBA）
        let px = bits as *mut u8;
        for i in 0..(w * h) as usize {
            std::ptr::swap(px.add(i * 4), px.add(i * 4 + 2));
        }
        // 掩码位图：全 0（alpha 位图语义）
        let mask_len = (((w + 15) / 16) * 2 * h) as usize;
        let mask_bits = vec![0u8; mask_len];
        let hbm_mask = windows::Win32::Graphics::Gdi::CreateBitmap(
            w,
            h,
            1,
            1,
            Some(mask_bits.as_ptr() as *const c_void),
        );
        if hbm_mask.0.is_null() {
            let _ = DeleteObject(hbm_color.into());
            ReleaseDC(None, screen);
            return None;
        }
        let ii = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };
        let icon = CreateIconIndirect(&ii);
        let _ = DeleteObject(hbm_color.into());
        let _ = DeleteObject(hbm_mask.into());
        ReleaseDC(None, screen);
        icon.ok()
    }
}

/// DRM 保护内容启发式：源窗口是前台时，抓其中心 1/2 区域，
/// 近纯黑（mean<3 且 stdev<1.5）→ 判为保护内容（BitBlt 与 DWM 缩略图都拿不到 DRM 帧）。
/// 已知局限（交付单记录）：仅前台窗口有意义（被遮挡窗口抓到的是遮挡物）；
/// 全黑暂停画面会误降级——代价是显示图标+标题，可接受。
fn is_protected_black(src: isize) -> bool {
    unsafe {
        let h = HWND(src as *mut c_void);
        if GetForegroundWindow().0 != h.0 {
            return false; // 非前台：画面可能被遮挡，抓屏无意义
        }
        let mut r = RECT::default();
        if GetWindowRect(h, &mut r).is_err() {
            return false;
        }
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        if ww < 40 || wh < 40 {
            return false;
        }
        // 中心 1/2（避开窗缘阴影）
        let (cw, ch) = (ww / 2, wh / 2);
        let (x0, y0) = (r.left + ww / 4, r.top + wh / 4);
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, cw, ch);
        let old = SelectObject(mem, bmp.into());
        let copied = BitBlt(mem, 0, 0, cw, ch, Some(screen), x0, y0, SRCCOPY).is_ok();
        let mut buf = vec![0u8; (cw * ch * 4) as usize];
        let mut bi = windows::Win32::Graphics::Gdi::BITMAPINFO {
            bmiHeader: windows::Win32::Graphics::Gdi::BITMAPINFOHEADER {
                biSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>()
                    as u32,
                biWidth: cw,
                biHeight: -ch,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let got = GetDIBits(
            mem,
            bmp,
            0,
            ch as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !copied || got == 0 {
            return false;
        }
        // 隔 4 像素采样
        let mut sum = 0.0f64;
        let mut sum2 = 0.0f64;
        let mut n = 0usize;
        for i in (0..buf.len()).step_by(16) {
            let l = 0.114 * buf[i] as f64 + 0.587 * buf[i + 1] as f64 + 0.299 * buf[i + 2] as f64;
            sum += l;
            sum2 += l * l;
            n += 1;
        }
        if n == 0 {
            return false;
        }
        let mean = sum / n as f64;
        let var = (sum2 / n as f64 - mean * mean).max(0.0);
        if !(mean < 3.0 && var.sqrt() < 1.5) {
            return false;
        }
        // B-ADV-1（评审 advisory）：纯黑只是必要条件——深色主题应用/终端全屏空窗口
        // 也是近纯黑。加标题关键词门控：疑似视频类应用才降级，其余保留缩略图。
        // 取舍：未列入关键词的 DRM 应用仍会显示黑缩略图（宁可漏降级，不误伤终端/IDE）。
        let mut buf = [0u16; 128];
        let len =
            windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(h, &mut buf).max(0) as usize;
        let title = String::from_utf16_lossy(&buf[..len]).to_lowercase();
        const VIDEO_KEYWORDS: [&str; 12] = [
            "video",
            "dvd",
            "netflix",
            "youtube",
            "movie",
            "film",
            "media",
            "plex",
            "vlc",
            "potplayer",
            "mpv",
            "播放",
        ];
        let hit = VIDEO_KEYWORDS.iter().any(|k| title.contains(k));
        log(&format!(
            "drm-suspect: uniform black + title-match={hit} (title='{title}')"
        ));
        hit
    }
}

/// 事件日志（证据链）：`%APPDATA%\dev.glassdesk.app\preview.log`，超 64KB 截到尾部 8KB。
fn log(line: &str) {
    let Some(p) = LOG_PATH.lock().ok().and_then(|g| g.clone()) else {
        return;
    };
    use std::io::Write;
    if let Ok(meta) = std::fs::metadata(&p) {
        if meta.len() > 64 * 1024 {
            if let Ok(all) = std::fs::read(&p) {
                let tail = &all[all.len().saturating_sub(8 * 1024)..];
                let _ = std::fs::write(&p, tail);
            }
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    {
        let st: SYSTEMTIME = unsafe { GetLocalTime() };
        let _ = writeln!(
            f,
            "[{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}] {}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds, line
        );
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    const H: f32 = 26.0; // 头部带（与生产常量同值，测试固定口径）

    /// 006 ②③⑤ 之外的 007 ①：1/2/3/5/6 窗的格数、换行行数与总宽。
    /// work_w=1920（不收缩）：1/2/3 行内单行；5/6 单行（200+6 间距远小于 1920）。
    #[test]
    fn layout_counts_and_widths_single_row() {
        for n in [1usize, 2, 3, 5, 6] {
            let l = layout_cells(n, 200.0, 150.0, 6.0, 10.0, H, 1920);
            assert_eq!(l.cells.len(), n, "n={n} 格数");
            let rows: std::collections::HashSet<i32> = l.cells.iter().map(|r| r.top).collect();
            assert_eq!(rows.len(), 1, "n={n} 应单行");
            let need = n as f32 * 200.0 + (n as f32 - 1.0) * 6.0 + 20.0;
            assert!(l.window_w as f32 >= need.min(1920.0) - 1.0, "n={n} 总宽");
            assert!(l.window_w <= 1920, "n={n} 不得超工作区");
        }
    }

    /// 007 ①：窄工作区强制换行——3 窗在 450px 宽（单格+间隙只够 2 格/行）
    /// → 2 行（2+1）；9 窗超 MAX_ROWS 上限，截断到 6 后换行。
    #[test]
    fn layout_wraps_when_narrow() {
        let l = layout_cells(3, 200.0, 150.0, 6.0, 10.0, H, 450);
        assert_eq!(l.cells.len(), 3);
        let rows: std::collections::HashSet<i32> = l.cells.iter().map(|r| r.top).collect();
        assert_eq!(rows.len(), 2, "450px 宽 → 3 窗换 2 行");
        // 截断到 6 后的 9 窗场景（collect_rows 先截，layout 收 6）
        let l6 = layout_cells(6, 200.0, 150.0, 6.0, 10.0, H, 450);
        let rows6: std::collections::HashSet<i32> = l6.cells.iter().map(|r| r.top).collect();
        assert_eq!(l6.cells.len(), 6);
        assert_eq!(rows6.len(), 3, "6 窗 2 格/行 → 3 行");
    }

    /// 007 ②：hit_cell_in 边界与间隙——格内四角命中、右边界半开、
    /// 格间间隙与头部带不命中。
    #[test]
    fn hit_cell_boundaries_and_gutters() {
        let l = layout_cells(3, 200.0, 150.0, 6.0, 10.0, H, 1000);
        let cells = &l.cells;
        // 格 0 中心命中
        let c0 = &cells[0];
        assert_eq!(
            hit_cell_in((c0.left + c0.right) / 2, (c0.top + c0.bottom) / 2, cells),
            Some(0)
        );
        // 右边界半开：right-1 命中格 0，right 命中下一格（或 None）
        assert_eq!(
            hit_cell_in(c0.right - 1, (c0.top + c0.bottom) / 2, cells),
            Some(0)
        );
        // 格间间隙：cell0.right .. cell1.left 之间（≥4px）不命中
        if cells.len() >= 2 {
            let gap_x = cells[0].right + (cells[1].left - cells[0].right) / 2;
            if gap_x < cells[1].left {
                assert_eq!(
                    hit_cell_in(gap_x, (cells[0].top + cells[0].bottom) / 2, cells),
                    None,
                    "间隙不响应"
                );
            }
        }
        // 头部带（第一格 top 之上）不命中
        assert_eq!(hit_cell_in(cells[0].left + 10, (H as i32) - 5, cells), None);
    }

    /// 007 ④：总宽 clamp——work_w=300 时单格收缩（≥CELL_MIN_W）且窗口宽 ≤300；
    /// work_w 小于格宽下限时贴工作区宽。
    #[test]
    fn layout_clamps_window_width_to_work_area() {
        let l = layout_cells(4, 200.0, 150.0, 6.0, 10.0, H, 300);
        assert!(l.window_w <= 300, "总宽 clamp 进工作区");
        for r in &l.cells {
            assert!(r.right <= l.window_w, "每格右缘不出窗");
        }
        // 极窄：不 panic，宽 ≥1
        let l2 = layout_cells(2, 200.0, 150.0, 6.0, 10.0, H, 60);
        assert!(l2.window_w >= 1 && l2.window_w <= 60);
        assert_eq!(l2.cells.len(), 2);
    }

    /// 007 ③：>MAX_ROWS 截断（9 → 6，保留 z 序最前的 6 个）。
    #[test]
    fn truncate_rows_caps_at_max() {
        let rows: Vec<PreviewWin> = (0..9usize)
            .map(|i| PreviewWin {
                src: 1000 + i as isize,
                title: format!("w{i}"),
                degraded: false,
            })
            .collect();
        let t = truncate_rows(rows);
        assert_eq!(t.len(), MAX_ROWS);
        assert_eq!(t[0].title, "w0", "保留 z 序最前");
        assert_eq!(t[MAX_ROWS - 1].title, "w5");
        // 不超限：原样返回
        let rows6: Vec<PreviewWin> = (0..6usize)
            .map(|i| PreviewWin {
                src: i as isize,
                title: format!("w{i}"),
                degraded: false,
            })
            .collect();
        let t2 = truncate_rows(rows6);
        assert_eq!(t2.len(), 6);
    }
}

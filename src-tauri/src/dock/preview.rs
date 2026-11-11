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
    DispatchMessageW, DrawIconEx, GetForegroundWindow, GetWindowRect, LoadCursorW, PeekMessageW,
    RegisterClassW, SetLayeredWindowAttributes, SetWindowPos, ShowWindow, TranslateMessage,
    DI_NORMAL, ICONINFO, LWA_COLORKEY, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_HIDE, SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use tauri::Manager;

use crate::dock::IconData;

/// 品红 colorkey：客户区涂品红 = 表面全透明（v0.1 玻璃层同款）。
const COLORKEY: COLORREF = COLORREF(0x00FF_00FF);
const CLASS_NAME: PCWSTR = w!("GlassDesk_Preview");
const WINDOW_TITLE: PCWSTR = w!("GlassDesk Preview");

// 尺寸基准（96 DPI 物理像素，show 时乘 scale）。220×170 贴近卡面「约 200×150」。
const BASE_W: f32 = 220.0;
const BASE_H: f32 = 170.0;
const BASE_H_DEGRADED: f32 = 40.0;
const HEADER_H: f32 = 26.0; // 头部带（图标 + 标题）高
const PAD: f32 = 10.0; // 缩略图区与窗缘间距
const GAP_ABOVE: i32 = 6; // 预览窗与图标上缘间距
const ROUND_R: f32 = 14.0; // 圆角半径（与 dock 药丸 18 略小，同材质语言）
const WATCHDOG_MS: u64 = 10_000; // 兜底：前端失联时预览最长存活

/// Show 请求（show_for 解析完毕后投递给预览线程）。
pub struct ShowReq {
    /// 源窗口（0 = 降级模式：图标 + 标题，无缩略图）
    src: isize,
    title: String,
    icon: IconData,
    /// 图标中心 x / 图标上缘 y（物理屏幕坐标，预览窗定位锚点）
    cx: i32,
    top: i32,
    scale: f32,
    degraded_reason: Option<&'static str>,
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

/// 解析悬停的快捷方式 → 源窗口 → 投递 Show。
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

    // 源窗口：z 序最前的可见匹配（复用任务 A 的 key 体系）
    let key = crate::dock::running::key_for_shortcut(&sc.aumid, &sc.path);
    let Some((src, minimized)) = crate::dock::running::find_window_for_key(&key) else {
        log(&format!(
            "show id={id} name='{}' -> no matching window, no preview",
            sc.name
        ));
        return Ok(());
    };

    // 降级判定（卡面 2.4）：最小化 → 图标+标题；DRM 黑块启发式 → 图标+标题
    let degraded_reason: Option<&'static str> = if minimized {
        Some("minimized")
    } else if is_protected_black(src) {
        Some("drm-suspect")
    } else {
        None
    };

    log(&format!(
        "show id={id} name='{}' src={} mode={}",
        sc.name,
        src,
        degraded_reason.unwrap_or("thumbnail")
    ));
    send(Cmd::Show(Box::new(ShowReq {
        src: if degraded_reason.is_some() { 0 } else { src },
        title: sc.name.clone(),
        icon: sc.icon.clone(),
        cx,
        top,
        scale,
        degraded_reason,
    })));
    Ok(())
}

// ---------------------------------------------------------------------------
// 预览线程
// ---------------------------------------------------------------------------

struct ThreadState {
    hwnd: HWND,
    thumb: isize,
    icon: isize, // HICON raw（DestroyIcon 归还）
    last_cmd: Instant,
}

impl Default for ThreadState {
    fn default() -> Self {
        ThreadState {
            hwnd: HWND::default(),
            thumb: 0,
            icon: 0,
            last_cmd: Instant::now(),
        }
    }
}

/// WM_PAINT 用（wndproc 与线程同体，锁竞争可忽略）。
struct PaintState {
    title: String,
    icon: isize,
    degraded: bool,
    scale: f32,
}

static PAINT: Mutex<Option<PaintState>> = Mutex::new(None);

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
    let degraded = req.degraded_reason.is_some();
    let w = (BASE_W * sc) as i32;
    let h = ((if degraded { BASE_H_DEGRADED } else { BASE_H }) * sc) as i32;

    // 定位：图标上方居中，夹到图标所在显示器内
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

    // 头部图标：RGBA → HICON（一次/show）
    let icon_raw = icon_from_rgba(&req.icon).map(|h| h.0 as isize).unwrap_or(0);
    if let Ok(mut g) = PAINT.lock() {
        *g = Some(PaintState {
            title: req.title.clone(),
            icon: icon_raw,
            degraded,
            scale: sc,
        });
    }

    st.hwnd = hwnd;
    st.icon = icon_raw;

    // 缩略图注册（降级模式无源窗口，跳过）
    if req.src != 0 {
        match DwmRegisterThumbnail(hwnd, HWND(req.src as *mut c_void)) {
            Ok(t) => {
                let dest = letterbox_dest(&req, w, h);
                let mut sr = RECT::default();
                let _ = GetWindowRect(HWND(req.src as *mut c_void), &mut sr);
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
                st.thumb = t;
            }
            Err(e) => {
                // 注册失败（罕见）：退化为图标+标题尺寸，不留空玻璃框
                log(&format!("thumb register failed: {e} -> degrade"));
                destroy_preview(st, "reg-failed");
                show_preview(
                    st,
                    ShowReq {
                        src: 0,
                        degraded_reason: Some("reg-failed"),
                        ..req
                    },
                );
                return;
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
    log(&format!(
        "preview open rect=({x},{y}) {w}x{h} glass={glass} thumb={}",
        if st.thumb != 0 { "ok" } else { "none" }
    ));
}

/// 缩略图目的地：源窗口宽高比等比装入内容区（居中 letterbox）。
/// DWM 不自动保比例（rcSource 直接拉伸到 rcDestination），不处理会压扁画面。
fn letterbox_dest(req: &ShowReq, win_w: i32, win_h: i32) -> RECT {
    let mut sr = RECT::default();
    unsafe {
        let _ = GetWindowRect(HWND(req.src as *mut c_void), &mut sr);
    }
    let (sw, sh) = ((sr.right - sr.left).max(1), (sr.bottom - sr.top).max(1));
    let pad = (PAD * req.scale) as i32;
    let header = (HEADER_H * req.scale) as i32;
    // 内容区：左右 PAD，上 header+6，下 PAD
    let (bx, by) = (pad, header + (6.0 * req.scale) as i32);
    let (bw, bh) = (win_w - pad * 2, win_h - by - pad);
    let k = (bw as f32 / sw as f32).min(bh as f32 / sh as f32);
    let (dw, dh) = ((sw as f32 * k) as i32, (sh as f32 * k) as i32);
    let dx = bx + (bw - dw) / 2;
    let dy = by + (bh - dh) / 2;
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
    if st.thumb != 0 {
        let _ = DwmUnregisterThumbnail(st.thumb);
        st.thumb = 0;
    }
    let _ = ShowWindow(st.hwnd, SW_HIDE);
    let _ = DestroyWindow(st.hwnd);
    st.hwnd = HWND::default();
    if st.icon != 0 {
        let _ = DestroyIcon(windows::Win32::UI::WindowsAndMessaging::HICON(
            st.icon as *mut c_void,
        ));
        st.icon = 0;
    }
    if let Ok(mut g) = PAINT.lock() {
        if let Some(p) = g.take() {
            if p.icon != 0 {
                let _ = DestroyIcon(windows::Win32::UI::WindowsAndMessaging::HICON(
                    p.icon as *mut c_void,
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
                    let (w, h) = (ps.rcPaint.right, ps.rcPaint.bottom);
                    // 头部带：不透明浅色条（dock 药丸同系浅色）。
                    // ⚠ 文字必须落在不透明面上：品红 colorkey 面上的 GDI 文字
                    // 会与 key 色混合渲染成白团（preview_poc 变体 E 实测，
                    // ClearType/ANTIALIASED 皆坏）；不透明填充上的文字颜色正确（变体 C）。
                    let band_h = if p.degraded {
                        h
                    } else {
                        (HEADER_H * sc) as i32
                    };
                    let band = CreateSolidBrush(COLORREF(0x00F5_F5F2));
                    let band_rect = RECT {
                        left: 0,
                        top: 0,
                        right: w,
                        bottom: band_h,
                    };
                    FillRect(hdc, &band_rect, band);
                    let _ = DeleteObject(HGDIOBJ(band.0));

                    let (ix, iy, isz, tx) = if p.degraded {
                        // 降级模式：整窗单行（图标 + 标题）
                        (
                            (8.0 * sc) as i32,
                            ((BASE_H_DEGRADED - 24.0) / 2.0 * sc) as i32,
                            (24.0 * sc) as i32,
                            8.0 + 24.0 + 8.0,
                        )
                    } else {
                        // 头部带：图标 16 + 标题
                        (
                            (PAD * sc) as i32,
                            ((HEADER_H - 18.0) / 2.0 * sc) as i32,
                            (18.0 * sc) as i32,
                            PAD + 18.0 + 6.0,
                        )
                    };
                    if p.icon != 0 {
                        let _ = DrawIconEx(
                            hdc,
                            ix,
                            iy,
                            windows::Win32::UI::WindowsAndMessaging::HICON(p.icon as *mut c_void),
                            isz,
                            isz,
                            0,
                            None,
                            DI_NORMAL,
                        );
                    }
                    // 标题：深色主体直接落在浅色带上（无 halo——halo 依赖 colorkey
                    // 面上的白字，正是坏掉的那条路径）
                    let mut title: Vec<u16> = p.title.encode_utf16().collect();
                    let mut r = RECT {
                        left: (tx * sc) as i32,
                        top: 0,
                        right: w - 4,
                        bottom: band_h,
                    };
                    SetBkMode(hdc, TRANSPARENT);
                    let font = create_ui_font(sc);
                    let old = SelectObject(hdc, font.into());
                    SetTextColor(hdc, COLORREF(0x0018_1818));
                    DrawTextW(
                        hdc,
                        &mut title,
                        &mut r,
                        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                    );
                    let _ = SelectObject(hdc, old);
                    let _ = DeleteObject(font.into());
                }
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
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

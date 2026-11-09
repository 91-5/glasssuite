//! dock 功能域：固定 / 临时图标、拖拽排序、游离图标、开始菜单、悬停预览、自动隐藏。
//!
//! 线程模型（卡面 D6 会把它写进 `docs/architecture.md`）：
//! - Tauri 主线程：建窗、resize、DWM 属性、事件分发；**不做任何可能卡住的系统调用**。
//! - `glasssuite-running-scan`（1.5s 周期，短命）：枚举可见窗口，2s 硬截止。
//! - `glasssuite-icon` / `glasssuite-dwm-preview` / `glasssuite-menu-scan` /
//!   `glasssuite-launch`：按需短命线程，各自带 800ms / 400ms / 2s / 1s 硬截止。
//! - 集合同步用**内容指纹**：Rust watcher 比对一次，前端再比对一次（v0.8.1 修复 #1），
//!   指纹不变就不 emit、不 setState，避免 transient 图标增减时整排重排。
//!
//! 隐私：日志只写事件类型 + 数量，进程名 / 路径 / 应用名一律不进日志（硬约束 13）。

pub mod config;
pub mod free;
pub mod icons;
pub mod menu;
pub mod preview;
pub mod running;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::state::SharedState;
use config::{DockConfig, IconMode, MenuConfig, Shortcut};

/// 运行中集合发生**实质**变化时才下发（前端仍会再比一次指纹）。
pub const RUNNING_EVENT: &str = "dock-running";
/// DWM 预览超时/不可用：前端收到后显示纯色块，不等待、不重试。
pub const PREVIEW_FALLBACK_EVENT: &str = "dock-preview-fallback";
/// 开始菜单开合。
pub const MENU_EVENT: &str = "dock-menu-state";

const POLL_INTERVAL: Duration = Duration::from_millis(1500);
pub const MENU_W: f64 = 320.0;
pub const MENU_H: f64 = 420.0;
/// 悬停预览承载窗尺寸（逻辑像素）。DWM 缩略图只能画进目标窗的客户区，
/// 而 dock 窗口高度只有一枚药丸，所以必须有一个与预览等大的独立窗口。
pub const PREVIEW_W: f64 = 220.0;
pub const PREVIEW_H: f64 = 140.0;

// ---------------------------------------------------------------------------
// 快照与状态
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DockEntry {
    /// `exe:<规范化路径>`，dock / 运行集合 / 预览三处共用的稳定标识。
    pub key: String,
    /// 配置里的稳定 id（仅固定项有；临时项等于 key）。
    pub id: String,
    pub name: String,
    pub path: String,
    /// 本机运行时提取的图标 data URL；`null` 时前端用自绘 SVG 占位。
    pub icon: Option<String>,
    pub running: bool,
    pub pinned: bool,
    #[serde(rename = "freeRect")]
    pub free_rect: Option<config::FreeRect>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DockSnapshot {
    pub pinned: Vec<DockEntry>,
    pub transients: Vec<DockEntry>,
    pub free: Vec<DockEntry>,
    pub auto_hide: bool,
    /// 内容指纹 = 实际渲染顺序的 key 列表（固定项 + 临时项）。前端据此决定要不要 setState。
    pub fingerprint: String,
}

/// 内容指纹：key 的**顺序**本身就是内容的一部分（拖拽排序必须能被看见），
/// 但不取 hwnd / 图标 / 窗口位置——那些字段变化不该触发重排（v0.8.1 修复 #1）。
///
/// 调用方保证顺序稳定：固定项按配置顺序，临时项按「首次出现顺序」（见 `merge_transient_order`）。
/// 指纹必须和渲染顺序用同一份列表，否则 `EnumWindows` 的 z-order 抖动会被误判成内容变化。
pub fn fingerprint(keys: &[String]) -> String {
    keys.join("|")
}

/// 稳定临时项显示顺序：保留仍在集合中的旧 key 的相对顺序，**新 key 只追加到末尾**。
///
/// 为什么不能按 key 排序：`EnumWindows` 的返回顺序随 z-order 变化，排序虽确定，但新开一个
/// 程序时它的 key 可能插进中间，把后面所有既有图标整体推走一枚图标宽度 —— 这正是
/// v0.8.1 要消灭的「图标一连串跳」（B4 实测曾打出 `dx=[0×5, -50×8]`）。
/// 只追加末尾时，既有图标的自然布局位不变，FLIP 的 dx 自然全 0。
fn merge_transient_order(order: &mut Vec<String>, keys: &[String]) {
    order.retain(|k| keys.iter().any(|n| n == k));
    for k in keys {
        if !order.iter().any(|o| o == k) {
            order.push(k.clone());
        }
    }
}

fn transient_entry(a: &running::RunningApp, icons: &HashMap<String, String>) -> DockEntry {
    DockEntry {
        key: a.key.clone(),
        id: a.key.clone(),
        name: name_from_path(&a.path),
        path: a.path.clone(),
        icon: icons.get(&a.key).cloned(),
        running: true,
        pinned: false,
        free_rect: None,
    }
}

fn name_from_path(path: &str) -> String {
    PathBuf::from(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("应用")
        .to_string()
}

pub struct DockState {
    pub data_dir: PathBuf,
    pub config: Mutex<DockConfig>,
    pub menu_config: Mutex<MenuConfig>,
    pub running: Mutex<Vec<running::RunningApp>>,
    /// 临时项显示顺序（key），与 `running` 分开存，避免 z-order 抖动影响渲染顺序。
    pub transient_order: Mutex<Vec<String>>,
    pub icons: Mutex<HashMap<String, String>>,
    pub last_fingerprint: Mutex<String>,
    pub preview: preview::PreviewController,
}

impl DockState {
    pub fn new(data_dir: PathBuf) -> Self {
        let cfg = config::load_dock(&data_dir);
        let menu_cfg = config::load_menu(&data_dir);
        Self {
            config: Mutex::new(cfg),
            menu_config: Mutex::new(menu_cfg),
            running: Mutex::new(Vec::new()),
            transient_order: Mutex::new(Vec::new()),
            icons: Mutex::new(HashMap::new()),
            last_fingerprint: Mutex::new(String::new()),
            preview: preview::PreviewController::default(),
            data_dir,
        }
    }

    /// watcher 每次扫完调用：先稳定顺序，再换运行集合。
    /// 两把锁**不重叠持有**，`snapshot()` 的加锁顺序不会与它构成环。
    pub fn set_running(&self, apps: Vec<running::RunningApp>) {
        let keys: Vec<String> = apps.iter().map(|a| a.key.clone()).collect();
        {
            let mut order = self.transient_order.lock().unwrap();
            merge_transient_order(&mut order, &keys);
        }
        *self.running.lock().unwrap() = apps;
    }

    pub fn snapshot(&self) -> DockSnapshot {
        let cfg = self.config.lock().unwrap();
        let apps = self.running.lock().unwrap();
        let order = self.transient_order.lock().unwrap();
        let icons = self.icons.lock().unwrap();
        let running_keys: std::collections::HashSet<&str> =
            apps.iter().map(|a| a.key.as_str()).collect();

        let mut pinned = Vec::new();
        let mut free = Vec::new();
        let mut pinned_keys = Vec::new();
        for s in &cfg.shortcuts {
            let key = running::key_for_path(&s.path);
            if s.mode == IconMode::Free {
                free.push(entry(
                    s,
                    key.clone(),
                    &icons,
                    running_keys.contains(key.as_str()),
                    true,
                ));
                continue;
            }
            pinned_keys.push(key.clone());
            pinned.push(entry(
                s,
                key.clone(),
                &icons,
                running_keys.contains(key.as_str()),
                true,
            ));
        }

        // 渲染顺序 = 稳定顺序，且指纹用的就是这份 all_keys（修复 #1 的服务端一半）。
        let mut transients = Vec::new();
        let mut all_keys = pinned_keys.clone();
        for key in order.iter() {
            if pinned_keys.contains(key) {
                continue;
            }
            if let Some(a) = apps.iter().find(|a| &a.key == key) {
                all_keys.push(key.clone());
                transients.push(transient_entry(a, &icons));
            }
        }
        // 兜底：首帧在 watcher 同步顺序之前就有人调 dock_state 时，按扫描顺序补齐，
        // 不让图标凭空消失。正常运行下这个循环不会命中。
        for a in apps.iter() {
            if all_keys.contains(&a.key) {
                continue;
            }
            all_keys.push(a.key.clone());
            transients.push(transient_entry(a, &icons));
        }

        DockSnapshot {
            pinned,
            transients,
            free,
            auto_hide: cfg.auto_hide,
            fingerprint: fingerprint(&all_keys),
        }
    }

    /// 按 key 找可启动路径：先查固定项，再查当前运行集合。
    fn path_for_key(&self, key: &str) -> Option<String> {
        if let Some(s) = self
            .config
            .lock()
            .unwrap()
            .shortcuts
            .iter()
            .find(|s| running::key_for_path(&s.path) == key)
        {
            return Some(s.path.clone());
        }
        self.running
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.key == key)
            .map(|a| a.path.clone())
    }
}

fn entry(
    s: &Shortcut,
    key: String,
    icons: &HashMap<String, String>,
    running: bool,
    pinned: bool,
) -> DockEntry {
    DockEntry {
        id: s.id.clone(),
        icon: icons
            .get(&key)
            .cloned()
            .or_else(|| s.icon.as_ref().map(|i| i.data_url.clone())),
        key,
        name: s.name.clone(),
        path: s.path.clone(),
        running,
        pinned,
        free_rect: s.free_rect.clone(),
    }
}

/// 启动 watcher。集合没变就完全不 emit（修复 #1 的服务端一半）。
pub fn spawn(app: AppHandle, dock: Arc<DockState>) {
    std::thread::Builder::new()
        .name("glasssuite-dock-watch".into())
        .spawn(move || loop {
            std::thread::sleep(POLL_INTERVAL);
            // None = 扫描超时，保持上一帧，绝不把图标清空。
            let Some(apps) = running::scan_apps_bounded() else {
                continue;
            };
            dock.set_running(apps);
            // 指纹取自快照本身：与前端拿到的渲染顺序逐字一致，
            // 不再拿未排序的枚举顺序另算一份（那样 z-order 一抖就误判成内容变化）。
            let snap = dock.snapshot();
            {
                let mut last = dock.last_fingerprint.lock().unwrap();
                if *last == snap.fingerprint {
                    continue;
                }
                *last = snap.fingerprint.clone();
            }
            let _ = app.emit(RUNNING_EVENT, snap);
            winplat::log::log("dock: running changed count=1");
        })
        .ok();
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn dock_state(dock: State<'_, Arc<DockState>>) -> DockSnapshot {
    dock.snapshot()
}

/// 异步取图标。前端对每个 key 调一次，失败返回 null，UI 用占位图标顶上。
#[tauri::command]
pub fn dock_icon(key: String, dock: State<'_, Arc<DockState>>) -> Option<String> {
    if let Some(cached) = dock.icons.lock().unwrap().get(&key).cloned() {
        return Some(cached);
    }
    let path = dock.path_for_key(&key)?;
    let icon = icons::extract_bounded(&path)?;
    dock.icons
        .lock()
        .unwrap()
        .insert(key, icon.data_url.clone());
    Some(icon.data_url)
}

#[tauri::command]
pub fn dock_launch(key: String, dock: State<'_, Arc<DockState>>) -> bool {
    let Some(path) = dock.path_for_key(&key) else {
        return false;
    };
    let ok = menu::launch_bounded(&path);
    winplat::log::log(&format!("dock: launch result={} count=1", u8::from(ok)));
    ok
}

#[tauri::command]
pub fn dock_launch_path(path: String) -> bool {
    menu::launch_bounded(&path)
}

/// 拖拽排序后持久化顺序（B5：重启后顺序保持）。
#[tauri::command]
pub fn dock_reorder(
    app: AppHandle,
    paths: Vec<String>,
    dock: State<'_, Arc<DockState>>,
) -> Result<(), String> {
    {
        let mut cfg = dock.config.lock().unwrap();
        config::reorder(&mut cfg, &paths);
        config::save_dock(&dock.data_dir, &cfg)?;
    }
    // 顺序变了指纹就变，前端会重排；不 emit 的话要等下一次运行集合变化才生效。
    let _ = app.emit(RUNNING_EVENT, dock.snapshot());
    Ok(())
}

#[tauri::command]
pub fn dock_auto_hide(
    app: AppHandle,
    enabled: bool,
    dock: State<'_, Arc<DockState>>,
) -> Result<(), String> {
    {
        let mut cfg = dock.config.lock().unwrap();
        cfg.auto_hide = enabled;
        config::save_dock(&dock.data_dir, &cfg)?;
    }
    // 指纹不变但 autoHide 变了：前端 install 会比对 autoHide，照样更新。
    let _ = app.emit(RUNNING_EVENT, dock.snapshot());
    Ok(())
}

/// 贴底唤出 / 收起：由前端监听 `dock-reveal` 后调用。
/// 收起时必须同时开穿透，否则隐藏的药丸仍会吃掉桌面点击。
#[tauri::command]
pub fn dock_set_revealed(
    window: WebviewWindow,
    shared: State<'_, Arc<SharedState>>,
    revealed: bool,
) {
    let _ = window.set_ignore_cursor_events(!revealed);
    shared
        .dock_transparent
        .store(!revealed, std::sync::atomic::Ordering::Relaxed);
}

/// 把固定项拖成游离图标（屏幕 CSS 坐标）。
#[tauri::command]
pub fn dock_set_free(
    app: AppHandle,
    dock: State<'_, Arc<DockState>>,
    id: String,
    x: i32,
    y: i32,
) -> Result<(), String> {
    {
        let mut cfg = dock.config.lock().unwrap();
        let Some(s) = cfg.shortcuts.iter_mut().find(|s| s.id == id) else {
            return Err("找不到该快捷方式".to_string());
        };
        s.mode = IconMode::Free;
        s.free_rect = Some(config::FreeRect {
            x,
            y,
            w: free::FREE_W as u32,
            h: free::FREE_H as u32,
        });
        config::save_dock(&dock.data_dir, &cfg)?;
    }
    let cfg = dock.config.lock().unwrap().clone();
    free::sync_windows(&app, &cfg)?;
    let _ = app.emit(RUNNING_EVENT, dock.snapshot());
    Ok(())
}

/// 拖回 dock（取消游离）。
#[tauri::command]
pub fn dock_undock_free(
    app: AppHandle,
    dock: State<'_, Arc<DockState>>,
    id: String,
) -> Result<(), String> {
    {
        let mut cfg = dock.config.lock().unwrap();
        let Some(s) = cfg.shortcuts.iter_mut().find(|s| s.id == id) else {
            return Err("找不到该快捷方式".to_string());
        };
        s.mode = IconMode::Dock;
        s.free_rect = None;
        config::save_dock(&dock.data_dir, &cfg)?;
    }
    let cfg = dock.config.lock().unwrap().clone();
    free::sync_windows(&app, &cfg)?;
    let _ = app.emit(RUNNING_EVENT, dock.snapshot());
    Ok(())
}

#[tauri::command]
pub fn dock_move_free(
    app: AppHandle,
    dock: State<'_, Arc<DockState>>,
    id: String,
    x: i32,
    y: i32,
) -> Result<(), String> {
    let cfg = {
        let mut cfg = dock.config.lock().unwrap();
        let Some(s) = cfg.shortcuts.iter_mut().find(|s| s.id == id) else {
            return Err("找不到该快捷方式".to_string());
        };
        if let Some(r) = s.free_rect.as_mut() {
            r.x = x;
            r.y = y;
        }
        config::save_dock(&dock.data_dir, &cfg)?;
        cfg.clone()
    };
    free::sync_windows(&app, &cfg)
}

// ---------------------------------------------------------------------------
// 开始菜单面板
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn dock_menu_apps() -> Vec<menu::MenuApp> {
    menu::list_apps_bounded()
}

#[tauri::command]
pub fn dock_menu_favourites(dock: State<'_, Arc<DockState>>) -> Vec<config::MenuEntry> {
    dock.menu_config.lock().unwrap().apps.clone()
}

#[tauri::command]
pub fn dock_menu_toggle(app: AppHandle, open: Option<bool>) -> Result<bool, String> {
    let win = app
        .get_webview_window("startmenu")
        .ok_or_else(|| "startmenu 窗口不存在".to_string())?;
    let currently = win.is_visible().unwrap_or(false);
    let next = open.unwrap_or(!currently);
    if next == currently {
        return Ok(next);
    }
    if next {
        win.show().map_err(|e| e.to_string())?;
        #[cfg(windows)]
        if let Ok(h) = win.hwnd() {
            let raw = h.0 as isize;
            let _ = winplat::style::hide_from_alt_tab(raw);
            let _ = winplat::style::set_no_activate(raw, true);
        }
    } else {
        win.hide().map_err(|e| e.to_string())?;
    }
    let _ = app.emit(MENU_EVENT, next);
    winplat::log::log(&format!("dock: menu open={next} count=1"));
    Ok(next)
}

// ---------------------------------------------------------------------------
// DWM 预览
// ---------------------------------------------------------------------------

/// `x`/`y` 是前端 `MouseEvent.screenX/screenY`（屏幕 CSS 像素），`w`/`h` 是期望预览尺寸。
#[tauri::command]
pub fn dock_preview_show(
    app: AppHandle,
    dock: State<'_, Arc<DockState>>,
    key: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> bool {
    let source = dock
        .running
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.key == key)
        .map(|a| a.hwnd)
        .unwrap_or(0);
    let win = match app.get_webview_window("preview") {
        Some(w) => w,
        None => return false,
    };
    let pw = w.clamp(1.0, PREVIEW_W);
    let ph = h.clamp(1.0, PREVIEW_H);
    let px = x - pw / 2.0;
    let py = y - ph - 8.0;
    if let Err(e) = crate::shell::place_at(&win, px, py, pw, ph) {
        winplat::log::log(&format!(
            "dock: preview place failed, degraded count=1: {e}"
        ));
        return false;
    }
    let _ = win.show();
    #[cfg(windows)]
    let dest = win.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    #[cfg(not(windows))]
    let dest = 0;
    let scale = win.scale_factor().unwrap_or(1.0);
    let rect = preview::PreviewRect {
        x: 0.0,
        y: 0.0,
        w: pw,
        h: ph,
    };
    let ok = dock.preview.show(dest, source, rect, scale);
    let _ = app.emit(PREVIEW_FALLBACK_EVENT, if ok { "" } else { key.as_str() });
    ok
}

#[tauri::command]
pub fn dock_preview_hide(app: AppHandle, dock: State<'_, Arc<DockState>>) {
    dock.preview.hide();
    if let Some(win) = app.get_webview_window("preview") {
        let _ = win.hide();
    }
    let _ = app.emit(PREVIEW_FALLBACK_EVENT, "");
}

// ---------------------------------------------------------------------------
// 启动编排
// ---------------------------------------------------------------------------

/// setup 内调用：同步游离窗口 → 起 watcher。
///
/// **调用顺序是硬约束**：`config::migrate_legacy` 必须在 `DockState::new` 之前跑完。
/// `load_dock` 发现目标文件不存在时会立刻写一份种子 `dock.json`，占住迁移目标；
/// 那样旧配置会被 `migrate_file` 的「不覆盖新配置」规则跳过，B8 的顺序/收藏就丢了。
/// 见 `lib.rs` setup 里的显式前置调用。
pub fn init(app: &AppHandle, dock: &Arc<DockState>) {
    // 开始菜单是第三个窗口，但**首屏隐藏**：只有用户点 dock 上的按钮才 show，
    // 不让一个空面板常驻。窗口尺寸 = 面板尺寸，不是全屏透明层 + 热区。
    match crate::shell::ensure_window(
        app,
        "startmenu",
        "dock/index.html#startmenu",
        MENU_W,
        MENU_H,
    ) {
        Ok(win) => {
            if let Err(e) =
                crate::shell::place_above_dock(&win, MENU_W, MENU_H, crate::shell::STARTUP_DOCK.1)
            {
                winplat::log::log(&format!("dock: menu place failed, degraded count=1: {e}"));
            }
        }
        Err(e) => winplat::log::log(&format!("dock: menu window failed, degraded count=1: {e}")),
    }

    // DWM 缩略图承载窗：与预览等大、首屏隐藏，只在悬停时 show。
    if let Err(e) = crate::shell::ensure_window(
        app,
        "preview",
        "dock/index.html#preview",
        PREVIEW_W,
        PREVIEW_H,
    ) {
        winplat::log::log(&format!(
            "dock: preview window failed, degraded count=1: {e}"
        ));
    }

    let cfg = dock.config.lock().unwrap().clone();
    if let Err(e) = free::sync_windows(app, &cfg) {
        winplat::log::log(&format!("dock: free sync failed, degraded count=1: {e}"));
    }
    spawn(app.clone(), Arc::clone(dock));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_tracks_order_and_membership() {
        let a = vec!["exe:b".to_string(), "exe:a".to_string()];
        let b = vec!["exe:a".to_string(), "exe:b".to_string()];
        assert_ne!(fingerprint(&a), fingerprint(&b), "拖拽排序必须改变指纹");
        assert_eq!(fingerprint(&a), "exe:b|exe:a");
        assert_ne!(fingerprint(&a), fingerprint(&a[..1]));
    }

    /// B4 回归：新开程序不能把已有临时图标往后推，否则 FLIP 会量出 -50 的 dx 并播跳动动画。
    #[test]
    fn transient_order_appends_new_keys_without_moving_existing() {
        let mut order = vec!["exe:b".to_string(), "exe:c".to_string()];
        let after_open = vec![
            "exe:a".to_string(),
            "exe:c".to_string(),
            "exe:b".to_string(),
        ];
        merge_transient_order(&mut order, &after_open);
        assert_eq!(
            order,
            vec![
                "exe:b".to_string(),
                "exe:c".to_string(),
                "exe:a".to_string()
            ],
            "新 key 只能追加到末尾，且枚举顺序抖动不得改变既有顺序"
        );
    }

    #[test]
    fn transient_order_drops_closed_apps_and_keeps_the_rest() {
        let mut order = vec![
            "exe:a".to_string(),
            "exe:b".to_string(),
            "exe:c".to_string(),
        ];
        merge_transient_order(&mut order, &["exe:c".to_string(), "exe:a".to_string()]);
        assert_eq!(order, vec!["exe:a".to_string(), "exe:c".to_string()]);
        assert_eq!(fingerprint(&order), "exe:a|exe:c");
    }

    #[test]
    fn name_falls_back_for_odd_paths() {
        assert_eq!(name_from_path(r"C:\Apps\Notepad.exe"), "Notepad");
        assert_eq!(name_from_path(""), "应用");
    }
}

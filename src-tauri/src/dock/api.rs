//! dock 命令层 —— 从 glassdesk `src-tauri/src/lib.rs` 移植的全部 `#[tauri::command]`。
//! 只改模块路径（dock:: → crate::dock::、menu:: → crate::dock::menu:: 等），逻辑零改动。

use tauri::Manager;

use super::free;
use super::menu;
use super::preview;
use super::running;

/// dock 首帧就绪上报：WebView2 内容完成首次绘制后前端调用一次。
/// Rust 侧在收到之前**不显示** dock —— 这是首帧黑闪的根因修复点。
#[tauri::command]
pub fn dock_ready() {
    crate::dock::mark_ready();
}

/// 任务 A：与 dock 快捷方式匹配的「运行中」图标 id 列表（前端 ~1s 轮询）。
/// 匹配在 Rust 侧完成：运行 key 集合（exe 路径 / AUMID）对 shortcut 逐条判，
/// 添加/移除图标不涉及 watcher 状态，天然不串位。
#[tauri::command]
pub fn get_running_ids(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let Some(p) = crate::dock::config_path(&app) else {
        return Err("无法定位配置目录".into());
    };
    let cfg = crate::dock::load_config(&p);
    let snap = running::snapshot();
    Ok(cfg
        .shortcuts
        .iter()
        .filter(|s| running::is_running(&snap, &s.aumid, &s.path))
        .map(|s| s.id.clone())
        .collect())
}

#[tauri::command]
pub fn list_shortcuts(app: tauri::AppHandle) -> Result<Vec<crate::dock::Shortcut>, String> {
    let Some(p) = crate::dock::config_path(&app) else {
        return Err("无法定位配置目录".into());
    };
    let mut cfg = crate::dock::load_config(&p);
    if cfg.shortcuts.is_empty() {
        // 首次启动：预置系统常见程序
        cfg.shortcuts = crate::dock::default_shortcuts();
        let _ = crate::dock::save_config(&p, &cfg);
    }
    Ok(cfg.shortcuts)
}

#[tauri::command]
pub fn add_shortcut(
    app: tauri::AppHandle,
    window: tauri::Window,
) -> Result<Vec<crate::dock::Shortcut>, String> {
    let Some(p) = crate::dock::config_path(&app) else {
        return Err("无法定位配置目录".into());
    };
    // v0.7.1（sir 反馈「添加用不了」）：dock 是桌面层窗口（D7），点 ⊕ 时它通常
    // 不在前台——GetOpenFileNameW 的模态对话框依附调用线程前台窗口，会弹在
    // 当前前台应用后面被完全盖住，表现为「点了没反应」。先把 dock 拉前台。
    if window.set_focus().is_err() {
        let _ = window.show();
        let _ = window.set_focus();
    }
    let mut cfg = crate::dock::load_config(&p);
    if let Some(path) = crate::dock::pick_program() {
        if !cfg.shortcuts.iter().any(|s| s.path == path) {
            cfg.shortcuts.push(crate::dock::build_shortcut(&path));
            crate::dock::save_config(&p, &cfg)?;
        }
    }
    Ok(cfg.shortcuts)
}

#[tauri::command]
pub fn remove_shortcut(
    app: tauri::AppHandle,
    id: String,
) -> Result<Vec<crate::dock::Shortcut>, String> {
    let Some(p) = crate::dock::config_path(&app) else {
        return Err("无法定位配置目录".into());
    };
    let mut cfg = crate::dock::load_config(&p);
    cfg.shortcuts.retain(|s| s.id != id);
    crate::dock::save_config(&p, &cfg)?;
    Ok(cfg.shortcuts)
}

/// v0.5.1（sir 反馈）：dock 图标拖动排序——按 path 顺序重排 dock.json 并持久化。
/// paths 里没提到的条目（理论上不会有）按原顺序追加在尾部，不丢数据。
#[tauri::command]
pub fn reorder_shortcuts(
    app: tauri::AppHandle,
    paths: Vec<String>,
) -> Result<Vec<crate::dock::Shortcut>, String> {
    let Some(p) = crate::dock::config_path(&app) else {
        return Err("无法定位配置目录".into());
    };
    let mut cfg = crate::dock::load_config(&p);
    let mut reordered: Vec<crate::dock::Shortcut> = Vec::with_capacity(cfg.shortcuts.len());
    for want in &paths {
        if let Some(pos) = cfg.shortcuts.iter().position(|s| &s.path == want) {
            reordered.push(cfg.shortcuts.remove(pos));
        }
    }
    reordered.append(&mut cfg.shortcuts);
    cfg.shortcuts = reordered;
    crate::dock::save_config(&p, &cfg)?;
    Ok(cfg.shortcuts)
}

#[tauri::command]
pub fn launch_shortcut(app: tauri::AppHandle, path: String) -> Result<(), String> {
    // 已运行应用走激活快路径（同 running 的 key 体系）；未运行才真的启动。
    // v0.6：反查走归一化（小写 + / → \）——前端传回大小写/分隔符不同的 path
    // 时不再把 UWP 应用当 Win32 启动（v0.4-a verdict advisory）。
    let aumid = crate::dock::config_path(&app)
        .map(|p| crate::dock::load_config(&p))
        .and_then(|cfg| {
            crate::dock::find_shortcut_by_path(&cfg, &path).and_then(|s| s.aumid.clone())
        });
    crate::dock::activate_or_launch(&aumid, &path)
}

/// 任务 B：悬停预览显示（前端悬停图标 ≥300ms 后调用）。
/// id = 快捷方式 id；x/y/w/h = 图标按钮矩形（CSS px）。
/// 应用未运行 / dock 隐藏时不弹（无源窗口可预览）。
#[tauri::command]
pub fn preview_show(
    app: tauri::AppHandle,
    id: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    preview::show_for(&app, &id, x, y, w, h)
}

/// 任务 B：销毁预览（鼠标移开 / 点击图标 / dock 隐藏）。
#[tauri::command]
pub fn preview_hide() -> Result<(), String> {
    preview::hide();
    Ok(())
}

/// 任务 C：开始菜单面板 toggle（⊞ 点击）。可见 → 收回；隐藏 → 定位 + 展开 + 聚焦。
#[tauri::command]
pub fn menu_toggle(app: tauri::AppHandle) -> Result<(), String> {
    menu::toggle(&app);
    Ok(())
}

/// 任务 C：显式收回面板（Esc / 图标点击启动后）。
#[tauri::command]
pub fn menu_hide(app: tauri::AppHandle) -> Result<(), String> {
    menu::hide(&app);
    Ok(())
}

/// 任务 C：面板条目 + 图标快照（前端 400ms 轮询直至 ready && icons_done）。
#[tauri::command]
pub fn get_menu_apps() -> Result<menu::MenuSnapshot, String> {
    Ok(menu::snapshot())
}

/// 任务 C（sir 反馈迭代）：添加收藏 → 返回最新 pinned path 列表。
#[tauri::command]
pub fn menu_add(path: String) -> Result<Vec<String>, String> {
    menu::add_pinned(&path)
}

/// 任务 C（sir 反馈迭代）：移除收藏 → 返回最新 pinned path 列表。
#[tauri::command]
pub fn menu_remove(path: String) -> Result<Vec<String>, String> {
    menu::remove_pinned(&path)
}

/// 任务 C（sir 反馈迭代）：面板内启动。失败返回 Err（前端不关面板，日志可见）。
#[tauri::command]
pub fn menu_launch(path: String) -> Result<(), String> {
    menu::launch_from_menu(&path)
}

/// 拖拽定位临时通道（前端关键事件 → menu.log）。
#[tauri::command]
pub fn debug_note(msg: String) -> Result<(), String> {
    menu::debug_note(&msg);
    Ok(())
}

/// v0.5：电源菜单——锁定。
#[tauri::command]
pub fn power_lock() -> Result<(), String> {
    menu::power_lock()
}

/// v0.5：电源菜单——睡眠（系统开休眠时可能进休眠，交付单已注明）。
#[tauri::command]
pub fn power_sleep() -> Result<(), String> {
    menu::power_sleep()
}

/// v0.5：电源菜单——关机 / 重启（UI 层二次确认后才许调用）。
#[tauri::command]
pub fn power_shutdown(reboot: bool) -> Result<(), String> {
    menu::power_shutdown(reboot)
}

/// 前端上报药丸实际尺寸，窗口收缩到内容大小——保证点击区域与可见区域一致。
///
/// v0.8.1（sir 2026-11-08 反馈「图标一连串跳动」）：resize 与水平重定位**一气完成**。
/// 旧实现只 set_size：Win32 resize 固定左上角、向右下扩展，dock 定位循环（POLL_MS
/// 节拍）晚一拍把窗口拉回「屏幕水平居中」——药丸宽度 ±58px 时（临时运行图标
/// 出现/消失），既有图标先随药丸重排移 −Δ/2、再随窗口右扩移 +Δ/2、再被定位循环
/// 反向移 Δ/2，三段位移各自触发一次 FLIP 动画 = 观感上的「一连串跳动」。
/// 现在「新窗口中心 = 屏幕中心」同步重定位 → 图标相对窗口纹丝不动，只是药丸
/// 两端对称伸缩。y 轴仍归定位循环管（任务栏几何只有循环那边知道）。
///
/// 坐标系（评审重点）：`w`/`h` 是前端 CSS 逻辑像素；`monitor.position()/size()` 与
/// `outer_position()` 都是物理像素。因此统一在**物理像素**域算 x，只用 scale 换算
/// 宽度，不受缩放比例与多显示器原点偏移影响（`monitor.position().x` 参与计算，
/// 副屏原点非 0 时不会把 dock 拉回主屏）。y 原样透传物理值，不来回换算。
#[tauri::command]
pub fn fit_dock(window: tauri::WebviewWindow, w: f64, h: f64) -> Result<(), String> {
    use tauri::{LogicalSize, PhysicalPosition};
    let w = w.max(120.0);
    let h = h.max(48.0);
    let scale = window.scale_factor().unwrap_or(1.0);
    if let Ok(Some(monitor)) = window.current_monitor() {
        let mw = monitor.size().width as f64;
        let w_phys = w * scale;
        if mw > w_phys {
            let x = monitor.position().x as f64 + (mw - w_phys) / 2.0;
            if let Ok(pos) = window.outer_position() {
                let _ = window.set_position(PhysicalPosition::new(x, pos.y as f64));
            }
        }
    }
    window
        .set_size(LogicalSize::new(w, h))
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// v0.7 Phase 1（OC-20261102-002）：dock 图标拖出成游离图标
// ---------------------------------------------------------------------------

/// 拖出阈值（前端与探针共用同一套口径——阈值只在 Rust 定义一处）。
#[derive(serde::Serialize)]
pub struct DragThresholds {
    min_px: f64,
    min_ms: u64,
    max_free: usize,
}

#[tauri::command]
pub fn drag_thresholds() -> DragThresholds {
    DragThresholds {
        min_px: crate::dock::DRAG_OUT_MIN_PX,
        min_ms: crate::dock::DRAG_OUT_MIN_MS,
        max_free: free::max_free_icons(),
    }
}

/// 配置版本号——前端据此判断「列表变了」并重拉（拖出/拖回都会 bump）。
#[tauri::command]
pub fn dock_config_version() -> u64 {
    crate::dock::config_version()
}

/// 拖出成游离图标：cx/cy = dock 客户区 CSS 坐标（落点）。
#[tauri::command]
pub fn free_drag_out(app: tauri::AppHandle, id: String, cx: f64, cy: f64) -> Result<(), String> {
    free::drag_out(&app, &id, cx, cy)
}

/// 游离图标双击/单击 → 启动或激活（复用 dock::activate_or_launch 的快路径）。
#[tauri::command]
pub fn free_launch(app: tauri::AppHandle, id: String) -> Result<(), String> {
    free::launch(&app, &id)
}

/// 游离图标右键 → 收回 dock。
#[tauri::command]
pub fn free_return(app: tauri::AppHandle, id: String) -> Result<(), String> {
    free::return_to_dock(&app, &id, None)
}

/// 游离图标拖动结束、落在 dock 区 → 前端回调（带落点槽位）。
#[tauri::command]
pub fn free_drop_result(
    app: tauri::AppHandle,
    id: String,
    slot: Option<usize>,
) -> Result<(), String> {
    free::return_to_dock(&app, &id, slot)
}

/// 游离窗口首帧就绪 → 显示（避免 WebView2 未画完的黑闪）。
#[tauri::command]
pub fn free_ready(app: tauri::AppHandle, id: String) -> Result<(), String> {
    free::show_free(&app, &free::label_for(&id));
    Ok(())
}

/// 游离图标拖动：**交接给系统原生拖动**（WM_NCLBUTTONDOWN/HTCAPTION）。
///
/// 前端只负责「越过 4px 阈值时交接一次」，窗口移动全程由系统的模态移动循环接管
/// （卡面 §1.3-1：不许用前端 pointer capture 拖窗口）。
#[tauri::command]
pub fn free_begin_drag(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let label = free::label_for(&id);
    let win = app
        .get_webview_window(&label)
        .ok_or_else(|| "游离窗口不存在".to_string())?;
    win.start_dragging().map_err(|e| e.to_string())
}

/// 游离窗口清单（自检/探针用）。
#[tauri::command]
pub fn free_status() -> Vec<free::FreeStatus> {
    free::status()
}

/// v0.8（OC-20261104-001）：未固定的运行中应用（临时图标）。
/// 复用 running.rs watcher 快照（无新轮询线程）；dock.json/menu.json 已固定的排除。
#[tauri::command]
pub fn get_transient_running(
    app: tauri::AppHandle,
) -> Result<Vec<crate::dock::TransientApp>, String> {
    let Some(p) = crate::dock::config_path(&app) else {
        return Err("无法定位配置目录".into());
    };
    let cfg = crate::dock::load_config(&p);
    let menu_paths = menu::all_pinned_paths();
    let snap = running::snapshot();
    Ok(crate::dock::transient_apps(&snap, &cfg, &menu_paths))
}

/// v0.8：右键临时图标 →「固定到 dock」（atomic_write + bump 版本）。
#[tauri::command]
pub fn pin_transient(app: tauri::AppHandle, key: String) -> Result<crate::dock::Shortcut, String> {
    let Some(p) = crate::dock::config_path(&app) else {
        return Err("无法定位配置目录".into());
    };
    crate::dock::pin_transient_key(&p, &key)
}

/// v0.8：单击临时图标 → 激活该应用（find_window_for_key 快路径）。
#[tauri::command]
pub fn transient_activate(key: String, path: String, aumid: Option<String>) -> Result<(), String> {
    let _ = key; // key 主要供前端标识；激活走与固定条目同一条 activate_or_launch 通路
    crate::dock::transient_activate(&aumid, &path)
}

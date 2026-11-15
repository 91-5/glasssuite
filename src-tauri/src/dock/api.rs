//! dock 命令层 —— 从 glassdesk `src-tauri/src/lib.rs` 移植的全部 `#[tauri::command]`。
//! 只改模块路径（dock:: → crate::dock::、menu:: → crate::dock::menu:: 等），逻辑零改动。

use std::time::{Duration, Instant};

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

/// 运行标识统一快照（OC-20261115-006：Rust 单一事实源）。
/// 事件 `dock-running` 推同一结构；本命令是前端的低频兜底轮询口。
/// （取代旧 `get_running_ids` + `get_transient_running` + `dock_config_version`
/// 三条轮询——缓存命中时纯内存，不再每 500ms 读盘两遍。）
#[tauri::command]
pub fn dock_running_snapshot() -> crate::dock::running::RunningSnapshot {
    running::running_snapshot()
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
    // OC-20261115-002 D5：右键移除此前是「点了没反应也不知道为什么」——
    // 成功/失败都必须落 menu.log，失败还要让前端弹 notice。
    let Some(p) = crate::dock::config_path(&app) else {
        let e = "无法定位配置目录".to_string();
        menu::debug_note(&format!("dock: remove {id} err={e}"));
        return Err(e);
    };
    let mut cfg = crate::dock::load_config(&p);
    cfg.shortcuts.retain(|s| s.id != id);
    match crate::dock::save_config(&p, &cfg) {
        Ok(()) => {
            menu::debug_note(&format!("dock: remove {id} ok"));
            Ok(cfg.shortcuts)
        }
        Err(e) => {
            menu::debug_note(&format!("dock: remove {id} err={e}"));
            Err(e)
        }
    }
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

/// 启动/激活快捷方式。**async + spawn_blocking（OC-20261115-005）**：
/// 反查 aumid 要读盘解析 dock.json、`find_window_for_key` 要 EnumWindows 全量枚举、
/// 未运行时 `ShellExecuteW` 可能被 LNK 解析/杀毒扫描挂数百 ms——这些阻塞调用
/// 此前都在 Tauri 主线程（约束 14 违例，点击期间整条事件循环停摆）。现在整段
/// 搬到阻塞线程池，主线程只负责 await。
#[tauri::command]
pub async fn launch_shortcut(app: tauri::AppHandle, path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || launch_shortcut_blocking(&app, &path))
        .await
        .map_err(|e| format!("launch task join failed: {e}"))?
}

/// launch_shortcut 的阻塞体（独立出来便于复用）。
fn launch_shortcut_blocking(app: &tauri::AppHandle, path: &str) -> Result<(), String> {
    // 已运行应用走激活快路径（同 running 的 key 体系）；未运行才真的启动。
    // v0.6：反查走归一化（小写 + / → \）——前端传回大小写/分隔符不同的 path
    // 时不再把 UWP 应用当 Win32 启动（v0.4-a verdict advisory）。
    let aumid = crate::dock::config_path(app)
        .map(|p| crate::dock::load_config(&p))
        .and_then(|cfg| {
            crate::dock::find_shortcut_by_path(&cfg, path).and_then(|s| s.aumid.clone())
        });
    crate::dock::activate_or_launch(&aumid, path)
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

/// 007：悬停**运行中未固定**图标 → 按 key 出多窗口预览（应用名/图标取目录缓存）。
#[tauri::command]
pub fn preview_show_transient(
    app: tauri::AppHandle,
    key: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    preview::show_for_transient(&app, &key, x, y, w, h)
}

/// 007 链 A/B 修复：前端 mouseleave 去重计时（150ms）后询问「光标还在预览窗
/// 或 dock 内吗」——在则继续等，否则才真正销毁预览。
///
/// 背景：预览窗是独立顶层窗，浮在 dock 上方（6px 间隙）。鼠标从图标往预览窗移动时，
/// 图标必然先触发 mouseleave → 前端无条件 preview_hide → 预览当场销毁，
/// 鼠标落到预览窗后面的窗口上（007 链 A）。另外 running-snapshot 推送导致图标元素
/// 重挂载时，旧元素会补发 mouseleave，鼠标完全不动时预览也会在 ~2.6s 后被销毁
/// （链 B）。本命令把「销毁预览」改为「延迟确认 + 光标实况裁决」。
#[tauri::command]
pub fn preview_cursor_inside(app: tauri::AppHandle) -> bool {
    let pt = unsafe {
        use windows::Win32::Foundation::POINT;
        let mut pt = POINT { x: 0, y: 0 };
        if windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt).is_err() {
            return true; // 查询失败：保守保留预览（dock 隐藏时有自己的级联销毁）
        }
        pt
    };
    // 1) 光标在预览窗内 → 保留
    if let Some(r) = preview::current_rect() {
        if pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom {
            return true;
        }
    }
    // 2) 光标在 dock 窗口内 → 保留（链 B 的重挂载 mouseleave 场景）
    if let Some(dock) = app.get_webview_window("dock") {
        if let (Ok(pos), Ok(size)) = (dock.outer_position(), dock.outer_size()) {
            if pt.x >= pos.x
                && pt.x < pos.x + size.width as i32
                && pt.y >= pos.y
                && pt.y < pos.y + size.height as i32
            {
                return true;
            }
        }
    }
    false
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
/// 启动开始菜单快捷方式（点击路径，005 非阻塞化：ShellExecute 在阻塞线程池）。
#[tauri::command]
pub async fn menu_launch(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || menu::launch_from_menu(&path))
        .await
        .map_err(|e| format!("menu launch task join failed: {e}"))?
}

/// 拖拽定位临时通道（前端关键事件 → menu.log）。
#[tauri::command]
pub fn debug_note(msg: String) -> Result<(), String> {
    menu::debug_note(&msg);
    Ok(())
}

/// OC-20261115-002 D4：一次按压手势开始。锁期间 dock_logic 不定位/不显隐，
/// fit_dock 请求排队。锁自带 600ms 超时，前端漏发 end 也不会死锁。
#[tauri::command]
pub fn dock_interaction_begin() -> Result<(), String> {
    crate::dock::interaction_begin();
    Ok(())
}

/// 一次按压手势结束：解锁并立刻补执行排队的 fit_dock。
#[tauri::command]
pub fn dock_interaction_end(app: tauri::AppHandle) -> Result<(), String> {
    crate::dock::interaction_end();
    crate::dock::flush_pending_fit(&app);
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
pub fn fit_dock(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    w: f64,
    h: f64,
) -> Result<(), String> {
    // OC-20261115-002 D4：按压手势期间绝不改窗口几何。请求排队，
    // 由 dock_interaction_end（松手）或 600ms 超时兜底线程补执行，≤600ms 落地。
    if crate::dock::interaction_locked() {
        crate::dock::queue_fit(w, h);
        menu::debug_note(&format!("dock: fit_dock queued w={w} h={h}"));
        let handle = app.clone();
        std::thread::spawn(move || {
            let deadline =
                Instant::now() + crate::dock::INTERACTION_TIMEOUT + Duration::from_millis(50);
            while crate::dock::interaction_locked() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            crate::dock::flush_pending_fit(&handle);
        });
        return Ok(());
    }
    apply_fit_dock(&window, w, h)
}

/// 真正的几何写入（主线程执行）。锁期间只允许经 flush 走到这里。
pub(crate) fn apply_fit_dock(window: &tauri::WebviewWindow, w: f64, h: f64) -> Result<(), String> {
    use tauri::{LogicalSize, PhysicalPosition};
    let w = w.max(120.0);
    let h = h.max(48.0);
    // 2026-11-16 修复（dock 整体不可见，OC-20261116-001）：旧实现在这里用
    // `window.current_monitor()` 算水平居中 x，而窗口的「当前显示器」取决于它此刻
    // 挂在哪块屏上——主屏任务栏被外部守卫藏住时 dock 被摆到两块屏之间的无人区，
    // `current_monitor()` 报 DISPLAY2，于是这里又把 x 拉到副屏，与 dock/mod.rs 的
    // 定位循环互相拉扯。
    //
    // 现在：**位置只由 dock/mod.rs 的定位循环拥有**（单一真相源，见该文件定位段），
    // 本函数只负责尺寸——内容变了多大就写多大。
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

// dock_config_version 已并入 dock_running_snapshot（OC-20261115-006 统一快照）。
// get_transient_running 已并入 dock_running_snapshot（OC-20261115-006 统一快照）；
// dock-transient-capped 提示随快照推送并去重（见 running.rs push_snapshot）。
// 008：free_* 七条命令已删除（游离图标功能退役，见 008 各步骤提交）。

/// v0.8：右键临时图标 →「固定到 dock」（atomic_write + bump 版本）。
#[tauri::command]
pub fn pin_transient(app: tauri::AppHandle, key: String) -> Result<crate::dock::Shortcut, String> {
    let Some(p) = crate::dock::config_path(&app) else {
        return Err("无法定位配置目录".into());
    };
    crate::dock::pin_transient_key(&p, &key)
}

/// v0.8：单击临时图标 → 激活该应用（find_window_for_key 快路径）。
/// 激活运行中未固定应用（点击路径，005 非阻塞化同 launch_shortcut：窗口枚举 +
/// 前台切换 + 可能的 ShellExecute 兜底，全部在阻塞线程池跑）。
#[tauri::command]
pub async fn transient_activate(
    key: String,
    path: String,
    aumid: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _ = key; // key 主要供前端标识；激活走与固定条目同一条 activate_or_launch 通路
        crate::dock::transient_activate(&aumid, &path)
    })
    .await
    .map_err(|e| format!("transient activate task join failed: {e}"))?
}

#[cfg(test)]
mod launch_tests {
    // 刻意不用 `use super::*`：本模块全部走全路径，避免引入未消费的导入。

    /// A1：无效路径 → `Err` 不 panic（ShellExecuteW 失败被转换，不弹不炸）。
    /// 刻意不经过 AppHandle：launch() 是纯 Win32 调用，lib 测试二进制里
    /// 不引入 mock runtime（与 wry/tray 同链接会触发 DLL 入口点问题）。
    #[test]
    fn invalid_path_returns_err_not_panic() {
        let r = crate::dock::launch("C:\\__gs_missing__\\none.exe");
        assert!(r.is_err(), "无效路径应返回 Err");
    }

    /// A1：已在运行 → 走激活分支，**不再启新实例**（决策层注入：found=Some 时
    /// launch 闭包一次都不该被调）。
    #[test]
    fn already_running_activates_without_launch() {
        static LAUNCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let r =
            crate::dock::activate_or_launch_decide(Some((1234, false)), "C:\\anything.exe", |_| {
                LAUNCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            });
        assert!(r.is_ok());
        assert_eq!(
            LAUNCHES.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "已在运行不得再启新实例"
        );
    }

    /// A1：阻塞路径不触碰主线程状态——在任意工作线程直接完成（无 Tauri
    /// 主线程/事件循环依赖；这就是把它搬进 spawn_blocking 的可测形态）。
    #[test]
    fn blocking_path_completes_off_main_thread() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r = crate::dock::launch("C:\\__gs_missing__\\none.exe");
            let _ = tx.send(r.is_err());
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            "工作线程上应返回 Err 而非挂死"
        );
    }
}

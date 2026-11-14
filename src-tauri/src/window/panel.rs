//! 展开面板与全屏让位。
//!
//! 岛采用**固定窗口 + 热区上报**模型（移植自 glasspill）：折叠窗 420×210、展开窗 720×480，
//! 窗口本身不随内容伸缩，透明区靠 `cursor` 的热区判定穿透。收起时按 `ISLAND_SIZE` 复位。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::shell::{self, Anchor};
use crate::state::{Hotzone, SharedState};

/// 展开面板尺寸（逻辑像素）。必须与 `packages/ipc-types` 的 `PANEL_SIZE` 一致。
pub const PANEL_W: f64 = 720.0;
pub const PANEL_H: f64 = 480.0;

/// 与 `packages/ipc-types` 的 `EVENTS.panelState` 一致。
pub const PANEL_EVENT: &str = "panel-state";

#[tauri::command]
pub fn expand_panel(
    app: AppHandle,
    state: tauri::State<'_, Arc<SharedState>>,
) -> Result<(), String> {
    if state.island_expanded.load(Ordering::Relaxed) {
        winplat::log::log("panel: expand ignored (already expanded)");
        return Ok(());
    }
    state.island_expanded.store(true, Ordering::Relaxed);

    let win = app
        .get_webview_window("island")
        .ok_or_else(|| {
            winplat::log::log("panel: expand FAILED - island window not found");
            "island 窗口不存在".to_string()
        })?;
    if let Err(e) = shell::place(&win, Anchor::Island, PANEL_W, PANEL_H) {
        winplat::log::log(&format!("panel: expand place FAILED: {e}"));
        return Err(e);
    }

    // 展开面板必须可交互：取消穿透，并把热区扩到整个面板。
    let _ = win.set_ignore_cursor_events(false);
    state.island_transparent.store(false, Ordering::Relaxed);
    *state.island_hotzone.lock().unwrap() = Hotzone {
        x: 0.0,
        y: 0.0,
        w: PANEL_W,
        h: PANEL_H,
    };

    let _ = app.emit(PANEL_EVENT, true);
    winplat::log::log("panel: expanded");
    Ok(())
}

#[tauri::command]
pub fn collapse_panel(
    app: AppHandle,
    state: tauri::State<'_, Arc<SharedState>>,
) -> Result<(), String> {
    if !state.island_expanded.load(Ordering::Relaxed) {
        return Ok(());
    }
    state.island_expanded.store(false, Ordering::Relaxed);

    let (w, h) = shell::STARTUP_ISLAND;

    let win = app
        .get_webview_window("island")
        .ok_or_else(|| "island 窗口不存在".to_string())?;
    shell::place(&win, Anchor::Island, w, h)?;

    // 热区清零：前端收到 `panel-state=false` 后会按当前模式重报（药丸或球）。
    // 清零而不是沿用旧矩形——否则面板收起那一刻，透明区里还留着能吃掉桌面点击的旧热区。
    *state.island_hotzone.lock().unwrap() = Hotzone::default();
    *state.ball_hotzone.lock().unwrap() = Hotzone::default();
    // 恢复穿透。若光标仍在药丸上，钩子会在 80ms 防抖后重新置为可交互。
    state.island_transparent.store(true, Ordering::Relaxed);
    let _ = win.set_ignore_cursor_events(true);

    let _ = app.emit(PANEL_EVENT, false);
    winplat::log::log("panel: collapsed");
    Ok(())
}

/// 统一全屏让位：一个检测源（`winplat::fullscreen`）同时决定**所有**窗显隐。
///
/// 旧两仓各判一次，出现过「岛藏了 dock 没藏」；合并后这里只问一次。
/// P4 增加了开始菜单与游离图标窗，因此遍历 `webview_windows()` 而不是硬编码两窗，
/// 并记住「让位前哪些窗是可见的」：退出全屏时不能把本来关着的开始菜单顺手弹出来。
pub fn spawn_fullscreen_watch(app: AppHandle, state: Arc<SharedState>) {
    let visible_before: Arc<std::sync::Mutex<std::collections::HashSet<String>>> =
        Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    std::thread::Builder::new()
        .name("glasssuite-fullscreen".into())
        .spawn(move || {
            let mut yielding = false;
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let fullscreen = winplat::fullscreen::foreground_is_fullscreen();
                if fullscreen == yielding {
                    continue;
                }
                yielding = fullscreen;

                let a = app.clone();
                let st = Arc::clone(&state);
                let seen = Arc::clone(&visible_before);
                let _ = app.run_on_main_thread(move || {
                    let mut seen = seen.lock().unwrap();
                    for (label, win) in a.webview_windows() {
                        if yielding {
                            if win.is_visible().unwrap_or(false) {
                                seen.insert(label);
                            }
                            let _ = win.hide();
                        } else if seen.contains(&label) {
                            let _ = win.show();
                            // show() 后 tao 可能重设 ExStyle，补回工具窗样式。
                            // NOACTIVATE 只给 island：dock 要保留拿焦点的能力
                            // （添加程序的文件对话框依附前台窗口，见 glassdesk v0.7.1）。
                            #[cfg(windows)]
                            if let Ok(h) = win.hwnd() {
                                let raw = h.0 as isize;
                                let _ = winplat::style::hide_from_alt_tab(raw);
                                if label != "dock" && label != "startmenu" {
                                    let _ = winplat::style::set_no_activate(raw, true);
                                }
                            }
                        }
                    }
                    if !yielding {
                        seen.clear();
                    }
                    st.island_visible.store(!yielding, Ordering::Relaxed);
                });
                let _ = app.emit("fullscreen-yield", yielding);
                winplat::log::log(&format!("fullscreen: yielding={yielding}"));
            }
        })
        .ok();
}

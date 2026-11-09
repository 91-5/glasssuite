//! 展开面板与全屏让位。
//!
//! 展开态是**唯一**允许固定窗口尺寸的场景（卡面 §1.4 红线 4）：用 `fit_window` 同一套
//! `place()` 做 resize，不引入 720×480 固定窗 + 常驻热区那套旧模型。收起时按缓存的
//! 折叠尺寸复位，热区也一并更新，避免出现「看不见的窗口吃掉鼠标」。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::shell::{self, Anchor};
use crate::state::{Hotzone, SharedState};

/// 展开面板尺寸（逻辑像素）。只有展开态使用。
pub const PANEL_W: f64 = 320.0;
pub const PANEL_H: f64 = 268.0;

/// 与 `packages/ipc-types` 的 `EVENTS.panelState` 一致。
pub const PANEL_EVENT: &str = "panel-state";

#[tauri::command]
pub fn expand_panel(
    app: AppHandle,
    state: tauri::State<'_, Arc<SharedState>>,
) -> Result<(), String> {
    if state.island_expanded.load(Ordering::Relaxed) {
        return Ok(());
    }
    state.island_expanded.store(true, Ordering::Relaxed);

    let win = app
        .get_webview_window("island")
        .ok_or_else(|| "island 窗口不存在".to_string())?;
    shell::place(&win, Anchor::Island, PANEL_W, PANEL_H)?;

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

    let (w, h) = *state.island_collapsed_size.lock().unwrap();
    let (w, h) = if w > 0.0 && h > 0.0 {
        (w, h)
    } else {
        shell::STARTUP_ISLAND
    };

    let win = app
        .get_webview_window("island")
        .ok_or_else(|| "island 窗口不存在".to_string())?;
    shell::place(&win, Anchor::Island, w, h)?;

    *state.island_hotzone.lock().unwrap() = Hotzone {
        x: 0.0,
        y: 0.0,
        w,
        h,
    };
    // 恢复穿透。若光标仍在药丸上，钩子会在 80ms 防抖后重新置为可交互。
    state.island_transparent.store(true, Ordering::Relaxed);
    let _ = win.set_ignore_cursor_events(true);

    let _ = app.emit(PANEL_EVENT, false);
    winplat::log::log("panel: collapsed");
    Ok(())
}

/// 统一全屏让位：一个检测源（`winplat::fullscreen`）同时决定两窗显隐。
///
/// 旧两仓各判一次，出现过「岛藏了 dock 没藏」；合并后这里只问一次。
pub fn spawn_fullscreen_watch(app: AppHandle, state: Arc<SharedState>) {
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
                let _ = app.run_on_main_thread(move || {
                    for label in ["island", "dock"] {
                        if let Some(win) = a.get_webview_window(label) {
                            let _ = if yielding { win.hide() } else { win.show() };
                            if !yielding {
                                // show() 后 tao 可能重设 ExStyle，补回工具窗 + 不抢焦点。
                                if let Ok(h) = win.hwnd() {
                                    let raw = h.0 as isize;
                                    let _ = winplat::style::hide_from_alt_tab(raw);
                                    let _ = winplat::style::set_no_activate(raw, true);
                                }
                            }
                        }
                    }
                    st.island_visible.store(!yielding, Ordering::Relaxed);
                    st.dock_visible.store(!yielding, Ordering::Relaxed);
                });
                let _ = app.emit("fullscreen-yield", yielding);
                winplat::log::log(&format!("fullscreen: yielding={yielding}"));
            }
        })
        .ok();
}

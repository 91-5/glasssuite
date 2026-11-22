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
use crate::window::cursor;

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

    // OC-20261115-001 D2：**改窗口之前**先把冻结区域写成覆盖展开前/后两个原点的
    // 整面板并集。旧实现先 place 再写热区，中间那段窗口已变大而热区仍是药丸的
    // 时序窗口，正是「快甩被误收起」的入口之一。
    cursor::pre_expand_region(state.inner().as_ref());

    let win = app.get_webview_window("island").ok_or_else(|| {
        winplat::log::log("panel: expand FAILED - island window not found");
        state.island_expanded.store(false, Ordering::Relaxed);
        cursor::refresh_region(state.inner().as_ref(), "expand-rollback");
        "island 窗口不存在".to_string()
    })?;
    if let Err(e) = shell::place(&win, Anchor::Island, PANEL_W, PANEL_H) {
        winplat::log::log(&format!("panel: expand place FAILED: {e}"));
        state.island_expanded.store(false, Ordering::Relaxed);
        cursor::refresh_region(state.inner().as_ref(), "expand-rollback");
        return Err(e);
    }

    // 展开面板必须可交互：取消穿透。展开期间 passthrough 不再随 enter/leave 翻转
    // （cursor 确认 leave 时只发 island-hover=false，由 collapse_panel 统一恢复）。
    let _ = win.set_ignore_cursor_events(false);
    state.island_transparent.store(false, Ordering::Relaxed);
    *state.island_scale.lock().unwrap() = win.scale_factor().unwrap_or(1.0);
    *state.island_hotzone.lock().unwrap() = Hotzone {
        x: 0.0,
        y: 0.0,
        w: PANEL_W,
        h: PANEL_H,
    };
    // place 之后按新原点精确刷新；与展开前保守区域取并集，只增不减。
    cursor::refresh_region_keep(state.inner().as_ref(), "post-expand");

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

    // 热区清零：前端收到 `panel-state=false` / `island-placed` 后会按当前模式重报
    // （药丸或球），每次上报都会重建冻结区域。清零而不是沿用旧矩形——否则面板
    // 收起那一刻，透明区里还留着能吃掉桌面点击的旧热区。
    *state.island_hotzone.lock().unwrap() = Hotzone::default();
    *state.ball_hotzone.lock().unwrap() = Hotzone::default();
    *state.ball_center.lock().unwrap() = (0.0, 0.0);
    cursor::clear_region(state.inner().as_ref(), "collapse");
    // 恢复穿透。若光标仍在药丸上，钩子会在 80ms 防抖后重新置为可交互。
    state.island_transparent.store(true, Ordering::Relaxed);
    let _ = win.set_ignore_cursor_events(true);

    // place() 内部已 emit 一次 island-placed；这里再显式发一次，兜住
    // 「前端 mode effect 先于本命令执行」的时序，保证折叠热区一定被重报。
    let _ = app.emit("island-placed", ());
    let _ = app.emit(PANEL_EVENT, false);
    winplat::log::log("panel: collapsed");
    Ok(())
}

/// 全屏让路退出后，各窗口要补回的 ExStyle 决策（纯函数，便于单测）。
///
/// 返回 `(tool_window, no_activate)`：
/// - island：常显装饰窗，补 TOOLWINDOW + NOACTIVATE（不进任务栏、点击不抢焦点）；
/// - dock / startmenu：工具窗但不抢焦点需求不同——dock 要能拿焦点（文件对话框依附），
///   startmenu 是弹出面板，二者只补 TOOLWINDOW、不补 NOACTIVATE；
/// - control：**标准系统窗口**，两个都不能补。旧实现用 `label != "dock" && label != "startmenu"`
///   把 control 一起误伤，导致它被加 NOACTIVATE（点击无法置前）+ TOOLWINDOW
///   （标题栏变窄、最小化按钮消失）——正是 sir 报的两个症状（OC-20261122-001）。
pub(crate) fn fullscreen_restore_styles(label: &str) -> (bool, bool) {
    match label {
        "island" => (true, true),
        "dock" | "startmenu" => (true, false),
        _ => (false, false),
    }
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
                            // show() 后 tao 可能重设 ExStyle，按窗口类型补回样式。
                            // OC-20261122-001：control 是标准窗口，绝不能补工具样式
                            // （否则标题栏最小化按钮消失 + 点击无法激活置前）。
                            #[cfg(windows)]
                            if let Ok(h) = win.hwnd() {
                                let raw = h.0 as isize;
                                let (tool, no_activate) = fullscreen_restore_styles(&label);
                                if tool {
                                    let _ = winplat::style::hide_from_alt_tab(raw);
                                }
                                if no_activate {
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

#[cfg(test)]
mod tests {
    use super::fullscreen_restore_styles;

    #[test]
    fn control_is_never_given_tool_styles() {
        // OC-20261122-001：控制页两个样式都不能补，否则最小化按钮消失+无法置前。
        assert_eq!(fullscreen_restore_styles("control"), (false, false));
        // 未知窗口也走安全默认（不误伤）。
        assert_eq!(fullscreen_restore_styles("something_else"), (false, false));
    }

    #[test]
    fn island_dock_startmenu_keep_their_styles() {
        assert_eq!(fullscreen_restore_styles("island"), (true, true));
        assert_eq!(fullscreen_restore_styles("dock"), (true, false));
        assert_eq!(fullscreen_restore_styles("startmenu"), (true, false));
    }
}

//! 窗口交互层：光标观测（唯一来源）、热区上报、展开面板、全屏让位。

pub mod cursor;
pub mod panel;

use std::sync::Arc;

use tauri::WebviewWindow;

use crate::state::{Hotzone, SharedState};

/// 前端上报交互热区（CSS 逻辑像素，相对窗口客户区原点）。
///
/// 热区**随内容尺寸动态上报**：窗口尺寸 = 内容尺寸，宽度一变热区就得跟着变，
/// 否则透明区会吃掉桌面点击。Rust 侧在 `fit_*` 里也会写同一份（兜底 resize 时序），
/// 两处写入同一个 `SharedState`，不存在两套真相。
#[tauri::command]
pub fn set_hotzone(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<SharedState>>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    target: Option<String>,
) {
    let label = target.unwrap_or_else(|| window.label().to_string());
    let scale = window.scale_factor().unwrap_or(1.0);
    let hz = Hotzone { x, y, w, h };
    if label == "dock" {
        *state.dock_hotzone.lock().unwrap() = hz;
        *state.dock_scale.lock().unwrap() = scale;
    } else {
        *state.island_hotzone.lock().unwrap() = hz;
        *state.island_scale.lock().unwrap() = scale;
    }
}

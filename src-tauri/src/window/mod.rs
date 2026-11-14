//! 窗口交互层：光标观测（唯一来源）、热区上报、展开面板、全屏让位。

pub mod cursor;
pub mod panel;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::WebviewWindow;

use crate::state::{Hotzone, SharedState};

/// 前端上报交互热区（CSS 逻辑像素，相对窗口客户区原点）。
///
/// OC-20261115-001：这里**不再是判定事实源**。上报只触发 Rust 侧重算冻结区域
/// （`cursor::refresh_region`），钩子只读 `SharedState::hit_region`。
/// 展开态尤其关键：前端按内容包围盒上报的矩形远小于整面板，旧实现会把它直接
/// 写进热区，导致右上角 AI/网络按钮落在热区外 → 快甩过去立刻判 leave → 收起。
/// 现在展开态一律忽略上报的矩形，区域由 Rust 单方面保持「整面板 ∪ 药丸 ∪ 球」。
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
    let _ = label; // dock 窗口不再走热区上报模型：它贴合内容、无需穿透，只有 island 上报。
    let scale = window.scale_factor().unwrap_or(1.0);
    *state.island_scale.lock().unwrap() = scale;
    let expanded = state.island_expanded.load(Ordering::Relaxed);
    let st = state.inner().as_ref();
    if expanded {
        winplat::log::log(&format!(
            "cursor: set_hotzone rect IGNORED (expanded) req=({x},{y},{w},{h})"
        ));
    } else {
        *state.island_hotzone.lock().unwrap() = Hotzone { x, y, w, h };
    }
    cursor::refresh_region(
        st,
        if expanded {
            "set_hotzone(expanded)"
        } else {
            "set_hotzone"
        },
    );
}

/// 折叠球热区上报（收起态）。与药丸热区取并集命中，球心用于算 `ball-look` 朝向。
///
/// 为什么球要单独一份而不是并进 `set_hotzone`：收起时药丸层是 `display:none`，
/// 若沿用同一个矩形，展开判定会在球外的空白处误触发 hover。
#[tauri::command]
pub fn set_ball_hotzone(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<SharedState>>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) {
    *state.ball_hotzone.lock().unwrap() = Hotzone { x, y, w, h };
    *state.ball_center.lock().unwrap() = (x + w / 2.0, y + h / 2.0);
    *state.island_scale.lock().unwrap() = window.scale_factor().unwrap_or(1.0);
    // 球热区也是判定区域的一部分：重算冻结区域（含球心物理坐标）。
    cursor::refresh_region(state.inner().as_ref(), "set_ball_hotzone");
}

/// AI 洞察静音开关（收起球右键 / 设置面板均可用）。静音期间后端不 emit。
#[tauri::command]
pub fn set_muted(state: tauri::State<'_, Arc<SharedState>>, muted: bool) {
    state
        .muted
        .store(muted, std::sync::atomic::Ordering::Relaxed);
}

/// 当前是否「没有 AI 可看」：未配置端点/密钥，或用户静音。
///
/// 前端用它决定收起态是否画洞察条（未配置时整条隐藏，不留空壳）。
#[tauri::command]
pub fn ai_disabled(
    state: tauri::State<'_, Arc<SharedState>>,
    ai: tauri::State<'_, Arc<crate::island::ai::AiState>>,
) -> bool {
    state.muted.load(std::sync::atomic::Ordering::Relaxed) || !ai.current().is_configured()
}

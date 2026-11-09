//! 「游离图标」：从 dock 拖出来、按屏幕坐标独立摆放的快捷方式。
//!
//! 实现取舍：每个游离图标是**一个恰好等于图标大小的独立窗口**（`free-<id>`），
//! 而不是一个铺满屏幕的透明大窗 + 常驻热区。这样：
//! - 不违反「窗口尺寸 = 内容尺寸」红线（硬约束 11）：没有透明宽区吃掉桌面点击，
//!   也不需要给大窗额外配一套穿透热区；
//! - 每个窗口本身可交互，点击/拖动都由它自己的 webview 处理；
//! - 图标消失时直接隐藏窗口，不留幽灵热区。
//!
//! 窗口复用 `dock/index.html#free/<id>` 的 hash 路由，前端仍是同一份构建产物。
//! 位置来自用户拖拽的屏幕 CSS 坐标，由 `place_at` 统一做逻辑 → 物理换算。

use std::collections::HashSet;

use tauri::{AppHandle, Manager};

use super::config::{DockConfig, IconMode, Shortcut};

/// 游离图标窗口尺寸（逻辑像素）。与 `.free-icon` 的 CSS 一致。
pub const FREE_W: f64 = 56.0;
pub const FREE_H: f64 = 56.0;
pub const LABEL_PREFIX: &str = "free-";

pub fn label(id: &str) -> String {
    format!("{LABEL_PREFIX}{id}")
}

pub fn url(id: &str) -> String {
    format!("dock/index.html#free/{id}")
}

pub fn free_shortcuts(cfg: &DockConfig) -> Vec<&Shortcut> {
    cfg.shortcuts
        .iter()
        .filter(|s| s.mode == IconMode::Free && s.free_rect.is_some())
        .collect()
}

/// 按当前配置对齐实际窗口集合：新增的建窗摆位，撤掉的隐藏。
///
/// 重复调用是幂等的；日志只记数量，不记应用名 / 路径（硬约束 13）。
pub fn sync_windows(app: &AppHandle, cfg: &DockConfig) -> Result<(), String> {
    let mut wanted = HashSet::new();
    for s in free_shortcuts(cfg) {
        let Some(rect) = s.free_rect.as_ref() else {
            continue;
        };
        let lbl = label(&s.id);
        wanted.insert(lbl.clone());
        let win = crate::shell::ensure_window(app, &lbl, &url(&s.id), FREE_W, FREE_H)?;
        crate::shell::place_at(&win, rect.x as f64, rect.y as f64, FREE_W, FREE_H)?;
        win.show().map_err(|e| e.to_string())?;
        // show() 后 tao 可能重设 ExStyle，补回工具窗 + 不抢焦点。
        #[cfg(windows)]
        if let Ok(h) = win.hwnd() {
            let raw = h.0 as isize;
            let _ = winplat::style::hide_from_alt_tab(raw);
            let _ = winplat::style::set_no_activate(raw, true);
        }
    }

    for (existing, win) in app.webview_windows() {
        if existing.starts_with(LABEL_PREFIX) && !wanted.contains(&existing) {
            let _ = win.hide();
            let _ = win.set_ignore_cursor_events(true);
        }
    }
    winplat::log::log(&format!("dock: free icons synced count={}", wanted.len()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dock_item(mode: IconMode, rect: bool) -> Shortcut {
        Shortcut {
            id: "sc1".to_string(),
            name: "A".to_string(),
            path: "a.exe".to_string(),
            aumid: None,
            icon: None,
            mode,
            free_rect: rect.then_some(super::super::config::FreeRect {
                x: 10,
                y: 20,
                w: 56,
                h: 56,
            }),
        }
    }

    #[test]
    fn only_free_items_with_a_rect_are_detached() {
        let cfg = DockConfig {
            shortcuts: vec![
                dock_item(IconMode::Dock, false),
                dock_item(IconMode::Free, false),
                dock_item(IconMode::Free, true),
            ],
            ..Default::default()
        };
        assert_eq!(free_shortcuts(&cfg).len(), 1);
    }

    #[test]
    fn labels_are_unique_and_url_uses_hash_routing() {
        assert_eq!(label("sc1"), "free-sc1");
        assert_eq!(url("sc1"), "dock/index.html#free/sc1");
        assert_ne!(label("sc1"), label("sc2"));
    }
}

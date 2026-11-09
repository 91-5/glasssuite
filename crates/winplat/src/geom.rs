//! 纯几何计算：不碰 Win32，可在任何平台单测。
//!
//! 全部坐标都是**物理像素**（Win32 `RECT` 口径：`right`/`bottom` 为开区间）。
//! 逻辑像素只在 Tauri 侧出现，跨边界时必须显式乘/除 `scale_factor`，
//! 不得在这里隐式换算——旧代码「逻辑像素宽度 + 物理像素原点」混算是
//! 多屏/高 DPI 错位的直接来源。

/// 矩形（物理像素，左上 / 右下）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub const fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    /// 任务栏候选矩形是否可信（可见态）。
    ///
    /// 判定口径：非空，且上边界落在屏幕内、下边界不为负——`top >= screen_height`
    /// 视作几何上不可能的矩形。
    pub fn is_plausible_taskbar(&self, screen_height: i32) -> bool {
        !self.is_empty() && self.top < screen_height && self.bottom > 0
    }

    /// 隐藏态任务栏矩形是否可信。
    ///
    /// 自动隐藏的任务栏在隐藏时**整个滑出屏幕**，所以停泊矩形的底边必然越过屏幕底边
    /// （实测 1080 高的屏上是 `bottom = 1126`）。若拿到的矩形底边恰好在屏幕内
    /// （例如 `(0, 1032, 1920, 1080)`），说明这是「位置停在可见态、状态却是隐藏」的幻影值，
    /// 必须丢弃并按屏底重建，否则 dock 会跟着上移约 46px。
    pub fn looks_like_hidden_dock(&self, screen_height: i32) -> bool {
        !self.is_empty() && self.bottom > screen_height
    }

    /// 让宽度为 `width` 的窗口在本矩形内水平居中，返回左上 `x`。
    pub fn centered_x(&self, width: i32) -> i32 {
        self.left + (self.width() - width) / 2
    }

    /// 让高度为 `height` 的窗口贴住本矩形底边（留 `margin_bottom`），返回左上 `y`。
    ///
    /// 用于「窗口落在矩形内部」的场景（例如在显示器工作区内贴底）。
    pub fn bottom_anchored_y(&self, height: i32, margin_bottom: i32) -> i32 {
        self.bottom - margin_bottom - height
    }

    /// 让高度为 `height` 的窗口落在本矩形**上边界之外**（留 `margin_above`），返回左上 `y`。
    ///
    /// 这是 dock 的定锚口径：无论任务栏可见还是自动隐藏，dock 都贴在任务栏**上沿**之上，
    /// 而不是贴隐藏停泊矩形那段越出屏幕的底边。实测：隐藏任务栏 `top=1078`、dock 高 76、
    /// 边距 6 → `y = 996`，底边 1072，与旧版一致。
    pub fn above_anchored_y(&self, height: i32, margin_above: i32) -> i32 {
        self.top - margin_above - height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_and_height() {
        let r = Rect::new(320, 1040, 1920, 1080);
        assert_eq!(r.width(), 1600);
        assert_eq!(r.height(), 40);
    }

    #[test]
    fn centered_on_primary() {
        let screen = Rect::new(0, 0, 1920, 1080);
        assert_eq!(screen.centered_x(1250), 335);
    }

    /// 副屏原点不是 0 时，居中必须相对**该屏**算，不能回落到主屏。
    #[test]
    fn centered_on_secondary_monitor() {
        let screen = Rect::new(1920, 0, 3840, 1080);
        assert_eq!(screen.centered_x(300), 2730);
    }

    #[test]
    fn bottom_anchor_leaves_margin() {
        let screen = Rect::new(0, 0, 1920, 1080);
        assert_eq!(screen.bottom_anchored_y(76, 6), 998);
    }

    #[test]
    fn empty_detection() {
        assert!(Rect::new(0, 0, 0, 0).is_empty());
        assert!(Rect::new(0, 0, -10, 40).is_empty());
        assert!(!Rect::new(0, 0, 10, 40).is_empty());
    }

    #[test]
    fn phantom_taskbar_rejected() {
        let screen_height = 1080;
        // 正常隐藏停泊矩形：底边越过屏幕底边 → 可信。
        let hidden = Rect::new(0, 1078, 1920, 1126);
        assert!(hidden.is_plausible_taskbar(screen_height));
        assert!(hidden.looks_like_hidden_dock(screen_height));
        // 幻影：位置停在可见态（底边恰好在屏幕内），状态却是隐藏 → 必须丢弃。
        let phantom = Rect::new(0, 1032, 1920, 1080);
        assert!(!phantom.looks_like_hidden_dock(screen_height));
        // 空矩形（任务栏被关闭）。
        assert!(!Rect::new(0, 1080, 1920, 1080).is_plausible_taskbar(screen_height));
    }

    /// 隐藏停泊矩形重建口径：贴屏底 2px；dock 贴任务栏**上沿**（不是越界的底边）。
    #[test]
    fn rebuild_hidden_dock_from_screen() {
        let screen = Rect::new(0, 0, 1920, 1080);
        let rebuilt = Rect::new(
            screen.left,
            screen.bottom - 2,
            screen.right,
            screen.bottom + 46,
        );
        assert_eq!(rebuilt.top, 1078);
        assert!(rebuilt.looks_like_hidden_dock(screen.bottom));
        // dock 贴任务栏上沿（留 6px）→ 左上 y=996、底边 1072，与旧版实测一致。
        assert_eq!(rebuilt.above_anchored_y(76, 6), 996);
        assert_eq!(rebuilt.above_anchored_y(76, 6) + 76, 1072);
    }
}

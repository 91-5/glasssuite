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

    /// 任务栏候选矩形是否可信。
    ///
    /// 旧 glassdesk 的坑（`AGENTS.md` 坑 13）：自动隐藏的任务栏在「被全屏应用唤出过又退出」
    /// 之后，`GetWindowRect` 会停在可见位置（例如 `top=1032`）而 `IsWindowVisible=false`，
    /// 真实停泊矩形是 `(0, 1078, 1920, 1126)`。直接信任该矩形会让窗口上移 40~46px。
    ///
    /// 判定口径：非空，且上边界落在屏幕内、下边界不为负——`top >= screen_height`
    /// 视作幻影矩形。
    pub fn is_plausible_taskbar(&self, screen_height: i32) -> bool {
        !self.is_empty() && self.top < screen_height && self.bottom > 0
    }

    /// 让宽度为 `width` 的窗口在本矩形内水平居中，返回左上 `x`。
    pub fn centered_x(&self, width: i32) -> i32 {
        self.left + (self.width() - width) / 2
    }

    /// 让高度为 `height` 的窗口贴住本矩形底边（留 `margin_bottom`），返回左上 `y`。
    pub fn bottom_anchored_y(&self, height: i32, margin_bottom: i32) -> i32 {
        self.bottom - margin_bottom - height
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
        // 正常隐藏停泊矩形：可信。
        assert!(Rect::new(0, 1078, 1920, 1126).is_plausible_taskbar(screen_height));
        // 幻影：top 落在屏幕之外。
        assert!(!Rect::new(0, 1126, 1920, 1174).is_plausible_taskbar(screen_height));
        // 空矩形（任务栏被关闭）。
        assert!(!Rect::new(0, 1080, 1920, 1080).is_plausible_taskbar(screen_height));
    }
}

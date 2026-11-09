//! dock 悬停预览：DWM 缩略图，带硬超时与纯色块降级。
//!
//! 旧 `glassdesk/src-tauri/src/preview.rs` 直接在命令线程里 `DwmRegisterThumbnail`，
//! 遇到最小化 / UWP / 远程窗口时可能卡住 DWM 往返。合并到单进程后这会连累 island，
//! 因此注册与属性更新整体放进独立短命线程，400ms 收不到结果就判降级：
//! 前端改用纯色块，**不等待、不重试、不阻塞 UI**（卡面 D5 / 硬约束 14）。
//!
//! 生命周期：`show` 先注销上一张，再注册新的一张；`hide` 只做一次注销。
//! 句柄是 `isize`（windows 0.62 的 HTHUMBNAIL 已扁平化），只在本模块内存里保存。

use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION,
    DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};

const PREVIEW_DEADLINE: Duration = Duration::from_millis(400);

/// 前端上报的预览矩形（CSS 逻辑像素，相对 dock 窗口客户区）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PreviewRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl PreviewRect {
    /// 逻辑 → 物理；非法尺寸返回 `None`，不把垃圾矩形交给 DWM。
    pub fn physical(&self, scale: f64) -> Option<RECT> {
        if self.w <= 0.0 || self.h <= 0.0 || scale <= 0.0 {
            return None;
        }
        let s = |v: f64| (v * scale).round() as i32;
        Some(RECT {
            left: s(self.x),
            top: s(self.y),
            right: s(self.x + self.w),
            bottom: s(self.y + self.h),
        })
    }
}

#[derive(Default)]
pub struct PreviewController {
    current: Mutex<Option<isize>>,
}

impl PreviewController {
    /// 注册一张新缩略图。返回 false 表示超时/失败，调用方应展示降级色块。
    pub fn show(
        &self,
        dest_hwnd: isize,
        source_hwnd: isize,
        logical: PreviewRect,
        scale: f64,
    ) -> bool {
        self.hide();
        if dest_hwnd == 0 || source_hwnd == 0 {
            return false;
        }
        let Some(rect) = logical.physical(scale) else {
            return false;
        };

        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("glasssuite-dwm-preview".into())
            .spawn(move || {
                let _ = tx.send(register(dest_hwnd, source_hwnd, rect));
            })
            .is_ok();
        if !spawned {
            return false;
        }
        match rx.recv_timeout(PREVIEW_DEADLINE) {
            Ok(Some(handle)) => {
                *self.current.lock().unwrap() = Some(handle);
                true
            }
            Ok(None) => {
                winplat::log::log("dock: preview unavailable, degraded count=1");
                false
            }
            Err(_) => {
                winplat::log::log("dock: preview timeout, degraded count=1");
                false
            }
        }
    }

    pub fn hide(&self) {
        if let Some(handle) = self.current.lock().unwrap().take() {
            unsafe {
                let _ = DwmUnregisterThumbnail(handle);
            }
        }
    }
}

fn register(dest: isize, source: isize, rect: RECT) -> Option<isize> {
    unsafe {
        let handle = DwmRegisterThumbnail(HWND(dest as *mut _), HWND(source as *mut _)).ok()?;
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_VISIBLE
                | DWM_TNP_OPACITY
                | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: rect,
            rcSource: RECT::default(),
            opacity: 235,
            fVisible: BOOL(1),
            fSourceClientAreaOnly: BOOL(1),
        };
        if DwmUpdateThumbnailProperties(handle, &props).is_err() {
            let _ = DwmUnregisterThumbnail(handle);
            return None;
        }
        Some(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_converts_logical_to_physical() {
        let r = PreviewRect {
            x: 10.0,
            y: 20.0,
            w: 100.0,
            h: 60.0,
        }
        .physical(1.5)
        .unwrap();
        assert_eq!((r.left, r.top, r.right, r.bottom), (15, 30, 165, 120));
    }

    #[test]
    fn degenerate_rects_are_rejected() {
        assert!(PreviewRect::default().physical(1.0).is_none());
        assert!(PreviewRect {
            w: 10.0,
            h: 10.0,
            ..Default::default()
        }
        .physical(0.0)
        .is_none());
    }

    #[test]
    fn hide_without_show_is_a_noop() {
        PreviewController::default().hide();
    }
}

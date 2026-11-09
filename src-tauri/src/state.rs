//! 进程级共享状态。
//!
//! 单进程双窗的代价是「状态只有一份」：媒体通道、最近快照、两个窗口各自的热区与穿透
//! 标志都放在这里，由 `Arc` 交给 WinRT 回调 / 鼠标钩子 / Tauri 命令共享。
//!
//! 为什么不用 per-window 的 struct：旧两仓各有一套 `SharedState`（岛一套、dock 一套），
//! 合并后光标观测必须收敛成**单一来源**（卡面 §1.4/§5），一个进程里放两份状态必然会
//! 出现「岛认为光标在热区、dock 认为不在」的语义漂移。

use std::sync::atomic::{AtomicBool, AtomicIsize};
use std::sync::{mpsc, Mutex, OnceLock};

use serde_json::Value;

use crate::island::media::MediaMsg;

/// 前端上报的交互热区，CSS 逻辑像素，相对窗口客户区原点。
///
/// 热区**随内容尺寸动态上报**（`useFitWindow` 测量后调 `set_hotzone`），不是固定矩形：
/// 窗口尺寸 = 内容尺寸，宽度一变热区就得跟着变，否则透明区会吃掉桌面点击。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hotzone {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Hotzone {
    /// 逻辑像素命中判定；换算成物理屏幕坐标由 `window::cursor` 完成（那里才知道缩放）。
    pub fn contains(&self, x: f64, y: f64) -> bool {
        self.w > 0.0
            && self.h > 0.0
            && x >= self.x
            && x < self.x + self.w
            && y >= self.y
            && y < self.y + self.h
    }
}

/// 最近一次系统采样（给 AI 本地规则用）。
///
/// 为什么要单独存一份：`metrics` 是唯一采样源，AI 不该再起一个 `sysinfo` 轮询；
/// 后端模块之间共享快照即可，避免「一个进程两套 CPU 数字」。
#[derive(Clone, Copy, Debug, Default)]
pub struct MetricsSnapshot {
    pub cpu: f64,
    pub mem_pct: f64,
    pub battery: Option<f64>,
    pub charging: bool,
}

#[derive(Default)]
pub struct SharedState {
    /// SMTC 控制通道。`OnceLock` 而非 `Option`：线程启动后只写一次，之后只读。
    pub media_tx: OnceLock<mpsc::Sender<MediaMsg>>,
    /// 最近一次媒体快照，供窗口重建 / 前端首帧用命令拉取，避免重新 WinRT 初始化。
    pub last_media: Mutex<Option<Value>>,

    pub island_hotzone: Mutex<Hotzone>,
    pub dock_hotzone: Mutex<Hotzone>,

    /// 两个窗口各自是否处于点击穿透态（WS_EX_TRANSPARENT）。
    pub island_transparent: AtomicBool,
    pub dock_transparent: AtomicBool,

    /// 原生窗口句柄（地址），供钩子回调做样式对账；0 = 尚未就绪。
    pub island_hwnd: AtomicIsize,
    pub dock_hwnd: AtomicIsize,

    /// 全屏让位期间窗口不可见，钩子此时不做穿透切换。
    pub island_visible: AtomicBool,
    pub dock_visible: AtomicBool,

    /// 1s 采样的最新系统指标（AI 只读）。
    pub metrics: Mutex<MetricsSnapshot>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotzone_rejects_empty_rect() {
        assert!(!Hotzone::default().contains(0.0, 0.0));
        assert!(!Hotzone {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 0.0
        }
        .contains(1.0, 0.0));
    }

    #[test]
    fn hotzone_edges_are_half_open() {
        let z = Hotzone {
            x: 4.0,
            y: 6.0,
            w: 100.0,
            h: 40.0,
        };
        assert!(z.contains(4.0, 6.0));
        assert!(z.contains(103.9, 45.9));
        assert!(!z.contains(104.0, 20.0), "右边界不含");
        assert!(!z.contains(50.0, 46.0), "下边界不含");
        assert!(!z.contains(3.9, 20.0));
    }
}

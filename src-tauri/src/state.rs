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
/// 热区**随内容尺寸动态上报**（前端测量内容包围盒后调 `set_hotzone`），不是固定矩形：
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

/// 物理屏幕坐标下的判定矩形（`[left,right) × [top,bottom)`）。
///
/// OC-20261115-001：**唯一供钩子读取的热区形态**。前端上报的 `Hotzone` 是逻辑坐标
/// 且有多个写入者，钩子回调里绝不能拿着它去做 `GetWindowRect` 换算——那等于
/// 每次采样重新解释一遍事实源。写入侧在命令/摆位时一次性换算成物理矩形并冻结，
/// 钩子只读这份冻结值。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ScreenRect {
    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        !self.is_empty() && x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// 逻辑热区 → 物理屏幕矩形。窗口原点在写入时现取一次。
    pub fn from_logical(origin: (i32, i32), hz: Hotzone, scale: f64) -> Self {
        if scale <= 0.0 || hz.w <= 0.0 || hz.h <= 0.0 {
            return Self::default();
        }
        let (ox, oy) = origin;
        let left = ox + (hz.x * scale).round() as i32;
        let top = oy + (hz.y * scale).round() as i32;
        Self {
            left,
            top,
            right: left + (hz.w * scale).round() as i32,
            bottom: top + (hz.h * scale).round() as i32,
        }
    }

    /// 单一固定尺寸矩形的物理化（展开面板整窗）。
    pub fn from_size(origin: (i32, i32), w: f64, h: f64, scale: f64) -> Self {
        Self::from_logical(
            origin,
            Hotzone {
                x: 0.0,
                y: 0.0,
                w,
                h,
            },
            scale,
        )
    }

    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    /// 外扩若干物理像素（展开面板边缘容差；收缩请用显式重建，不要用负 padding）。
    pub fn inflate(self, px: i32) -> Self {
        if self.is_empty() || px <= 0 {
            return self;
        }
        Self {
            left: self.left - px,
            top: self.top - px,
            right: self.right + px,
            bottom: self.bottom + px,
        }
    }

    /// Liang–Barsky 段-矩形相交。
    ///
    /// 为什么必须用它替代「当前采样点 ∈ 矩形」：`WH_MOUSE_LL` 相邻两帧之间光标
    /// 可能跨过整块面板边缘，中间路径从未被采样。点内判定会把这种跳帧直接当成
    /// 「离开」。段判把「这一段路上是否碰过热区」作为事实，跳帧自然被覆盖。
    /// `p0 == p1`（首次采样 / 光标静止）时退化为点内判定。
    pub fn intersects_segment(self, p0: (i32, i32), p1: (i32, i32)) -> bool {
        if self.is_empty() {
            return false;
        }
        let x0 = p0.0 as f64;
        let y0 = p0.1 as f64;
        let dx = p1.0 as f64 - x0;
        let dy = p1.1 as f64 - y0;
        let edges = [
            (-dx, x0 - self.left as f64),
            (dx, self.right as f64 - x0),
            (-dy, y0 - self.top as f64),
            (dy, self.bottom as f64 - y0),
        ];
        let mut enter = 0.0_f64;
        let mut exit = 1.0_f64;
        for (p, q) in edges {
            if p == 0.0 {
                if q < 0.0 {
                    return false; // 平行于该边且在边外
                }
                continue;
            }
            let t = q / p;
            if p < 0.0 {
                if t > exit {
                    return false;
                }
                enter = enter.max(t);
            } else {
                if t < enter {
                    return false;
                }
                exit = exit.min(t);
            }
        }
        enter <= exit
    }
}

/// 冻结后的命中区域 + 写入代次。代次用于日志对账（哪次判定看的是哪版热区）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HitRegion {
    pub rect: ScreenRect,
    pub generation: u64,
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
    /// 折叠球热区：收起时药丸缩成一颗球，球单独上报热区（移植自 glasspill）。
    /// 与 `island_hotzone` 取并集做命中判定——球本身也要可点，否则点不回来。
    pub ball_hotzone: Mutex<Hotzone>,

    /// OC-20261115-001：钩子**唯一**读取的判定区域（物理屏幕坐标，已冻结）。
    /// 写入点全部在 Rust 侧命令/摆位里（`window::cursor::refresh_region`），
    /// 前端上报只能触发重算，不能直接写这里，更不能缩小展开态的区域。
    pub hit_region: Mutex<HitRegion>,

    /// 球心的物理屏幕坐标（写入侧冻结；钩子不再在回调里 `GetWindowRect`）。
    pub ball_center_screen: Mutex<Option<(i32, i32)>>,

    /// 球心（逻辑像素，相对窗口客户区）。`cursor` 用它算 `ball-look` 的朝向。
    pub ball_center: Mutex<(f64, f64)>,

    /// AI 洞察静音：true 时后端不再 emit `ai-insight`（移植自 glasspill 的 set_muted）。
    pub muted: AtomicBool,

    /// 岛是否处于点击穿透态（WS_EX_TRANSPARENT）。
    /// dock 窗口贴合内容、没有透明宽区，永远保持可点击，因此不再需要穿透标志。
    pub island_transparent: AtomicBool,

    /// 原生窗口句柄（地址），供钩子回调做样式对账；0 = 尚未就绪。
    pub island_hwnd: AtomicIsize,

    /// 全屏让位期间窗口不可见，钩子此时不做穿透切换。
    pub island_visible: AtomicBool,

    /// 1s 采样的最新系统指标（AI 只读）。
    pub metrics: Mutex<MetricsSnapshot>,

    /// 岛逻辑→物理换算用的缩放因子。由 `set_hotzone` 命令刷新（唯一写入点），
    /// 钩子回调只读；窗口实时原点由 `GetWindowRect` 现取，因此窗口移动后热区自动跟随。
    pub island_scale: Mutex<f64>,

    /// 展开态标记。展开时 `fit_island` 忽略内容尺寸上报（否则 ResizeObserver
    /// 会把窗口缩回药丸大小）；收起时用缓存的折叠尺寸复位。
    pub island_expanded: AtomicBool,
    pub island_collapsed_size: Mutex<(f64, f64)>,
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

    fn rect() -> ScreenRect {
        ScreenRect {
            left: 100,
            top: 100,
            right: 200,
            bottom: 180,
        }
    }

    #[test]
    fn segment_crosses_rect_without_any_endpoint_inside() {
        // 复现「快甩」：两端都在热区外，整条路径却从热区中间穿过。
        assert!(rect().intersects_segment((50, 140), (250, 140)));
        assert!(rect().intersects_segment((150, 50), (150, 230)));
        assert!(rect().intersects_segment((0, 0), (300, 300)));
    }

    #[test]
    fn segment_misses_when_path_stays_outside() {
        assert!(!rect().intersects_segment((50, 50), (250, 60)));
        assert!(!rect().intersects_segment((10, 10), (20, 20)));
        assert!(!rect().intersects_segment((250, 140), (300, 140)));
        // 从热区外移到热区外、但擦着上边平行而过
        assert!(!rect().intersects_segment((50, 99), (250, 99)));
    }

    #[test]
    fn degenerate_segment_is_point_test() {
        assert!(rect().intersects_segment((150, 140), (150, 140)));
        assert!(!rect().intersects_segment((150, 99), (150, 99)));
        assert!(!ScreenRect::default().intersects_segment((150, 140), (150, 140)));
    }

    #[test]
    fn segment_leaving_rect_still_counts_as_inside_that_frame() {
        // 起点在内、终点在外：段相交 → 该帧仍算 inside。
        // 下一帧 p0 已是外侧点，段退化/不相交 → 正常进入 leave 流程，
        // 所以段判只把「决定离开」推迟一帧，不会让面板永远收不起来。
        assert!(rect().intersects_segment((150, 140), (400, 140)));
        let next = ((400, 140), (400, 140));
        assert!(!rect().intersects_segment(next.0, next.1));
    }

    #[test]
    fn union_and_inflate_only_grow_region() {
        let small = ScreenRect {
            left: 150,
            top: 120,
            right: 160,
            bottom: 130,
        };
        assert_eq!(rect().union(small), rect(), "小矩形不得缩小现有区域");
        assert_eq!(ScreenRect::default().union(rect()), rect());
        assert_eq!(rect().inflate(10).left, 90);
        assert_eq!(rect().inflate(10).bottom, 190);
    }

    #[test]
    fn logical_to_screen_uses_scale_and_window_origin() {
        let hz = Hotzone {
            x: 10.0,
            y: 20.0,
            w: 100.0,
            h: 50.0,
        };
        assert_eq!(
            ScreenRect::from_logical((1000, 50), hz, 1.0),
            ScreenRect {
                left: 1010,
                top: 70,
                right: 1110,
                bottom: 120
            }
        );
        assert_eq!(
            ScreenRect::from_logical((1000, 50), hz, 1.25),
            ScreenRect {
                left: 1013,
                top: 75,
                right: 1138,
                bottom: 138
            }
        );
    }
}

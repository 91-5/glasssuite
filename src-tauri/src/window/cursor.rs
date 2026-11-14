//! **唯一光标观测源**：一个 `WH_MOUSE_LL` 低级鼠标钩子驱动 island 的点击穿透。
//!
//! 为什么必须单一：旧两仓各有一套观测（岛用 LL 钩子、dock 用 200ms `GetCursorPos`
//! 轮询），合并到同一进程后就是两套延迟、两套「已唤出」语义。本模块只保留岛侧
//! 穿透；**dock 的唤出/隐藏改回 glassdesk 原生实现**（`dock::dock_logic`：光标
//! 靠底 + 悬停判定 + 失焦收回，500ms 兜底轮询），hook 不再插手 dock。
//!
//! 用途声明（README 同步）：该钩子**只读取光标位置**，用于判断光标是否落在岛热区，
//! 从而开关鼠标穿透。它不记录按键、不记录窗口标题、不注入任何进程，
//! 也不把任何内容发到网络。
//!
//! 三个实现红线（旧仓踩过）：
//! 1. 一旦设上 `WS_EX_TRANSPARENT`，窗口收不到任何鼠标消息，DOM `mouseenter` 永远不触发
//!    —— 热区判定**必须留在 Rust 侧**，不能靠前端 DOM 事件解除穿透。
//! 2. 穿透开关走 tao 的 `set_ignore_cursor_events`，不要裸写 ExStyle：
//!    tao 在任何状态变化时会整写 ExStyle（apply_diff），裸写会被冲掉。
//! 3. 钩子回调只做内存计算 + 命中判定，样式切换派发到主线程；回调耗时微秒级，
//!    不会触发系统 LL 钩子超时驱逐。
//!
//! 钩子须在主线程 `setup` 内安装：独立线程 + `GetMessageW` 泵实测会无声停摆
//! （事件全丢且无线程退出痕迹），而 tao 主循环本身就是常驻消息泵。
//!
//! ## OC-20261115-001（展开后快甩被误收起）
//! 判定从「当前采样点 ∈ 热区」改为「上一次采样点 → 当前点的**线段**与热区相交」，
//! 并收敛事实源：钩子只读 `SharedState::hit_region`（写入时已冻结成物理屏幕矩形），
//! 不再在回调里 `GetWindowRect`、也不再读逻辑热区。展开态的区域由 Rust 单方面写成
//! 「整面板 ∪ 药丸 ∪ 球 ∪ 外扩 10px」，前端上报不得缩小它；离开判定再挂 150ms
//! 延迟释放窗口，期间任一采样重新 inside 即取消。展开期间不翻转 passthrough。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetWindowRect, SetWindowsHookExW, MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_MOUSEMOVE,
};

use crate::state::{HitRegion, ScreenRect, SharedState};
use crate::window::panel::{PANEL_H, PANEL_W};

/// 进入热区防抖：避免擦边而过就取消穿透。
const ENTER_DEBOUNCE: Duration = Duration::from_millis(80);
/// 离开热区滞回：防快速进出时样式高频切换（只防擦边，不防跳帧）。
const LEAVE_HYSTERESIS: Duration = Duration::from_millis(250);
/// 离开判定后的延迟释放窗口：期间任何一帧重新 inside 都取消收起。
/// 与 `LEAVE_HYSTERESIS` 取 max 而非累加，总延迟仍是 250ms，不牺牲收起手感。
pub const LEAVE_RELEASE: Duration = Duration::from_millis(150);
/// 展开面板边缘容差（逻辑像素；写入时按 scale 换算成物理像素）。
const REGION_PAD_LOGICAL: f64 = 10.0;
/// 折叠球视线方向（移植自 glasspill：球心→光标量化为 8 方向）。
const LOOK_DIRS: [&str; 8] = ["e", "se", "s", "sw", "w", "nw", "n", "ne"];
/// 「尚未投递」哨兵。
const LOOK_UNSET: u8 = 255;
/// 两次视线投递的最小间隔（方向变化本身已节流，这是兜底）。
const LOOK_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// 球心→光标偏移量化为 LOOK_DIRS 下标；dx=dy=0（光标正压球心）返回 None。
fn look_index(dx: i32, dy: i32) -> Option<u8> {
    if dx == 0 && dy == 0 {
        return None;
    }
    let angle = (dy as f64).atan2(dx as f64).to_degrees();
    Some((((angle + 22.5).rem_euclid(360.0)) / 45.0).floor() as u8 % 8)
}

/// 延迟释放判定（纯函数，单测覆盖）。
///
/// `last_in_ago`：距最后一次 inside 的时长；`pending`：本次 leave 已等待的时长。
/// 两者取「都满足」而不是把两个时长相加，避免退化成 400ms 的迟钝收起。
fn release_ready(last_in_ago: Option<Duration>, pending: Duration) -> bool {
    if pending < LEAVE_RELEASE {
        return false;
    }
    last_in_ago
        .map(|d| d >= LEAVE_HYSTERESIS)
        .unwrap_or(true)
}

static CTX: Mutex<Option<Arc<HookCtx>>> = Mutex::new(None);
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

struct HookCtx {
    app: AppHandle,
    state: Arc<SharedState>,
    /// 上一次采样点（物理屏幕坐标）。段判的 p0；首次采样退化为点判。
    last_pt: Mutex<Option<(i32, i32)>>,
    /// 最近一次「判 leave」的起点；Some = 收起正在延迟释放中。
    leave_since: Mutex<Option<Instant>>,
    /// 展开态已发出过收起请求，等 `collapse_panel` 复位，避免每帧重复 emit。
    collapse_sent: AtomicBool,
    island_last_in: Mutex<Option<Instant>>,
    island_last_toggle: Mutex<Instant>,
    /// 最近投递的视线方向下标（LOOK_UNSET = 尚未投递）。
    look_dir: AtomicU8,
    look_last_emit: Mutex<Option<Instant>>,
}

pub fn is_installed() -> bool {
    HOOK_INSTALLED.load(Ordering::Relaxed)
}

/// 窗口原点（物理屏幕坐标）。**只在写入侧调用**，钩子回调里绝不碰 Win32。
fn window_origin(hwnd: isize) -> Option<(i32, i32)> {
    if hwnd == 0 {
        return None;
    }
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut rect) }.is_err() {
        return None;
    }
    Some((rect.left, rect.top))
}

/// 把冻结的区域写进共享状态（唯一写入点），并落一行可统计的区域变更日志。
fn write_region(state: &SharedState, rect: ScreenRect, src: &str) {
    let mut slot = state.hit_region.lock().unwrap();
    let generation = slot.generation + 1;
    let expanded = state.island_expanded.load(Ordering::Relaxed);
    *slot = HitRegion { rect, generation };
    winplat::log::log(&format!(
        "cursor: region gen={generation} rect=({},{},{},{}) expanded={expanded} src={src}",
        rect.left, rect.top, rect.right, rect.bottom
    ));
}

/// 冻结球心的物理坐标，供钩子算 `ball-look`（视线不是判定，但同样不再在回调里取窗口 rect）。
fn freeze_ball_center(state: &SharedState, origin: (i32, i32), scale: f64) {
    let (bx, by) = *state.ball_center.lock().unwrap();
    let center = if bx > 0.0 || by > 0.0 {
        Some((
            origin.0 + (bx * scale).round() as i32,
            origin.1 + (by * scale).round() as i32,
        ))
    } else {
        None
    };
    *state.ball_center_screen.lock().unwrap() = center;
}

/// 重算冻结区域：**Rust 侧唯一权威**。
///
/// 展开态强制并入整面板（并外扩 10px），因此前端按内容包围盒上报的小矩形
/// 无论何时到达都不可能把区域缩小——这就是「展开瞬间被二次缩小」的结构性修复。
pub fn refresh_region(state: &SharedState, src: &str) {
    refresh_region_inner(state, src, false);
}

/// 展开后的精确刷新：`place()` 刚改过窗口，Win32 rect 可能晚一拍才更新。
/// 与展开前的保守区域取并集，保证这次刷新**只增不减**。
pub fn refresh_region_keep(state: &SharedState, src: &str) {
    refresh_region_inner(state, src, true);
}

fn refresh_region_inner(state: &SharedState, src: &str, keep_previous: bool) {
    let hwnd = state.island_hwnd.load(Ordering::Relaxed);
    let scale = *state.island_scale.lock().unwrap();
    let Some(origin) = window_origin(hwnd) else {
        return;
    };
    if scale <= 0.0 {
        return;
    }
    let expanded = state.island_expanded.load(Ordering::Relaxed);
    let island = ScreenRect::from_logical(origin, *state.island_hotzone.lock().unwrap(), scale);
    let ball = ScreenRect::from_logical(origin, *state.ball_hotzone.lock().unwrap(), scale);
    freeze_ball_center(state, origin, scale);

    let mut rect = island.union(ball);
    if expanded {
        let panel = ScreenRect::from_size(origin, PANEL_W, PANEL_H, scale);
        let pad = (REGION_PAD_LOGICAL * scale).round() as i32;
        rect = rect.union(panel).inflate(pad);
    }
    if keep_previous {
        rect = state.hit_region.lock().unwrap().rect.union(rect);
    }
    write_region(state, rect, src);
}

/// 展开**改窗口之前**先写一次保守区域。
///
/// `Anchor::Island` 是水平居中：420→720 会把窗口左上角左移 `(720-420)/2 * scale`。
/// 若只按旧原点写整面板，place 完成前的那几毫秒新面板左半边会落在区域外。
/// 因此这里把「旧原点面板」与「新原点面板」取并集，place 后再精确刷新。
pub fn pre_expand_region(state: &SharedState) {
    let hwnd = state.island_hwnd.load(Ordering::Relaxed);
    let scale = *state.island_scale.lock().unwrap();
    let Some((left, top)) = window_origin(hwnd) else {
        return;
    };
    if scale <= 0.0 {
        return;
    }
    let shift = ((PANEL_W - crate::shell::ISLAND_W) * scale / 2.0).round() as i32;
    let now = ScreenRect::from_size((left, top), PANEL_W, PANEL_H, scale);
    let after = ScreenRect::from_size((left - shift, top), PANEL_W, PANEL_H, scale);
    let island = ScreenRect::from_logical(
        (left, top),
        *state.island_hotzone.lock().unwrap(),
        scale,
    );
    let ball = ScreenRect::from_logical((left, top), *state.ball_hotzone.lock().unwrap(), scale);
    let pad = (REGION_PAD_LOGICAL * scale).round() as i32;
    let rect = now
        .union(after)
        .union(island)
        .union(ball)
        .inflate(pad);
    write_region(state, rect, "pre-expand");
}

/// 收起时清空冻结区域（代次仍递增），等前端按折叠形态重报后重建。
pub fn clear_region(state: &SharedState, src: &str) {
    *state.ball_center_screen.lock().unwrap() = None;
    write_region(state, ScreenRect::default(), src);
}

/// 在主线程安装钩子（须在 `run` 的事件循环启动前，即 `setup` 内调用）。
pub fn install(app: AppHandle, state: Arc<SharedState>) {
    // 启动期第一份冻结区域：lib.rs 的 cache_hwnd 已把 island_hotzone 铺成启动尺寸、
    // island_scale / island_hwnd 也已就绪，这里换算一次即可。
    refresh_region(&state, "install");

    *CTX.lock().unwrap() = Some(Arc::new(HookCtx {
        app,
        state,
        last_pt: Mutex::new(None),
        leave_since: Mutex::new(None),
        collapse_sent: AtomicBool::new(false),
        island_last_in: Mutex::new(None),
        island_last_toggle: Mutex::new(Instant::now() - Duration::from_secs(10)),
        look_dir: AtomicU8::new(LOOK_UNSET),
        look_last_emit: Mutex::new(None),
    }));

    // WH_MOUSE_LL 不要求模块句柄：hMod 传空，threadId=0 表示全局。
    let hook =
        unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse_proc), Default::default(), 0) };
    match hook {
        Ok(_) => {
            HOOK_INSTALLED.store(true, Ordering::Relaxed);
            winplat::log::log("cursor: WH_MOUSE_LL installed (position-only, single source)");
        }
        Err(e) => winplat::log::log(&format!("cursor: WH_MOUSE_LL install failed: {e}")),
    }
}

unsafe extern "system" fn ll_mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_MOUSEMOVE {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        // try_lock：钩子回调绝不能为等一把锁而阻塞输入管线。
        if let Ok(guard) = CTX.try_lock() {
            if let Some(ctx) = guard.as_ref() {
                ctx.process(POINT {
                    x: info.pt.x,
                    y: info.pt.y,
                });
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

impl HookCtx {
    fn process(&self, pt: POINT) {
        let now = Instant::now();
        let p1 = (pt.x, pt.y);
        let p0 = self.last_pt.lock().unwrap().replace(p1).unwrap_or(p1);

        // 只读冻结区域：这是钩子唯一承认的事实源。
        let region = *self.state.hit_region.lock().unwrap();
        let inside = region.rect.intersects_segment(p0, p1);
        let via = if inside && !region.rect.contains(p1.0, p1.1) {
            "segment"
        } else {
            "point"
        };
        let expanded = self.state.island_expanded.load(Ordering::Relaxed);
        if !expanded {
            self.collapse_sent.store(false, Ordering::Relaxed);
        }

        let trace = format!(
            "t={} p0=({},{}) p1=({},{}) gen={} region=({},{},{},{}) via={} expanded={expanded}",
            epoch_ms(),
            p0.0,
            p0.1,
            p1.0,
            p1.1,
            region.generation,
            region.rect.left,
            region.rect.top,
            region.rect.right,
            region.rect.bottom,
            via
        );

        // ---- island：穿透切换 ----
        // 注意：**展开态不豁免**。光标离开面板仍然是收起面板的唯一路径。
        // 区别只在于：展开态确认 leave 后不翻 passthrough，而是发 hover=false
        // 让前端走 collapse_panel，由它统一恢复穿透。
        if self.state.island_visible.load(Ordering::Relaxed) {
            let transparent = self.state.island_transparent.load(Ordering::Relaxed);
            if inside {
                if self.leave_since.lock().unwrap().take().is_some() {
                    winplat::log::log(&format!("cursor: leave-cancelled {trace}"));
                }
                *self.island_last_in.lock().unwrap() = Some(now);
                let cooled =
                    now.duration_since(*self.island_last_toggle.lock().unwrap()) >= ENTER_DEBOUNCE;
                if transparent && cooled && !self.collapse_sent.load(Ordering::Relaxed) {
                    self.apply_island(false, now);
                }
            } else if !transparent && !self.collapse_sent.load(Ordering::Relaxed) {
                let mut leave = self.leave_since.lock().unwrap();
                if leave.is_none() {
                    *leave = Some(now);
                    winplat::log::log(&format!("cursor: leave-decision {trace}"));
                }
                let since = match *leave {
                    Some(t) => t,
                    None => now,
                };
                drop(leave);

                let last_in_ago = self
                    .island_last_in
                    .lock()
                    .unwrap()
                    .map(|t| now.duration_since(t));
                let pending = now.duration_since(since);
                if release_ready(last_in_ago, pending) {
                    *self.leave_since.lock().unwrap() = None;
                    winplat::log::log(&format!(
                        "cursor: leave-confirmed {trace} pending_ms={}",
                        pending.as_millis()
                    ));
                    if expanded {
                        // passthrough 保持 false，展开→收起之间零翻转。
                        self.collapse_sent.store(true, Ordering::Relaxed);
                        let _ = self.app.emit("island-hover", false);
                        winplat::log::log(
                            "cursor: expanded leave -> island-hover=false (passthrough unchanged)",
                        );
                    } else {
                        self.apply_island(true, now);
                    }
                }
            }
        }

        // ---- 折叠球视线：球心是写入侧冻结的物理坐标 ----
        if let Some((cx, cy)) = *self.state.ball_center_screen.lock().unwrap() {
            let (dx, dy) = (pt.x - cx, pt.y - cy);
            if let Some(idx) = look_index(dx, dy) {
                if self.look_dir.load(Ordering::Relaxed) != idx {
                    let cooled = self
                        .look_last_emit
                        .lock()
                        .unwrap()
                        .map(|t| now.duration_since(t) >= LOOK_MIN_INTERVAL)
                        .unwrap_or(true);
                    if cooled {
                        self.look_dir.store(idx, Ordering::Relaxed);
                        *self.look_last_emit.lock().unwrap() = Some(now);
                        let _ = self.app.emit("ball-look", LOOK_DIRS[idx as usize]);
                    }
                }
            }
        }
    }

    fn apply_island(&self, transparent: bool, now: Instant) {
        winplat::log::log(if transparent {
            "cursor: island -> passthrough=true (leave)"
        } else {
            "cursor: island -> passthrough=false (enter)"
        });
        self.state
            .island_transparent
            .store(transparent, Ordering::Relaxed);
        *self.island_last_toggle.lock().unwrap() = now;
        if transparent {
            *self.island_last_in.lock().unwrap() = None;
        }
        let app = self.app.clone();
        let _ = self.app.run_on_main_thread(move || {
            if let Some(win) = app.get_webview_window("island") {
                let _ = win.set_ignore_cursor_events(transparent);
            }
        });
        // true = 光标在药丸上（前端 hover 放大）；false = 收拢。
        let _ = self.app.emit("island-hover", !transparent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_index_quantizes_eight_directions() {
        assert_eq!(look_index(0, 0), None);
        assert_eq!(look_index(10, 0), Some(0)); // e
        assert_eq!(look_index(0, 10), Some(2)); // s
        assert_eq!(look_index(-10, 0), Some(4)); // w
        assert_eq!(look_index(0, -10), Some(6)); // n
    }

    #[test]
    fn release_waits_for_pending_window() {
        let long_ago = Some(Duration::from_millis(300));
        assert!(!release_ready(long_ago, Duration::from_millis(100)));
        assert!(release_ready(long_ago, Duration::from_millis(150)));
    }

    #[test]
    fn release_respects_leave_hysteresis() {
        let just_in = Some(Duration::from_millis(50));
        assert!(!release_ready(just_in, Duration::from_millis(300)));
        let long_ago = Some(Duration::from_millis(260));
        assert!(release_ready(long_ago, Duration::from_millis(200)));
    }

    #[test]
    fn release_without_any_inside_history_is_allowed() {
        assert!(release_ready(None, Duration::from_millis(150)));
        assert!(!release_ready(None, Duration::from_millis(0)));
    }

    #[test]
    fn release_window_never_exceeds_perceptible_budget() {
        // 硬约束：不得靠把释放窗口拉长来「看起来修好」。
        assert!(LEAVE_RELEASE.as_millis() <= 400);
        assert!(LEAVE_HYSTERESIS.as_millis() <= 400);
        // 且两者取 max，不累加：250ms 后即可收起。
        assert!(release_ready(
            Some(Duration::from_millis(251)),
            Duration::from_millis(151)
        ));
    }
}

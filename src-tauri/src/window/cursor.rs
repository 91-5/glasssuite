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

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetWindowRect, SetWindowsHookExW, MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_MOUSEMOVE,
};

use crate::state::{Hotzone, SharedState};

/// 进入热区防抖：避免擦边而过就取消穿透。
const ENTER_DEBOUNCE: Duration = Duration::from_millis(80);
/// 离开热区滞回：防快速进出时样式高频切换。
const LEAVE_HYSTERESIS: Duration = Duration::from_millis(250);
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

static CTX: Mutex<Option<Arc<HookCtx>>> = Mutex::new(None);
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

struct HookCtx {
    app: AppHandle,
    state: Arc<SharedState>,
    island_last_in: Mutex<Option<Instant>>,
    island_last_toggle: Mutex<Instant>,
    /// 最近投递的视线方向下标（LOOK_UNSET = 尚未投递）。
    look_dir: AtomicU8,
    look_last_emit: Mutex<Option<Instant>>,
}

pub fn is_installed() -> bool {
    HOOK_INSTALLED.load(Ordering::Relaxed)
}

/// 在主线程安装钩子（须在 `run` 的事件循环启动前，即 `setup` 内调用）。
pub fn install(app: AppHandle, state: Arc<SharedState>) {
    *CTX.lock().unwrap() = Some(Arc::new(HookCtx {
        app,
        state,
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
    // ---- 诊断（dp 大脑 A1/A5）：在系统投递前逐层记录按键事件 ----
    // 钩子是最早能看到真实鼠标按键的地方：若这里都没有 WM_LBUTTONDOWN，
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

impl HookCtx {
    fn process(&self, pt: POINT) {
        let now = Instant::now();
        let in_island = self.hit_test(pt);

        // ---- island：穿透切换 ----
        // 注意：**展开态不豁免**。glasspill 原版没有 expanded 门控——展开后光标
        // 离开面板（超过 LEAVE_HYSTERESIS）恰恰是收起面板的唯一路径。一旦在这
        // 里加 `!expanded` 短路，前端永远收不到 island-hover=false，面板卡死。
        if self.state.island_visible.load(Ordering::Relaxed) {
            let transparent = self.state.island_transparent.load(Ordering::Relaxed);
            if in_island {
                *self.island_last_in.lock().unwrap() = Some(now);
                let cooled =
                    now.duration_since(*self.island_last_toggle.lock().unwrap()) >= ENTER_DEBOUNCE;
                if transparent && cooled {
                    self.apply_island(false, now);
                }
            } else if !transparent {
                let cooled = self
                    .island_last_in
                    .lock()
                    .unwrap()
                    .map(|t| now.duration_since(t) >= LEAVE_HYSTERESIS)
                    .unwrap_or(true);
                if cooled {
                    self.apply_island(true, now);
                }
            }
        }

        // ---- 折叠球视线：球心→光标量化为 8 方向，仅方向变化时投递 ----
        if let Some((cx, cy)) = self.ball_center_phys() {
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

    /// 热区是 CSS 逻辑像素；窗口原点现取（窗口移动/DPI 变化后自动跟随），
    /// 缩放因子由 `set_hotzone` 命令缓存。
    ///
    /// 岛的命中 = 药丸热区 ∪ 折叠球热区：收起时只有球可见，展开/悬停时只有药丸可见，
    /// 并集让两种形态共用同一条穿透逻辑（球点不到就展不开）。
    fn hit_test(&self, pt: POINT) -> bool {
        let s = &self.state;
        let scale = *s.island_scale.lock().unwrap();
        let hwnd = s.island_hwnd.load(Ordering::Relaxed);
        hit_window(hwnd, &s.island_hotzone.lock().unwrap(), scale, pt)
            || hit_window(hwnd, &s.ball_hotzone.lock().unwrap(), scale, pt)
    }

    /// 球心换算到物理屏幕坐标（窗口原点现取，DPI/移动后自动跟随）。
    /// 热区尚未上报（球不可见）时返回 None，避免起步阶段朝 0,0 乱投递。
    fn ball_center_phys(&self) -> Option<(i32, i32)> {
        let hwnd = self.state.island_hwnd.load(Ordering::Relaxed);
        if hwnd == 0 {
            return None;
        }
        let (bx, by) = *self.state.ball_center.lock().unwrap();
        if bx <= 0.0 && by <= 0.0 {
            return None;
        }
        let scale = *self.state.island_scale.lock().unwrap();
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut rect) }.is_err() {
            return None;
        }
        Some((
            rect.left + (bx * scale).round() as i32,
            rect.top + (by * scale).round() as i32,
        ))
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

fn hit_window(hwnd: isize, hz: &Hotzone, scale: f64, pt: POINT) -> bool {
    if hwnd == 0 || scale <= 0.0 || hz.w <= 0.0 || hz.h <= 0.0 {
        return false;
    }
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut rect) }.is_err() {
        return false;
    }
    let left = rect.left + (hz.x * scale).round() as i32;
    let top = rect.top + (hz.y * scale).round() as i32;
    let right = left + (hz.w * scale).round() as i32;
    let bottom = top + (hz.h * scale).round() as i32;
    pt.x >= left && pt.x < right && pt.y >= top && pt.y < bottom
}

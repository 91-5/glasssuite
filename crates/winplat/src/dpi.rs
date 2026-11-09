//! DPI 感知：进程级声明，全仓唯一入口。
//!
//! **为什么必须在最早调用**：`SetProcessDpiAwarenessContext` 只对「此后创建」的窗口生效，
//! 且一旦窗口已存在再调用会失败（`ERROR_ACCESS_DENIED`）。旧 glasspill 主进程漏了这一步，
//! 结果窗口在 125% / 150% 缩放下按虚拟像素摆放，与任务栏几何（物理像素）对不齐。
//!
//! **为什么只允许一次**：进程内重复调用不报错但语义混乱（后一次可能被忽略），且让
//! 「谁是真相源」变得不可追溯。这里用 `AtomicBool` 守住，第二次调用返回 `false`。

use std::sync::atomic::{AtomicBool, Ordering};

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// 声明进程为 Per-Monitor-Aware V2。
///
/// 返回 `Ok(true)` 表示本次调用完成声明；`Ok(false)` 表示此前已声明过（幂等）。
pub fn init_per_monitor_v2() -> Result<bool, String> {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return Ok(false);
    }
    imp::set_per_monitor_v2()
}

/// 查询当前进程的 DPI 感知上下文（调试用）。
pub fn current_context() -> Option<String> {
    imp::current_context()
}

#[cfg(windows)]
mod imp {
    use windows::Win32::UI::HiDpi::{
        GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    pub fn set_per_monitor_v2() -> Result<bool, String> {
        // SAFETY: 只传常量，不持有任何指针；失败用 Result 上报，不 panic。
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
            .map(|()| true)
            .map_err(|e| format!("SetProcessDpiAwarenessContext(PER_MONITOR_V2) 失败：{e}"))
    }

    pub fn current_context() -> Option<String> {
        // SAFETY: 无参调用，返回值是进程内不透明句柄值，仅作比较/打印。
        let ctx = unsafe { GetThreadDpiAwarenessContext() };
        Some(format!("{:?}", ctx))
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn set_per_monitor_v2() -> Result<bool, String> {
        Err("DPI 感知仅在 Windows 上可用".into())
    }

    pub fn current_context() -> Option<String> {
        None
    }
}

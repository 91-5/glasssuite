//! ⛔⛔⛔ 禁止启用（DEAD CODE — REFERENCE ONLY）⛔⛔⛔
//!
//! 本模块是 v0.1 三轮 PoC 的 B 路实现（直改任务栏 Accent/backdrop），已实测否决：
//! - apply_accent（SetWindowCompositionAttribute）在 build 26300 上触发粘性合成模式切换，
//!   亮度 235.9→216.7，DWMSBT_NONE / ACCENT_DISABLED 均无法还原，会话内不可逆。
//! - 产品链（lib.rs → main）零调用点，不进入 shipped 二进制执行路径。
//! - 保留原因：仅作为 PoC bins（src/bin/accent_probe.rs、overlay_poc.rs）的编译依赖
//!   与历史证据链（artifacts/taskbar_poc_report.md）。
//!
//! ⚠️ 任何新代码不得调用本模块。任务栏状态异常 → 重启 explorer.exe 即还原。
//! ⚠️ 处置记录：AgnesCode REVIEW-OC-20261027-001 advisory A-2；v0.2.x 卡
//!    OC-20261029-001 Deliverable 4 选「禁注」而非删除，保住 PoC bins 编译。
// ---- 以下为原文件头 ----
//! 任务栏玻璃质感 —— 合成器 Accent 材质（`SetWindowCompositionAttribute`）。
//!
//! 选型依据见 `artifacts/taskbar_poc_report.md`：
//! - A 路（`DWMWA_SYSTEMBACKDROP_TYPE`）：属性写入成功但对任务栏不渲染（PoC v3 实测，
//!   可见任务栏上六个工况像素零响应），否决。
//! - B 路（Accent）：v2 在透明态任务栏上实测到 Acrylic 模糊真实渲染，确认管线可用。
//!
//! 关键事实（v3 实测得出，修正上一手的错误假设）：
//! - `ACCENT_DISABLED`(0) **不是** explorer 的默认态。写成 DISABLED 会让任务栏变成
//!   不透明纯色底，XAML 的半透明背景不再透出任何 backdrop——看起来像「坏了」。
//! - explorer 的默认态近似 `ACCENT_ENABLE_GRADIENT`(1) + color 0；Accent 材质只在
//!   XAML 半透明背景下可见。

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, FindWindowW};

pub const WCA_ACCENT_POLICY: i32 = 19;

pub const ACCENT_DISABLED: i32 = 0;
pub const ACCENT_ENABLE_GRADIENT: i32 = 1;
pub const ACCENT_ENABLE_TRANSPARENTGRADIENT: i32 = 2;
pub const ACCENT_ENABLE_BLURBEHIND: i32 = 3;
pub const ACCENT_ENABLE_ACRYLICBLURBEHIND: i32 = 4;

/// explorer 任务栏的近似默认态（Accent(1) GRADIENT + color 0）。
/// 退出/还原时写回这个状态；explorer.exe 重启是最终兜底（Accent 是运行时态，不落盘）。
pub const ACCENT_DEFAULT_STATE: i32 = ACCENT_ENABLE_GRADIENT;
pub const ACCENT_DEFAULT_COLOR: u32 = 0;

#[repr(C)]
struct AccentPolicy {
    accent_state: i32,
    accent_flags: i32,
    gradient_color: u32,
    animation_id: i32,
}

#[repr(C)]
struct WindowCompositionAttribData {
    attrib: i32,
    pv_data: *mut c_void,
    cb_data: usize,
}

type PfnSetWindowCompositionAttribute =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttribData) -> i32;

/// 动态解析 `SetWindowCompositionAttribute`。
///
/// 该函数是 user32 的未文档化导出，Windows SDK 不提供导入库；
/// 静态链接会在 LNK2019 失败，且未来系统版本可能移除。
/// 解析不到就如实返回 None，不静默降级。
fn swca() -> Option<PfnSetWindowCompositionAttribute> {
    static PFN: OnceLock<Option<PfnSetWindowCompositionAttribute>> = OnceLock::new();
    *PFN.get_or_init(|| unsafe {
        let user32 =
            windows::Win32::System::LibraryLoader::GetModuleHandleW(w!("user32.dll")).ok()?;
        let addr = windows::Win32::System::LibraryLoader::GetProcAddress(
            user32,
            s!("SetWindowCompositionAttribute"),
        )?;
        Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            PfnSetWindowCompositionAttribute,
        >(addr))
    })
}

/// 施加 Accent 材质。`gradient_color` 为 AABBGGRR。
/// `accent_flags=2` 仅作用于窗口矩形本身，不溢到子窗口。
pub fn apply_accent(hwnd: HWND, state: i32, gradient_color: u32) -> bool {
    let Some(f) = swca() else { return false };
    let mut policy = AccentPolicy {
        accent_state: state,
        accent_flags: 2,
        gradient_color,
        animation_id: 0,
    };
    let mut data = WindowCompositionAttribData {
        attrib: WCA_ACCENT_POLICY,
        pv_data: &mut policy as *mut AccentPolicy as *mut c_void,
        cb_data: std::mem::size_of::<AccentPolicy>(),
    };
    unsafe { f(hwnd, &mut data) != 0 }
}

/// 还原到 explorer 近似默认态。
pub fn restore_default(hwnd: HWND) -> bool {
    apply_accent(hwnd, ACCENT_DEFAULT_STATE, ACCENT_DEFAULT_COLOR)
}

/// 找出所有任务栏窗口：主屏 `Shell_TrayWnd` + 每个副屏一个 `Shell_SecondaryTrayWnd`。
/// 双屏必须都处理，否则副屏任务栏保持原样、观感割裂。
pub fn find_taskbars() -> Vec<(HWND, &'static str)> {
    let mut out = Vec::new();
    unsafe {
        if let Ok(h) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) {
            if !h.0.is_null() {
                out.push((h, "Shell_TrayWnd"));
            }
        }
        // FindWindowExW 逐个枚举同窗口类的后续窗口（副屏任务栏可能不止一个）
        let mut after = HWND::default();
        loop {
            let h = match FindWindowExW(
                None,
                Some(after),
                w!("Shell_SecondaryTrayWnd"),
                PCWSTR::null(),
            ) {
                Ok(h) if !h.0.is_null() => h,
                _ => break,
            };
            out.push((h, "Shell_SecondaryTrayWnd"));
            after = h;
        }
    }
    out
}

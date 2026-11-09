//! GlassSuite 平台层（`winplat`）。
//!
//! **职责**：把 Windows 平台能力收敛成唯一实现，任何窗口（island / dock / startmenu）
//! 都从这里取，不允许各自另写一套。这是合并两套旧代码后消除「两套任务栏几何、
//! 两套全屏语义、两套 DPI 处理」的落点。
//!
//! **边界**：
//! - 纯计算（矩形换算、居中/贴底、幻影矩形判定）放在不依赖 Win32 的模块里，可单测；
//! - 需要操作系统调用的能力（任务栏枚举、DPI、ExStyle、单实例、日志、配置）在
//!   `cfg(windows)` 下实现，非 Windows 平台只暴露纯计算部分，保证 CI 的
//!   `cargo test --workspace` 在 Linux 上也能跑通纯逻辑测试。
//!
//! **纪律**：`SetProcessDpiAwarenessContext` 全仓只允许在此 crate 调用一次，且必须在
//! 任何窗口创建之前完成（旧 glasspill 主进程漏了这一步，导致窗口在高 DPI 下错位）。

pub mod geom;

pub use geom::Rect;

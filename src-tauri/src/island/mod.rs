//! 岛（island）功能域。
//!
//! 每个子模块是一个「采样/订阅线程 + Tauri 事件」，界面只订阅事件，不自己定时轮询 IPC：
//! 轮询放前端会随窗口数量线性增长，且窗口隐藏时照样空转。
//!
//! 需要联网或读取隐私数据的模块默认关闭（见 `crate::settings`）。

pub mod metrics;
pub mod network;
pub mod weather;

/// 前端诊断通道：让 UI 侧的关键事件也进日志，便于排查「后端有数据但界面没反应」。
#[tauri::command]
pub fn log_frontend(msg: String) {
    winplat::log::log(&format!("[ui] {msg}"));
}

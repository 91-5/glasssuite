//! 守卫意图文件写入器（OC-20261118-002，dp 方案 B）。
//!
//! App 只写意图，不直接 ShowWindow / 写注册表 HideIcons——避免和 250ms 守卫打架。
//! 意图文件 `%APPDATA%\com.glasssuite.app\guard_state.json`：
//! ```json
//! { "taskbar_hidden": true, "desktop_icons_hidden": true, "heartbeat": 1731900000, "writer_pid": 12345 }
//! ```
//! `heartbeat` 是 Unix 秒（整数）：零新增依赖，守卫侧 `FromUnixTimeSeconds` 即可比较。
//! 每 5s 刷新心跳；app 崩/退出后心跳停更，守卫 10s 超时 → 全显示（fail-safe）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 守卫读取的意图结构。字段语义见模块文档。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuardIntent {
    pub taskbar_hidden: bool,
    pub desktop_icons_hidden: bool,
    pub heartbeat: u64,
    pub writer_pid: u32,
}

/// 进程内当前意图（不含心跳，心跳由写盘时填当前时间）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IntentFlags {
    pub taskbar_hidden: bool,
    pub desktop_icons_hidden: bool,
}

static FLAGS: OnceLock<Mutex<IntentFlags>> = OnceLock::new();

fn flags() -> &'static Mutex<IntentFlags> {
    FLAGS.get_or_init(|| {
        Mutex::new(IntentFlags {
            // 与默认 settings.control 对齐：岛/Dock 开，任务栏/图标不隐藏。
            taskbar_hidden: false,
            desktop_icons_hidden: false,
        })
    })
}

pub fn intent_path() -> PathBuf {
    crate::settings::data_dir().join("guard_state.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 原子写意图文件（tmp + rename），heartbeat = 当前 Unix 秒。
fn write_current(flags: IntentFlags) -> std::io::Result<()> {
    let intent = GuardIntent {
        taskbar_hidden: flags.taskbar_hidden,
        desktop_icons_hidden: flags.desktop_icons_hidden,
        heartbeat: now_secs(),
        writer_pid: std::process::id(),
    };
    winplat::config::save_json(&intent_path(), &intent)
}

/// 更新意图的标志位并立刻落盘（前端切开关时调用）。
pub fn set_flags(taskbar_hidden: bool, desktop_icons_hidden: bool) -> std::io::Result<()> {
    let next = IntentFlags {
        taskbar_hidden,
        desktop_icons_hidden,
    };
    *flags().lock().unwrap() = next;
    write_current(next)
}

/// 启动心跳线程：每 5s 用当前 flags 重写意图（只更新 heartbeat）。
/// 返回 Arc 仅为把线程句柄持有者交回调用方（当前实现线程 detach，返回值保留扩展位）。
pub fn spawn_heartbeat() -> Arc<()> {
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let cur = *flags().lock().unwrap();
        if let Err(e) = write_current(cur) {
            winplat::log::log(&format!("guard intent: 心跳写失败: {e}"));
        }
    });
    Arc::new(())
}

/// 启动时初始化：写入一份当前意图（带心跳）。
pub fn initialize(taskbar_hidden: bool, desktop_icons_hidden: bool) {
    if let Err(e) = set_flags(taskbar_hidden, desktop_icons_hidden) {
        winplat::log::log(&format!("guard intent: 初始化写失败: {e}"));
    } else {
        winplat::log::log("guard intent: 已初始化");
    }
}

/// 退出时删除意图文件 → 守卫读到"文件不存在"立即走安全默认（全显示），
/// 不必等 10s 心跳超时。替代 001 的 `-Restore` spawn 方案（方案 B 对齐）。
pub fn clear_on_exit() {
    let p = intent_path();
    match std::fs::remove_file(&p) {
        Ok(()) => winplat::log::log("guard intent: 退出已清除（守卫将全显示）"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => winplat::log::log(&format!("guard intent: 退出清除失败: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_intent_json_shape_stable() {
        // 锁定字段名：守卫脚本按这些名字读，改动必须同步。
        let v = GuardIntent {
            taskbar_hidden: true,
            desktop_icons_hidden: false,
            heartbeat: 1_700_000_000,
            writer_pid: 42,
        };
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"taskbar_hidden\":true"));
        assert!(s.contains("\"desktop_icons_hidden\":false"));
        assert!(s.contains("\"heartbeat\":1700000000"));
        assert!(s.contains("\"writer_pid\":42"));
    }

    #[test]
    fn now_secs_is_plausible_unix_timestamp() {
        let n = now_secs();
        assert!(n > 1_600_000_000, "心跳时间戳应大于 2020 年: {n}");
    }
}

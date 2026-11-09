//! 系统通知摘要（**纯本地读取，不上报**）。
//!
//! 移植自参考仓 `glasspill/src-tauri/src/notification/*`，按本卡 D2 与隐私硬约束收敛：
//! - `db.rs`：`wpndatabase.db` 的 **WAL 快照只读复制**（db + `-wal`，不复制 `-shm`）。
//!   绝不直接打开活动库；临时副本读完立即删除。
//! - `listener.rs`：WinRT `UserNotificationListener` 事件（非 MSIX 打包下常不可用，静默降级）。
//! - `uia.rs`     ：UIA 兜底，覆盖部分不走 WinRT / 通知中心的来源，默认关闭。
//! - `coverage_matrix.rs` 是旧仓内部诊断物，按卡面要求**不入仓**。
//!
//! **隐私红线（卡面 §6 13）**：
//! - 只取 toast 的第一段文本作摘要标题；其余正文**不进入本进程的数据结构**，
//!   更不落盘、不进日志。原文由 Windows 自己的横幅展示。
//! - 日志只允许「事件类型 + 数量」：不写应用名、标题、正文、窗口类名。
//! - `settings.notifications.enabled` 默认 `false`，未启用时整个模块不启动。
//! - 三条路径在汇聚处去重，避免同一条通知被重复弹。

pub mod db;
pub mod listener;
pub mod uia;

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::settings::NotificationConfig;

/// 与 `packages/ipc-types` 的 `EVENTS.notification` 逐字一致。
pub const EVENT: &str = "notification";

/// 标题最大字符数（按 Unicode 字符，不切 UTF-8 字节）。前端还会再做一次 CSS 省略。
const TITLE_CHARS: usize = 60;

/// 去重窗口：同一来源在 8 秒内报出相同 (app, title) 只算一条。
const DEDUP_WINDOW: Duration = Duration::from_secs(8);

/// 内部采集结果。**没有 body 字段**——正文从一开始就不进我们的内存。
#[derive(Clone, Debug)]
pub struct RawNotification {
    pub app: String,
    pub title: String,
    pub arrived_at: u64,
    pub source: &'static str,
}

/// 发给前端的摘要条。只有应用名 + 截断标题 + 时间 + 来源类型。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationSummary {
    pub app: String,
    pub title: String,
    pub arrived_at: u64,
    pub source: &'static str,
}

impl From<RawNotification> for NotificationSummary {
    fn from(raw: RawNotification) -> Self {
        Self {
            app: raw.app,
            title: truncate(&raw.title, TITLE_CHARS),
            arrived_at: raw.arrived_at,
            source: raw.source,
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// 启动通知功能域。`enabled=false` 时**完全不启动任何线程**（B3「off 完全无反应」）。
pub fn spawn(handle: AppHandle, cfg: NotificationConfig) {
    if !cfg.enabled {
        winplat::log::log("notifications: 未启用，跳过（纯本地读取，不上报）");
        return;
    }

    std::thread::Builder::new()
        .name("glasssuite-notifications".into())
        .spawn(move || {
            let (tx, rx) = mpsc::channel::<RawNotification>();

            let db_started = db::spawn(tx.clone());
            let listener_started = listener::spawn(tx.clone());
            let uia_started = uia::spawn(tx, cfg.uia_fallback);
            // 只记「启用了哪几类事件源 + 数量」，不记任何内容。
            winplat::log::log(&format!(
                "notifications: sources started count={}",
                [db_started, listener_started, uia_started]
                    .iter()
                    .filter(|b| **b)
                    .count()
            ));

            let mut recent: VecDeque<(Instant, String)> = VecDeque::new();
            let mut keys = HashSet::<String>::new();

            while let Ok(item) = rx.recv() {
                let now = Instant::now();
                while let Some((t, _)) = recent.front() {
                    if now.duration_since(*t) >= DEDUP_WINDOW {
                        if let Some((_, k)) = recent.pop_front() {
                            keys.remove(&k);
                        }
                    } else {
                        break;
                    }
                }

                let key = format!("{}|{}", item.app, item.title);
                if !keys.insert(key.clone()) {
                    continue; // 三路重复上报
                }
                recent.push_back((now, key));

                let source = item.source;
                let summary: NotificationSummary = item.into();
                let _ = handle.emit(EVENT, summary);
                // 日志：事件类型 + 数量，无正文、无应用名。
                winplat::log::log(&format!("notifications: delivered source={source} count=1"));
            }
        })
        .expect("spawn notification aggregator");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_truncates_by_chars_not_bytes() {
        let raw = RawNotification {
            app: "app".into(),
            title: "一二三四五六七八九十".repeat(10),
            arrived_at: 1,
            source: "wpndatabase",
        };
        let s: NotificationSummary = raw.into();
        assert_eq!(s.title.chars().count(), TITLE_CHARS + 1); // 含省略号
        assert!(s.title.ends_with('…'));
        assert!(!s.title.contains("十十一"));
    }

    #[test]
    fn summary_never_carries_body_field() {
        // 序列化结果里不能出现正文字段——正文根本没进数据结构。
        let s: NotificationSummary = RawNotification {
            app: "app".into(),
            title: "标题".into(),
            arrived_at: 42,
            source: "uia-fallback",
        }
        .into();
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("body").is_none());
        assert_eq!(v["title"], "标题");
        assert_eq!(v["source"], "uia-fallback");
    }

    #[test]
    fn truncate_keeps_short_text_intact() {
        assert_eq!(truncate("短标题", TITLE_CHARS), "短标题");
    }
}

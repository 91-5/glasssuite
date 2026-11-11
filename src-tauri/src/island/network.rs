//! 网络状态 —— 30s 一次 HEAD 探测 + 延迟测量。
//!
//! **默认关闭**：`networkProbeUrl` 为空就不联网（旧版固定探测某个商业站点，
//! 开源版不能替用户决定往谁家发请求）。用户想显示网络灯就填一个自己认可的地址
//! （UI 面板或直接改 settings.json 均可）。
//!
//! 热加载（OC-20261110-003）：探测地址存在 `NetworkState` 槽里，常驻线程每轮读取；
//! `settings_update` 换槽即生效，关闭（置 null/空白）后停止探测，重新打开 ~1s 内
//! 首探。未配置时线程只空转轮询开关，**零外联**。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

/// 首次延迟：等窗口就绪。
const FIRST_DELAY: Duration = Duration::from_secs(10);
/// 探测周期。
const INTERVAL: Duration = Duration::from_secs(30);
/// 关闭态下轮询开关的节奏：保存配置后 ~1s 内首探。
const OFF_POLL: Duration = Duration::from_secs(1);

/// 进程内共享的网络探测开关（`settings.networkProbeUrl`）。写入方 `settings_update`，
/// 读取方是常驻探测线程。
#[derive(Debug, Default)]
pub struct NetworkState {
    url: Mutex<Option<String>>,
}

impl NetworkState {
    pub fn new(url: Option<String>) -> Self {
        Self {
            url: Mutex::new(url),
        }
    }

    /// 当前探测目标；`None`/空白串都视为关闭（UI 留空 = off，B4 开关语义）。
    pub fn current(&self) -> Option<String> {
        self.url
            .lock()
            .unwrap()
            .clone()
            .filter(|u| !u.trim().is_empty())
    }

    pub fn set(&self, url: Option<String>) {
        *self.url.lock().unwrap() = url;
    }
}

/// 启动常驻探测线程。未配置时只低频轮询开关，不产生任何请求。
pub fn spawn(handle: AppHandle, state: Arc<NetworkState>) {
    if state.current().is_some() {
        winplat::log::log("network: probe enabled, starting supervisor");
    } else {
        winplat::log::log("network: 未配置探测地址（默认不联网）；保存配置后自动生效");
    }
    std::thread::Builder::new()
        .name("glasssuite-network".into())
        .spawn(move || {
            std::thread::sleep(FIRST_DELAY);
            loop {
                let Some(url) = state.current() else {
                    std::thread::sleep(OFF_POLL);
                    continue;
                };

                let start = Instant::now();
                let online = ureq::head(&url)
                    .timeout(Duration::from_secs(5))
                    .call()
                    .is_ok();
                let latency = start.elapsed().as_millis() as u64;

                let _ = handle.emit(
                    "network-status",
                    serde_json::json!({ "online": online, "latency": latency }),
                );

                std::thread::sleep(INTERVAL);
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_off() {
        let st = NetworkState::new(None);
        assert!(st.current().is_none(), "默认不指向任何地址");
    }

    #[test]
    fn blank_url_means_off() {
        // UI 保存空串/纯空白 = 关闭开关，不能被当成有效探测目标。
        for blank in ["".to_string(), "   ".to_string(), "\t\n".to_string()] {
            let st = NetworkState::new(Some(blank));
            assert!(st.current().is_none());
        }
    }

    #[test]
    fn set_toggles_state_both_ways() {
        let st = NetworkState::new(None);
        st.set(Some("https://example.com".into()));
        assert_eq!(st.current().as_deref(), Some("https://example.com"));
        st.set(None);
        assert!(st.current().is_none(), "关闭后停止探测语义");
    }
}

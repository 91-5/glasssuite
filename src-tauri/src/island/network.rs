//! 网络状态 —— 30s 一次 HEAD 探测 + 延迟测量。
//!
//! **默认关闭**：`networkProbeUrl` 为空就不联网（旧版固定探测某个商业站点，
//! 开源版不能替用户决定往谁家发请求）。用户想显示网络灯就填一个自己认可的地址。

use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

/// 首次延迟：等窗口就绪。
const FIRST_DELAY: Duration = Duration::from_secs(10);
/// 探测周期。
const INTERVAL: Duration = Duration::from_secs(30);

pub fn spawn(handle: AppHandle, url: Option<String>) {
    let Some(url) = url.filter(|u| !u.trim().is_empty()) else {
        winplat::log::log("network: 未配置探测地址，跳过（默认不联网）");
        return;
    };

    std::thread::spawn(move || {
        std::thread::sleep(FIRST_DELAY);
        loop {
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
    });
}

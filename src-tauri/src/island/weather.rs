//! 天气 —— Open-Meteo（免 key），15min 刷新。
//!
//! **默认关闭**：`settings.json` 里没填 `weather` 就不联网（旧版把开发者自己的坐标写死了，
//! 开源版必须由用户自己填）。填了但网络失败时按 15min 重试，不做快速重试——
//! 天气不是实时交互功能，退避比抢时间重要。

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::settings::WeatherConfig;

/// 首次延迟：等窗口与 WebView 就绪，避免首帧事件无人订阅。
const FIRST_DELAY: Duration = Duration::from_secs(5);
/// 刷新周期。Open-Meteo 免费额度按请求计，15min 完全够用。
const REFRESH: Duration = Duration::from_secs(900);
/// 失败后的快速重试间隔：重启后首拉失败不该空窗 15 分钟（2026-11-14 实测）。
const QUICK_RETRY: Duration = Duration::from_secs(60);
/// 快速重试次数上限，超过后退回常规节奏（别对故障源打请求）。
const QUICK_RETRY_MAX: u32 = 3;
/// 单轮拉取的硬截止。ureq 的 timeout 不覆盖 DNS/connect 挂死
/// （ai/mod.rs 同款教训），放短命线程里等结果，超时放弃本轮。
const FETCH_DEADLINE: Duration = Duration::from_secs(30);

/// 归一化后的天气快照。只给界面需要的最小集合，不透传第三方 JSON 结构。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeatherSnapshot {
    /// 气温（摄氏度）。
    pub temp_c: f64,
    /// WMO 天气代码（https://open-meteo.com/en/docs 的 weathercode 表）。
    pub code: u32,
    /// 当地是否白天。
    pub is_day: bool,
}

pub fn spawn(handle: AppHandle, config: Option<WeatherConfig>) {
    let Some(cfg) = config else {
        winplat::log::log("weather: 未配置，跳过（默认不联网）");
        return;
    };

    std::thread::spawn(move || {
        // 线程 panic 不能静默死（2026-11-14：进程内无线程级 panic 钩子，
        // 静默死亡的表象就是「天气再也不更新」）。记日志后退出，重启应用恢复。
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            weather_loop(&handle, &cfg);
        }));
        if result.is_err() {
            winplat::log::log("weather: 线程 panic，已退出（重启应用恢复）");
        }
    });
}

fn weather_loop(handle: &AppHandle, cfg: &WeatherConfig) {
    std::thread::sleep(FIRST_DELAY);
    let mut failures: u32 = 0;
    loop {
        match fetch_with_deadline(cfg) {
            Ok(snapshot) => {
                let _ = handle.emit("weather", snapshot);
                winplat::log::log("weather: updated");
                failures = 0;
            }
            Err(e) => {
                failures += 1;
                winplat::log::log(&format!("weather: 拉取失败（连续 {failures} 次）：{e}"));
            }
        }
        let wait = if failures == 0 || failures > QUICK_RETRY_MAX {
            REFRESH
        } else {
            QUICK_RETRY
        };
        std::thread::sleep(wait);
    }
}

/// 硬截止包装：DNS/connect 挂死时 ureq 的 timeout 不生效
/// （ai/mod.rs:354 同款教训），放短命线程里等结果，超时放弃本轮。
/// 超时后阻塞线程会泄漏一个，量级（每 15min 至多一个）可接受。
fn fetch_with_deadline(cfg: &WeatherConfig) -> Result<WeatherSnapshot, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let cfg2 = cfg.clone();
    std::thread::Builder::new()
        .name("glasssuite-weather-fetch".into())
        .spawn(move || {
            let _ = tx.send(fetch(&cfg2));
        })
        .map_err(|e| format!("spawn: {e}"))?;
    match rx.recv_timeout(FETCH_DEADLINE) {
        Ok(res) => res,
        Err(_) => Err(format!(
            "硬截止超时（{}s），本轮放弃",
            FETCH_DEADLINE.as_secs()
        )),
    }
}

fn url_for(cfg: &WeatherConfig) -> String {
    let tz = cfg.timezone.as_deref().unwrap_or("auto");
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current_weather=true&timezone={}",
        cfg.latitude, cfg.longitude, tz
    )
}

/// 备选源（wttr.in，`format=j1` JSON）。Open-Meteo（OVH 线路）在部分网络下
/// TLS 握手超时（2026-11-14 实测：TCP 可达但 ClientHello 无响应，三种 TLS
/// 栈均复现），wttr.in 可达——主源失败时自动切换，恢复后仍优先主源。
fn fallback_url_for(cfg: &WeatherConfig) -> String {
    format!(
        "https://wttr.in/{},{}?format=j1",
        cfg.latitude, cfg.longitude
    )
}

fn fetch(cfg: &WeatherConfig) -> Result<WeatherSnapshot, String> {
    match fetch_primary(cfg) {
        Ok(s) => Ok(s),
        Err(primary_err) => match fetch_fallback(cfg) {
            Ok(s) => {
                winplat::log::log("weather: primary failed, fallback ok (wttr.in)");
                Ok(s)
            }
            Err(fallback_err) => Err(format!("{primary_err}; fallback: {fallback_err}")),
        },
    }
}

fn fetch_primary(cfg: &WeatherConfig) -> Result<WeatherSnapshot, String> {
    let body = ureq::get(&url_for(cfg))
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("HTTP: {e}"))?
        .into_string()
        .map_err(|e| format!("body: {e}"))?;
    parse(&body)
}

fn fetch_fallback(cfg: &WeatherConfig) -> Result<WeatherSnapshot, String> {
    let body = ureq::get(&fallback_url_for(cfg))
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("HTTP: {e}"))?
        .into_string()
        .map_err(|e| format!("body: {e}"))?;
    parse_fallback(&body)
}

/// 从 Open-Meteo 的 `current_weather` 里取三个字段。抽出来便于单测（无网络）。
fn parse(body: &str) -> Result<WeatherSnapshot, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("JSON: {e}"))?;
    let current = json
        .get("current_weather")
        .ok_or_else(|| "缺少 current_weather 字段".to_string())?;
    Ok(WeatherSnapshot {
        temp_c: current
            .get("temperature")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| "缺少 temperature".to_string())?,
        code: current
            .get("weathercode")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        is_day: current.get("is_day").and_then(|v| v.as_u64()).unwrap_or(1) == 1,
    })
}

/// wttr.in `format=j1`：`current_condition[0]` 的 `temp_C`（字符串）、
/// `weatherCode`（字符串）、`isday`（"yes"/"no"）。
fn parse_fallback(body: &str) -> Result<WeatherSnapshot, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("JSON: {e}"))?;
    let cc = json
        .get("current_condition")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| "缺少 current_condition".to_string())?;
    let temp_c = cc
        .get("temp_C")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| "缺少 temp_C".to_string())?;
    let code = cc
        .get("weatherCode")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let is_day = cc.get("isday").and_then(|v| v.as_str()) != Some("no");
    Ok(WeatherSnapshot {
        temp_c,
        code,
        is_day,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_current_weather() {
        let body = r#"{"current_weather":{"temperature":23.4,"weathercode":3,"is_day":1}}"#;
        let s = parse(body).unwrap();
        assert!((s.temp_c - 23.4).abs() < 1e-9);
        assert_eq!(s.code, 3);
        assert!(s.is_day);
    }

    #[test]
    fn parse_missing_field_is_error_not_panic() {
        assert!(parse(r#"{"lat":1}"#).is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn url_uses_configured_coords_not_builtin() {
        let cfg = WeatherConfig {
            latitude: 48.85,
            longitude: 2.35,
            timezone: Some("Europe/Paris".into()),
        };
        let url = url_for(&cfg);
        assert!(url.contains("latitude=48.85"));
        assert!(url.contains("longitude=2.35"));
        assert!(url.contains("timezone=Europe/Paris"));
    }

    #[test]
    fn fallback_url_uses_configured_coords() {
        let cfg = WeatherConfig {
            latitude: 21.27,
            longitude: 110.35,
            timezone: None,
        };
        let url = fallback_url_for(&cfg);
        assert!(url.starts_with("https://wttr.in/"));
        assert!(url.contains("21.27,110.35"));
    }

    /// wttr.in `format=j1` 的 current_condition 结构（字段名与 Open-Meteo 完全不同）。
    #[test]
    fn parse_fallback_current_condition() {
        let body = r#"{"current_condition":[{"temp_C":"27","weatherCode":"113","isday":"yes"}]}"#;
        let s = parse_fallback(body).unwrap();
        assert!((s.temp_c - 27.0).abs() < 1e-9);
        assert_eq!(s.code, 113);
        assert!(s.is_day);
    }

    #[test]
    fn parse_fallback_missing_field_is_error() {
        assert!(parse_fallback(r#"{"current_condition":[]}"#).is_err());
        assert!(parse_fallback("not json").is_err());
    }
}

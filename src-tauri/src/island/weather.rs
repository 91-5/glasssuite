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
        std::thread::sleep(FIRST_DELAY);
        loop {
            match fetch(&cfg) {
                Ok(snapshot) => {
                    let _ = handle.emit("weather", snapshot);
                    winplat::log::log("weather: updated");
                }
                Err(e) => winplat::log::log(&format!("weather: 拉取失败：{e}")),
            }
            std::thread::sleep(REFRESH);
        }
    });
}

fn url_for(cfg: &WeatherConfig) -> String {
    let tz = cfg.timezone.as_deref().unwrap_or("auto");
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current_weather=true&timezone={}",
        cfg.latitude, cfg.longitude, tz
    )
}

fn fetch(cfg: &WeatherConfig) -> Result<WeatherSnapshot, String> {
    let body = ureq::get(&url_for(cfg))
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("HTTP: {e}"))?
        .into_string()
        .map_err(|e| format!("body: {e}"))?;
    parse(&body)
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
}

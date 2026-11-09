//! 用户配置。
//!
//! **默认全部关闭**需要联网或读取隐私数据的功能（天气、网络探测、AI）。用户把想要
//! 的功能填进 `%APPDATA%\com.glasssuite.app\settings.json` 即可打开——首次运行会写一份
//! 全默认的模板，方便发现有哪些开关。
//!
//! 为什么要文件而不是设置面板：桌面美化工具的用户画像偏「愿意改配置文件」，
//! 先把配置落到文件、语义定清楚，面板可以后面再加，避免一上来就做一堆 UI 状态管理。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use winplat::config;

/// 天气配置。`latitude`/`longitude` 由用户自己填——**不内置任何人的位置**。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WeatherConfig {
    pub latitude: f64,
    pub longitude: f64,
    /// IANA 时区名，例如 `Asia/Shanghai`、`Europe/Berlin`。缺省按系统本地时间。
    #[serde(default)]
    pub timezone: Option<String>,
}

/// 应用配置。所有字段都有默认值：缺键 = 用默认（关闭）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// 卡片顶部显示天气。`null` = 关闭（默认，不联网）。
    #[serde(default)]
    pub weather: Option<WeatherConfig>,
    /// 网络探测地址（例如 `https://example.com`）。`null` = 关闭（默认，不联网）。
    #[serde(default, rename = "networkProbeUrl")]
    pub network_probe_url: Option<String>,
    /// AI 摘要服务地址（OpenAI 兼容）。`null` = 关闭（默认）。**不得内置任何私有网关。**
    #[serde(default, rename = "aiBaseUrl")]
    pub ai_base_url: Option<String>,
    /// AI 密钥。建议改用环境变量 `GLASSSUITE_AI_KEY`，避免把密钥写进磁盘。
    #[serde(default, rename = "aiApiKey")]
    pub ai_api_key: Option<String>,
}

/// 配置文件名。放在 `%APPDATA%\com.glasssuite.app\` 下。
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// 读配置；文件不存在时写一份默认模板并返回默认值。
pub fn load(data_dir: &Path) -> Settings {
    let file = path(data_dir);
    if let Some(s) = config::load_json::<Settings>(&file) {
        return s;
    }
    let defaults = Settings::default();
    if !file.exists() {
        // 写模板只是「让用户发现有哪些开关」，失败不影响启动。
        if let Err(e) = config::save_json(&file, &defaults) {
            winplat::log::log(&format!("写默认配置失败：{e}"));
        }
    } else {
        winplat::log::log("配置文件解析失败，本次使用默认值（文件保持原样）");
    }
    defaults
}

/// 把当前配置交给前端（只读）：岛据此决定显示哪些区块。
#[tauri::command]
pub fn settings_get(settings: tauri::State<'_, Settings>) -> Settings {
    settings.inner().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_writes_template_and_defaults_all_off() {
        let dir = std::env::temp_dir().join("gs-settings-default");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let s = load(&dir);
        assert_eq!(s, Settings::default());
        assert!(path(&dir).is_file(), "首次运行应写默认模板");
        assert!(s.weather.is_none(), "天气默认关闭");
        assert!(s.network_probe_url.is_none(), "网络探测默认关闭");
        assert!(s.ai_base_url.is_none(), "AI 默认不指向任何地址");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let dir = std::env::temp_dir().join("gs-settings-partial");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            path(&dir),
            br#"{"weather":{"latitude":31.2,"longitude":121.5}}"#,
        )
        .unwrap();

        let s = load(&dir);
        let w = s.weather.expect("天气应已启用");
        assert_eq!(w.latitude, 31.2);
        assert_eq!(w.timezone, None);
        assert!(s.network_probe_url.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

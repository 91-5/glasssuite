//! AI 配置 —— **默认 null、零外联、密钥不入库**。
//!
//! 与参考仓 `glasspill/src-tauri/src/ai/config.rs` 的关键差异（本卡 D3 红线）：
//! - `base_url` **默认 `None`**，且**禁止**回落 `apihub.agnes-ai.com`（旧仓私有网关）。
//!   配置里若仍指向该域名，直接判为未配置并记一行日志，不做静默替换。
//! - 密钥只来自 `settings.aiApiKey` 或环境变量 `GLASSSUITE_AI_KEY`；**绝不写进仓库、
//!   绝不进日志**；建议用环境变量而不是把密钥落在配置文件里。
//! - `GLASSSUITE_AI_ENABLED=0` 是总 kill switch。
//! - 未配置齐（enabled + base_url + key）时一切 AI 功能静默跳过。

use crate::settings::Settings;

pub const ENV_KEY: &str = "GLASSSUITE_AI_KEY";
pub const ENV_ENABLED: &str = "GLASSSUITE_AI_ENABLED";

/// 旧仓私有网关。出现即视为未配置，绝不作为默认值使用。
const FORBIDDEN_HOST: &str = "apihub.agnes-ai.com";

#[derive(Clone, Debug)]
pub struct AiConfig {
    pub enabled: bool,
    /// `None` = 没有任何外联目标。
    pub base_url: Option<String>,
    /// `None` = 只做本地规则，不调 API。
    pub api_key: Option<String>,
    pub model: String,
    pub timeout_secs: u64,
}

impl AiConfig {
    pub fn load(s: &Settings) -> Self {
        let enabled = match std::env::var(ENV_ENABLED) {
            Ok(v) => !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            ),
            Err(_) => true,
        };

        let base_url = s
            .ai_base_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .and_then(|u| {
                if u.contains(FORBIDDEN_HOST) {
                    winplat::log::log("ai: 配置指向私有网关，已忽略（不回落内置地址）");
                    None
                } else {
                    Some(u)
                }
            });

        let api_key = s
            .ai_api_key
            .clone()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| std::env::var(ENV_KEY).ok().filter(|k| !k.trim().is_empty()));

        Self {
            enabled,
            base_url,
            api_key,
            model: s
                .ai_model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "gpt-4o-mini".to_string()),
            timeout_secs: 20,
        }
    }

    /// 只有三项齐备才算「已配置」；否则 spawn 直接返回，零外联。
    pub fn is_configured(&self) -> bool {
        self.enabled && self.base_url.is_some() && self.api_key.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_unconfigured_and_have_no_url() {
        let cfg = AiConfig::load(&Settings::default());
        assert!(cfg.base_url.is_none(), "base_url 默认必须是 null");
        assert!(cfg.api_key.is_none());
        assert!(!cfg.is_configured());
    }

    #[test]
    fn private_gateway_is_never_a_fallback() {
        let s = Settings {
            ai_base_url: Some("https://apihub.agnes-ai.com/v1".into()),
            ai_api_key: Some("test-key".into()),
            ..Settings::default()
        };
        let cfg = AiConfig::load(&s);
        assert!(cfg.base_url.is_none());
        assert!(!cfg.is_configured(), "私有网关不得启用 AI");
    }

    #[test]
    fn explicit_user_endpoint_enables_ai() {
        let s = Settings {
            ai_base_url: Some("https://llm.example.com/v1".into()),
            ai_api_key: Some("test-key".into()),
            ..Settings::default()
        };
        let cfg = AiConfig::load(&s);
        assert!(cfg.is_configured());
        assert_eq!(cfg.base_url.as_deref(), Some("https://llm.example.com/v1"));
        assert_eq!(cfg.model, "gpt-4o-mini");
    }

    #[test]
    fn endpoint_without_key_stays_local() {
        let s = Settings {
            ai_base_url: Some("https://llm.example.com/v1".into()),
            ..Settings::default()
        };
        let cfg = AiConfig::load(&s);
        assert!(!cfg.is_configured(), "没有密钥不得外联");
    }
}

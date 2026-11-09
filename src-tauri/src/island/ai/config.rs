//! AI 配置 —— **默认 null、零外联、密钥不入库**。
//!
//! 与参考仓 `glasspill/src-tauri/src/ai/config.rs` 的关键差异（本卡 D3 红线）：
//! - **仓内没有任何内置 / 回落的 API 端点**：`base_url` 默认 `None`，只有用户显式填写的
//!   绝对 http(s) 地址才被接受。旧仓那个私有网关既不作为默认值，也不作为域名黑名单
//!   写进源码——把私有域名（哪怕拼成片段）留在公开仓库里本身就是卫生问题。
//! - 密钥只来自 `settings.aiApiKey` 或环境变量 `GLASSSUITE_AI_KEY`；**绝不写进仓库、
//!   绝不进日志**；建议用环境变量而不是把密钥落在配置文件里。
//! - `GLASSSUITE_AI_ENABLED=0` 是总 kill switch。
//! - 未配置齐（enabled + base_url + key）时一切 AI 功能静默跳过。

use crate::settings::Settings;

pub const ENV_KEY: &str = "GLASSSUITE_AI_KEY";
pub const ENV_ENABLED: &str = "GLASSSUITE_AI_ENABLED";

/// 只接受用户显式填写的绝对 http(s) 地址。仓内不存在任何默认端点。
///
/// 不做域名黑名单：用户自己填什么端点是用户的选择，把它写死在源码里反而会
/// 把私有基础设施信息带进公开仓库。安全性靠「默认没有端点」这条结构性约束。
fn usable_base_url(raw: &str) -> Option<String> {
    let url = raw.trim().trim_end_matches('/');
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = after_scheme.split('/').next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    Some(url.to_string())
}

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
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .and_then(usable_base_url);

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
    fn there_is_no_builtin_fallback_endpoint() {
        let cfg = AiConfig::load(&Settings::default());
        assert!(cfg.base_url.is_none(), "没有任何内置端点可以回落");
        assert!(!cfg.is_configured());
    }

    #[test]
    fn malformed_urls_are_rejected_instead_of_guessed_at() {
        for bad in ["not-a-url", "https://", "file:///tmp/x", "ftp://host/v1"] {
            assert_eq!(usable_base_url(bad), None, "{bad} 不该被当成可用端点");
        }
        assert_eq!(
            usable_base_url("  https://llm.example.com/v1/  "),
            Some("https://llm.example.com/v1".to_string())
        );
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

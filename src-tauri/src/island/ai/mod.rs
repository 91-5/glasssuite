//! AI 后台感知层：**未配置即零外联**。
//!
//! 从参考仓 `glasspill/src-tauri/src/ai/{mod,suggest,config}.rs` 收敛：
//! - `config.rs`：`base_url` 默认 null、私有网关禁用、密钥只从配置/环境变量读；
//! - `suggest.rs`：纯本地硬规则（电量 / CPU / 内存 / 深夜），不调 API、不联网；
//! - 本文件：未配置齐时**一行日志后直接 return**，连线程都不启动（B13 子集）；
//!   配置齐了才起独立线程，LLM 调用走短命线程 + 硬截止，绝不阻塞事件循环。
//!
//! 隐私：不把通知内容喂给 AI（D2 的摘要条只在本地展示）；发往用户自配端点的只有
//! 一条由本地数值派生的状态短句。日志只记 kind + 数量，不记文本、URL 与密钥。

pub mod config;
pub mod suggest;

use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::state::SharedState;
use config::AiConfig;

/// 与前端 `EVENTS.aiInsight` 对应的事件名。
pub const EVENT: &str = "ai-insight";

pub fn spawn(handle: AppHandle, cfg: AiConfig, state: Arc<SharedState>) {
    if !cfg.is_configured() {
        // 明确记一行，便于验收「AI 未配置时零外联」时从日志侧确认。
        winplat::log::log("ai: 未配置 base_url 或密钥，跳过（零外联）");
        return;
    }
    winplat::log::log("ai: configured, starting local rules count=1");

    std::thread::Builder::new()
        .name("glasssuite-ai".into())
        .spawn(move || run(handle, cfg, state))
        .ok();
}

fn run(handle: AppHandle, cfg: AiConfig, state: Arc<SharedState>) {
    let mut suggester = suggest::Suggester::default();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let snapshot = {
            let m = state.metrics.lock().unwrap();
            suggest::Snapshot {
                cpu: m.cpu,
                mem_pct: m.mem_pct,
                battery: m.battery,
                charging: m.charging,
            }
        };
        let Some((kind, local)) = suggester.evaluate(&snapshot, false, Instant::now(), local_hm())
        else {
            continue;
        };

        // 用户显式配了端点才发请求；硬截止保证 DNS/TCP 挂住时只是降级为本地文案。
        let (text, source) = match call_with_deadline(&cfg, &local) {
            Some(t) if !t.trim().is_empty() => (t, "ai"),
            _ => (local, "local"),
        };
        winplat::log::log(&format!("ai: insight kind={kind} source={source} count=1"));
        let _ = handle.emit(
            EVENT,
            json!({ "kind": kind, "text": text, "source": source, "at": now_unix_ms() }),
        );
    }
}

/// 带硬截止的 LLM 调用：ureq 的 timeout 不覆盖 DNS / connect 挂死，
/// 所以放到短命线程里等结果，超时就放弃本批（参考仓踩过的坑）。
fn call_with_deadline(cfg: &AiConfig, text: &str) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let cfg2 = cfg.clone();
    let text2 = text.to_string();
    std::thread::Builder::new()
        .name("glasssuite-ai-call".into())
        .spawn(move || {
            let _ = tx.send(llm_polish(&cfg2, &text2));
        })
        .ok()?;
    rx.recv_timeout(Duration::from_secs(cfg.timeout_secs + 3))
        .ok()
        .flatten()
}

/// OpenAI 兼容 `/chat/completions`。仅把本地派生的一句状态短句发出去。
fn llm_polish(cfg: &AiConfig, text: &str) -> Option<String> {
    let base = cfg.base_url.as_ref()?.trim_end_matches('/');
    let key = cfg.api_key.as_ref()?;
    let body = json!({
        "model": cfg.model,
        "max_tokens": 80,
        "messages": [{
            "role": "user",
            "content": format!("把下面这句桌面状态提示润色成不超过 20 字的中文，只输出正文：{text}")
        }]
    });
    let resp = ureq::post(&format!("{base}/chat/completions"))
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(cfg.timeout_secs))
        // 不启用 ureq 的 json feature：手工序列化即可，少一个依赖面。
        .send_string(&body.to_string())
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&resp.into_string().ok()?).ok()?;
    Some(
        v["choices"][0]["message"]["content"]
            .as_str()?
            .trim()
            .to_string(),
    )
}

#[cfg(windows)]
fn local_hm() -> Option<(u32, u32)> {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st: SYSTEMTIME = unsafe { GetLocalTime() };
    Some((st.wHour as u32, st.wMinute as u32))
}

#[cfg(not(windows))]
fn local_hm() -> Option<(u32, u32)> {
    None
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn unconfigured_ai_never_calls_llm() {
        let cfg = AiConfig::load(&Settings::default());
        assert!(!cfg.is_configured());
        // 未配置时 llm_polish 在 base_url 处立即返回 None，不产生任何 HTTP 请求。
        assert!(llm_polish(&cfg, "电量低").is_none());
    }

    #[test]
    fn configured_but_unreachable_endpoint_degrades() {
        // 用一个必然不可达的保留地址 + 极短超时，验证调用返回 None 而不是 panic。
        let cfg = AiConfig {
            enabled: true,
            base_url: Some("http://127.0.0.1:1/v1".into()),
            api_key: Some("dummy".into()),
            model: "test".into(),
            timeout_secs: 1,
        };
        assert!(llm_polish(&cfg, "状态").is_none());
    }
}

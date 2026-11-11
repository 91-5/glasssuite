//! AI 后台感知层：**未配置即零外联**。
//!
//! 从参考仓 `glasspill/src-tauri/src/ai/{mod,suggest,config}.rs` 收敛：
//! - `config.rs`：`base_url` 默认 null、私有网关禁用、密钥只从配置/环境变量读；
//! - `suggest.rs`：纯本地硬规则（电量 / CPU / 内存 / 深夜），不调 API、不联网；
//! - 本文件：常驻 supervisor 每 tick 读 `AiState` 里的最新配置，未配齐时**只 sleep
//!   不动作**（零外联、也不发提示）；配齐才走本地规则 → LLM，LLM 调用走短命线程 +
//!   硬截止，绝不阻塞事件循环。
//!
//! 热加载（OC-20261110-003 D1）：配置存在 `AiState` 槽里，`settings_update` /
//! `ai_reload` 换槽即生效——线程永不重启，所以「未配 ⇄ 配齐」切换天然正确，也不存在
//! 需要收敛的旧任务；唯一的外联线程有硬截止（timeout + 3s），翻转瞬间在飞的那一次
//! 调用最多再活一个截止期就收敛，不会累积泄漏。
//!
//! 隐私：不把通知内容喂给 AI（D2 的摘要条只在本地展示）；发往用户自配端点的只有
//! 一条由本地数值派生的状态短句。日志只记 kind + 数量与配置布尔，不记文本、URL 与密钥。

pub mod config;
pub mod suggest;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::settings::Settings;
use crate::state::SharedState;
use config::AiConfig;

/// 与前端 `EVENTS.aiInsight` 对应的事件名。
pub const EVENT: &str = "ai-insight";

/// 进程内共享的 AI 配置槽。写入方是 `settings_update` / `ai_reload`（保存即生效），
/// 读取方是常驻 supervisor（每 tick 取最新值）。
#[derive(Debug)]
pub struct AiState {
    config: Mutex<AiConfig>,
}

impl AiState {
    pub fn new(cfg: AiConfig) -> Self {
        Self {
            config: Mutex::new(cfg),
        }
    }

    /// 当前生效配置（每 tick clone 一次，开销可忽略）。
    pub fn current(&self) -> AiConfig {
        self.config.lock().unwrap().clone()
    }

    /// 换入新配置，返回旧配置（调用方一般不用）。
    pub fn swap(&self, cfg: AiConfig) -> AiConfig {
        std::mem::replace(&mut self.config.lock().unwrap(), cfg)
    }
}

/// 启动常驻 supervisor。无论当前是否配置都只起**这一个**线程：未配置时它每秒
/// 醒来检查一次槽（几乎零开销），保存配置后 ≤1s 即开始按新配置工作。
pub fn spawn(handle: AppHandle, ai: Arc<AiState>, state: Arc<SharedState>) {
    if ai.current().is_configured() {
        winplat::log::log("ai: configured, starting local rules count=1");
    } else {
        // 明确记一行，便于验收「AI 未配置时零外联」时从日志侧确认。
        winplat::log::log("ai: 未配置 base_url 或密钥（零外联）；保存配置后自动生效");
    }
    std::thread::Builder::new()
        .name("glasssuite-ai".into())
        .spawn(move || run(handle, ai, state))
        .ok();
}

/// 前端保存配置后的显式重载入口：从 managed Settings 重新派生 `AiConfig` 换入槽。
/// `settings_update` 已经原子应用过一次，这里是幂等兜底（卡片 D2 的「调 AI reload」）。
/// 返回「AI 是否已配齐」，前端用它显示状态；URL/密钥不进日志。
#[tauri::command]
pub fn ai_reload(
    settings: tauri::State<'_, Mutex<Settings>>,
    ai: tauri::State<'_, Arc<AiState>>,
) -> bool {
    let cfg = AiConfig::load(&settings.lock().unwrap());
    let configured = cfg.is_configured();
    ai.swap(cfg);
    winplat::log::log(&format!("ai: reload applied configured={configured}"));
    configured
}

fn run(handle: AppHandle, ai: Arc<AiState>, state: Arc<SharedState>) {
    let mut suggester = suggest::Suggester::default();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        // 每 tick 读最新配置：热加载就在这一行——换槽后下一个 tick 即按新配置工作。
        let cfg = ai.current();
        if !cfg.is_configured() {
            continue; // 未配齐：零外联，也不产生任何提示。
        }
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

    /// 热加载核心：换槽后 supervisor 读到的配置立即翻转（未配 ⇄ 配齐可逆）。
    #[test]
    fn ai_state_swap_toggles_runtime_config() {
        let ai = AiState::new(AiConfig::load(&Settings::default()));
        assert!(!ai.current().is_configured(), "初始未配置");

        let full = Settings {
            ai_base_url: Some("https://llm.example.com/v1".into()),
            ai_api_key: Some("test-key".into()),
            ..Settings::default()
        };
        let old = ai.swap(AiConfig::load(&full));
        assert!(ai.current().is_configured(), "换入配齐配置后即生效");
        assert!(!old.is_configured(), "swap 返回被换下的旧配置");

        // 翻回去：UI 关掉开关 / 清空端点后，下一个 tick 零外联。
        ai.swap(AiConfig::load(&Settings::default()));
        assert!(!ai.current().is_configured());
    }

    /// 命令层语义（B3）：`ai_reload` 从 managed Settings 派生配置换入槽并回报配齐状态。
    /// State 无法在单测里构造，所以测的是命令体用的同一派生+换槽路径。
    #[test]
    fn reload_derives_config_from_settings_into_state() {
        let ai = AiState::new(AiConfig::load(&Settings::default()));
        let store = Mutex::new(Settings {
            ai_base_url: Some("https://llm.example.com/v1".into()),
            ai_api_key: Some("test-key".into()),
            ..Settings::default()
        });

        // ai_reload 命令体的三步：派生 → 换槽 → 回报。
        let cfg = AiConfig::load(&store.lock().unwrap());
        let configured = cfg.is_configured();
        ai.swap(cfg);
        assert!(configured);
        assert!(ai.current().is_configured());
    }
}

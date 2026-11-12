//! AI 后台感知层：**未配置即零外联**。
//!
//! 从参考仓 `glasspill/src-tauri/src/ai/{mod,suggest,config}.rs` 收敛，并按
//! OC-20261113-001 P3 扩展为「主动建议」引擎：
//! - `config.rs`：`base_url` 默认 null、私有网关禁用、密钥只从配置/环境变量读；
//! - `suggest.rs`：注入式规则引擎（日程/久坐/勿扰/夜关怀 + 环境硬规则）；
//! - 本文件：常驻 supervisor 每 tick 读配置与设置，**`proactive=false`（默认）时
//!   除夜间状态切换外什么都不做**；开启后模板直出（AI 未配齐也零外联），
//!   LLM 只做异步润色且受三重闸门约束。
//!
//! LLM 纪律（卡面 P3 §3–5）：
//! - **绝不挡在展示前**：规则触发 → 立即渲染模板 → 异步 LLM（deadline 2.5s），
//!   只有「展示 <300ms 内返回」才替换文案；超时/失败静默保留模板（B5）；
//! - **日程类预生成**：午饭/晚饭提醒在窗口前 5 分钟先跑 LLM，触发时文案已就绪
//!   （成本不变、延迟归零、无网时提前降级到模板）；
//! - **频率硬约束**：全局 LLM 日预算 ≤8 次/天（`LlmGate`，B5 判据）；同规则最小
//!   间隔 6h；结果缓存 key = (rule_id, day)。
//!
//! 热加载（OC-20261110-003 D1）：AI 配置在 `AiState` 槽里换槽即生效；本卡起
//! proactive/nightCare/久坐间隔直接每 tick 读 managed `Settings`，保存即生效。
//!
//! 隐私：不把通知内容喂给 AI；发往用户自配端点的只有本地数值派生的状态短句。
//! 日志只记 kind + 布尔，不记文本、URL 与密钥。

pub mod config;
pub mod suggest;

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::settings::Settings;
use crate::state::SharedState;
use config::AiConfig;
use suggest::{Clock, InputState};

/// 与前端 `EVENTS.aiInsight` 对应的事件名。
pub const EVENT: &str = "ai-insight";
/// 夜间状态表达（23:00–07:00）：岛变暗、dock 降饱和。payload `{ on: bool }`。
pub const NIGHT_EVENT: &str = "night-state";
/// 全局 LLM 日预算（卡面 P3 §5：≤8 次/天）。
const LLM_DAILY_BUDGET: u32 = 8;
/// 同规则两次 LLM 调用的最小间隔。
const LLM_RULE_GAP: Duration = Duration::from_secs(6 * 60 * 60);
/// 展示后仍可替换文案的时间窗。
const REPLACE_WINDOW: Duration = Duration::from_millis(300);
/// LLM 硬截止（卡面 P3 §3：2.5s）。
const LLM_DEADLINE: Duration = Duration::from_millis(2500);

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

/// 「测试连接」结果：ok=端点可达且鉴权通过（HTTP 2xx/4xx 都算「连上了」，
/// 4xx 说明端点活着但密钥/参数有问题——用户能据此区分「网不通」和「key 错」）。
#[derive(serde::Serialize)]
pub struct AiTestResult {
    pub ok: bool,
    pub status: u16,
    pub detail: String,
}

/// 显式向用户配置的端点发一次 GET {base}/models（OpenAI 兼容标准端点）。
/// 触发条件只有前端按钮，不在任何轮询路径上。短命线程 + 硬截止，不阻塞事件循环。
#[tauri::command]
pub fn ai_test_connection(settings: tauri::State<'_, Mutex<Settings>>) -> AiTestResult {
    let cfg = AiConfig::load(&settings.lock().unwrap());
    if !cfg.is_configured() {
        return AiTestResult {
            ok: false,
            status: 0,
            detail: "未配齐：端点或密钥缺失，或端点格式不合法".into(),
        };
    }
    let base = cfg
        .base_url
        .as_deref()
        .unwrap()
        .trim_end_matches('/')
        .to_string();
    let key = cfg.api_key.as_deref().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("glasssuite-ai-test".into())
        .spawn(move || {
            let r = ureq::get(&format!("{base}/models"))
                .set("Authorization", &format!("Bearer {key}"))
                .timeout(Duration::from_secs(8))
                .call();
            let outcome = match r {
                Ok(resp) => (resp.status(), true),
                // 4xx/5xx：端点本身活着，只是拒绝这次请求（密钥/权限问题）
                Err(ureq::Error::Status(st, _)) => (st, true),
                Err(_) => (0, false),
            };
            let _ = tx.send(outcome);
        });
    match rx.recv_timeout(Duration::from_secs(11)) {
        Ok((st, reached)) if reached => AiTestResult {
            ok: true,
            status: st,
            detail: format!("连接成功（HTTP {st}）"),
        },
        Ok((st, _)) => AiTestResult {
            ok: false,
            status: st,
            detail: if st > 0 {
                format!("端点拒绝（HTTP {st}，检查密钥或模型权限）")
            } else {
                "无法连接：超时或网络不通".into()
            },
        },
        Err(_) => AiTestResult {
            ok: false,
            status: 0,
            detail: "测试超时".into(),
        },
    }
}

/// LLM 预算闸门（纯状态机，B5 的「日预算超限停调 LLM」单测口）：
/// - 日预算 ≤ `CAP` 次/天（按本地日翻转清零）；
/// - 同规则最小间隔 `RULE_GAP`。
struct LlmGate {
    day: u64,
    calls: u32,
    per_rule: HashMap<&'static str, Instant>,
}

impl LlmGate {
    fn new() -> Self {
        Self {
            day: 0,
            calls: 0,
            per_rule: HashMap::new(),
        }
    }

    /// 是否允许给 `rule` 发起一次 LLM 调用；允许则记账（预算 -1、间隔起点更新）。
    fn allow(&mut self, day: u64, now: Instant, rule: &'static str) -> bool {
        if self.day != day {
            self.day = day;
            self.calls = 0;
        }
        if self.calls >= LLM_DAILY_BUDGET {
            return false;
        }
        if let Some(prev) = self.per_rule.get(rule) {
            if now.duration_since(*prev) < LLM_RULE_GAP {
                return false;
            }
        }
        self.calls += 1;
        self.per_rule.insert(rule, now);
        true
    }
}

fn run(handle: AppHandle, ai: Arc<AiState>, state: Arc<SharedState>) {
    let mut suggester = suggest::Suggester::default();
    let mut gate = LlmGate::new();
    // 预生成/润色缓存：key = (rule, 本地日)（卡面 coarse_state 在日程类规则上恒定，
    // 环境类规则走 300ms 替换窗不进缓存）。
    let mut cache: HashMap<(&'static str, u64), Option<String>> = HashMap::new();
    // 夜间状态表达的当前值（None = 尚未 emit 过）。
    let mut night_on: Option<bool> = None;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        // 每 tick 读最新配置与设置：热加载就在这两行——保存后下一个 tick 即生效。
        let cfg = ai.current();
        let (proactive, night_care, sedentary_interval) = {
            let s = handle.state::<Mutex<Settings>>();
            let s = s.lock().unwrap();
            let mins = s.sedentary_interval_min.unwrap_or(60).clamp(15, 240);
            (
                s.proactive,
                s.night_care,
                Duration::from_secs(mins * 60),
            )
        };

        // 夜间状态表达（23:00–07:00 岛变暗/dock 降饱和）：只在 proactive 开启时
        // 生效（默认零视觉变化）；关闭瞬间发一次 off，确保不留暗态。
        if proactive {
            if let Some(c) = local_clock() {
                let on = c.in_dnd();
                if night_on != Some(on) {
                    night_on = Some(on);
                    let _ = handle.emit(NIGHT_EVENT, json!({ "on": on }));
                    winplat::log::log(&format!("ai: night-state on={on}"));
                }
            }
        } else if night_on != Some(false) {
            night_on = Some(false);
            let _ = handle.emit(NIGHT_EVENT, json!({ "on": false }));
        }

        if !proactive {
            continue; // 默认关：零建议、零外联（硬约束 3）
        }
        // 静音（收起球右键 / 设置面板）期间既不外联也不 emit —— 静音要连请求一起停，
        // 否则「静音」只是看不见，仍然在偷偷联网。
        if state.muted.load(std::sync::atomic::Ordering::Relaxed) {
            continue;
        }
        let Some(clock) = local_clock() else {
            continue;
        };
        let configured = cfg.is_configured();

        // 日程类预生成：11:15–11:20 / 17:15–17:20 窗口内先把文案算好。
        // 未配齐 AI 时跳过（触发时走模板，零外联，B6）。
        if configured {
            if let Some(kind) = suggest::meal_pregen(clock) {
                if !cache.contains_key(&(kind, clock.day))
                    && gate.allow(clock.day, Instant::now(), kind)
                {
                    let template = suggest::meal_template(kind);
                    let r = call_with_deadline(&cfg, template, LLM_DEADLINE)
                        .filter(|t| !t.trim().is_empty());
                    winplat::log::log(&format!(
                        "ai: pregen kind={kind} ok={}",
                        r.is_some()
                    ));
                    // 失败也记 Some(None)：今天不再重复烧预算（模板兜底）。
                    cache.insert((kind, clock.day), r);
                }
            }
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
        let media_playing = media_playing(&state);
        let ctx = suggest::Ctx {
            snap: &snapshot,
            muted: false,
            now: Instant::now(),
            clock,
            input: InputState {
                idle: last_input_idle(),
            },
            media_playing,
            night_care,
            sedentary_interval,
        };
        let Some(sugg) = suggester.evaluate(&ctx) else {
            continue;
        };

        // 展示：预生成缓存优先（AI 文案准时到位），否则模板直出（source=rule）。
        let cached = cache.get(&(sugg.kind, clock.day)).cloned().flatten();
        if let Some(text) = cached {
            winplat::log::log(&format!("ai: insight kind={} source=ai count=1", sugg.kind));
            let _ = handle.emit(
                EVENT,
                json!({ "kind": sugg.kind, "text": text, "source": "ai", "at": now_unix_ms() }),
            );
            continue;
        }
        winplat::log::log(&format!(
            "ai: insight kind={} source=rule count=1",
            sugg.kind
        ));
        let _ = handle.emit(
            EVENT,
            json!({ "kind": sugg.kind, "text": sugg.text, "source": "rule", "at": now_unix_ms() }),
        );

        // 反应式润色：模板**已展示**，LLM 在后台跑；只有 300ms 内返回才替换
        // （避免文案翻转），超时/失败静默保留模板（B5）。
        if configured && gate.allow(clock.day, Instant::now(), sugg.kind) {
            let shown_at = Instant::now();
            let r = call_with_deadline(&cfg, &sugg.text, LLM_DEADLINE)
                .filter(|t| !t.trim().is_empty());
            if let Some(text) = r {
                if shown_at.elapsed() < REPLACE_WINDOW {
                    winplat::log::log(&format!(
                        "ai: insight kind={} source=ai count=1",
                        sugg.kind
                    ));
                    let _ = handle.emit(
                        EVENT,
                        json!({ "kind": sugg.kind, "text": text, "source": "ai", "at": now_unix_ms() }),
                    );
                }
            }
        }
    }
}

/// 媒体播放中（看视频豁免久坐）：读 SMTC 最近快照的 playing 字段。
fn media_playing(state: &Arc<SharedState>) -> bool {
    state
        .last_media
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|v| v.get("playing").and_then(|p| p.as_bool()))
        .unwrap_or(false)
}

/// 带硬截止的 LLM 调用：ureq 的 timeout 不覆盖 DNS / connect 挂死，
/// 所以放到短命线程里等结果，超时就放弃本批（参考仓踩过的坑）。
/// `deadline` 由调用方给：预生成/反应式都是 2.5s（卡面 P3 §3）。
fn call_with_deadline(cfg: &AiConfig, text: &str, deadline: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let cfg2 = cfg.clone();
    let text2 = text.to_string();
    std::thread::Builder::new()
        .name("glasssuite-ai-call".into())
        .spawn(move || {
            let _ = tx.send(llm_polish(&cfg2, &text2));
        })
        .ok()?;
    rx.recv_timeout(deadline).ok().flatten()
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

/// 本地时钟注入（B5 的注入式测试在 suggest.rs 层做；这里只负责 Win32 读取）。
#[cfg(windows)]
fn local_clock() -> Option<Clock> {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st = unsafe { GetLocalTime() };
    Some(Clock {
        hour: st.wHour as u32,
        minute: st.wMinute as u32,
        day: days_from_civil(st.wYear as i64, st.wMonth as u32, st.wDay as u32),
    })
}

#[cfg(not(windows))]
fn local_clock() -> Option<Clock> {
    None // 非 Windows 运行时不产生建议（规则层单测直接注入时钟）
}

/// 距上次输入的空闲时长（久坐检测的核心信号）。读取失败 / 非 Windows
/// 按「离开 1h」处理：功能静默关闭，不会误报。
#[cfg(windows)]
fn last_input_idle() -> Duration {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    let tick = unsafe { GetTickCount() };
    if unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        Duration::from_millis(u64::from(tick.wrapping_sub(info.dwTime)))
    } else {
        Duration::from_secs(3600)
    }
}

#[cfg(not(windows))]
fn last_input_idle() -> Duration {
    Duration::from_secs(3600)
}

/// 平历日 → 自 1970-01-01 的天数（Howard Hinnant 的 days_from_civil）。
/// 用于「本地日历日」做每日标记与夜 key（UTC 天数在本地时区会差一天）。
fn days_from_civil(y: i64, m: u32, d: u32) -> u64 {
    let y = if i64::from(m) <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let mp = (i64::from(m) + 9).rem_euclid(12) as u64; // 三月起算的月序 [0, 11]
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era as u64 * 146_097 + doe - 719_468
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

    /// B5：日预算超限停调 LLM；同规则 6h 间隔；按本地日翻转清零。
    #[test]
    fn llm_gate_daily_budget_and_rule_gap() {
        let mut g = LlmGate::new();
        let t0 = Instant::now();
        let day = 20_000;
        // 每次调用间隔 7h（>6h 规则间隔），8 次刚好烧完日预算。
        for i in 0..8u64 {
            assert!(
                g.allow(day, t0 + Duration::from_secs(7 * 3600 * i), "lunch"),
                "预算内应放行"
            );
        }
        assert!(
            !g.allow(day, t0 + Duration::from_secs(7 * 3600 * 8), "lunch"),
            "预算超限停调（B5）"
        );
        assert!(
            !g.allow(day, t0 + Duration::from_secs(7 * 3600 * 8), "sedentary"),
            "预算是全局的，换规则也不放行"
        );

        // 次日（时间前进到预算日最后一笔 56h 之后）：预算清零放行；
        // 同规则 6h 间隔仍拦；跨过 6h 放行。
        let next_day = t0 + Duration::from_secs(86_400 * 3);
        assert!(g.allow(day + 1, next_day, "lunch"));
        assert!(!g.allow(day + 1, next_day + Duration::from_secs(3600), "lunch"));
        assert!(g.allow(
            day + 1,
            next_day + Duration::from_secs(6 * 3600 + 1),
            "lunch"
        ));
    }

    /// 平历日换算抽查：已知日期对得上「自 1970-01-01 的天数」。
    #[test]
    fn days_from_civil_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 1, 1), 20_454);
        assert_eq!(days_from_civil(2026, 11, 13), days_from_civil(2026, 11, 12) + 1);
        assert_eq!(days_from_civil(2024, 2, 29), days_from_civil(2024, 3, 1) - 1);
    }
}

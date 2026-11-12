//! 用户配置。
//!
//! **默认全部关闭**需要联网或读取隐私数据的功能（天气、网络探测、AI）。用户把想要
//! 的功能填进 `%APPDATA%\com.glasssuite.app\settings.json` 即可打开——首次运行会写一份
//! 全默认的模板，方便发现有哪些开关。
//!
//! 配置有两个写入面，语义完全一致（同一文件、同一校验）：
//! - 直接改文件：重启后生效；
//! - UI 面板（island → AI 与网络设置）：`settings_update` 原子完成
//!   「写回文件 → 更新 managed state → 应用运行时（AI / 网络探测热加载）」，
//!   无需重启。本卡（OC-20261110-003）补齐的就是后者。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

/// 通知读取开关。**默认关闭**：读通知内容属于隐私操作，必须由用户显式打开。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NotificationConfig {
    #[serde(default)]
    pub enabled: bool,
    /// UIA 兜底轮询。默认关闭：旧仓实测会与主路径重复上报且依赖 Toast 窗口存续，
    /// 只有确实需要覆盖「不走通知中心的来源」时才打开。
    #[serde(default, rename = "uiaFallback")]
    pub uia_fallback: bool,
}

/// dock 行为配置。
///
/// `auto_hide` 默认 **false**：验收基线要求「两窗同时可见」，默认隐藏会让 dock
/// 首帧不可见，也让人误以为程序没起来。用户在配置里显式打开即可。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DockSettings {
    /// 自动隐藏（贴底 4px 唤出 / 离开 800ms 收起）。
    #[serde(default)]
    pub auto_hide: bool,
}

/// 应用配置。所有字段都有默认值：缺键 = 用默认（关闭）。
///
/// 硬约束 12：天气坐标、网络探测地址、AI `base_url`、歌词源，首启模板里**全部为 null**。
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
    /// AI 密钥。建议改用环境变量 `GLASSSUITE_AI_KEY`，避免把密钥写进磁盘；
    /// UI 面板保存的密钥走这里（仅用户自填，默认模板仍为 null）。
    #[serde(default, rename = "aiApiKey")]
    pub ai_api_key: Option<String>,
    /// AI 总开关（UI 面板）。`null`/缺省 = 开（与基线行为一致），`false` = 用户显式关闭。
    /// 不是外联字段：外联仍由「端点 + 密钥配齐」门控；env `GLASSSUITE_AI_ENABLED=0`
    /// 是更高优先级的 kill switch。
    #[serde(default, rename = "aiEnabled")]
    pub ai_enabled: Option<bool>,
    /// 歌词源地址（LRCLIB 兼容 `/api/get`、`/api/search`）。`null` = 关闭（默认，不联网）。
    #[serde(default, rename = "lyricsSourceUrl")]
    pub lyrics_source_url: Option<String>,
    /// AI 模型名。仅在 `aiBaseUrl` 与密钥都配好时生效。
    #[serde(default, rename = "aiModel")]
    pub ai_model: Option<String>,
    /// 系统通知摘要读取。默认关闭。
    #[serde(default)]
    pub notifications: NotificationConfig,
    /// dock 行为。
    #[serde(default)]
    pub dock: DockSettings,
    /// AI 主动建议总开关（OC-20261113-001 P3）。**默认 false**（硬约束 3：功能默认关）。
    /// 开启后：日程/久坐/环境规则走本地模板（零外联），LLM 仅在端点配齐时做异步润色。
    #[serde(default, rename = "proactive")]
    pub proactive: bool,
    /// 熬夜关怀（01:00 后持续打字时的一条 insight，一夜一次）。默认 false。
    #[serde(default, rename = "nightCare")]
    pub night_care: bool,
    /// 久坐提醒间隔（分钟）。缺省 = 60（上限一天 6 次，写死在 suggester）。
    #[serde(default, rename = "sedentaryIntervalMin")]
    pub sedentary_interval_min: Option<u64>,
}

/// 配置文件名。放在 `%APPDATA%\com.glasssuite.app\` 下。
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// 应用数据目录（`%APPDATA%\com.glasssuite.app`）。无 `APPDATA` 时退回临时目录。
/// ingest token 等运行时文件也放这里（与 settings.json 同一目录语义）。
pub fn data_dir() -> PathBuf {
    config::app_data_dir(winplat::IDENTIFIER).unwrap_or_else(std::env::temp_dir)
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
pub fn settings_get(settings: tauri::State<'_, Mutex<Settings>>) -> Settings {
    settings.lock().unwrap().clone()
}

/// 写回 settings.json（原子：临时文件 + rename，见 `winplat::config::save_json`）。
/// 独立出来便于单测：命令层只做「写盘 → 换内存态 → 应用运行时」的编排。
fn persist(data_dir: &Path, next: &Settings) -> Result<(), String> {
    config::save_json(&path(data_dir), next).map_err(|e| format!("配置写入失败：{e}"))
}

/// 把新配置应用进运行时：替换 managed state、派生并换入 AI 配置、更新网络探测开关。
///
/// 返回「AI 是否已配齐」（只回布尔，URL/密钥不进日志）。热加载机制见
/// `island::ai::AiState`：AI 常驻线程每个 tick 读最新配置，换槽即生效，不重启线程、
/// 不留旧任务；网络探测线程同理（`island::network::NetworkState`）。
fn apply(
    next: &Settings,
    store: &Mutex<Settings>,
    ai: &crate::island::ai::AiState,
    network: &crate::island::network::NetworkState,
) -> bool {
    let cfg = {
        let mut cur = store.lock().unwrap();
        *cur = next.clone();
        crate::island::ai::config::AiConfig::load(&cur)
    };
    let configured = cfg.is_configured();
    ai.swap(cfg);
    network.set(next.network_probe_url.clone());
    configured
}

/// 前端保存配置：写回文件 + 更新 managed state + 应用运行时，一次调用全部完成。
///
/// 写盘失败时不动内存态并返回 `Err`（前端提示保存失败）；成功则新配置即刻生效，
/// `ai_reload` 之后只是幂等兜底。async 是刻意的：约束 14 禁止阻塞式系统调用跑在
/// UI/事件循环线程，文件 I/O 交给 Tauri 的异步命令线程。
#[tauri::command]
pub async fn settings_update<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    store: tauri::State<'_, Mutex<Settings>>,
    ai: tauri::State<'_, Arc<crate::island::ai::AiState>>,
    network: tauri::State<'_, Arc<crate::island::network::NetworkState>>,
    notif: tauri::State<'_, Arc<crate::island::notification::NotificationState>>,
    next: Settings,
) -> Result<Settings, String> {
    persist(&data_dir(), &next)?;

    let configured = apply(&next, &store, &ai, &network);
    // 通知域总开关热启停（OC-20261113-001 P2）：开 = 拉起三源 + ingest 监听；
    // 关 = 全部停止（端口解绑，netstat 零监听）。
    crate::island::notification::apply_runtime(&app, &notif, &next.notifications);
    winplat::log::log(&format!(
        "settings: 已保存 ai_configured={configured} notifications_enabled={}",
        next.notifications.enabled
    ));
    Ok(next)
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
        assert!(s.ai_api_key.is_none());
        assert!(s.lyrics_source_url.is_none(), "歌词源默认关闭");
        assert!(!s.notifications.enabled, "通知读取默认关闭");

        // 硬约束 12：首启模板里所有外联字段必须是 null，不能有内置地址。
        let raw = std::fs::read_to_string(path(&dir)).unwrap();
        let template: serde_json::Value = serde_json::from_str(&raw).unwrap();
        for key in [
            "weather",
            "networkProbeUrl",
            "aiBaseUrl",
            "aiApiKey",
            "lyricsSourceUrl",
        ] {
            assert!(
                template.get(key).is_some_and(|v| v.is_null()),
                "首启模板的 {key} 必须为 null，实际为 {:?}",
                template.get(key)
            );
        }
        assert_eq!(template["notifications"]["enabled"].as_bool(), Some(false));
        assert_eq!(
            template["aiEnabled"].as_bool(),
            None,
            "首启模板不预置 AI 开关（null = 跟随默认开）"
        );

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

    /// D1 单测：`settings_update` 的写回路径 → reload 出正确 `AiConfig`；
    /// 未配齐（缺 key）仍然零外联。
    #[test]
    fn update_persists_and_reload_yields_configured_ai() {
        let dir = std::env::temp_dir().join("gs-settings-update");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let next = Settings {
            ai_base_url: Some("https://llm.example.com/v1".into()),
            ai_api_key: Some("test-key".into()),
            ai_model: Some("my-model".into()),
            ..Settings::default()
        };
        persist(&dir, &next).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded, next, "写回后再读应原样回来");
        let cfg = crate::island::ai::config::AiConfig::load(&loaded);
        assert!(cfg.is_configured(), "端点+密钥配齐后 reload 应视为已配置");
        assert_eq!(cfg.model, "my-model");

        // 缺 key：写回同样的文件再 reload，仍未配置——零外联门槛不因写回路径松动。
        let no_key = Settings {
            ai_api_key: None,
            ..next.clone()
        };
        persist(&dir, &no_key).unwrap();
        let cfg = crate::island::ai::config::AiConfig::load(&load(&dir));
        assert!(!cfg.is_configured());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `settings_update` 的应用路径：内存态整体替换 + AI/网络运行时同步生效，
    /// 开关可逆（B4 的命令层语义）。
    #[test]
    fn apply_swaps_runtime_states_and_reports_configured() {
        use crate::island::ai;
        use crate::island::network;

        let store = Mutex::new(Settings::default());
        let ai = ai::AiState::new(ai::config::AiConfig::load(&Settings::default()));
        let network = network::NetworkState::new(None);

        // 只填端点缺 key：AI 槽仍未配置（零外联），网络开关保持关闭。
        let partial = Settings {
            ai_base_url: Some("https://llm.example.com/v1".into()),
            ..Settings::default()
        };
        assert!(!apply(&partial, &store, &ai, &network));
        assert!(!ai.current().is_configured());
        assert!(network.current().is_none());

        // 配齐 + 打开网络探测：一次 apply 两处同时生效，managed state 已是新值。
        let full = Settings {
            ai_base_url: Some("https://llm.example.com/v1".into()),
            ai_api_key: Some("k".into()),
            network_probe_url: Some("https://example.com".into()),
            ..Settings::default()
        };
        assert!(apply(&full, &store, &ai, &network));
        assert!(ai.current().is_configured());
        assert_eq!(network.current().as_deref(), Some("https://example.com"));
        assert_eq!(store.lock().unwrap().ai_api_key.as_deref(), Some("k"));

        // 再关：AI 回到未配置、空白探测地址视为关闭——保存开关语义可逆。
        let off = Settings {
            network_probe_url: Some("   ".into()),
            ..Settings::default()
        };
        assert!(!apply(&off, &store, &ai, &network));
        assert!(!ai.current().is_configured());
        assert!(network.current().is_none());
    }
}

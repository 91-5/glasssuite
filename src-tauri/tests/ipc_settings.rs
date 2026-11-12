//! IPC 级集成测试：`settings_update` / `ai_reload`（follow-up 卡 OC-20261110-004）。
//!
//! 与 003 交付的命令层单测（`persist()`/`apply()` 拆分）互补：这里走的是**与前端
//! `invoke` 同构**的完整通路——`generate_handler!` 注册 → `MockRuntime` webview →
//! `tauri::test::get_ipc_response`。命令参数经 IPC 反序列化、managed state 经
//! `Manager::manage` 解析，任何一边的契约漂移（字段重命名、状态类型不匹配、
//! handler 未注册）这里都会炸。
//!
//! 落盘隔离：`settings_update` 内部按 `%APPDATA%\com.glasssuite.app\settings.json`
//! 定位配置文件，测试把进程级 `APPDATA` 重定向到临时目录（所有 IPC 测试共用一把
//! 锁串行执行），绝不碰真实用户配置。写盘失败路径用「`com.glasssuite.app` 占位成
//! 普通文件」复现 `create_dir_all` 失败（Windows 上 `create_dir_all` 对「路径中某段
//! 是文件」返回 `Err`，Linux 上 mkdir 同样 Err）。

use std::sync::{Arc, Mutex};

use glasssuite::island::ai::{self, config::AiConfig};
use glasssuite::island::network::NetworkState;
use glasssuite::settings::{self, Settings};

use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime};
use tauri::WebviewWindowBuilder;

/// APPDATA 重定向串行锁：所有 IPC 测试都要改写进程级 `APPDATA`，必须串行。
static ENV_LOCK: Mutex<()> = Mutex::new(());

enum Sandbox {
    /// `com.glasssuite.app` 是可写目录 —— 正常路径。
    Dir,
    /// `com.glasssuite.app` 是一个普通文件而非目录 —— `create_dir_all` 失败，复现写盘失败。
    BlockedByFile,
}

/// 临时 APPDATA 守卫：创建时重定向 `APPDATA` 到隔离沙盒，`Drop` 时还原并清理。
struct AppDataGuard {
    tmp: std::path::PathBuf,
    prior: Option<std::ffi::OsString>,
}

impl AppDataGuard {
    fn new(name: &str, sandbox: Sandbox) -> Self {
        let tmp =
            std::env::temp_dir().join(format!("glasssuite-ipc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("建临时 APPDATA 根目录");
        match sandbox {
            Sandbox::Dir => {
                std::fs::create_dir_all(tmp.join("com.glasssuite.app")).expect("建应用数据目录");
            }
            Sandbox::BlockedByFile => {
                // 占位成普通文件而非目录 → save_json 的 create_dir_all 失败（跨平台成立）。
                std::fs::write(tmp.join("com.glasssuite.app"), b"not a directory")
                    .expect("占位文件");
            }
        }
        let prior = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", &tmp);
        Self { tmp, prior }
    }

    fn settings_file(&self) -> std::path::PathBuf {
        self.tmp.join("com.glasssuite.app").join("settings.json")
    }
}

impl Drop for AppDataGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var("APPDATA", v),
            None => std::env::remove_var("APPDATA"),
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn build_app(initial: &Settings) -> tauri::App<MockRuntime> {
    let ai_state = Arc::new(ai::AiState::new(AiConfig::load(initial)));
    let network = Arc::new(NetworkState::new(initial.network_probe_url.clone()));
    let notif = Arc::new(glasssuite::island::notification::NotificationState::new());
    let store = Mutex::new(initial.clone());

    mock_builder()
        .invoke_handler(tauri::generate_handler![
            settings::settings_get,
            settings::settings_update,
            ai::ai_reload,
        ])
        .manage(store)
        .manage(ai_state)
        .manage(network)
        .manage(notif)
        .build(mock_context(noop_assets()))
        .expect("failed to build mock app")
}

fn ipc(
    webview: &tauri::WebviewWindow<MockRuntime>,
    cmd: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    get_ipc_response(
        webview,
        tauri::webview::InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "http://tauri.localhost".parse().expect("valid url"),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.to_string(),
        },
    )
    .map(|b| {
        b.deserialize::<serde_json::Value>()
            .expect("响应可反序列化")
    })
}

/// B1 happy path：配齐端点+密钥 → 写盘成功 → managed state 整体替换 → 返回新配置；
/// `settings_get` 反映新配置；`ai_reload` 幂等且报「已配齐」。
#[test]
fn settings_update_persisters_and_applies_runtime() {
    let _lock = ENV_LOCK.lock().expect("ENV_LOCK 未中毒");
    let env = AppDataGuard::new("happy", Sandbox::Dir);

    let app = build_app(&Settings::default());
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("建 mock webview");

    let next = Settings {
        ai_base_url: Some("https://llm.example.com/v1".into()),
        ai_api_key: Some("test-key".into()),
        ai_model: Some("my-model".into()),
        ..Settings::default()
    };
    let saved = ipc(
        &webview,
        "settings_update",
        serde_json::json!({ "next": next }),
    )
    .expect("配齐端点+密钥应保存成功");

    // IPC 返回的就是落盘后的新配置（前端用它刷新本地态）
    assert_eq!(saved["aiBaseUrl"], "https://llm.example.com/v1");
    assert_eq!(saved["aiApiKey"], "test-key");
    assert_eq!(saved["aiModel"], "my-model");

    // 落盘内容与 IPC 返回一致（front-end 与磁盘不会分叉）
    let on_disk: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(env.settings_file()).expect("settings.json 已写回"),
    )
    .expect("settings.json 可解析");
    assert_eq!(on_disk, saved, "写回文件应与返回值一致");

    // settings_get 走同一份 managed state，应反映新配置
    let cur = ipc(&webview, "settings_get", serde_json::Value::Null).expect("settings_get 成功");
    assert_eq!(cur["aiModel"], "my-model");

    // ai_reload（前端 saveSettings 的第二步）：从 managed state 重派生并换槽
    let reload1 = ipc(&webview, "ai_reload", serde_json::Value::Null).expect("ai_reload 成功");
    assert_eq!(
        reload1,
        serde_json::Value::Bool(true),
        "配齐后 reload 应报 true"
    );
    // 幂等：重复调用结果稳定
    let reload2 =
        ipc(&webview, "ai_reload", serde_json::Value::Null).expect("ai_reload 幂等重跑成功");
    assert_eq!(reload2, serde_json::Value::Bool(true), "reload 应幂等");
}

/// B1 反例 / ADV-2 写盘失败路径：目录不可写 → 返回 `Err`，**内存态不被破坏**，
/// `settings_get` 仍是旧配置，`ai_reload` 仍报未配齐（零外联门不因保存失败打开）。
#[test]
fn write_failure_leaves_memory_and_ai_slot_untouched() {
    let _lock = ENV_LOCK.lock().expect("ENV_LOCK 未中毒");
    let env = AppDataGuard::new("writefail", Sandbox::BlockedByFile);

    let app = build_app(&Settings::default());
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("建 mock webview");

    let next = Settings {
        ai_base_url: Some("https://llm.example.com/v1".into()),
        ai_api_key: Some("test-key".into()),
        ..Settings::default()
    };
    let res = ipc(
        &webview,
        "settings_update",
        serde_json::json!({ "next": next }),
    );
    assert!(res.is_err(), "目录不可写应返回 Err，而不是吞掉错误");

    // 内存态未被破坏：settings_get 仍是初始（默认全关）配置
    let cur = ipc(&webview, "settings_get", serde_json::Value::Null).expect("settings_get 成功");
    assert_eq!(
        cur,
        serde_json::to_value(Settings::default()).expect("默认配置可序列化"),
        "写盘失败不得改动内存态"
    );
    assert!(!env.settings_file().exists(), "失败路径不应留下残缺文件");

    // AI 槽也未被换入新配置：reload 仍报未配齐（零外联）
    let reload = ipc(&webview, "ai_reload", serde_json::Value::Null).expect("ai_reload 成功");
    assert_eq!(
        reload,
        serde_json::Value::Bool(false),
        "写盘失败后 AI 应仍处于未配齐态"
    );
}

/// B2 supervisor 换槽可逆性（集成层）：未配 ⇄ 仅端点 ⇄ 配齐 ⇄ 清空，`ai_reload`
/// 的返回值必须跟着翻——配合 003 的换槽单测，热加载的正确性在命令层与 IPC 层各落一档。
#[test]
fn ai_reload_tracks_configured_toggle() {
    let _lock = ENV_LOCK.lock().expect("ENV_LOCK 未中毒");
    let _env = AppDataGuard::new("toggle", Sandbox::Dir);

    let app = build_app(&Settings::default());
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("建 mock webview");

    // 起始未配置
    assert_eq!(
        ipc(&webview, "ai_reload", serde_json::Value::Null).unwrap(),
        serde_json::Value::Bool(false)
    );

    // 只填端点缺 key：仍不算配齐（零外联门）
    let partial = Settings {
        ai_base_url: Some("https://llm.example.com/v1".into()),
        ..Settings::default()
    };
    ipc(
        &webview,
        "settings_update",
        serde_json::json!({ "next": partial }),
    )
    .unwrap();
    assert_eq!(
        ipc(&webview, "ai_reload", serde_json::Value::Null).unwrap(),
        serde_json::Value::Bool(false)
    );

    // 配齐：端点 + key
    let full = Settings {
        ai_base_url: Some("https://llm.example.com/v1".into()),
        ai_api_key: Some("test-key".into()),
        ai_model: Some("my-model".into()),
        ..Settings::default()
    };
    ipc(
        &webview,
        "settings_update",
        serde_json::json!({ "next": full }),
    )
    .unwrap();
    assert_eq!(
        ipc(&webview, "ai_reload", serde_json::Value::Null).unwrap(),
        serde_json::Value::Bool(true)
    );

    // 翻回未配置（UI 关掉开关 / 清空端点 + key）
    ipc(
        &webview,
        "settings_update",
        serde_json::json!({ "next": Settings::default() }),
    )
    .unwrap();
    assert_eq!(
        ipc(&webview, "ai_reload", serde_json::Value::Null).unwrap(),
        serde_json::Value::Bool(false)
    );
}

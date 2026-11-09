//! GlassSuite 应用层：**一个进程，两个无边框窗口**。
//!
//! 分层（对应卡内 `crates/shell` 的职责，落在 `src-tauri/` 这个 app crate 里）：
//! - `shell`    ：建窗、摆位、内容自适应尺寸（几何全部来自 `winplat`）；
//! - `state`    ：进程级共享状态（媒体通道、热区、穿透标志），供钩子和命令共用；
//! - `island`   ：岛的功能域（系统指标 / 天气 / 网络 / 媒体 / 通知 / 歌词 / AI）；
//! - `settings` ：用户配置（默认全部关闭需要联网或读取隐私数据的功能）。
//!
//! 启动顺序是硬约束，别调换：
//! 1. 声明 DPI 感知（必须在任何窗口创建之前，否则窗口按虚拟像素摆放）；
//! 2. 初始化日志与单实例锁（第二实例直接退出，不创建窗口、不抢焦点）；
//! 3. 创建窗口 → 设为工具窗口 + 不抢焦点 → 摆位 → 最后 show（避免先出现在默认位置）；
//! 4. 窗口就绪后再启动功能域采样线程（早于窗口订阅会丢首帧事件）。

pub mod island;
pub mod settings;
pub mod shell;
pub mod state;

use std::sync::Arc;

use tauri::Manager;

/// 应用入口。`main.rs` 只调这一个函数，便于以后接 `cargo test` 里跑集成测试。
pub fn run() {
    // 1. DPI：全仓唯一调用点。
    if let Err(e) = winplat::dpi::init_per_monitor_v2() {
        eprintln!("GlassSuite: DPI 声明失败：{e}");
    }

    // 2. 日志 + 单实例。guard 必须活到进程结束——写进 match 分支会被提前 Drop。
    let data_dir =
        winplat::config::app_data_dir(winplat::IDENTIFIER).unwrap_or_else(std::env::temp_dir);
    let _ = winplat::log::init(&data_dir);
    winplat::log::log("== GlassSuite 启动 ==");

    let _instance_guard = match winplat::instance::acquire(winplat::instance::NAME) {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => {
            winplat::log::log("already running");
            println!("already running");
            std::process::exit(0);
        }
        Err(e) => {
            // 取不到锁不是致命错误：继续启动，但记下来以便排查。
            winplat::log::log(&format!("单实例锁创建失败，继续启动：{e}"));
            None
        }
    };

    let shared = Arc::new(state::SharedState::default());

    tauri::Builder::default()
        .manage(Arc::clone(&shared))
        .invoke_handler(tauri::generate_handler![
            shell::fit_dock,
            shell::fit_island,
            settings::settings_get,
            island::log_frontend,
            island::media::media_control,
            island::media::media_state,
            island::lyrics::lyrics_state,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // 配置先于窗口：窗口尺寸/内容依赖配置（例如天气是否显示）。
            let settings = settings::load(&data_dir);
            app.manage(settings);

            shell::create_windows(&handle)?;
            winplat::log::log("窗口就绪：island + dock");

            let st = app.state::<Arc<state::SharedState>>().inner().clone();
            cache_hwnds(&handle, &st);

            // 功能域线程（各自内部按配置决定是否真的联网/读取）。
            let s = app.state::<settings::Settings>().inner().clone();

            // 歌词源与 AI 配置必须在媒体/AI 线程启动前定下来：两者都遵循
            // 「默认 null → 零外联」，配置没配齐时模块内部直接短路。
            island::lyrics::configure(s.lyrics_source_url.clone());
            let ai_cfg = island::ai::config::AiConfig::load(&s);

            island::metrics::spawn(handle.clone(), Arc::clone(&st));
            island::weather::spawn(handle.clone(), s.weather.clone());
            island::network::spawn(handle.clone(), s.network_probe_url.clone());
            island::media::spawn(handle.clone(), Arc::clone(&st));
            island::notification::spawn(handle.clone(), s.notifications.clone());
            island::ai::spawn(handle.clone(), ai_cfg, Arc::clone(&st));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Tauri 运行失败");
}

/// 把两窗的原生句柄与可见态写进共享状态，供鼠标钩子做样式对账。
#[cfg(windows)]
fn cache_hwnds(app: &tauri::AppHandle, state: &Arc<state::SharedState>) {
    use std::sync::atomic::Ordering;
    for (label, hwnd_slot, visible_slot) in [
        ("island", &state.island_hwnd, &state.island_visible),
        ("dock", &state.dock_hwnd, &state.dock_visible),
    ] {
        if let Some(win) = app.get_webview_window(label) {
            if let Ok(h) = win.hwnd() {
                hwnd_slot.store(h.0 as isize, Ordering::SeqCst);
            }
            visible_slot.store(true, Ordering::SeqCst);
        }
    }
}

#[cfg(not(windows))]
fn cache_hwnds(_app: &tauri::AppHandle, _state: &Arc<state::SharedState>) {}

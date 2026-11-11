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

pub mod dock;
pub mod island;
pub mod settings;
pub mod shell;
pub mod state;
pub mod window;

use std::sync::{Arc, Mutex};

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
        // updater 只注册能力：`plugins.updater.endpoints` 为空且前端不主动 check()，
        // 因此默认零外联；签名公钥在 tauri.conf.json，私钥只在 CI secrets 里。
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(Arc::clone(&shared))
        .invoke_handler(tauri::generate_handler![
            shell::fit_dock,
            shell::fit_island,
            settings::settings_get,
            settings::settings_update,
            island::ai::ai_reload,
            island::log_frontend,
            island::media::media_control,
            island::media::media_state,
            island::lyrics::lyrics_state,
            window::set_hotzone,
            window::set_ball_hotzone,
            window::set_muted,
            window::ai_disabled,
            window::panel::expand_panel,
            window::panel::collapse_panel,
            dock::dock_state,
            dock::dock_icon,
            dock::dock_launch,
            dock::dock_launch_path,
            dock::dock_reorder,
            dock::dock_auto_hide,
            dock::dock_set_revealed,
            dock::dock_set_free,
            dock::dock_undock_free,
            dock::dock_move_free,
            dock::dock_menu_apps,
            dock::dock_menu_favourites,
            dock::dock_menu_toggle,
            dock::dock_preview_show,
            dock::dock_preview_hide,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // 配置先于窗口：窗口尺寸/内容依赖配置（例如天气是否显示）。
            // 放进 Mutex 是因为 settings_update 要整体替换它（前端保存即生效）。
            let settings = settings::load(&data_dir);
            app.manage(Mutex::new(settings));

            shell::create_windows(&handle)?;
            winplat::log::log("窗口就绪：island + dock");

            let st = app.state::<Arc<state::SharedState>>().inner().clone();
            cache_hwnds(&handle, &st);

            // 单一光标观测源：LL 钩子在主线程 setup 内安装（独立线程消息泵会静默停摆）。
            window::cursor::install(handle.clone(), Arc::clone(&st));

            // 初始穿透只给 island：dock 窗口贴合内容、没有透明宽区，必须保持可点击。
            // 钩子安装失败时降级为「两窗都可交互」，绝不留下收不到任何输入的幽灵窗。
            let passthrough = window::cursor::is_installed();
            if let Some(island_win) = handle.get_webview_window("island") {
                let _ = island_win.set_ignore_cursor_events(passthrough);
            }
            st.island_transparent
                .store(passthrough, std::sync::atomic::Ordering::SeqCst);
            st.dock_transparent
                .store(false, std::sync::atomic::Ordering::SeqCst);
            winplat::log::log(&format!("cursor: island initial passthrough={passthrough}"));

            window::panel::spawn_fullscreen_watch(handle.clone(), Arc::clone(&st));

            // dock 数据层：迁移旧配置 → 载入 → 建开始菜单/游离窗 → 起运行集合 watcher。
            // 迁移必须先于 DockState::new：后者会在目标文件缺失时写种子 dock.json，
            // 把迁移目标占住（真实启动验证时发现过，见 dock::init 文档注释）。
            dock::config::migrate_legacy(&data_dir);
            let dock_state = Arc::new(dock::DockState::new(data_dir.clone()));
            app.manage(Arc::clone(&dock_state));
            dock::init(&handle, &dock_state);

            // 功能域线程（各自内部按配置决定是否真的联网/读取）。
            let s = app
                .state::<Mutex<settings::Settings>>()
                .lock()
                .unwrap()
                .clone();

            // 歌词源与 AI 配置必须在媒体/AI 线程启动前定下来：两者都遵循
            // 「默认 null → 零外联」，配置没配齐时模块内部直接短路。
            island::lyrics::configure(s.lyrics_source_url.clone());
            let ai_cfg = island::ai::config::AiConfig::load(&s);

            // AI / 网络的运行时配置槽：settings_update / ai_reload 换槽即热加载，
            // 常驻线程每轮读取（见 island::ai::AiState / island::network::NetworkState）。
            let ai_state = Arc::new(island::ai::AiState::new(ai_cfg));
            app.manage(Arc::clone(&ai_state));
            let network_state = Arc::new(island::network::NetworkState::new(
                s.network_probe_url.clone(),
            ));
            app.manage(Arc::clone(&network_state));

            island::metrics::spawn(handle.clone(), Arc::clone(&st));
            island::weather::spawn(handle.clone(), s.weather.clone());
            island::network::spawn(handle.clone(), network_state);
            island::media::spawn(handle.clone(), Arc::clone(&st));
            island::notification::spawn(handle.clone(), s.notifications.clone());
            island::ai::spawn(handle.clone(), ai_state, Arc::clone(&st));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Tauri 运行失败");
}

/// 把两窗的原生句柄与可见态写进共享状态，供鼠标钩子做样式对账。
#[cfg(windows)]
fn cache_hwnds(app: &tauri::AppHandle, state: &Arc<state::SharedState>) {
    use std::sync::atomic::Ordering;
    for (label, hwnd_slot, visible_slot, hz_slot, scale_slot, startup) in [
        (
            "island",
            &state.island_hwnd,
            &state.island_visible,
            &state.island_hotzone,
            &state.island_scale,
            shell::STARTUP_ISLAND,
        ),
        (
            "dock",
            &state.dock_hwnd,
            &state.dock_visible,
            &state.dock_hotzone,
            &state.dock_scale,
            shell::STARTUP_DOCK,
        ),
    ] {
        if let Some(win) = app.get_webview_window(label) {
            if let Ok(h) = win.hwnd() {
                hwnd_slot.store(h.0 as isize, Ordering::SeqCst);
            }
            visible_slot.store(true, Ordering::SeqCst);
            // 首帧就用启动尺寸铺满热区：前端上报之前也必须能命中，否则窗口永远穿透。
            *scale_slot.lock().unwrap() = win.scale_factor().unwrap_or(1.0);
            *hz_slot.lock().unwrap() = state::Hotzone {
                x: 0.0,
                y: 0.0,
                w: startup.0,
                h: startup.1,
            };
        }
    }
}

#[cfg(not(windows))]
fn cache_hwnds(_app: &tauri::AppHandle, _state: &Arc<state::SharedState>) {}

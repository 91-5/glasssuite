//! GlassSuite 应用层：**一个进程，三个无边框窗口**。
//!
//! 分层：
//! - `shell`    ：建窗、摆位（几何全部来自 `winplat`）；
//! - `state`    ：进程级共享状态（媒体通道、岛热区、穿透标志），供钩子和命令共用；
//! - `island`   ：岛的功能域（系统指标 / 天气 / 网络 / 媒体 / 通知 / 歌词 / AI）；
//! - `dock`     ：glassdesk v0.8.1 直移植（药丸 / 开始菜单 / 预览 / 游离图标 / 运行集合）；
//! - `settings` ：用户配置（默认全部关闭需要联网或读取隐私数据的功能）。
//!
//! 启动顺序是硬约束，别调换：
//! 1. 声明 DPI 感知（必须在任何窗口创建之前，否则窗口按虚拟像素摆放）；
//! 2. 初始化日志与单实例锁（第二实例直接退出，不创建窗口、不抢焦点）；
//! 3. 创建窗口 → 设为工具窗口 → 摆位 → 显示（dock/startmenu 先隐藏，由各自逻辑显隐）；
//! 4. 窗口就绪后再启动功能域采样线程（早于窗口订阅会丢首帧事件）。

pub mod cli;
pub mod control;
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

    // panic hook 必须在日志就绪后、任何线程启动前安装：默认 panic 只打到 stderr，
    // 而窗口应用的 stderr 无人看——2026-11-14 两次静默退出（Event Log 零记录、
    // 零转储）就是这么失踪的。落进 glasssuite.log 才有破案线索。
    std::panic::set_hook(Box::new(move |info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".into());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string payload>".into());
        winplat::log::log(&format!("PANIC at {loc}: {msg}"));
    }));

    winplat::log::log("== GlassSuite 启动 ==");

    // OC-20261118-001：自研命名锁已移除，改用 tauri-plugin-single-instance。
    // 旧锁在二启时直接 exit(0)、不通知首实例 → 用户双击 .lnk "没反应"。
    // 插件必须在 builder 链首注册（见下方 .plugin(single_instance)）。

    let shared = Arc::new(state::SharedState::default());

    // OC-20261118-003 §2.1 / OC-20261121-003：全局逃生热键（首选 Ctrl+Alt+H，
    // 占用时依次回退 Ctrl+Alt+K / Ctrl+Alt+J）。全藏任务栏/图标后唯一的键盘入口。
    // 注册放在 setup 里用 `on_shortcut`（返回 Result，可捕获），**不用 Builder::with_shortcut**——
    // 后者把 OS 级注册错误装进 plugin.setup 的 `?`，会让 tauri build() 返回 Err；
    // 若再被 unwrap_or_else(panic) 放大就是全 app 崩溃（2026-11-21 真机实测：ctrl+alt+g
    // 被占用 → 整个 app 起不来，桌面回原生态）。现在占用只降级为"告警 + 热键不工作"。
    let hotkey_plugin = tauri_plugin_global_shortcut::Builder::new().build();

    tauri::Builder::default()
        // OC-20261118-001：单实例唤起控制页。必须第一个注册。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            winplat::log::log("second instance: 唤起控制页");
            let _ = control::ensure_control_window(app);
        }))
        // OC-20261118-003：全局逃生热键（注册失败也挂空插件，保持 handler 链稳定）。
        .plugin(hotkey_plugin)
        // updater 只注册能力：`plugins.updater.endpoints` 为空且前端不主动 check()，
        // 因此默认零外联；签名公钥在 tauri.conf.json，私钥只在 CI secrets 里。
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(Arc::clone(&shared))
        .invoke_handler(tauri::generate_handler![
            // shell（岛几何）
            shell::fit_island,
            // 设置
            settings::settings_get,
            settings::settings_update,
            island::ai::ai_reload,
            island::ai::ai_test_connection,
            // 控制中心四开关（OC-20261118-002）
            control::api::control_get_state,
            control::api::control_hotkey_status,
            control::api::control_live_status,
            control::api::control_set_island,
            control::api::control_set_dock,
            control::api::control_set_icons,
            control::api::control_set_taskbar,
            // 岛功能域
            island::log_frontend,
            island::media::media_control,
            island::media::media_state,
            island::lyrics::lyrics_state,
            // 岛窗口交互
            window::set_hotzone,
            window::set_ball_hotzone,
            window::set_muted,
            window::ai_disabled,
            window::panel::expand_panel,
            window::panel::collapse_panel,
            // dock（glassdesk v0.8.1 命令层，dock/api.rs）
            dock::api::dock_ready,
            dock::api::list_shortcuts,
            dock::api::add_shortcut,
            dock::api::remove_shortcut,
            dock::api::reorder_shortcuts,
            dock::api::launch_shortcut,
            dock::api::fit_dock,
            dock::api::preview_show,
            dock::api::preview_show_transient,
            dock::api::preview_hide,
            dock::api::preview_cursor_inside,
            dock::api::menu_toggle,
            dock::api::menu_hide,
            dock::api::get_menu_apps,
            dock::api::menu_add,
            dock::api::menu_remove,
            dock::api::menu_launch,
            dock::api::debug_note,
            // OC-20261115-002：按压手势交互锁（仅此两行注册，几何逻辑在 dock/api.rs）
            dock::api::dock_interaction_begin,
            dock::api::dock_interaction_end,
            dock::api::power_lock,
            dock::api::power_sleep,
            dock::api::power_shutdown,
            dock::api::dock_running_snapshot,
            dock::api::pin_transient,
            dock::api::transient_activate,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // OC-20261118-003 §2.1：在 setup 里注册逃生热键（见上方 build 处说明）。
            // on_shortcut 返回 Result：占用/失败只记日志 + 置标志让控制页告警，绝不 panic。
            // OC-20261121-003：改为多候选依次尝试，首个注册成功者生效——单一组合被占用
            // 就彻底没有逃生入口是安全隐患（浮香 #8 advisory），换键 + fallback 一并根治。
            {
                use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
                // H=Hide 首选；K/J 作后备。均为本机实测空闲组合（2026-11-21）。
                const CANDIDATES: [&str; 3] = ["ctrl+alt+h", "ctrl+alt+k", "ctrl+alt+j"];
                let mut registered: Option<&str> = None;
                for combo in CANDIDATES {
                    let reg = app.global_shortcut().on_shortcut(combo, |app, _sc, ev| {
                        if ev.state == ShortcutState::Pressed {
                            // 不写死键名：闭包不捕获循环变量，避免 'static 生命周期问题。
                            winplat::log::log("逃生热键触发：唤起控制页");
                            let _ = control::ensure_control_window(app);
                        }
                    });
                    if reg.is_ok() {
                        registered = Some(combo);
                        break;
                    }
                }
                match registered {
                    Some(combo) => {
                        control::state::set_hotkey_registered(true);
                        winplat::log::log(&format!("逃生热键 {combo} 注册成功"));
                    }
                    None => {
                        control::state::set_hotkey_registered(false);
                        winplat::log::log(
                            "逃生热键注册失败（三个候选均被占用/无效）：控制页将告警，app 正常运行",
                        );
                    }
                }
            }

            // 配置先于窗口：窗口尺寸/内容依赖配置（例如天气是否显示）。
            // 放进 Mutex 是因为 settings_update 要整体替换它（前端保存即生效）。
            let settings = settings::load(&data_dir);
            app.manage(Mutex::new(settings.clone()));

            // OC-20261118-002：守卫意图初始化 + 心跳。
            // 必须在窗口创建前写意图（守卫读文件即可生效），心跳线程保证活着时持续刷新。
            control::state::initialize(
                settings.control.taskbar_hidden,
                settings.control.desktop_icons_hidden,
            );
            control::state::spawn_heartbeat();

            // 窗口：island 立即显示；dock / startmenu 先隐藏（glassdesk 模型）。
            shell::create_windows(&handle)?;
            shell::create_dock_windows(&handle)?;
            winplat::log::log("窗口就绪：island + dock + startmenu");

            // OC-20261118-002：窗口创建后应用持久化的四开关（岛/Dock 初始可见态）。
            // 必须在 create_windows 之后，否则 get_webview_window 返回 None。
            control::api::apply_on_boot(&handle, &settings.control);

            // OC-20261118-001：首实例若带 --control（桌面 .lnk）则弹控制页。
            // 开机 Run key 不带此参数 → 只岛+dock 自启，桌面保持原生。
            control::boot_if_requested(&handle);

            let st = app.state::<Arc<state::SharedState>>().inner().clone();
            cache_hwnd(&handle, &st);

            // 单一光标观测源：LL 钩子在主线程 setup 内安装（独立线程消息泵会静默停摆）。
            window::cursor::install(handle.clone(), st.clone());

            // 初始穿透只给 island：dock 窗口贴合内容、没有透明宽区，必须保持可点击。
            // 钩子安装失败时降级为「两窗都可交互」，绝不留下收不到任何输入的幽灵窗。
            let passthrough = window::cursor::is_installed();
            if let Some(island_win) = handle.get_webview_window("island") {
                let _ = island_win.set_ignore_cursor_events(passthrough);
            }
            st.island_transparent
                .store(passthrough, std::sync::atomic::Ordering::SeqCst);
            winplat::log::log(&format!("cursor: island initial passthrough={passthrough}"));

            window::panel::spawn_fullscreen_watch(handle.clone(), Arc::clone(&st));

            // ---- dock 域（glassdesk setup 移植）----
            dock_setup(&handle);

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

            island::metrics::spawn(handle.clone(), st.clone());
            island::weather::spawn(handle.clone(), s.weather.clone());
            island::network::spawn(handle.clone(), network_state);
            island::media::spawn(handle.clone(), st.clone());
            // 通知域：默认关（零线程零监听）；总开关热启停经 NotificationState
            // 由 settings_update 驱动（OC-20261113-001 P2）。
            let notif_state = Arc::new(island::notification::NotificationState::new());
            app.manage(Arc::clone(&notif_state));
            island::notification::spawn(handle.clone(), s.notifications.clone(), notif_state);
            island::ai::spawn(handle.clone(), ai_state, st);
            Ok(())
        })
        .build(tauri::generate_context!())
        .map(run_with_exit_logging)
        .unwrap_or_else(|e| {
            winplat::log::log(&format!("Tauri 构建失败: {e}"));
            panic!("Tauri 构建失败: {e}");
        });
}

/// 事件循环退出原因观测（只记日志，不改变行为）。
///
/// `.run()` 不给退出原因；`build().run(callback)` 能看到 `RunEvent::ExitRequested`
/// （所有窗口关闭/显式退出）与 `RunEvent::Exit`。2026-11-14 两次静默退出时
/// 日志戛然而止、无任何记录——有了这里，下次失踪至少留一行「死于何因」。
fn run_with_exit_logging(app: tauri::App) {
    use tauri::RunEvent;
    app.run(|app_handle, event| {
        if let RunEvent::ExitRequested { code, .. } = event {
            winplat::log::log(&format!(
                "exit requested: code={code:?} windows_alive={}",
                app_handle
                    .webview_windows()
                    .iter()
                    .filter(|(_, w)| w.is_visible().unwrap_or(false))
                    .count()
            ));
        }
    });
}

/// dock 域初始化（从 glassdesk `lib.rs` setup 移植）。
///
/// 顺序硬约束：玻璃 → 工具窗口 → 按配置置顶 → 隐藏 → 数据层 → 逻辑线程。
/// `dock_logic` 与 `spawn_*` 依赖窗口已存在，故必须在 `create_dock_windows` 之后。
fn dock_setup(handle: &tauri::AppHandle) {
    use std::sync::atomic::AtomicBool;

    // dock 窗口：磨砂玻璃 + 工具窗口 + 按配置置顶 + 初始隐藏（dock_logic 决定何时显示）。
    if let Some(d) = handle.get_webview_window("dock") {
        #[cfg(windows)]
        if let Ok(h) = d.hwnd() {
            let scale = d.scale_factor().unwrap_or(1.0);
            let radius = (18.0 * scale) as i32;
            let _ =
                dock::desktop::enable_glass(windows::Win32::Foundation::HWND(h.0), Some(radius));
            let _ = winplat::style::hide_from_alt_tab(h.0 as isize);
        }
        let top = handle
            .get_webview_window("dock")
            .and_then(|_| dock::config_path(handle))
            .map(|p| dock::load_config(&p))
            .map(|c| c.dock_top_layer)
            .unwrap_or(false);
        let _ = d.set_always_on_top(top);
        let _ = d.hide();
    }

    // 开始菜单面板：同款玻璃 + 工具窗口 + 初始隐藏；失焦收回。
    if let Some(sm) = handle.get_webview_window("startmenu") {
        #[cfg(windows)]
        if let Ok(h) = sm.hwnd() {
            let scale = sm.scale_factor().unwrap_or(1.0);
            let radius = (14.0 * scale) as i32;
            let _ =
                dock::desktop::enable_glass(windows::Win32::Foundation::HWND(h.0), Some(radius));
            let _ = winplat::style::hide_from_alt_tab(h.0 as isize);
        }
        let _ = sm.hide();
        let app = handle.clone();
        sm.on_window_event(move |event| {
            if let tauri::WindowEvent::Focused(focused) = event {
                if *focused {
                    dock::menu::log_focus(true);
                } else {
                    dock::menu::log_focus(false);
                    dock::menu::on_blur(&app);
                }
            }
        });
    }

    // dock 主逻辑线程（自动隐藏 / 置顶 / 任务栏让位）。
    let h = handle.clone();
    std::thread::spawn(move || {
        dock::dock_logic(h, std::sync::Arc::new(AtomicBool::new(false)));
    });

    // 运行集合 watcher（dock running 图标与临时图标共用）。
    // 006：先注入句柄——watcher 发现运行集合变化时主动推统一快照事件。
    dock::running::set_app_handle(handle.clone());
    dock::running::spawn_watcher();

    // 预览渲染进程：读 dock.json 同级目录的图标缓存。
    if let Some(dir) = dock::config_path(handle).and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        dock::preview::spawn(handle.clone(), dir);
    }

    // 开始菜单数据源。
    if let Some(dir) = dock::config_path(handle).and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        dock::menu::init(dir);
    }
}

/// 把 island 的原生句柄与可见态写进共享状态，供鼠标钩子做样式对账。
#[cfg(windows)]
fn cache_hwnd(app: &tauri::AppHandle, state: &Arc<state::SharedState>) {
    use std::sync::atomic::Ordering;
    if let Some(win) = app.get_webview_window("island") {
        if let Ok(h) = win.hwnd() {
            state.island_hwnd.store(h.0 as isize, Ordering::SeqCst);
        }
        state.island_visible.store(true, Ordering::SeqCst);
        // 首帧就用启动尺寸铺满热区：前端上报之前也必须能命中，否则窗口永远穿透。
        *state.island_scale.lock().unwrap() = win.scale_factor().unwrap_or(1.0);
        *state.island_hotzone.lock().unwrap() = state::Hotzone {
            x: 0.0,
            y: 0.0,
            w: shell::STARTUP_ISLAND.0,
            h: shell::STARTUP_ISLAND.1,
        };
    }
}

#[cfg(not(windows))]
fn cache_hwnd(_app: &tauri::AppHandle, _state: &Arc<state::SharedState>) {}

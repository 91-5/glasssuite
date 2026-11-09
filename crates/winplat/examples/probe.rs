//! 平台层现场探针：打印本机真实的屏幕 / 任务栏 / 前台全屏判定结果。
//!
//! 用途：改 `winplat` 的几何计算后，先跑这个对比一眼，不要靠想象。
//!
//! ```powershell
//! cargo run -p winplat --example probe
//! ```
//!
//! 输出全部是物理像素。与「设置 → 显示 → 缩放 125%」对照时注意：`monitor.bounds`
//! 应等于分辨率（如 1920×1080），而不是逻辑尺寸（1536×864）。

fn main() {
    let declared = winplat::dpi::init_per_monitor_v2();
    println!("dpi: {:?}", declared);
    println!("dpi context: {:?}", winplat::dpi::current_context());

    let primary = winplat::screen::primary_screen();
    println!("primary screen: {primary:?}");

    for (i, m) in winplat::screen::monitors().iter().enumerate() {
        println!(
            "monitor[{i}] primary={} bounds={:?} work={:?}",
            m.primary, m.bounds, m.work
        );
    }

    let bars = winplat::taskbar::taskbars();
    if bars.is_empty() {
        println!("taskbar: none (亦可能任务栏被替换为第三方实现)");
    }
    for b in &bars {
        println!(
            "taskbar primary={} visible={} rect={:?} monitor={:?}",
            b.primary, b.visible, b.rect, b.monitor
        );
        // dock 贴该任务栏上沿、留 6px 时窗口左上角（与生产口径一致）。
        println!(
            "  -> dock y (h=76, margin=6): {}",
            b.rect.above_anchored_y(76, 6)
        );
        println!("  -> dock x (w=1250): {}", b.monitor.centered_x(1250));
    }

    match winplat::fullscreen::foreground() {
        Some(f) => println!(
            "foreground: class={:?} rect={:?} monitor={:?} fullscreen={}",
            f.class, f.rect, f.monitor, f.fullscreen
        ),
        None => println!("foreground: none"),
    }

    println!("single instance name: {}", winplat::instance::NAME);
    // 注意：guard 必须绑定到 main 作用域——写进 match 分支里会在该分支结束时就 Drop，
    // 互斥量当场释放（第一版探针实际踩过这个坑）。
    let instance_guard = winplat::instance::acquire(winplat::instance::NAME);
    match &instance_guard {
        Ok(Some(_)) => println!("single instance: acquired (本次为首个实例)"),
        Ok(None) => println!("single instance: already running (应退出码 0)"),
        Err(e) => println!("single instance: error {e}"),
    }
    println!(
        "app data dir: {:?}",
        winplat::config::app_data_dir(winplat::IDENTIFIER)
    );

    // 持有单实例锁 2 秒：并发跑两次本探针，第二次应打印 "already running"。
    std::thread::sleep(std::time::Duration::from_millis(2000));
}

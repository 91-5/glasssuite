//! 系统指标采样 —— CPU/内存/电池/GPU，1s 一轮，统一调度器。
//!
//! 为什么放在 Rust：`sysinfo` 每次刷新要遍历进程表，放前端就是每秒几十个 IPC 往返；
//! 放后端一次采样一次广播，界面只管画。
//!
//! 电池走 `GetSystemPowerStatus`（台式机返回 `None`）；GPU 走 NVML 直连（无 NVIDIA 卡
//! 返回 `None`）。两者不可用都不是错误，界面显示 "—" 即可。

use std::time::Duration;

use sysinfo::System;
use tauri::{AppHandle, Emitter};
use winplat::{gpu::GpuProbe, power};

/// 采样周期。1s 是「图表顺滑」与「不吵醒 CPU」的折中；面板收起时前端只是不画，
/// 后端照采（`sysinfo` 1s 轮询开销可忽略，实测 <0.1% 单核）。
const TICK: Duration = Duration::from_secs(1);

pub fn spawn(handle: AppHandle) {
    std::thread::spawn(move || {
        let mut sys = System::new_all();
        let mut gpu = GpuProbe::init();

        // 首次 refresh：CPU 使用率需要两次采样间隔才有有效值。
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        std::thread::sleep(Duration::from_millis(100));
        sys.refresh_cpu_usage();
        sys.refresh_memory();

        loop {
            std::thread::sleep(TICK);

            // 必须同时刷新 CPU 和内存 —— 否则内存值是初始快照，图表不会动。
            sys.refresh_cpu_usage();
            sys.refresh_memory();

            let cpu = sys.global_cpu_usage() as f64;
            let mem_total = sys.total_memory();
            let mem_used = sys.used_memory();
            let mem_pct = if mem_total > 0 {
                mem_used as f64 * 100.0 / mem_total as f64
            } else {
                0.0
            };

            let battery = power::battery().map(|b| {
                serde_json::json!({
                    "charge": b.charge,
                    "state": if b.charging { "charging" } else { "discharging" },
                })
            });

            let _ = handle.emit(
                "sys-metrics",
                serde_json::json!({
                    "cpu": cpu,
                    "memUsed": mem_used,
                    "memTotal": mem_total,
                    "memPct": mem_pct,
                    "battery": battery,
                    "gpu": gpu.as_mut().and_then(|g| g.utilization()),
                }),
            );
        }
    });
}

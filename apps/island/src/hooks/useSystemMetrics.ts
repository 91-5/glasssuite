import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

export interface SysMetrics {
  cpu: number;
  memUsed: number;
  memTotal: number;
  battery: { charge: number; state: string } | null;
  /** GPU 使用率 0-100；无 NVIDIA 卡 / 驱动缺失时为 null */
  gpu: number | null;
}

export interface MetricsHistory {
  cpu: number[];
  mem: number[];
  gpu: number[];
}

const MAX_POINTS = 60; // 60s 历史

/**
 * 系统指标 hook —— 订阅 Rust sysinfo 事件。
 * dp 大脑评审 #3.4：统一 Rust 调度器推送，前端订阅，不各自轮询。
 */
export function useSystemMetrics(): { metrics: SysMetrics | null; history: MetricsHistory } {
  const [metrics, setMetrics] = useState<SysMetrics | null>(null);
  const [history, setHistory] = useState<MetricsHistory>({ cpu: [], mem: [], gpu: [] });

  useEffect(() => {
    const un = listen<SysMetrics>("sys-metrics", (e) => {
      const m = e.payload;
      const memPct = m.memTotal > 0 ? (m.memUsed / m.memTotal) * 100 : 0;
      setMetrics(m);
      setHistory((prev) => {
        const cpu = [...prev.cpu, m.cpu];
        const mem = [...prev.mem, memPct];
        // GPU 缺失时沿用上一个点（避免曲线断掉），全程无值就不入列
        const prevGpu = prev.gpu[prev.gpu.length - 1];
        const gpuValue = m.gpu ?? prevGpu ?? null;
        const gpu = gpuValue === null ? prev.gpu : [...prev.gpu, gpuValue];
        if (cpu.length > MAX_POINTS) cpu.shift();
        if (mem.length > MAX_POINTS) mem.shift();
        if (gpu.length > MAX_POINTS) gpu.shift();
        return { cpu, mem, gpu };
      });
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  return { metrics, history };
}

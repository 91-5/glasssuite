/** 展开面板的指标历史：只订阅后端 1s 事件，不在前端另起采样定时器。 */
import { useEffect, useState } from "react";
import { EVENTS, subscribe, type SysMetrics } from "@glasssuite/ipc-types";

const MAX_POINTS = 30;

export interface MetricsSample {
  cpu: number;
  mem: number;
  gpu: number | null;
}

export function useMetricsHistory() {
  const [latest, setLatest] = useState<SysMetrics | null>(null);
  const [history, setHistory] = useState<MetricsSample[]>([]);

  useEffect(
    () =>
      subscribe<SysMetrics>(EVENTS.sysMetrics, (m) => {
        setLatest(m);
        setHistory((prev) => [
          ...prev.slice(-(MAX_POINTS - 1)),
          { cpu: m.cpu, mem: m.memPct, gpu: m.gpu },
        ]);
      }),
    [],
  );

  return { latest, history };
}

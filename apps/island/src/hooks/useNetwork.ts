import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

export interface NetworkStatus {
  online: boolean;
  latency: number;
}

const MAX_POINTS = 60;

/**
 * 网络状态 hook —— 订阅 network-status 事件 + 延迟历史曲线。
 */
export function useNetwork(): { status: NetworkStatus | null; history: number[] } {
  const [status, setStatus] = useState<NetworkStatus | null>(null);
  const [history, setHistory] = useState<number[]>([]);

  useEffect(() => {
    const un = listen<NetworkStatus>("network-status", (e) => {
      const s = e.payload;
      setStatus(s);
      if (s.online) {
        setHistory((prev) => {
          const next = [...prev, s.latency];
          if (next.length > MAX_POINTS) next.shift();
          return next;
        });
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  return { status, history };
}

/**
 * 岛的数据钩子：订阅后端事件 + 读一次配置。
 *
 * 全部用 `subscribe` 而不是前端定时 invoke：事件是后端 1s/15min/30min 的节奏，
 * 前端只当「最新快照」用，不需要自己算轮询。
 */
import { useEffect, useState } from "react";
import {
  EVENTS,
  getSettings,
  subscribe,
  type Settings,
  type SysMetrics,
  type WeatherSnapshot,
} from "@glasssuite/ipc-types";

/** 用户配置；`null` = 还没读到（首帧用默认布局，避免闪烁）。 */
export function useSettings(): Settings | null {
  const [settings, setSettings] = useState<Settings | null>(null);

  useEffect(() => {
    let alive = true;
    getSettings()
      .then((s) => {
        if (alive) setSettings(s);
      })
      .catch(() => {
        /* 非 Tauri 环境：当作未配置 */
      });
    return () => {
      alive = false;
    };
  }, []);

  return settings;
}

/** 天气快照；`null` = 未配置或还没收到第一份数据。 */
export function useWeather(): WeatherSnapshot | null {
  const [weather, setWeather] = useState<WeatherSnapshot | null>(null);
  useEffect(() => subscribe<WeatherSnapshot>(EVENTS.weather, setWeather), []);
  return weather;
}

/** 电池（无电池的台式机保持 `null`）。 */
export function useBattery(): SysMetrics["battery"] {
  const [battery, setBattery] = useState<SysMetrics["battery"]>(null);
  useEffect(
    () =>
      subscribe<SysMetrics>(EVENTS.sysMetrics, (m) => {
        // 只在电量整数值变化时才 setState：1s 一次的新对象引用会让药丸每秒钟重建一次。
        setBattery((prev) => {
          if (prev === null && m.battery === null) return prev;
          if (
            prev &&
            m.battery &&
            Math.round(prev.charge) === Math.round(m.battery.charge) &&
            prev.state === m.battery.state
          ) {
            return prev;
          }
          return m.battery;
        });
      }),
    [],
  );
  return battery;
}

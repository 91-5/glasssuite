/**
 * 岛的数据钩子：订阅后端事件 + 读一次配置。
 *
 * 全部用 `subscribe` 而不是前端定时 invoke：事件是后端 1s/15min/30min 的节奏，
 * 前端只当「最新快照」用，不需要自己算轮询。
 */
import { useEffect, useState } from "react";
import {
  EVENTS,
  aiReload,
  getSettings,
  logFrontend,
  subscribe,
  updateSettings,
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

/** `saveSettings` 的结果：保存后的配置 + 后端视角「AI 是否已配齐」。 */
export interface SaveResult {
  saved: Settings;
  /** enabled + base_url + key 三项齐备；ai_reload 兜底失败时按保存值推算。 */
  aiConfigured: boolean;
}

/**
 * 保存配置并触发 AI 重载（卡片 D2 的「保存 → reload」链路）。
 *
 * `settings_update` 已在后端原子应用（AI / 网络探测随之热加载），这里的 `ai_reload`
 * 是幂等兜底：失败不影响保存结果（配置已经生效），只记一条前端诊断日志。
 * 写盘失败时抛错，由调用方提示。
 */
export async function saveSettings(next: Settings): Promise<SaveResult> {
  const saved = await updateSettings(next);
  const aiConfigured = await aiReload().catch(() => {
    logFrontend("[settings] ai_reload 兜底失败（settings_update 已应用，按保存值推算）").catch(
      () => {
        /* 非 Tauri 环境 */
      },
    );
    // 兜底失败时按保存值推算给状态条用；env 密钥场景可能因此少算，不影响真实行为。
    return Boolean(saved.aiBaseUrl && saved.aiApiKey && saved.aiEnabled !== false);
  });
  return { saved, aiConfigured };
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

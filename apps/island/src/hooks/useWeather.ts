import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

export interface WeatherData {
  current_weather: {
    temperature: number;
    windspeed: number;
    winddirection: number;
    weathercode: number;
    is_day: number;
  };
}

/** GlassSuite 的归一天气快照（`@glasssuite/ipc-types` 的 WeatherSnapshot）。 */
interface RawWeather {
  tempC: number;
  code: number;
  isDay: boolean;
}

/** 归一封装的天气快照 → Open-Meteo 风格（组件层沿用 glasspill 的字段名）。 */
function toWeatherData(w: RawWeather): WeatherData {
  return {
    current_weather: {
      temperature: w.tempC,
      windspeed: 0,
      winddirection: 0,
      weathercode: w.code,
      is_day: w.isDay ? 1 : 0,
    },
  };
}

/**
 * 天气 hook —— 订阅 Rust weather 事件。
 * Open-Meteo 15min 刷新，展开时显示缓存。
 */
export function useWeather(): WeatherData | null {
  const [weather, setWeather] = useState<WeatherData | null>(null);

  useEffect(() => {
    const un = listen<RawWeather>("weather", (e) => {
      setWeather(toWeatherData(e.payload));
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  return weather;
}

/** WMO weathercode → 中文简述 */
export function weatherLabel(code: number): string {
  if (code === 0) return "晴";
  if (code === 1 || code === 2) return "多云";
  if (code === 3) return "阴";
  if (code === 45 || code === 48) return "雾";
  if (code >= 51 && code <= 65) return "雨";
  if (code >= 66 && code <= 67) return "雪";
  if (code >= 71 && code <= 77) return "雪";
  if (code >= 80 && code <= 82) return "阵雨";
  if (code >= 95) return "雷暴";
  return "—";
}

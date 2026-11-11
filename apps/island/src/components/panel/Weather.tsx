import type { WeatherData } from "../../hooks/useWeather";
import { weatherLabel } from "../../hooks/useWeather";

/** 城市名（坐标 21.27°N, 110.00°E → 广东海洋大学/湛江） */
const CITY = "湛江";

interface WeatherProps {
  weather: WeatherData | null;
}

/**
 * 天气小组件 —— Open-Meteo 数据，15min 刷新。
 * sir 2026-10-26: 风速不要展示，只保留城市+温度+天气标签
 */
export function Weather({ weather }: WeatherProps) {
  if (!weather) {
    return <div className="weather-widget dim">天气加载中…</div>;
  }

  const cw = weather.current_weather;

  return (
    <div className="weather-widget">
      <div className="weather-main">
        <span className="weather-city">{CITY}</span>
        <span className="weather-temp">{cw.temperature.toFixed(1)}°C</span>
        <span className="weather-label">{weatherLabel(cw.weathercode)}</span>
      </div>
    </div>
  );
}

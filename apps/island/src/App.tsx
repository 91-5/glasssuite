import { useEffect, useRef, useState } from "react";
import { GlassSurface, useFitWindow } from "@glasssuite/shared-ui";
import { useBattery, useSettings, useWeather } from "./hooks";
import { useLyrics } from "./lyrics/useLyrics";
import { Artwork, Controls, Progress, useMedia } from "./media";
import { PrivacyNotice, SummaryBar } from "./notification/SummaryBar";
import { useNotifications, usePrivacyNotice } from "./notification/useNotifications";

/** 药丸到窗口边缘的留白（逻辑像素），必须与 `island.css` 里 `.island` 的 padding 一致。 */
const ISLAND_PADDING = 4;

/** 5 秒一刷：界面只显示到分钟，1 秒一刷纯属浪费重渲染。 */
const CLOCK_TICK_MS = 5_000;

const WEEKDAYS = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

function formatTime(date: Date): string {
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

function formatDate(date: Date): string {
  return `${date.getMonth() + 1}月${date.getDate()}日 ${WEEKDAYS[date.getDay()]}`;
}

/** WMO 天气代码 → 图标。够用的粗分类，不做一码一图。 */
function weatherGlyph(code: number, isDay: boolean): string {
  if (code === 0) return isDay ? "☀" : "☾";
  if (code <= 2) return isDay ? "⛅" : "☁";
  if (code === 3) return "☁";
  if (code <= 48) return "≋"; // 雾
  if (code <= 57) return "☂"; // 毛毛雨
  if (code <= 67) return "☂";
  if (code <= 77) return "❄";
  if (code <= 82) return "☂";
  if (code <= 86) return "❄";
  return "⚡";
}

/**
 * island 窗口根组件。
 *
 * 内容优先级：首启隐私提示（一次性） > 通知摘要条（6s） > 媒体 > 时钟。
 * 岛宽度/高度随内容变化，尺寸由 `useFitWindow` 报给后端 resize，位置由 Rust 侧统一算。
 */
export default function App() {
  const pillRef = useRef<HTMLDivElement>(null);
  const [now, setNow] = useState(() => new Date());
  const settings = useSettings();
  const weather = useWeather();
  const battery = useBattery();
  const { media, positionMs, control } = useMedia();
  const lyric = useLyrics(positionMs);
  const notification = useNotifications();
  const showPrivacyNotice = usePrivacyNotice(Boolean(settings?.notifications?.enabled));
  useFitWindow("island", pillRef, ISLAND_PADDING);

  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), CLOCK_TICK_MS);
    return () => window.clearInterval(id);
  }, []);

  const showWeather = Boolean(settings?.weather) && weather !== null;

  let content;
  if (showPrivacyNotice) {
    content = <PrivacyNotice />;
  } else if (notification) {
    content = <SummaryBar item={notification} />;
  } else if (media) {
    content = (
      <div className="island-media">
        <div className="island-media-row">
          <Artwork src={media.artwork} />
          <span className="island-media-text">
            <span className="island-media-title" title={media.title}>
              {media.title || "未知曲目"}
            </span>
            <span className="island-media-artist">{media.artist || "未知艺人"}</span>
          </span>
          <Controls
            playing={media.playing}
            canPlay={media.canPlay}
            canPause={media.canPause}
            canNext={media.canNext}
            canPrev={media.canPrev}
            onAction={(action) => control(action)}
          />
        </div>
        <Progress
          positionMs={positionMs}
          durationMs={media.durationMs}
          onSeek={(ms) => control("seek", ms)}
        />
        {lyric ? <span className="island-lyric" title={lyric}>{lyric}</span> : null}
      </div>
    );
  } else {
    content = (
      <>
        <span className="island-clock">{formatTime(now)}</span>
        <span className="island-divider" aria-hidden="true" />
        <span className="island-date">{formatDate(now)}</span>

        {showWeather && weather ? (
          <>
            <span className="island-divider" aria-hidden="true" />
            <span className="island-weather" title={`WMO ${weather.code}`}>
              <span className="island-weather-glyph" aria-hidden="true">
                {weatherGlyph(weather.code, weather.isDay)}
              </span>
              {Math.round(weather.tempC)}°
            </span>
          </>
        ) : null}

        {battery ? (
          <span
            className="island-battery"
            title={battery.state === "charging" ? "充电中" : "使用电池"}
          >
            <span className="island-battery-glyph" aria-hidden="true">
              {battery.state === "charging" ? "⚡" : ""}
            </span>
            {Math.round(battery.charge)}%
          </span>
        ) : null}
      </>
    );
  }

  const isMedia = !showPrivacyNotice && !notification && Boolean(media);

  return (
    <div className="island">
      <GlassSurface
        className={`island-pill${isMedia ? " island-pill--media" : ""}`}
        radius={22}
        padding={isMedia ? "8px 12px" : "6px 16px"}
        ref={pillRef}
      >
        {content}
      </GlassSurface>
    </div>
  );
}

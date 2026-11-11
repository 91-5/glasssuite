import { useEffect, useMemo, useState, type CSSProperties, type MouseEvent } from "react";
import type { MediaState } from "../../types";
import { Artwork } from "./Artwork";
import { Controls } from "./Controls";
import { Progress } from "./Progress";
import { Clock } from "./Clock";
import { LineChart } from "../panel/LineChart";
import { Weather } from "../panel/Weather";
import type { SysMetrics, MetricsHistory } from "../../hooks/useSystemMetrics";
import type { WeatherData } from "../../hooks/useWeather";
import type { NetworkStatus } from "../../hooks/useNetwork";
import { currentLyricText, type LyricsState } from "../../hooks/useLyrics";

export type IslandMode = "collapsed" | "hover" | "expanded";

interface PillProps {
  media: MediaState | null;
  positionMs: number;
  mode: IslandMode;
  tint: string;
  idle: boolean;
  metrics: SysMetrics | null;
  history: MetricsHistory;
  weather: WeatherData | null;
  musicActive: boolean;
  battery: { charge: number; state: string } | null;
  network: NetworkStatus | null;
  networkHistory: number[];
  lyrics: LyricsState | null;
  onExpand: () => void;
  onControl: (action: string) => void;
  onSeek: (ms: number) => void;
}

/**
 * 药丸容器。
 * - layer-compact: 收拢/悬停态 —— 时钟 + 左侧轮换 + 右侧歌名/轮换
 * - layer-expanded: 展开态 —— 音乐控制 + CPU/内存/FPS/网络图表 + 天气
 *
 * sir 2026-10-26: 眼睛→时钟，删AI聊天，修溢出，轮换滚动+长度一致。
 */
export function Pill({ media, positionMs, mode, tint, idle, metrics, history, weather, musicActive, battery, network, networkHistory, lyrics, onExpand, onControl, onSeek }: PillProps) {
  const playing = media?.playing ?? false;

  /**
   * 当前歌词行 —— 按插值后的 positionMs 本地二分选行，无额外定时器。
   * 暂停时 positionMs 不前进，行自然停住，比清空更自然。
   */
  const lyricText = useMemo(() => {
    if (!lyrics || lyrics.lines.length === 0) return "";
    return currentLyricText(lyrics.lines, positionMs);
  }, [lyrics, positionMs]);

  // 左侧轮换：内存↔CPU
  const [leftIndex, setLeftIndex] = useState(0);
  // 右侧轮换：电量↔天气（仅无媒体时）
  const [rightIndex, setRightIndex] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => {
      setLeftIndex((i) => (i + 1) % 2);
      setRightIndex((i) => (i + 1) % 2);
    }, 3000);
    return () => clearInterval(timer);
  }, []);

  const leftText =
    leftIndex === 0
      ? metrics && metrics.memTotal > 0
        ? `内存 ${((metrics.memUsed / metrics.memTotal) * 100).toFixed(0)}%`
        : ""
      : metrics
        ? `CPU ${metrics.cpu.toFixed(0)}%`
        : "";

  const rightText =
    rightIndex === 0
      ? battery
        ? `电量 ${battery.charge.toFixed(0)}%`
        : ""
      : weather
        ? `${weather.current_weather.temperature.toFixed(0)}°C`
        : "";

  const handleClick = (e: MouseEvent) => {
    e.stopPropagation();
    if (mode !== "expanded") onExpand();
  };

  return (
    <div
      className={`pill ${mode}`}
      style={{ "--tint": tint } as CSSProperties}
      onClick={handleClick}
    >
      {/* 收拢/悬停层
          - 播放态：封面在最左 + 右侧歌词（无歌词时退回歌名）
          - 空闲态：左右轮换信息 + 时钟
          通知不上主岛：岛下方只显示 AI 归纳（InsightBar），原文归 Windows 横幅 */}
      <div className={`layer-compact ${idle ? "idle" : ""}`}>
        {media ? (
          <>
            <Artwork media={media} size="sm" />
            <div
              className="collapsed-lyric"
              key={lyricText || media.title}
              title={lyricText || media.title}
            >
              {lyricText || media.title || media.artist || ""}
            </div>
          </>
        ) : (
          <>
            {leftText && (
              <div key={`l${leftIndex}`} className="collapsed-rotate collapsed-rotate-left">
                {leftText}
              </div>
            )}
            <Clock musicActive={musicActive} />
            {rightText && (
              <div key={`r${rightIndex}`} className="collapsed-rotate collapsed-rotate-right">
                {rightText}
              </div>
            )}
          </>
        )}
      </div>

      {/* 展开层 —— 删AI聊天，修溢出 */}
      <div className="layer-expanded">
        <div className="exp-head">
          <Artwork media={media} size="lg" />
          <div className="meta wide">
            <div className="title">{media?.title || ""}</div>
            <div className="artist">{media?.artist || "—"}</div>
            <div className="album">{media?.album || ""}</div>
            {lyricText && <div className="exp-lyric">{lyricText}</div>}
          </div>
        </div>
        <Progress positionMs={positionMs} durationMs={media?.durationMs ?? 0} onSeek={onSeek} />
        <Controls media={media} playing={playing} onControl={onControl} />
        <div className="panel-widgets">
          <LineChart
            data={history.cpu}
            color="rgba(116,142,255,1)"
            max={100}
            label="CPU"
            value={metrics ? `${metrics.cpu.toFixed(1)}%` : "—"}
          />
          <LineChart
            data={history.mem}
            color="rgba(116,200,142,1)"
            max={100}
            label="内存"
            value={
              metrics && metrics.memTotal > 0
                ? `${((metrics.memUsed / metrics.memTotal) * 100).toFixed(1)}%`
                : "—"
            }
          />
          <LineChart
            data={history.gpu}
            color="rgba(255,165,80,1)"
            max={100}
            label="GPU"
            value={metrics?.gpu != null ? `${metrics.gpu}%` : "—"}
          />
          <LineChart
            data={networkHistory}
            color="rgba(180,140,255,1)"
            max={200}
            label="网络"
            value={network?.online ? `在线 ${network.latency}ms` : "离线"}
          />
          <Weather weather={weather} />
        </div>
      </div>
    </div>
  );
}

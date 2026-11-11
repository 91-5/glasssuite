import { useEffect, useState } from "react";
import {
  EVENTS,
  subscribe,
  type AiInsight,
  type MediaAction,
  type MediaState,
  type NetworkStatus,
} from "@glasssuite/ipc-types";
import { Controls } from "../media/Controls";
import { useSettings, useWeather } from "../hooks";
import { useMetricsHistory } from "./useMetricsHistory";
import { SettingsForm } from "./Settings";

/** 纯 SVG 迷你走势图，不引入图表库。values 为 0~100 的百分比序列。 */
function Sparkline({ values, stroke }: { values: number[]; stroke: string }) {
  const w = 132;
  const h = 26;
  if (values.length < 2) {
    return (
      <svg viewBox={`0 0 ${w} ${h}`} className="panel-spark" aria-hidden="true">
        <line x1="0" y1={h - 1} x2={w} y2={h - 1} stroke={stroke} strokeWidth="1" opacity="0.4" />
      </svg>
    );
  }
  const points = values
    .map((v, i) => {
      const x = (i / (values.length - 1)) * w;
      const y = h - (Math.min(100, Math.max(0, v)) / 100) * (h - 2) - 1;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return (
    <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className="panel-spark" aria-hidden="true">
      <polyline points={points} fill="none" stroke={stroke} strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

function MetricRow({
  label,
  value,
  values,
  stroke,
}: {
  label: string;
  value: string;
  values: number[];
  stroke: string;
}) {
  return (
    <div className="panel-metric">
      <span className="panel-metric-label">{label}</span>
      <span className="panel-metric-value">{value}</span>
      <Sparkline values={values} stroke={stroke} />
    </div>
  );
}

/**
 * 展开面板：CPU / 内存 / GPU 走势 + 网络 + 天气 + 媒体控制 + AI 一行洞察；
 * 头部 ⚙ 进入「AI 与网络」设置第二视图（保存即热加载，见 ./Settings）。
 *
 * 面板用「窗口 resize 到 PANEL_SIZE」实现，不是固定尺寸窗 + 常驻热区；
 * 收起时后端按缓存的折叠尺寸复位。
 */
export function Panel({
  onClose,
  media,
  control,
}: {
  onClose: () => void;
  media: MediaState | null;
  control: (action: MediaAction, positionMs?: number) => void;
}) {
  const { latest, history } = useMetricsHistory();
  const weather = useWeather();
  const settings = useSettings();
  const [showSettings, setShowSettings] = useState(false);
  const [network, setNetwork] = useState<NetworkStatus | null>(null);
  const [insight, setInsight] = useState<AiInsight | null>(null);

  useEffect(() => subscribe<NetworkStatus>(EVENTS.networkStatus, setNetwork), []);
  useEffect(() => subscribe<AiInsight>(EVENTS.aiInsight, setInsight), []);

  const cpu = history.map((s) => s.cpu);
  const mem = history.map((s) => s.mem);
  const gpu = history.map((s) => s.gpu ?? 0);

  return (
    <div className="panel">
      <div className="panel-head">
        <span className="panel-title">{showSettings ? "AI 与网络" : "GlassSuite"}</span>
        <div className="panel-head-actions">
          {!showSettings ? (
            <button
              type="button"
              className="panel-close"
              onClick={() => setShowSettings(true)}
              aria-label="AI 与网络设置"
            >
              ⚙
            </button>
          ) : null}
          <button
            type="button"
            className="panel-close"
            onClick={showSettings ? () => setShowSettings(false) : onClose}
            aria-label={showSettings ? "返回面板" : "收起面板"}
          >
            {showSettings ? "‹" : "✕"}
          </button>
        </div>
      </div>

      {showSettings ? (
        <SettingsForm settings={settings} />
      ) : (
        <>
          <div className="panel-metrics">
            <MetricRow label="CPU" value={`${(latest?.cpu ?? 0).toFixed(0)}%`} values={cpu} stroke="#8ad7ff" />
            <MetricRow label="内存" value={`${(latest?.memPct ?? 0).toFixed(0)}%`} values={mem} stroke="#b39cff" />
            <MetricRow
              label="GPU"
              value={latest?.gpu == null ? "—" : `${latest.gpu.toFixed(0)}%`}
              values={latest?.gpu == null ? [] : gpu}
              stroke="#7ee0b8"
            />
          </div>

          <div className="panel-grid">
            <span className="panel-cell">
              <span className="panel-cell-label">网络</span>
              <span className="panel-cell-value">
                {network ? (network.online ? `${network.latency} ms` : "离线") : "未启用"}
              </span>
            </span>
            <span className="panel-cell">
              <span className="panel-cell-label">天气</span>
              <span className="panel-cell-value">
                {weather ? `${Math.round(weather.tempC)}°` : "未启用"}
              </span>
            </span>
            <span className="panel-cell">
              <span className="panel-cell-label">电量</span>
              <span className="panel-cell-value">
                {latest?.battery ? `${Math.round(latest.battery.charge)}%` : "—"}
              </span>
            </span>
          </div>

          {media ? (
            <div className="panel-media">
              <span className="panel-media-text" title={media.title}>
                {media.title || "未知曲目"} · {media.artist || "未知艺人"}
              </span>
              <Controls
                playing={media.playing}
                canPlay={media.canPlay}
                canPause={media.canPause}
                canNext={media.canNext}
                canPrev={media.canPrev}
                onAction={control}
              />
            </div>
          ) : null}

          {insight ? (
            <div className="panel-insight" title={insight.text}>
              {insight.text}
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}

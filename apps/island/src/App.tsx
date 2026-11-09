import { useEffect, useRef, useState } from "react";
import { GlassSurface, useFitWindow } from "@glasssuite/shared-ui";

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

/**
 * island 窗口根组件。
 *
 * 岛是**贴屏幕上沿居中**的窄窗口，宽度随内容变化（时间、日期、后续的媒体信息）。
 * 前端的唯一职责是把内容尺寸报给后端（`useFitWindow`），位置由 Rust 侧统一算。
 */
export default function App() {
  const pillRef = useRef<HTMLDivElement>(null);
  const [now, setNow] = useState(() => new Date());
  useFitWindow("island", pillRef, ISLAND_PADDING);

  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), CLOCK_TICK_MS);
    return () => window.clearInterval(id);
  }, []);

  return (
    <div className="island">
      <GlassSurface className="island-pill" radius={22} padding="6px 16px" ref={pillRef}>
        <span className="island-clock">{formatTime(now)}</span>
        <span className="island-divider" aria-hidden="true" />
        <span className="island-date">{formatDate(now)}</span>
        <span className="island-dot" aria-hidden="true" />
      </GlassSurface>
    </div>
  );
}

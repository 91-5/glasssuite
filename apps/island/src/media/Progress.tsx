import type { KeyboardEvent, MouseEvent } from "react";

/**
 * 可点击 seek 的进度条。
 *
 * 宽度走 `transition: width 1s linear`：后端事件驱动、前端 1Hz 插值，两者都不产生
 * 高频更新，靠 CSS 过渡把每秒一次跳变抹平。durationMs=0（直播/未知时长）时不可 seek。
 */
export function Progress({
  positionMs,
  durationMs,
  onSeek,
}: {
  positionMs: number;
  durationMs: number;
  onSeek: (ms: number) => void;
}) {
  const seekable = durationMs > 0;
  const pct = seekable
    ? Math.min(100, Math.max(0, (positionMs / durationMs) * 100))
    : 0;

  const seekFrom = (clientX: number, el: HTMLElement) => {
    if (!seekable) return;
    const r = el.getBoundingClientRect();
    if (r.width <= 0) return;
    const ratio = Math.min(1, Math.max(0, (clientX - r.left) / r.width));
    onSeek(ratio * durationMs);
  };

  const onClick = (e: MouseEvent<HTMLDivElement>) => seekFrom(e.clientX, e.currentTarget);
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!seekable) return;
    if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
      const delta = (e.key === "ArrowRight" ? 5 : -5) * 1000;
      onSeek(Math.min(durationMs, Math.max(0, positionMs + delta)));
      e.preventDefault();
    }
  };

  return (
    <div
      className={`island-progress${seekable ? " is-seekable" : ""}`}
      onClick={onClick}
      onKeyDown={onKeyDown}
      role="slider"
      tabIndex={seekable ? 0 : -1}
      aria-label="播放进度"
      aria-valuemin={0}
      aria-valuemax={Math.round(durationMs)}
      aria-valuenow={Math.round(positionMs)}
      title={seekable ? "点击跳转" : "时长未知"}
    >
      <div className="island-progress-fill" style={{ width: `${pct}%` }} />
    </div>
  );
}

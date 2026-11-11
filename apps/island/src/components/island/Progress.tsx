import { useState } from "react";

function fmt(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

interface ProgressProps {
  positionMs: number;
  durationMs: number;
  onSeek: (ms: number) => void;
}

/**
 * 进度条：fill 用 transform: scaleX（合成层），位置由 1s 插值 tick 驱动，无过渡动画。
 * 拖动期间用本地 drag 值，pointerup / keyup 时才发 seek。
 */
export function Progress({ positionMs, durationMs, onSeek }: ProgressProps) {
  const [drag, setDrag] = useState<number | null>(null);
  const dur = durationMs > 0 ? durationMs : 1;
  const shown = drag ?? Math.min(positionMs, dur);
  const frac = Math.min(Math.max(shown / dur, 0), 1);

  const commit = () => {
    if (drag != null) {
      onSeek(drag);
      setDrag(null);
    }
  };

  return (
    <div className="progress">
      <span className="time">{fmt(shown)}</span>
      <div className="track">
        <div className="fill" style={{ transform: `scaleX(${frac})` }} />
        <input
          type="range"
          min={0}
          max={dur}
          step={250}
          value={Math.min(shown, dur)}
          onChange={(e) => setDrag(Number(e.target.value))}
          onPointerUp={commit}
          onKeyUp={commit}
          aria-label="播放进度"
        />
      </div>
      <span className="time">{fmt(durationMs)}</span>
    </div>
  );
}

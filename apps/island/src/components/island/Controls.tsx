import type { MediaState } from "../../types";

const PLAY_SVG = (
  <svg viewBox="0 0 24 24" width="15" height="15" fill="currentColor" aria-hidden>
    <path d="M8 5.5v13l11-6.5-11-6.5z" />
  </svg>
);
const PAUSE_SVG = (
  <svg viewBox="0 0 24 24" width="15" height="15" fill="currentColor" aria-hidden>
    <path d="M7 5h3.6v14H7zM13.4 5H17v14h-3.6z" />
  </svg>
);
const NEXT_SVG = (
  <svg viewBox="0 0 24 24" width="13" height="13" fill="currentColor" aria-hidden>
    <path d="M6 6l8.5 6L6 18V6zm10 0h2.5v12H16V6z" />
  </svg>
);
const PREV_SVG = (
  <svg viewBox="0 0 24 24" width="13" height="13" fill="currentColor" aria-hidden>
    <path d="M18 6l-8.5 6L18 18V6zM5.5 6H8v12H5.5V6z" />
  </svg>
);

interface ControlsProps {
  media: MediaState | null;
  playing: boolean;
  onControl: (action: string) => void;
}

/** 播放控制行：上一曲 / 播放暂停 / 下一曲。按钮可用性跟随 SMTC ControlCapabilities。 */
export function Controls({ media, playing, onControl }: ControlsProps) {
  const canPlay = media?.canPlay ?? false;
  const canPause = media?.canPause ?? false;
  const canNext = media?.canNext ?? false;
  const canPrev = media?.canPrev ?? false;
  const canToggle = playing ? canPause : canPlay;

  return (
    <div className="controls">
      <button
        className="ctl"
        disabled={!canPrev}
        onClick={(e) => {
          e.stopPropagation();
          onControl("prev");
        }}
        aria-label="上一曲"
      >
        {PREV_SVG}
      </button>
      <button
        className="ctl play"
        disabled={!canToggle && !!media}
        onClick={(e) => {
          e.stopPropagation();
          onControl("play_pause");
        }}
        aria-label={playing ? "暂停" : "播放"}
      >
        {playing ? PAUSE_SVG : PLAY_SVG}
      </button>
      <button
        className="ctl"
        disabled={!canNext}
        onClick={(e) => {
          e.stopPropagation();
          onControl("next");
        }}
        aria-label="下一曲"
      >
        {NEXT_SVG}
      </button>
    </div>
  );
}

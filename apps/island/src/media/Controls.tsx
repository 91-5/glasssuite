import type { MediaAction } from "@glasssuite/ipc-types";

/** 上一首 / 播放暂停 / 下一首。按钮可用性直接来自 SMTC 的 Controls 位，不做猜测。 */
export function Controls({
  playing,
  canPlay,
  canPause,
  canNext,
  canPrev,
  onAction,
}: {
  playing: boolean;
  canPlay: boolean;
  canPause: boolean;
  canNext: boolean;
  canPrev: boolean;
  onAction: (action: MediaAction) => void;
}) {
  const toggleEnabled = playing ? canPause : canPlay;
  return (
    <div className="island-controls">
      <button
        type="button"
        className="island-control"
        disabled={!canPrev}
        onClick={() => onAction("prev")}
        aria-label="上一首"
      >
        ⏮
      </button>
      <button
        type="button"
        className="island-control island-control--primary"
        disabled={!toggleEnabled}
        onClick={() => onAction("play_pause")}
        aria-label={playing ? "暂停" : "播放"}
      >
        {playing ? "⏸" : "▶"}
      </button>
      <button
        type="button"
        className="island-control"
        disabled={!canNext}
        onClick={() => onAction("next")}
        aria-label="下一首"
      >
        ⏭
      </button>
    </div>
  );
}

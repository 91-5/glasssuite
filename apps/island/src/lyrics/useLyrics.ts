/**
 * 歌词：整曲一次下发，前端按插值出的 positionMs 二分选行。
 *
 * 不用后端 2~5Hz 定时推「当前行」——前端本来就在 1Hz 插值进度，多一条推送通道
 * 只会白烧 CPU。歌词源未配置时后端永不 emit，这里保持 null。
 */
import { useEffect, useState } from "react";
import {
  EVENTS,
  getLyricsState,
  subscribe,
  type LyricLine,
  type LyricsState,
} from "@glasssuite/ipc-types";

function lineAt(lines: LyricLine[], pos: number): string | null {
  if (lines.length === 0) return null;
  let lo = 0;
  let hi = lines.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (lines[mid].t <= pos) {
      found = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return found >= 0 ? lines[found].text : null;
}

export function useLyrics(positionMs: number): string | null {
  const [state, setState] = useState<LyricsState | null>(null);

  useEffect(() => {
    let alive = true;
    getLyricsState()
      .then((s) => {
        if (alive && s) setState(s);
      })
      .catch(() => {
        /* 非 Tauri 环境 */
      });
    const stop = subscribe<LyricsState | null>(EVENTS.lyrics, (s) => setState(s));
    return () => {
      alive = false;
      stop();
    };
  }, []);

  return lineAt(state?.lines ?? [], positionMs);
}

/** 导出供单测/调试用的纯函数（不依赖 React）。 */
export { lineAt };

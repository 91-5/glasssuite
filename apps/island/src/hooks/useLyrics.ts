import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

/**
 * 歌词（LRCLIB）—— Rust 侧一次拉全曲并解析 LRC，前端只做「按播放位置选行」。
 *
 * 为什么不在 Rust 侧定时推送当前行：那需要 2-5Hz 定时器持续烧 CPU，
 * 而前端本来就在插值 positionMs（SMTC position 更新粒度粗，靠 baseUnixMs + rate 推算），
 * 本地选行零额外 IPC、零额外定时器。
 */
export interface LyricLine {
  /** 起始时间（毫秒，相对曲首） */
  t: number;
  text: string;
}

export interface LyricsState {
  title: string;
  artist: string;
  lines: LyricLine[];
}

export function useLyrics(): LyricsState | null {
  const [lyrics, setLyrics] = useState<LyricsState | null>(null);

  useEffect(() => {
    const un = listen<LyricsState | null>("lyrics", (e) => {
      const p = e.payload;
      if (!p || !Array.isArray(p.lines) || p.lines.length === 0) {
        setLyrics(null);
      } else {
        setLyrics(p);
      }
    });
    // 启动补一次快照：Rust 侧 emit 可能早于本组件订阅（首帧加载慢时必现），
    // 之后缓存命中不再重复 emit，不补这一步会整首歌都没歌词。
    invoke<LyricsState | null>("lyrics_state")
      .then((s) => {
        if (s && Array.isArray(s.lines) && s.lines.length > 0) setLyrics(s);
      })
      .catch(() => {});
    return () => {
      un.then((dispose) => dispose());
    };
  }, []);

  return lyrics;
}

/** 二分查找：最后一个 t <= positionMs 的行号；早于首行返回 -1。 */
export function pickLyricLine(lines: LyricLine[], positionMs: number): number {
  let lo = 0;
  let hi = lines.length - 1;
  let ans = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (lines[mid].t <= positionMs) {
      ans = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return ans;
}

/**
 * 当前应展示的歌词文本。
 * LRC 里间奏常用「只有时间戳、没有文字」的行表示（实测《稻香》42 行里有 2 条），
 * 直接显示会出现空白闪断 —— 这里向前回退到最近一条非空行，符合常见播放器行为。
 */
export function currentLyricText(lines: LyricLine[], positionMs: number): string {
  let idx = pickLyricLine(lines, positionMs);
  while (idx >= 0 && lines[idx].text.trim() === "") {
    idx -= 1;
  }
  return idx >= 0 ? lines[idx].text : "";
}

/**
 * 媒体状态 + 1Hz 本地插值。
 *
 * 为什么插值而不是让后端推进度：SMTC 的 TimelineProperties 只在状态变化时更新，
 * 播放中可能几秒都不推一次。前端按「收到快照时的本地时刻」做锚点推进，
 * 每秒一次重渲染即可；暂停时直接锁在锚点位置，**进度停住不前进**。
 *
 * 不引入 30Hz 定时器：那是旧版眼睛组件用完就删掉的东西，药丸进度用不上。
 * 进度条本身带 1s CSS 过渡，视觉上是连续的。
 */
import { useCallback, useEffect, useState } from "react";
import {
  EVENTS,
  getMediaState,
  mediaControl,
  subscribe,
  type MediaAction,
  type MediaState,
} from "@glasssuite/ipc-types";

const TICK_MS = 1_000;

interface Anchor {
  state: MediaState;
  /** 锚点处的进度（毫秒）。 */
  pos: number;
  /** 锚点对应的本机时刻（Date.now）。 */
  at: number;
}

/** IntelliSense 友好的返回值。 */
export interface MediaView {
  /** 当前媒体；`null` = 无媒体，界面回落时钟。 */
  media: MediaState | null;
  /** 插值后的当前进度（毫秒）。 */
  positionMs: number;
  /** 下发控制指令。 */
  control: (action: MediaAction, positionMs?: number) => void;
}

export function useMedia(): MediaView {
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  // 只用来触发重算；值本身没有消费者。
  const [, forceTick] = useState(0);

  const install = useCallback((snapshot: MediaState | null) => {
    if (!snapshot) {
      setAnchor(null);
      return;
    }
    const now = Date.now();
    // 播放器报的 baseUnixMs 是 FILETIME 换算来的，与本机时钟可能有偏差。
    // 只有「本机比它晚 0~5s」才当作事件投递延迟补进去；其它情况（时钟不同步、
    // 陈旧值）一律忽略，否则进度会直接冲到曲尾（旧仓踩过的坑，见卡面 §7）。
    const skew = now - (snapshot.baseUnixMs || now);
    const latency = skew > 0 && skew < 5_000 ? skew : 0;
    const rate = snapshot.rate > 0 ? snapshot.rate : 1;
    setAnchor({
      state: snapshot,
      pos: snapshot.positionMs + latency * rate,
      at: now,
    });
  }, []);

  useEffect(() => {
    let alive = true;
    // 首帧补快照：emit 可能早于组件订阅（与歌词快照同款竞态）。
    getMediaState()
      .then((s) => {
        if (alive && s) install(s);
      })
      .catch(() => {
        /* 非 Tauri 环境：保持无媒体 */
      });
    const stop = subscribe<MediaState | null>(EVENTS.mediaState, install);
    return () => {
      alive = false;
      stop();
    };
  }, [install]);

  // 仅在播放时起 1Hz 定时器；暂停时没有定时器，进度天然停住。
  useEffect(() => {
    if (!anchor?.state.playing) return;
    const id = window.setInterval(() => forceTick((n) => n + 1), TICK_MS);
    return () => window.clearInterval(id);
  }, [anchor]);

  const control = useCallback((action: MediaAction, positionMs?: number) => {
    mediaControl(action, positionMs).catch(() => {
      /* 后端还没就绪（无媒体时）：静默 */
    });
  }, []);

  let positionMs = 0;
  if (anchor) {
    const { state, pos, at } = anchor;
    if (state.playing) {
      const rate = state.rate > 0 ? state.rate : 1;
      const elapsed = (Date.now() - at) * rate;
      positionMs = state.durationMs > 0 ? Math.min(pos + elapsed, state.durationMs) : pos + elapsed;
    } else {
      positionMs = pos;
    }
  }

  return { media: anchor?.state ?? null, positionMs, control };
}

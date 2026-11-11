import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import type { MediaState } from "../types";

/** 后端原始快照：GlassSuite 用 `artwork` 命名封面。 */
interface RawMedia extends Omit<MediaState, "art"> {
  artwork?: string | null;
}

/** 后端字段 → UI 字段的唯一映射点。 */
function toUi(raw: RawMedia): MediaState {
  const { artwork, ...rest } = raw;
  return { ...rest, art: artwork ?? null };
}

/**
 * SMTC 状态 hook。
 * 事件驱动：Rust 侧订阅 SMTC 事件推送 `media-state`，本 hook 只做接收 + 位置插值。
 * 插值计时器仅在「正在播放且页面可见」时存在（不可见/暂停即停，无常驻 rAF）。
 */
export function useMedia() {
  const [media, setMedia] = useState<MediaState | null>(null);
  const [positionMs, setPositionMs] = useState(0);
  const mediaRef = useRef(media);
  mediaRef.current = media;
  const [visible, setVisible] = useState(!document.hidden);

  useEffect(() => {
    const unlistenState = listen<RawMedia>("media-state", (e) => {
      const mapped = toUi(e.payload);
      setMedia(mapped);
      if (!mapped.playing) setPositionMs(mapped.positionMs);
    });
    // 启动时拉一次缓存快照，恢复 UI
    invoke<RawMedia | null>("media_state")
      .then((s) => {
        if (s) {
          const mapped = toUi(s);
          setMedia(mapped);
          if (!mapped.playing) setPositionMs(mapped.positionMs);
        }
      })
      .catch(() => {});
    const onVis = () => setVisible(!document.hidden);
    document.addEventListener("visibilitychange", onVis);
    return () => {
      unlistenState.then((f) => f());
      document.removeEventListener("visibilitychange", onVis);
    };
  }, []);

  useEffect(() => {
    const m = mediaRef.current;
    if (!m || !m.playing || !visible) {
      if (m && !m.playing) setPositionMs(m.positionMs);
      return;
    }
    const tick = () => {
      const cur = mediaRef.current;
      if (!cur) return;
      const p = cur.positionMs + (Date.now() - cur.baseUnixMs) * cur.rate;
      setPositionMs(Math.min(Math.max(p, 0), cur.durationMs > 0 ? cur.durationMs : p));
    };
    tick();
    const id = window.setInterval(tick, 1000);
    return () => window.clearInterval(id);
  }, [media, visible]);

  return { media, positionMs };
}

/** 封面主色提取（仅在封面变化时执行一次，画进 12x12 canvas 取均值）。 */
export function dominantTint(dataUrl: string): Promise<string> {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => {
      try {
        const c = document.createElement("canvas");
        c.width = 12;
        c.height = 12;
        const ctx = c.getContext("2d");
        if (!ctx) return resolve("116,142,255");
        ctx.drawImage(img, 0, 0, 12, 12);
        const d = ctx.getImageData(0, 0, 12, 12).data;
        let r = 0,
          g = 0,
          b = 0,
          n = 0;
        for (let i = 0; i < d.length; i += 4) {
          r += d[i];
          g += d[i + 1];
          b += d[i + 2];
          n++;
        }
        resolve(`${Math.round(r / n)},${Math.round(g / n)},${Math.round(b / n)}`);
      } catch {
        resolve("116,142,255");
      }
    };
    img.onerror = () => resolve("116,142,255");
    img.src = dataUrl;
  });
}

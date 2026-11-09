/**
 * dock 前端状态：内容指纹比对、按需取图标、贴底唤出。
 *
 * v0.8.1 防跳修复 #1 的前端一半：后端 watcher 已经做过一次指纹比对，这里再做一次。
 * 两道闸门是故意的——窗口重建 / 补发快照 / StrictMode 双挂载都可能让同一份快照
 * 重复到达，只要有一次 `setState` 就会让整排图标重新测量并触发 FLIP 动画。
 */
import { useEffect, useState } from "react";
import {
  EMPTY_DOCK,
  EVENTS,
  dockSetRevealed,
  getDockIcon,
  getDockState,
  subscribe,
  type DockSnapshot,
} from "@glasssuite/ipc-types";

export function useDockSnapshot(): DockSnapshot {
  const [snap, setSnap] = useState<DockSnapshot>(EMPTY_DOCK);

  useEffect(() => {
    let alive = true;
    let lastFp = "";
    let lastAutoHide = false;

    const install = (next: DockSnapshot) => {
      if (!alive) return;
      // 指纹相同且自动隐藏设置没变 → 一个字节都不改，React 不会重渲染，FLIP 不会跑。
      if (next.fingerprint === lastFp && next.autoHide === lastAutoHide) return;
      lastFp = next.fingerprint;
      lastAutoHide = next.autoHide;
      setSnap(next);
    };

    getDockState().then(install).catch(() => {
      /* 非 Tauri 环境（浏览器里调布局） */
    });
    const off = subscribe<DockSnapshot>(EVENTS.runningChanged, install);
    return () => {
      alive = false;
      off();
    };
  }, []);

  return snap;
}

/** 进程级图标缓存：同一 key 只向后端要一次，超时/失败也缓存 null，避免反复打 Shell API。 */
const ICON_CACHE = new Map<string, string | null>();

export function useDockIcon(key: string, initial: string | null): string | null {
  const [url, setUrl] = useState<string | null>(() => ICON_CACHE.get(key) ?? initial);

  useEffect(() => {
    if (initial || ICON_CACHE.has(key)) return;
    let alive = true;
    getDockIcon(key)
      .then((v) => {
        ICON_CACHE.set(key, v);
        if (alive && v) setUrl(v);
      })
      .catch(() => {
        ICON_CACHE.set(key, null);
      });
    return () => {
      alive = false;
    };
  }, [key, initial]);

  return url;
}

/**
 * 贴底唤出 / 自动隐藏。收起时除了加 CSS 类，还必须调用 `dock_set_revealed(false)`
 * 打开鼠标穿透——否则隐形药丸仍会吃掉桌面点击（旧仓踩过）。
 */
export function useDockReveal(autoHide: boolean): boolean {
  const [revealed, setRevealed] = useState(true);

  useEffect(() => {
    const apply = (v: boolean) => {
      const next = autoHide ? v : true;
      setRevealed(next);
      dockSetRevealed(next).catch(() => {
        /* 非 Tauri 环境 */
      });
    };
    apply(true);
    return subscribe<boolean>(EVENTS.dockReveal, apply);
  }, [autoHide]);

  return revealed;
}

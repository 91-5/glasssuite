/** 通知摘要条：只保留最近一条，显示 6s 后自动回到常态（媒体/时钟）。 */
import { useEffect, useState } from "react";
import { EVENTS, subscribe, type NotificationItem } from "@glasssuite/ipc-types";

const VISIBLE_MS = 6_000;

export function useNotifications(): NotificationItem | null {
  const [item, setItem] = useState<NotificationItem | null>(null);

  useEffect(() => subscribe<NotificationItem>(EVENTS.notification, setItem), []);

  useEffect(() => {
    if (!item) return;
    const id = window.setTimeout(() => setItem(null), VISIBLE_MS);
    return () => window.clearTimeout(id);
  }, [item]);

  return item;
}

/**
 * 首启一次性隐私提示：用户显式打开通知读取后，第一次启动告知「纯本地，不上报」。
 * 只在 localStorage 里记一个不含任何内容的标记。
 */
const NOTICE_KEY = "glasssuite.privacy-notice.v1";

export function usePrivacyNotice(enabled: boolean): boolean {
  const [show, setShow] = useState(false);

  useEffect(() => {
    if (!enabled) return;
    try {
      if (localStorage.getItem(NOTICE_KEY)) return;
    } catch {
      return; // 隐私模式下 localStorage 不可用，不阻塞启动
    }
    setShow(true);
    const id = window.setTimeout(() => {
      setShow(false);
      try {
        localStorage.setItem(NOTICE_KEY, "1");
      } catch {
        /* 忽略：下次启动再提示一次即可 */
      }
    }, 5_000);
    return () => window.clearTimeout(id);
  }, [enabled]);

  return show;
}

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

/** 通知流的原始缓冲上限（内存显示用，不落盘；渲染时再做同日同源去重）。 */
const STREAM_CAP = 100;

/**
 * 展开面板的**通知流**（OC-20261113-001 P2 的 L1 呈现通道）。
 *
 * 只在内存里保留最近 STREAM_CAP 条摘要（应用名 + 截断标题 + 时间——后端
 * 本来就不发正文）；「同日同类只展示最新一条」的归并在渲染侧做，事件层
 * 保持无损，改展示策略不用回炉后端。
 */
export function useNotificationStream(): NotificationItem[] {
  const [items, setItems] = useState<NotificationItem[]>([]);

  useEffect(
    () =>
      subscribe<NotificationItem>(EVENTS.notification, (item) => {
        setItems((prev) => [item, ...prev].slice(0, STREAM_CAP));
      }),
    [],
  );

  return items;
}

/** 通知摘要去重：同日（本地日历日）同源只保留最新一条。 */
export function latestPerDayApp(items: NotificationItem[]): NotificationItem[] {
  const seen = new Set<string>();
  const out: NotificationItem[] = [];
  for (const item of items) {
    const key = `${new Date(item.arrivedAt).toDateString()}|${item.app}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(item);
  }
  return out;
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

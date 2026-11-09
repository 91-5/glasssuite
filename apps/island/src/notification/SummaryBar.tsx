import type { NotificationItem } from "@glasssuite/ipc-types";

/**
 * 岛上的通知摘要条：应用名 + 截断标题。
 *
 * 原文（正文）不在这里、也不在任何前端状态里——按卡面隐私约束，原文归 Windows
 * 自己的横幅。后端发来的 payload 本身就没有 body 字段。
 */
export function SummaryBar({ item }: { item: NotificationItem }) {
  return (
    <span className="island-notification">
      <span className="island-notification-dot" aria-hidden="true" />
      <span className="island-notification-app">{item.app}</span>
      <span className="island-notification-title" title={item.title}>
        {item.title}
      </span>
    </span>
  );
}

/** 首启一次性隐私提示（只在用户显式启用通知读取后出现一次）。 */
export function PrivacyNotice() {
  return (
    <span className="island-notification island-notification--privacy">
      <span aria-hidden="true">🔒</span>
      <span>通知摘要为纯本地读取，不上报</span>
    </span>
  );
}

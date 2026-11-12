/**
 * 展开面板的通知流（L1 呈现通道，OC-20261113-001 P2）。
 *
 * 刻意克制：只是面板里的一节静态列表——不弹 toast、不响、不抢焦点（卡面
 * 「不做 toast 强推」）；每行只有应用名 + 摘要标题 + 到达时间（后端契约里
 * 根本没有正文字段）。同日同源只展示最新一条（去重见 useNotifications）。
 */
import type { NotificationItem } from "@glasssuite/ipc-types";
import { latestPerDayApp } from "./useNotifications";

/** 面板一屏最多展示的行数：多了没意义，完整历史归系统通知中心。 */
const MAX_ROWS = 6;

function timeOf(ts: number): string {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

export function NotificationStream({ items }: { items: NotificationItem[] }) {
  const latest = latestPerDayApp(items).slice(0, MAX_ROWS);
  if (latest.length === 0) {
    return (
      <div className="notif-stream">
        <div className="notif-stream-head">通知</div>
        <div className="notif-empty">暂无通知（开启同步后由应用主动上报）</div>
      </div>
    );
  }
  return (
    <div className="notif-stream">
      <div className="notif-stream-head">通知</div>
      {latest.map((n) => (
        <div className="notif-row" key={`${n.arrivedAt}|${n.app}|${n.title}`} title={n.title}>
          <span className="notif-app">{n.app}</span>
          <span className="notif-title">{n.title}</span>
          <span className="notif-time">{timeOf(n.arrivedAt)}</span>
        </div>
      ))}
    </div>
  );
}

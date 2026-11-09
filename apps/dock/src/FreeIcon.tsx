/**
 * 游离图标窗口（`dock/index.html#free/<id>`）。
 *
 * 每个游离图标是一个**恰好等于图标大小**的独立窗口，所以这里没有透明宽区、
 * 不需要点击穿透热区，也不会挡住桌面其它位置。拖动时把屏幕坐标交给后端
 * `dock_move_free` 持久化，双击回到 dock。
 */
import { useEffect, useRef, useState } from "react";
import {
  EVENTS,
  dockLaunch,
  dockMoveFree,
  dockUndockFree,
  getDockState,
  subscribe,
  type DockEntry,
  type DockSnapshot,
} from "@glasssuite/ipc-types";
import { DockIcon } from "./icons";

export default function FreeIcon({ id }: { id: string }) {
  const [entry, setEntry] = useState<DockEntry | null>(null);
  const dragging = useRef<{ dx: number; dy: number } | null>(null);

  useEffect(() => {
    const install = (s: DockSnapshot) => {
      setEntry(s.free.find((e) => e.id === id) ?? null);
    };
    getDockState().then(install).catch(() => {});
    return subscribe<DockSnapshot>(EVENTS.runningChanged, install);
  }, [id]);

  if (!entry) return <div className="free-window" />;

  return (
    <div className="free-window">
      <DockIcon
        entry={entry}
        onClick={() => {
          if (!dragging.current) dockLaunch(entry.key).catch(() => {});
        }}
        onDoubleClick={() => dockUndockFree(entry.id).catch(() => {})}
        onPointerDown={(e) => {
          const rect = e.currentTarget.getBoundingClientRect();
          dragging.current = { dx: e.clientX - rect.left, dy: e.clientY - rect.top };
          e.currentTarget.setPointerCapture(e.pointerId);
        }}
        onPointerMove={(e) => {
          if (!dragging.current) return;
          const x = Math.round(e.screenX - dragging.current.dx);
          const y = Math.round(e.screenY - dragging.current.dy);
          dockMoveFree(entry.id, x, y).catch(() => {});
        }}
        onPointerUp={(e) => {
          dragging.current = null;
          e.currentTarget.releasePointerCapture(e.pointerId);
        }}
      />
    </div>
  );
}

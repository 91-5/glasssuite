/**
 * dock 前端入口。同一个构建产物按 URL hash 承担四种窗口角色：
 *   (无 hash)       底部 dock 药丸
 *   #startmenu      开始菜单面板
 *   #free/<id>      一枚游离图标
 *   #preview        DWM 缩略图承载层
 *
 * 布局红线：药丸 **左锚定**（v0.8.1 修复 #5，见 dock.css 的 `justify-content: flex-start`）。
 * 窗口宽度随图标增减变化，左锚定让既有图标在视口内的自然位与窗口宽度解耦；
 * 居中锚定会让整排图标先被推开、再被重新居中拉回，看起来就是「图标在跳」。
 */
import { useCallback, useRef, type DragEvent, type MouseEvent } from "react";
import { GlassSurface, useFitWindow } from "@glasssuite/shared-ui";
import {
  dockLaunch,
  dockMenuToggle,
  dockPreviewHide,
  dockPreviewShow,
  dockReorder,
  dockSetFree,
  type DockEntry,
} from "@glasssuite/ipc-types";
import { DockIcon, StartButton } from "./icons";
import { useDockReveal, useDockSnapshot } from "./useDock";
import { useFlip } from "./useFlip";
import StartMenu from "./StartMenu";
import FreeIcon from "./FreeIcon";
import PreviewSurface from "./PreviewSurface";

/** 药丸到窗口边缘的留白（逻辑像素），必须与 `dock.css` 里 `.dock` 的 padding 一致。 */
const DOCK_PADDING = 6;
const PREVIEW_W = 220;
const PREVIEW_H = 140;

function Dock() {
  const pillRef = useRef<HTMLDivElement>(null);
  const dragFrom = useRef<string | null>(null);
  const snapshot = useDockSnapshot();
  const revealed = useDockReveal(snapshot.autoHide);

  const { pinned, transients } = snapshot;
  const signature = [...pinned, ...transients].map((e) => e.key).join("|");
  const label = `pinned=${pinned.length} transients=${transients.length}`;
  const flipRef = useFlip(signature, label);

  useFitWindow("dock", pillRef, DOCK_PADDING);

  // 两个 hook 都要测同一个药丸节点：用回调 ref 合并，避免再包一层影响尺寸测量。
  const setRefs = useCallback(
    (node: HTMLDivElement | null) => {
      pillRef.current = node;
      flipRef.current = node;
    },
    [flipRef],
  );

  const reorder = (targetKey: string) => {
    const from = dragFrom.current;
    dragFrom.current = null;
    if (!from || from === targetKey) return;
    const paths = pinned.map((e) => e.path);
    const fromPath = pinned.find((e) => e.key === from)?.path;
    const targetPath = pinned.find((e) => e.key === targetKey)?.path;
    if (!fromPath || !targetPath) return;
    const next = paths.filter((p) => p !== fromPath);
    next.splice(next.indexOf(targetPath), 0, fromPath);
    dockReorder(next).catch(() => {});
  };

  const hoverPreview = (entry: DockEntry, e: MouseEvent<HTMLButtonElement>) => {
    if (!entry.running) return;
    // 返回 false 时后端会发 dock-preview-fallback，承载窗自己显示纯色块；
    // 命令本身抛错（非 Tauri 环境）就静默忽略，绝不阻塞悬停。
    dockPreviewShow(entry.key, e.screenX, e.screenY, PREVIEW_W, PREVIEW_H).catch(() => {});
  };

  return (
    <div className={revealed ? "dock" : "dock dock--hidden"}>
      <GlassSurface
        className="dock-pill"
        radius={18}
        padding="6px 8px"
        ref={setRefs}
      >
        <StartButton onClick={() => dockMenuToggle().catch(() => {})} />

        {pinned.map((entry) => (
          <DockIcon
            key={entry.key}
            entry={entry}
            draggable
            onDragStart={(e: DragEvent<HTMLButtonElement>) => {
              dragFrom.current = entry.key;
              e.dataTransfer.effectAllowed = "move";
            }}
            onDragOver={(e) => e.preventDefault()}
            onDrop={(e) => {
              e.preventDefault();
              reorder(entry.key);
            }}
            onDragEnd={() => {
              dragFrom.current = null;
            }}
            onClick={() => dockLaunch(entry.key).catch(() => {})}
            onMouseEnter={(e) => hoverPreview(entry, e)}
            onMouseLeave={() => dockPreviewHide().catch(() => {})}
            // 右键把固定项拖成游离图标；不引入铺满屏幕的透明大窗 + 常驻热区。
            onContextMenu={(e) => {
              e.preventDefault();
              dockSetFree(entry.id, e.screenX, e.screenY).catch(() => {});
            }}
          />
        ))}

        {transients.length > 0 ? <span className="dock-sep" aria-hidden="true" /> : null}
        {transients.map((entry) => (
          <DockIcon
            key={entry.key}
            entry={entry}
            onClick={() => dockLaunch(entry.key).catch(() => {})}
            onMouseEnter={(e) => hoverPreview(entry, e)}
            onMouseLeave={() => dockPreviewHide().catch(() => {})}
          />
        ))}

        {pinned.length === 0 && transients.length === 0 ? (
          <span className="dock-empty">dock 载入中…</span>
        ) : null}
      </GlassSurface>
    </div>
  );
}

export default function App() {
  const hash = window.location.hash;
  if (hash.startsWith("#startmenu")) return <StartMenu />;
  if (hash.startsWith("#free/")) return <FreeIcon id={hash.slice("#free/".length)} />;
  if (hash.startsWith("#preview")) return <PreviewSurface />;
  return <Dock />;
}

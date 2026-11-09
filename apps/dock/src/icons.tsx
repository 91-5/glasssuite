import type { ButtonHTMLAttributes, ReactElement } from "react";
import type { DockEntry } from "@glasssuite/ipc-types";
import { useDockIcon } from "./useDock";

/**
 * 图标资源分两层：
 * 1. 优先用系统 API 在用户本机运行时提取的位图（`dock_icon` 命令，带 800ms 超时）；
 * 2. 取不到时用下面这些**原创 SVG 线条**占位。
 *
 * 开源包里不含任何第三方商标/图标文件；`title` 只显示应用名，不显示路径。
 */

export type GlyphName =
  | "files"
  | "browser"
  | "terminal"
  | "music"
  | "settings"
  | "editor"
  | "chat";

const GLYPHS: Record<GlyphName, ReactElement> = {
  files: (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4l1.6 2H19a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 19H4.5A1.5 1.5 0 0 1 3 17.5z" />
    </svg>
  ),
  browser: (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="8.5" />
      <path d="M3.5 12h17M12 3.5c2.2 2.4 3.3 5.4 3.3 8.5s-1.1 6.1-3.3 8.5c-2.2-2.4-3.3-5.4-3.3-8.5S9.8 5.9 12 3.5z" />
    </svg>
  ),
  terminal: (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
      <path d="M7.5 10l2.5 2.5-2.5 2.5M13 15.5h4" />
    </svg>
  ),
  music: (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 18V7l10-2v11" />
      <circle cx="7" cy="18" r="2.2" />
      <circle cx="17" cy="16" r="2.2" />
    </svg>
  ),
  settings: (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="3.2" />
      <path d="M12 3.5v2M12 18.5v2M3.5 12h2M18.5 12h2M6 6l1.4 1.4M16.6 16.6L18 18M18 6l-1.4 1.4M7.4 16.6L6 18" />
    </svg>
  ),
  editor: (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M5 4.5h9.5L19 9v10.5H5z" />
      <path d="M14 4.5V9h5M8.5 13.5h7M8.5 16.5h4.5" />
    </svg>
  ),
  chat: (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M4.5 5.5h15v10h-8l-4 3.5v-3.5h-3z" />
      <path d="M8.5 9.5h7M8.5 12.2h4.5" />
    </svg>
  ),
};

const GLYPH_ORDER = Object.keys(GLYPHS) as GlyphName[];

/** 按 key 稳定选一枚占位图标，避免每次渲染换花色。 */
export function glyphFor(key: string): GlyphName {
  let h = 0;
  for (let i = 0; i < key.length; i += 1) h = (h * 31 + key.charCodeAt(i)) >>> 0;
  return GLYPH_ORDER[h % GLYPH_ORDER.length];
}

export interface DockIconProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  entry: DockEntry;
}

/**
 * 一枚 dock 图标。`data-flip-key` 供 `useFlip` 定位；`data-flip-key` 放在展开属性之后，
 * 保证不会被调用方误覆盖。
 */
export function DockIcon({ entry, className, ...rest }: DockIconProps) {
  const iconUrl = useDockIcon(entry.key, entry.icon);
  return (
    <button
      type="button"
      className={["gs-icon-button", "dock-icon", className].filter(Boolean).join(" ")}
      title={entry.name}
      {...rest}
      data-flip-key={entry.key}
    >
      {iconUrl ? (
        <img className="dock-icon-img" src={iconUrl} alt="" draggable={false} />
      ) : (
        GLYPHS[glyphFor(entry.key)]
      )}
      {entry.running ? <span className="dock-running" aria-hidden="true" /> : null}
    </button>
  );
}

/** 开始菜单按钮（不是应用图标，不参与 FLIP）。 */
export function StartButton(props: ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      type="button"
      className="gs-icon-button dock-icon dock-start"
      title="开始菜单"
      aria-label="开始菜单"
      {...props}
    >
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z" />
      </svg>
    </button>
  );
}

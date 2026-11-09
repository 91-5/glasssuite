import type { ReactElement } from "react";

/**
 * dock 的图标集合。
 *
 * 这些是**占位图标**（原创 SVG 线条），不是从任何软件包里提取的资源——开源项目不打包
 * 他人商标，真实图标后续由系统 API（`SHGetFileInfo` / `IShellItemImageFactory`）
 * 在运行时换取 data URL。
 *
 * `running` 是「该程序正在运行」的小圆点：dock 里固定图标与临时运行图标共用一套按钮，
 * 用圆点而非改变图标底色来表示运行态，避免图标在运行/退出时整体变色（视觉跳动）。
 */
export interface DockItem {
  key: string;
  label: string;
  glyph: GlyphName;
}

export type GlyphName = "files" | "browser" | "terminal" | "music" | "settings" | "editor" | "chat";

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

export const PINNED: DockItem[] = [
  { key: "files", label: "文件", glyph: "files" },
  { key: "browser", label: "浏览器", glyph: "browser" },
  { key: "terminal", label: "终端", glyph: "terminal" },
  { key: "editor", label: "编辑器", glyph: "editor" },
  { key: "settings", label: "设置", glyph: "settings" },
];

/** 未固定但正在运行的程序（真实实现里由进程监视推送）。 */
export const TRANSIENTS: DockItem[] = [
  { key: "music", label: "音乐", glyph: "music" },
  { key: "chat", label: "聊天", glyph: "chat" },
];

export function DockIcon({ item, running = false }: { item: DockItem; running?: boolean }) {
  return (
    <button className="gs-icon-button dock-icon" type="button" title={item.label}>
      {GLYPHS[item.glyph]}
      {running ? <span className="dock-running" aria-hidden="true" /> : null}
    </button>
  );
}

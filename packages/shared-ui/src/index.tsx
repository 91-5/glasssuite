/**
 * 玻璃表面：island 与 dock 的唯一视觉基元。
 *
 * 两个窗口都要求「背景透明 + 圆角玻璃 + 不接收窗口边框」，因此玻璃效果只能靠
 * 图层绘制（半透明填充 + 内高光 + 外阴影）或 Win32 侧 `DwmEnableBlurBehindWindow`。
 * 这里只做「画出来的玻璃」这一层；需要真模糊时由 Rust 侧叠加，不改本组件。
 */
import type { CSSProperties, ReactNode } from "react";

export interface GlassSurfaceProps {
  children: ReactNode;
  /** 圆角（px），默认 16。 */
  radius?: number;
  /** 内边距，默认 `5px 12px`。 */
  padding?: string;
  className?: string;
  style?: CSSProperties;
}

export function GlassSurface({
  children,
  radius = 16,
  padding = "5px 12px",
  className,
  style,
}: GlassSurfaceProps) {
  return (
    <div
      className={["gs-glass", className].filter(Boolean).join(" ")}
      style={{ borderRadius: radius, padding, ...style }}
    >
      {children}
    </div>
  );
}

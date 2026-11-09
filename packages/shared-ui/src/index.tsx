/**
 * 玻璃表面：island 与 dock 的唯一视觉基元。
 *
 * 两个窗口都要求「背景透明 + 圆角玻璃 + 不接收窗口边框」，因此玻璃效果只能靠
 * 图层绘制（半透明填充 + 内高光 + 外阴影）或 Win32 侧 `DwmEnableBlurBehindWindow`。
 * 这里只做「画出来的玻璃」这一层；需要真模糊时由 Rust 侧叠加，不改本组件。
 */
import type { CSSProperties, MouseEventHandler, ReactNode, Ref } from "react";

export { useFitWindow } from "./useFitWindow";
export type { FitTarget } from "./useFitWindow";

export interface GlassSurfaceProps {
  children: ReactNode;
  /** 圆角（px），默认 16。 */
  radius?: number;
  /** 内边距，默认 `5px 12px`。 */
  padding?: string;
  className?: string;
  style?: CSSProperties;
  /** 点击表面空白处（例如展开面板）。内部按钮自行 stopPropagation 或由调用方过滤。 */
  onClick?: MouseEventHandler<HTMLDivElement>;
  /**
   * 指向玻璃元素本身。`useFitWindow` 必须测它——测内部内容会少算内边距，
   * 窗口就会比药丸窄，右侧被裁掉。
   */
  ref?: Ref<HTMLDivElement>;
}

export function GlassSurface({
  children,
  radius = 16,
  padding = "5px 12px",
  className,
  style,
  onClick,
  ref,
}: GlassSurfaceProps) {
  return (
    <div
      ref={ref}
      className={["gs-glass", className].filter(Boolean).join(" ")}
      style={{ borderRadius: radius, padding, ...style }}
      onClick={onClick}
    >
      {children}
    </div>
  );
}

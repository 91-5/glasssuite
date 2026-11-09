import { useRef } from "react";
import { GlassSurface, useFitWindow } from "@glasssuite/shared-ui";
import { PINNED, TRANSIENTS, DockIcon } from "./icons";

/** 药丸到窗口边缘的留白（逻辑像素），必须与 `dock.css` 里 `.dock` 的 padding 一致。 */
const DOCK_PADDING = 6;

/**
 * dock 窗口根组件。
 *
 * 布局是硬约束：药丸 **左锚定**（`justify-content: flex-start`）。窗口宽度会随图标增减
 * 变化，左锚定保证既有图标在视口内的位置与窗口宽度解耦；若居中锚定，窗口每变宽一次，
 * 所有图标先被重排推开、再被重新居中拉回，看起来就是「图标在跳」。
 *
 * 尺寸上报交给 `useFitWindow`，测量的对象是药丸本身（不是内部图标行）——药丸含自己的
 * 内边距，用内部行去测会少算两边内边距，窗口比药丸窄，右侧图标被裁。
 */
export default function App() {
  const pillRef = useRef<HTMLDivElement>(null);
  useFitWindow("dock", pillRef, DOCK_PADDING);

  return (
    <div className="dock">
      <GlassSurface className="dock-pill" radius={18} padding="6px 8px" ref={pillRef}>
        {PINNED.map((item) => (
          <DockIcon key={item.key} item={item} />
        ))}
        <span className="dock-sep" aria-hidden="true" />
        {TRANSIENTS.map((item) => (
          <DockIcon key={item.key} item={item} running />
        ))}
      </GlassSurface>
    </div>
  );
}

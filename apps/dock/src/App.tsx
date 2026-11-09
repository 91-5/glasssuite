import { GlassSurface } from "@glasssuite/shared-ui";

/**
 * dock 窗口根组件（P0 骨架）。
 *
 * 后续阶段在此接入：固定图标行、transient 运行图标、拖拽排序、拖出成游离图标、
 * 悬停预览。药丸左锚定（`justify-content: flex-start`）是硬要求——窗口宽度会随图标
 * 增减变化，左锚定保证既有图标在视口内的位置与窗口宽度解耦，否则会出现
 * 「图标随窗口右扩再被拉回」的三段位移。
 */
export default function App() {
  return (
    <div className="dock">
      <GlassSurface className="dock-pill">
        <span className="dock-hint">GlassSuite</span>
      </GlassSurface>
    </div>
  );
}

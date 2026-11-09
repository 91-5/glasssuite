import { GlassSurface } from "@glasssuite/shared-ui";

/**
 * island 窗口根组件（P0 骨架）。
 *
 * 后续阶段在此接入：媒体（SMTC）、通知摘要、歌词、系统采样、时钟。
 * 岛与 dock 同进程不同窗口，状态通过 `ipc-types` 里的契约走事件，不共享 React 状态。
 */
export default function App() {
  return (
    <div className="island">
      <GlassSurface className="island-pill" radius={20} padding="6px 14px">
        <span className="island-hint">GlassSuite</span>
      </GlassSurface>
    </div>
  );
}

/**
 * DWM 预览承载窗的可见层（`dock/index.html#preview`）。
 *
 * 正常情况下 DWM 直接把缩略图画进这个窗口，本层保持全透明；
 * 后端注册超时/失败时发 `dock-preview-fallback`，这里改成一块纯色占位，
 * **不等待、不重试、不卡 UI**（B5 的预览超时降级判据）。
 */
import { useEffect, useState } from "react";
import { EVENTS, subscribe } from "@glasssuite/ipc-types";

export default function PreviewSurface() {
  const [fallback, setFallback] = useState(false);

  useEffect(
    () =>
      subscribe<string>(EVENTS.dockPreviewFallback, (key) => {
        setFallback(Boolean(key));
      }),
    [],
  );

  return <div className={fallback ? "preview-surface preview-surface--fallback" : "preview-surface"} />;
}

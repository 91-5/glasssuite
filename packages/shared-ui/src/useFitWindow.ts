/**
 * 内容尺寸上报钩子：窗口尺寸 = 内容尺寸 + 两侧留白。
 *
 * 为什么需要它：dock / island 都是「无边框 + 窗口贴合内容」的窗口。窗口必须跟着内容
 * 增长/收缩，否则透明的宽窗口会吃掉桌面上的鼠标事件（用户点空白处点不动图标）。
 * 而位置计算在 Rust 侧（`winplat`），前端只负责报尺寸——单一真相源。
 *
 * 实现要点：
 * - 用 `ResizeObserver` 而不是轮询：内容变化是离散事件，轮询既延迟又烧 CPU。
 * - 回调里再套一层 `requestAnimationFrame`，把同一批多次尺寸变化合并成一次调用
 *   （字体加载、图标就位都会连续触发 observer）。
 * - 尺寸向上取整（`Math.ceil`）：亚像素尺寸被截断会导致内容被裁掉 1px。
 * - 不在 Tauri 里跑（浏览器里 `pnpm dev` 调试）时 `invoke` 会抛错，这里吞掉——
 *   调试布局不该因为缺少后端而报红。
 */
import { useEffect, type RefObject } from "react";
import { fitDock, fitIsland } from "@glasssuite/ipc-types";

export type FitTarget = "dock" | "island";

const report = (target: FitTarget, w: number, h: number): Promise<void> => {
  const req = { w, h };
  return target === "dock" ? fitDock(req) : fitIsland(req);
};

/**
 * @param target  上报给哪个窗口（决定后端用哪种对齐规则）
 * @param ref     内容元素（药丸）。窗口尺寸 = 药丸尺寸 + `padding * 2`
 * @param padding 内容元素到窗口边缘的留白（逻辑像素），与 CSS 中 `.dock` / `.island`
 *                的 padding 必须一致
 */
export function useFitWindow(
  target: FitTarget,
  ref: RefObject<HTMLElement | null>,
  padding: number,
): void {
  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    let frame = 0;
    let last = "";

    const flush = () => {
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 || rect.height === 0) return;
      const w = Math.ceil(rect.width) + padding * 2;
      const h = Math.ceil(rect.height) + padding * 2;
      const key = `${w}x${h}`;
      if (key === last) return; // 尺寸没变就不打扰后端
      last = key;
      report(target, w, h).catch(() => {
        /* 非 Tauri 环境：布局调试期间忽略 */
      });
    };

    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(flush);
    };

    const observer = new ResizeObserver(schedule);
    observer.observe(el);
    schedule();

    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [target, ref, padding]);
}

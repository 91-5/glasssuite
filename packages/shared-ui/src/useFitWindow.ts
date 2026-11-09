/**
 * 内容尺寸上报 + 交互热区上报（两者必须同拍）。
 *
 * 为什么需要它：dock / island 都是「无边框 + 窗口贴合内容」的窗口。窗口必须跟着内容
 * 增长/收缩，否则透明的宽窗口会吃掉桌面上的鼠标事件；而 Rust 侧的点击穿透热区也
 * 必须跟着同一份尺寸走，否则窗口变宽后新区域永远命中不到。
 *
 * 实现要点：
 * - 用 `ResizeObserver` 而不是轮询：内容变化是离散事件，轮询既延迟又烧 CPU。
 * - 回调套一层 `requestAnimationFrame`，把同一批多次尺寸变化合并成一次调用。
 * - 尺寸向上取整：亚像素被截断会导致内容被裁掉 1px。
 * - 展开态用 `panelSize` 固定上报，且 `fit:false` 暂停 fit_*，避免把面板缩回药丸。
 * - 不在 Tauri 里跑（浏览器里 `pnpm dev` 调试）时 invoke 抛错，统一吞掉。
 */
import { useEffect, type RefObject } from "react";
import { fitDock, fitIsland, setHotzone } from "@glasssuite/ipc-types";

export type FitTarget = "dock" | "island";

export interface FitOptions {
  /** 是否上报 `fit_*`（展开态传 false）。默认 true。 */
  fit?: boolean;
  /** 展开态固定窗口尺寸（逻辑像素）；给了就不测量 DOM。 */
  panelSize?: { w: number; h: number };
}

const report = (target: FitTarget, w: number, h: number): Promise<void> => {
  const req = { w, h };
  return target === "dock" ? fitDock(req) : fitIsland(req);
};

/**
 * @param target  上报给哪个窗口
 * @param ref     内容元素（药丸）
 * @param padding 内容元素到窗口边缘的留白（逻辑像素），与 CSS padding 一致
 * @param options 展开态控制
 */
export function useFitWindow(
  target: FitTarget,
  ref: RefObject<HTMLElement | null>,
  padding: number,
  options: FitOptions = {},
): void {
  const fit = options.fit ?? true;
  const pw = options.panelSize?.w ?? 0;
  const ph = options.panelSize?.h ?? 0;
  const hasPanel = pw > 0 && ph > 0;

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    let frame = 0;
    let last = "";

    const flush = () => {
      // 用 offsetWidth/Height 而非 getBoundingClientRect：后者包含 CSS transform，
      // 悬停放大（scale）会被误报成「内容变宽」→ 窗口跟着变大 → 热区再变大，自激循环。
      // offsetWidth 是未变换的布局尺寸，正是窗口应该跟随的尺寸。
      const w = hasPanel ? pw : el.offsetWidth + padding * 2;
      const h = hasPanel ? ph : el.offsetHeight + padding * 2;
      if (w <= 0 || h <= 0) return;
      const key = `${w}x${h}|${fit}`;
      if (key === last) return; // 尺寸没变就不打扰后端
      last = key;
      if (fit) {
        report(target, w, h).catch(() => {
          /* 非 Tauri 环境 */
        });
      }
      // 热区与 fit 同一次上报：窗口一变宽，新区域立刻可命中。
      setHotzone(0, 0, w, h, target).catch(() => {
        /* 非 Tauri 环境 */
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
  }, [target, ref, padding, fit, hasPanel, pw, ph]);
}

/**
 * FLIP（First / Last / Invert / Play）与 v0.8.1 防跳修复 #2、#3。
 *
 * 旧仓「图标一连串跳」的根因有两层：
 *  1. 上一帧动画留下的 inline `transform` 没剥掉就去 `getBoundingClientRect()`，
 *     读到的是动画中间态而非自然布局位，于是每次都算出假位移 → 又触发一次动画。
 *  2. dock 窗口宽度变化时，若窗口整体被居中/平移，所有图标会**同幅**平移；
 *     这是窗口几何变化，不是图标重排，却被当成 N 个独立位移动画。
 *
 * 对应修复：
 *  - #2 测量前把 inline transform 临时置 `none`，量完立刻还原；
 *  - #3 若所有公共 key 的 (dx, dy) 都相同且非零，判为几何变化，整批跳过动画；
 *  - 两种情况都把 dx 记成 0 写进日志，这是 B4 的判据（transient 增减时 dx 全 0）。
 *
 * 配合后端的 #4（fit_dock 先按新中心 set_position）与 #5（药丸 flex-start 左锚定），
 * transient 图标增减时既有图标在视口里的自然位根本不变，dx 自然就是 0。
 */
import { useLayoutEffect, useRef, type RefObject } from "react";
import { logFlip } from "@glasssuite/ipc-types";

type RectMap = Map<string, DOMRect>;

const EPS = 0.5;
const DURATION = "transform 180ms cubic-bezier(.2,.8,.2,1)";

function measure(root: HTMLElement): { nodes: HTMLElement[]; rects: RectMap } {
  const nodes = Array.from(
    root.querySelectorAll<HTMLElement>("[data-flip-key]"),
  ).filter((n) => Boolean(n.dataset.flipKey));

  // 修复 #2：剥掉 inline transform 再量，读的是自然布局位。
  const saved = nodes.map((n) => n.style.transform);
  nodes.forEach((n) => {
    n.style.transform = "none";
  });
  const rects: RectMap = new Map();
  nodes.forEach((n) => rects.set(n.dataset.flipKey as string, n.getBoundingClientRect()));
  nodes.forEach((n, i) => {
    n.style.transform = saved[i];
  });
  return { nodes, rects };
}

/**
 * @param signature 内容变化时改变的字符串（key 列表），驱动一次测量
 * @param label     写进日志的事件标签，只含类型与数量，不含应用名
 */
export function useFlip(signature: string, label: string): RefObject<HTMLDivElement | null> {
  const ref = useRef<HTMLDivElement>(null);
  const first = useRef<RectMap>(new Map());

  useLayoutEffect(() => {
    const root = ref.current;
    if (!root) return;
    const { nodes, rects } = measure(root);

    const before = first.current;
    const common = nodes.filter((n) => before.has(n.dataset.flipKey as string));
    const dx = common.map(
      (n) =>
        (before.get(n.dataset.flipKey as string) as DOMRect).left -
        (rects.get(n.dataset.flipKey as string) as DOMRect).left,
    );
    const dy = common.map(
      (n) =>
        (before.get(n.dataset.flipKey as string) as DOMRect).top -
        (rects.get(n.dataset.flipKey as string) as DOMRect).top,
    );

    // 修复 #3：全员同幅平移 → 窗口几何变化，跳过动画。
    const uniform =
      common.length > 0 &&
      dx.every((d) => Math.abs(d - dx[0]) < EPS) &&
      dy.every((d) => Math.abs(d - dy[0]) < EPS);
    const geometry = uniform && (Math.abs(dx[0]) > EPS || Math.abs(dy[0]) > EPS);

    if (geometry) {
      logFlip(`${label} keys=${common.length} reason=geometry-skip dx=[${dx.map(() => 0).join(",")}]`);
      first.current = rects;
      return;
    }

    const deltas = dx.map((d) => (Math.abs(d) < EPS ? 0 : Math.round(d)));
    const moved: HTMLElement[] = [];
    common.forEach((n, i) => {
      if (Math.abs(dx[i]) > EPS || Math.abs(dy[i]) > EPS) {
        n.style.transition = "none";
        n.style.transform = `translate(${dx[i]}px, ${dy[i]}px)`;
        moved.push(n);
      }
    });
    logFlip(
      `${label} keys=${common.length} reason=${uniform ? "no-move" : "layout"} dx=[${deltas.join(",")}]`,
    );

    if (moved.length) {
      requestAnimationFrame(() => {
        moved.forEach((n) => {
          n.style.transition = DURATION;
          n.style.transform = "";
        });
      });
    }
    first.current = rects;
  }, [signature, label]);

  return ref;
}

/**
 * dock 行内拖拽的**纯决策核心**（OC-20261113-001 P1）。
 *
 * 刻意与 React / DOM / Tauri 零依赖：快照语义、落点判定、滞回死区、让位偏移、
 * 重排应用全是纯函数/纯状态机，`App.tsx` 只负责测量（pointerdown 快照）与把
 * 结果画出来。B1/B2 的走查脚本（`scripts/drag-walkthrough.ts`）因此可以脱离
 * WebView 直接驱动同一份逻辑：10/10 落点 + 死区来回不抖。
 *
 * 坐标口径：一律用**槽位中心的客户区 x**（CSS px）。快照在 pointerdown 时刻
 * 一次性测量（此刻没有任何 inline transform / hover 位移）；拖动期间让位靠
 * transform 完成、不改变布局，所以快照恒有效——这也从结构上消灭了旧实现
 * 写死 base=15/pitch=50（图标尺寸一改就系统性偏移）的根因。
 *
 * 落点语义（`drop`）：**在「不含被拖项」序列中的插入位**（0..n-1），即被拖
 * 图标最终落位前、它左边有几枚其他图标。松手重排 = 原序列先 splice 掉被拖项，
 * 再 splice 到 `drop`——两条 splice 用的就是同一个数，不需要换算。
 */

/** pointerdown 时刻的行快照：所有槽位中心 x（原始顺序，含被拖项）。 */
export interface RowSnapshot {
  centers: number[];
}

/** 滞回死区 = 0.25 × 槽距（卡面 P1 方案第 4 条；B2 的判据口径）。 */
export const DEADZONE_RATIO = 0.25;

/** 等宽图标的槽距（中心间距）。图标不足两枚时返回 0。 */
export function slotPitch(centers: number[]): number {
  return centers.length >= 2 ? centers[1] - centers[0] : 0;
}

/** 被拖项之外的中心点（升序 = 原始相对顺序，跳过被拖项）。 */
export function otherCenters(centers: number[], dragged: number): number[] {
  return centers.filter((_, i) => i !== dragged);
}

/**
 * 原始落点（无滞回）：x 左边有几枚**其他**图标的中心，就插到第几位。
 * 天然 clamp 在 [0, n-1] —— 拖出列表两端也不会越界（卡面「重排区 clamp」）。
 */
export function rawDrop(others: number[], x: number): number {
  let k = 0;
  while (k < others.length && others[k] < x) k++;
  return k;
}

/** 让位偏移（槽位数）：被拖项为 0（由幽灵代替）；其余按最终序平移 ±1 槽。 */
export function makeWayOffsets(
  dragged: number,
  drop: number,
  count: number
): number[] {
  const out = new Array<number>(count).fill(0);
  for (let j = 0; j < count; j++) {
    if (j === dragged) continue;
    if (j < dragged) {
      // 被拖项插到它前面（drop <= j）→ 右让一槽
      if (j >= drop) out[j] = 1;
    } else {
      // j 在被拖项右侧：drop >= j 时被拖项插到它前面 → 左让一槽
      if (drop >= j) out[j] = -1;
    }
  }
  return out;
}

/**
 * 一次行内拖拽的决策状态机。
 *
 * - 构造 = pointerdown 快照（此刻无 transform，测到的即自然位）；
 * - `update(x)` = 每次指针移动喂入幽灵中心 x，返回滞回后的插入位；
 *   死区内的来回移动**不产生切换**（`switches` 只在真正换槽时 +1，B2 观测口）；
 * - `offsets()` = 当前让位偏移（渲染直接用）；
 * - `drop` = 松手时的最终插入位（others 语义，直接供 splice）。
 */
export class DragSession {
  readonly centers: number[];
  readonly others: number[];
  readonly dragged: number;
  readonly deadzone: number;
  /** 当前插入位（others 语义）。 */
  current: number;
  /** 真实换槽次数（B2 抖动观测：死区内来回拖动必须为 0）。 */
  switches = 0;

  constructor(centers: number[], dragged: number, ratio: number = DEADZONE_RATIO) {
    if (dragged < 0 || dragged >= centers.length) {
      throw new Error(`dragged index ${dragged} out of range ${centers.length}`);
    }
    this.centers = [...centers];
    this.others = otherCenters(centers, dragged);
    this.dragged = dragged;
    this.deadzone = slotPitch(centers) * ratio;
    // 初始插入位 = 被拖项原位（它左侧的其他图标恰好有 dragged 枚）。
    this.current = dragged;
  }

  /** 快照槽距（等宽图标的中心间距）。 */
  slotPitch(): number {
    return slotPitch(this.centers);
  }

  /** 喂入幽灵中心 x，返回滞回后的插入位。 */
  update(x: number): number {
    const raw = rawDrop(this.others, x);
    if (raw !== this.current) {
      if (raw > this.current) {
        // 向右切：**严格越过**「被跨过的那枚图标中心」右侧死区才提交
        // （边界本身 ±deadzone 属于死区内部，不提交——B2 的判据口径）
        const boundary = this.others[raw - 1];
        if (x > boundary + this.deadzone) this.commit(raw);
      } else {
        // 向左切：严格越过目标位左侧那枚中心 - 死区才提交
        const boundary = this.others[raw];
        if (x < boundary - this.deadzone) this.commit(raw);
      }
    }
    return this.current;
  }

  private commit(next: number): void {
    this.current = next;
    this.switches++;
  }

  /** 当前让位偏移（槽位数组，含被拖项的 0）。 */
  offsets(): number[] {
    return makeWayOffsets(this.dragged, this.current, this.centers.length);
  }

  /** 松手时的最终插入位（others 语义）。 */
  get drop(): number {
    return this.current;
  }
}

/** 把让位偏移翻译成 inline transform（App 渲染用；0 → undefined 清掉样式）。 */
export function offsetToTransform(offset: number, pitch: number): string | undefined {
  if (offset === 0 || pitch <= 0) return undefined;
  return `translateX(${offset * pitch}px)`;
}

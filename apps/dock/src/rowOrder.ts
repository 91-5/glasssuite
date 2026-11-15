/**
 * dock 行内排序的**纯决策核心**（OC-20261115-005「已在运行先前置」）。
 *
 * 与 dragOrder.ts 同一纪律：与 React/DOM/Tauri 零依赖，走查脚本可脱离
 * WebView 直接驱动同一份逻辑。
 *
 * 语义：**pinned 图标中已在运行的排到 dock 行前面**，两组内部各自保持原
 * 相对顺序（稳定切分，不是按运行时长重排）；transients（运行中未固定）
 * 的位置不归本函数管（OC-20261115-003 刚把它们定在 pinned 右侧，本卡不动）。
 */

/**
 * 稳定切分：`isRunning` 为真的排前面，其余按原顺序跟在后面。
 * `running` 集为空时返回原序（行为与 005 之前完全一致）。
 */
export function orderRunningFirst<T>(items: T[], isRunning: (item: T) => boolean): T[] {
  const running: T[] = [];
  const idle: T[] = [];
  for (const item of items) {
    (isRunning(item) ? running : idle).push(item);
  }
  return running.concat(idle);
}

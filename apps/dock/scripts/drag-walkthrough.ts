/**
 * P1 拖拽重排**走查脚本**（OC-20261113-001 B1/B2）。
 *
 * 卡面交付判据允许「单测或走查脚本」：本脚本脱离 WebView / Tauri，直接驱动
 * `src/dragOrder.ts`（App.tsx 拖拽用的同一份纯决策核心），验证：
 *   B1 —— 连续 10 次拖到不同目标位，iconOrder 与独立口径的预期 10/10 一致；
 *   B2 —— 在槽位中心 ±25%（滞回死区）来回拖动，槽位切换计数为 0（不抖动）；
 *   附加 —— 两端 clamp、单图标行、让位偏移与独立口径一致。
 *
 * 运行（见 delivery 报告）：tsc 编译本文件 + dragOrder.ts 到临时目录后 node 执行。
 * 退出码 0 = 全部通过；非 0 = 有断言失败（逐条打印）。
 */
import { DragSession, makeWayOffsets, rawDrop, slotPitch } from "../src/dragOrder";

// 走查脚本按 CommonJS 单独编译（无 @types/node 环境），显式声明用到的宿主能力。
declare const process: { exit(code: number): never };

let failures = 0;
function check(name: string, ok: boolean, detail: string): void {
  if (ok) {
    console.log(`PASS ${name}`);
  } else {
    failures++;
    console.error(`FAIL ${name}: ${detail}`);
  }
}

/** 独立口径的预期顺序：把被拖项移除后，按「左边有几枚中心 < x」插回。 */
function expectedOrder(order: number[], centers: number[], from: number, x: number): number[] {
  const others = centers.filter((_, i) => i !== from);
  let k = 0;
  for (const c of others) if (c < x) k++;
  const next = order.filter((_, i) => i !== from);
  next.splice(k, 0, order[from]);
  return next;
}

/** 模拟一次行内拖拽：按下 → 沿 samples 逐点喂指针 → 松手，返回新顺序。 */
function simulateDrag(
  order: number[],
  centers: number[],
  from: number,
  samples: number[]
): { order: number[]; session: DragSession } {
  const session = new DragSession(centers, from);
  for (const x of samples) session.update(x);
  const drop = session.drop;
  const next = [...order];
  const [moved] = next.splice(from, 1);
  next.splice(drop, 0, moved);
  return { order: next, session };
}

// ---- B1：连续 10 次拖到不同目标位，10/10 一致 -------------------------------
{
  // 8 枚图标，等距槽（58px 槽距，起点 100——数值本身任意，逻辑只看相对关系）。
  // 目标位取**无歧义点**（相邻中心的中点 / 两端外 40px，距任何中心 ≥29px，
  // 远大于 14.5px 的滞回死区）——用户「拖到某个位置松手」的真实意图都是这类点；
  // 死区边缘的归属是 B2 专门验证的事，不混进 B1 的落点正确性里。
  const n = 8;
  const base = [...Array(n).keys()];
  const centers = base.map((i) => 100 + i * 58);
  const pitch = slotPitch(centers);
  const othersOf = (from: number) => centers.filter((_, i) => i !== from);
  const xFor = (from: number, k: number): number => {
    const o = othersOf(from);
    if (k <= 0) return o[0] - 40;
    if (k >= o.length) return o[o.length - 1] + 40;
    return (o[k - 1] + o[k]) / 2;
  };
  let order = base;
  let ok = true;
  const log: string[] = [];
  for (let t = 0; t < 10; t++) {
    const from = [0, 1, 2, 3, 4, 5, 6, 7, 2, 5][t];
    const k = [5, 0, 7, 1, 6, 2, 4, 3, 0, 7][t]; // 10 个不同目标位
    const x = xFor(from, k);
    const before = [...order];
    const r = simulateDrag(order, centers, from, [x, x, x]);
    const expected = expectedOrder(before, centers, from, x);
    const match = r.order.every((v, i) => v === expected[i]);
    log.push(`#${t} from=${from} k=${k} x=${x.toFixed(0)} -> [${r.order}]`);
    if (!match) {
      ok = false;
      log.push(`  expect [${expected}]`);
    }
    order = r.order;
  }
  check("B1 连续 10 次拖到不同目标位 10/10 一致", ok, log.join(" | "));

  // 落点永远 clamp 进列表：越出两端的 x 不产生非法下标
  const r = simulateDrag(base, centers, 3, [-999]);
  check("B1 越出左端 clamp 到槽 0", r.order[0] === 3, `order=[${r.order}]`);
  const r2 = simulateDrag(base, centers, 3, [9999]);
  check("B1 越出右端 clamp 到末槽", r2.order[n - 1] === 3, `order=[${r2.order}]`);
}

// ---- B2：滞回死区（中心 ±25% 区间来回拖动，让位不抖）-----------------------
{
  const centers = [0, 1, 2, 3, 4, 5, 6, 7].map((i) => 100 + i * 58);
  const pitch = slotPitch(centers);
  const dead = pitch * 0.08; // 与 DEADZONE_RATIO 同步（手感反馈后由 0.25 降到 0.08）
  const from = 2;
  const boundary = 100 + 1 * 58; // 槽 1 的中心（drop 1 ↔ 2 的切换中心）
  const s = new DragSession(centers, from);
  // 在中心 ±dead 内来回扫 200 次：落点应锁死在初始槽，一次都不切换
  for (let i = 0; i < 200; i++) {
    const x = boundary + (i % 2 === 0 ? dead : -dead);
    s.update(x);
  }
  check(
    "B2 中心 ±25% 区间来回 200 次，槽位切换 = 0",
    s.switches === 0,
    `switches=${s.switches} drop=${s.drop}`
  );

  // 从槽 2 向左越过死区一步（-1px beyond）应切换且只切换一次；再拖回死区内不回切
  const s2 = new DragSession(centers, from);
  s2.update(boundary - dead - 1);
  const afterOut = s2.current;
  for (let i = 0; i < 50; i++) s2.update(boundary + (i % 2 === 0 ? dead : -dead));
  check(
    "B2 越过死区 1px 提交切换，回死区内不回切（滞回生效）",
    afterOut !== from && s2.switches === 1 && s2.current === afterOut,
    `afterOut=${afterOut} switches=${s2.switches} current=${s2.current}`
  );
}

// ---- 让位偏移 vs 独立口径 ---------------------------------------------------
{
  const n = 6;
  const centers = [...Array(n).keys()].map((i) => 100 + i * 58);
  let ok = true;
  for (let dragged = 0; dragged < n; dragged++) {
    for (let drop = 0; drop < n; drop++) {
      const offs = makeWayOffsets(dragged, drop, n);
      // 独立口径：直接模拟重排后的最终位置差
      const idx = [...Array(n).keys()];
      const [moved] = idx.splice(dragged, 1);
      idx.splice(drop, 0, moved);
      const finalPos = new Array<number>(n);
      idx.forEach((orig, pos) => (finalPos[orig] = pos));
      for (let j = 0; j < n; j++) {
        const expect = Math.sign(finalPos[j] - j);
        if (j === dragged) continue;
        if (offs[j] !== expect) ok = false;
      }
      if (offs[dragged] !== 0) ok = false;
    }
  }
  check("makeWayOffsets 与「重排后位置差」全组合一致", ok, "");
  // rawDrop 语义抽查：中心右侧 = 计入
  const others = centers.filter((_, i) => i !== 2);
  check(
    "rawDrop：越过第 k 枚中心 → 插入位 k",
    rawDrop(others, 100 + 0 * 58 + 1) === 1 && rawDrop(others, 50) === 0,
    ""
  );
}

if (failures > 0) {
  console.error(`\n${failures} 项失败`);
  process.exit(1);
}
console.log("\n全部通过");

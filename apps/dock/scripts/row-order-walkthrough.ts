/**
 * 005「已在运行先前置」走查脚本：脱离 WebView 驱动 `src/rowOrder.ts` 的同一份
 * 排序逻辑，验证：运行中前置、组内稳定、空集/全运行集不重排、id 映射可逆。
 * 运行方式与 drag-walkthrough 相同（tsc 编译到临时目录后 node 执行）。
 */
import { orderRunningFirst } from "../src/rowOrder";

declare const process: { exit(code: number): never };

let failures = 0;
function check(name: string, ok: boolean, detail: string): void {
  if (ok) console.log(`PASS ${name}`);
  else {
    failures++;
    console.error(`FAIL ${name}: ${detail}`);
  }
}

interface Sc {
  id: string;
  running: boolean;
}

// 6 个 pinned：c2/c0/c4 在运行
const items: Sc[] = [
  { id: "c0", running: true },
  { id: "c1", running: false },
  { id: "c2", running: true },
  { id: "c3", running: false },
  { id: "c4", running: true },
  { id: "c5", running: false },
];
const out = orderRunningFirst(items, (s) => s.running);
check(
  "运行中的排前面，组内保持原相对顺序",
  out.map((s) => s.id).join(",") === "c0,c2,c4,c1,c3,c5",
  `[${out.map((s) => s.id)}]`
);

// 空运行集：原样返回（与 005 之前行为一致）
const none = orderRunningFirst(items, () => false);
check(
  "空运行集 = 原序不变",
  none.map((s) => s.id).join(",") === "c0,c1,c2,c3,c4,c5",
  `[${none.map((s) => s.id)}]`
);

// 全在运行：原序不变（全部前置 = 相对顺序不变）
const all = orderRunningFirst(items, () => true);
check(
  "全在运行 = 原序不变",
  all.map((s) => s.id).join(",") === "c0,c1,c2,c3,c4,c5",
  `[${all.map((s) => s.id)}]`
);

// 稳定性：多次运行结果一致（无隐藏状态）
const a = orderRunningFirst(items, (s) => s.running).map((s) => s.id);
const b = orderRunningFirst(items, (s) => s.running).map((s) => s.id);
check("纯函数：两次调用结果一致", a.join(",") === b.join(","), `[${a}] vs [${b}]`);

// 前置不改集合：display→config 映射可逆（__gdPickDrop 的换算依据）
const configOrder = ["c0", "c1", "c2", "c3", "c4", "c5"];
const display = orderRunningFirst(items, (s) => s.running).map((s) => s.id);
const roundTrip = display.map((id) => configOrder.indexOf(id)).sort((x, y) => x - y);
check(
  "display→config 下标映射可逆（同一集合）",
  roundTrip.join(",") === "0,1,2,3,4,5",
  `[${roundTrip}]`
);

if (failures > 0) {
  console.error(`\n${failures} 项失败`);
  process.exit(1);
}
console.log("\n全部通过");

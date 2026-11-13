import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { DragSession, makeWayOffsets, offsetToTransform } from "./dragOrder";

interface IconData {
  w: number;
  h: number;
  /** RGBA，base64 */
  rgba: string;
}

interface Shortcut {
  id: string;
  name: string;
  path: string;
  icon: IconData;
  /** 图标所在层（v0.7 Phase 1）：dock = 在 dock 行；free = 游离图标（独立顶层窗口） */
  mode?: "dock" | "free";
  /** 游离模式下的窗口矩形（物理像素虚拟桌面坐标） */
  free_rect?: { x: number; y: number; w: number; h: number } | null;
}

/** v0.8：未固定的运行中应用（dock 行临时图标） */
interface TransientApp {
  key: string;
  name: string;
  path: string;
  aumid: string | null;
  icon: IconData | null;
}

/** 图标下方的「运行中」指示条（OC-20261030-003 任务 A） */
function RunDot() {
  return <span className="run-dot" aria-hidden />;
}

function IconCanvas({ icon }: { icon: IconData }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    try {
      const bin = atob(icon.rgba);
      const bytes = new Uint8ClampedArray(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      ctx.putImageData(new ImageData(bytes, icon.w, icon.h), 0, 0);
    } catch (e) {
      console.error("icon decode failed", e);
    }
  }, [icon]);
  return <canvas ref={ref} width={icon.w} height={icon.h} className="dock-icon-img" />;
}

function App() {
  const [icons, setIcons] = useState<Shortcut[]>([]);
  const [running, setRunning] = useState<Set<string>>(new Set());
  // v0.8：运行中但未固定的应用（临时图标，固定右侧、RunDot 常亮）
  const [transients, setTransients] = useState<TransientApp[]>([]);
  const pillRef = useRef<HTMLDivElement>(null);
  // 提示条（例：游离图标达上限被拒 —— 不许静默失败）
  const [notice, setNotice] = useState<string | null>(null);
  // 任务 B：悬停预览。300ms 定时（卡面：悬停 300~500ms 弹出），
  // 到点把图标按钮的真实 rect（CSS px）交给 Rust 定位预览窗；
  // 移开/点击立即销毁（Rust 侧 DestroyWindow，不残留句柄）。
  const hoverTimer = useRef<number | undefined>(undefined);

  const enterIcon = (sc: Shortcut) => (e: React.MouseEvent<HTMLButtonElement>) => {
    if (dragRef.current) return; // 已按下（含未起拖）不弹预览——预览窗 TOPMOST 会吃掉拖动事件
    const el = e.currentTarget;
    window.clearTimeout(hoverTimer.current);
    hoverTimer.current = window.setTimeout(() => {
      if (dragRef.current) return;
      const r = el.getBoundingClientRect();
      invoke("preview_show", { id: sc.id, x: r.left, y: r.top, w: r.width, h: r.height })
        .catch(() => {});
    }, 300);
  };

  const leaveIcon = () => {
    window.clearTimeout(hoverTimer.current);
    invoke("preview_hide").catch(() => {});
  };

  useEffect(() => {
    invoke<Shortcut[]>("list_shortcuts")
      .then(setIcons)
      .catch((e) => console.error("list_shortcuts failed", e));
  }, []);

  // 配置版本轮询：dock.json 被改过（拖出/拖回/外部改配置）就重拉列表。
  // 游离图标进出 dock 行都靠它刷新（卡面 §1.3-5 的补位动画由此触发）。
  // v0.7.1（sir 反馈频闪）：拖动进行中跳过校准——drag_out 写盘会立刻 bump 版本，
  // 整表重拉会覆盖本地让位动画的状态，图标在两个位置间横跳（频闪）。
  // 松手后 dragRef 已清 null，下一轮 tick 自然恢复校准。
  const draggingRef = useRef(false);
  useEffect(() => {
    let stopped = false;
    let timer = 0;
    let lastV = -1;
    const tick = () => {
      if (draggingRef.current) {
        timer = window.setTimeout(tick, 400);
        return;
      }
      invoke<number>("dock_config_version")
        .then((v) => {
          if (stopped) return;
          if (v !== lastV) {
            lastV = v;
            invoke<Shortcut[]>("list_shortcuts")
              .then((list) => {
                if (!stopped) setIcons(list);
              })
              .catch(() => {});
          }
        })
        .catch(() => {})
        .finally(() => {
          if (!stopped) timer = window.setTimeout(tick, 400);
        });
    };
    tick();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, []);

  // 游离图标原生拖动落在 dock 区 → Rust 问前端要落点槽位（DOM 几何只有前端知道）。
  useEffect(() => {
    const pick = (id: string, cssX: number) => {
      const els = Array.from(
        document.querySelectorAll<HTMLElement>(".dock-icons [data-drag]")
      );
      const row = pillRef.current?.getBoundingClientRect();
      // 落点离药丸太远 → 不算拖回，保持游离
      if (!row || !els.length) return;
      if (cssX < row.left - 24 || cssX > row.right + 24) return;
      let best = 0;
      let bestD = Infinity;
      els.forEach((el, i) => {
        const r = el.getBoundingClientRect();
        const d = Math.abs(r.left + r.width / 2 - cssX);
        if (d < bestD) {
          bestD = d;
          best = i;
        }
      });
      invoke("free_drop_result", { id, slot: best }).catch(() => {});
    };
    (window as unknown as { __gdPickDrop?: (id: string, x: number) => void }).__gdPickDrop =
      pick;
    return () => {
      delete (window as unknown as { __gdPickDrop?: (id: string, x: number) => void })
        .__gdPickDrop;
    };
  }, [icons]);

  // 运行状态轮询：Rust 侧 watcher（事件驱动 + 500ms 兜底）维护缓存，
  // 这里只取一次轻量结果，不产生高频轮询压力。
  // 运行状态轮询：Rust 侧 watcher（事件驱动 + 500ms 兜底）维护缓存，
  // 这里只取一次轻量结果，不产生高频轮询压力。
  // v0.8.1（sir 2026-11-08 反馈「图标一连串跳动」）：只在内容真变时 setState。
  // 旧实现每 500ms 无条件 new Set / 新数组引用 → 每 500ms 一次无谓 re-render，
  // 而 FLIP 补位 effect 无依赖数组会随之重跑（自激振荡的一环，见 FLIP 注释）。
  const transientLenRef = useRef(-1); // [TEMP-EVIDENCE]
  useEffect(() => {
    let stopped = false;
    let timer = 0;
    let lastRunIds: string[] | null = null;
    let lastTransientFp: string | null = null;
    const tick = () => {
      invoke<string[]>("get_running_ids")
        .then((ids) => {
          if (stopped) return;
          const prev = lastRunIds;
          if (
            prev === null ||
            prev.length !== ids.length ||
            ids.some((v, i) => v !== prev[i])
          ) {
            lastRunIds = ids;
            setRunning(new Set(ids));
          }
        })
        .catch(() => {});
      // v0.8：同一 500ms 节奏取临时条目（Rust 侧复用 watcher 快照，无额外轮询线程）
      invoke<TransientApp[]>("get_transient_running")
        .then((list) => {
          if (stopped) return;
          // 指纹 = key 序列 + 图标就绪态；不变就不 setState
          const fp = list.map((t) => `${t.key}|${t.icon ? 1 : 0}`).join(";");
          if (fp !== lastTransientFp) {
            lastTransientFp = fp;
            if (list.length !== transientLenRef.current) {
              transientLenRef.current = list.length;
              invoke("debug_note", { msg: `transients n=${list.length}` }).catch(() => {});
            }
            setTransients(list);
          }
        })
        .catch(() => {});
      timer = window.setTimeout(tick, 500);
    };
    tick();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, []);

  // 首帧就绪上报：WebView2 完成首次绘制后通知 Rust。
  // Rust 在收到之前不显示 dock —— 首帧黑闪的根因修复点。
  useEffect(() => {
    let reported = false;
    const report = () => {
      if (reported) return;
      reported = true;
      invoke("dock_ready").catch(() => {});
    };
    // 两帧 rAF = 至少完成过一次真实合成
    requestAnimationFrame(() => requestAnimationFrame(() => report()));
    // 兜底：隐藏状态下 rAF 可能不触发（超时即视为就绪，避免 dock 永远不出现）
    const timer = window.setTimeout(report, 1500);
    return () => window.clearTimeout(timer);
  }, []);

  // 窗口收缩到药丸实际大小：点击区域与可见区域一致。
  // v0.8.1：transients 每 500ms 拿到新数组引用会重跑本 effect；尺寸真变了才上报——
  // 否则每秒 2 次无意义 set_size（Rust 侧还要过重定位判定）。
  // 边距 = 外层 .dock 左右 padding（当前 6px×2）——药丸左锚定后窗口宽度必须严格
  // 等于「药丸宽 + 12」，否则右侧被裁 2px（旧的 +8 是 padding=4 时代的常数）。
  const DOCK_PAD_X = 6;
  const lastFit = useRef<{ w: number; h: number } | null>(null);
  useEffect(() => {
    const pill = pillRef.current;
    if (!pill) return;
    const r = pill.getBoundingClientRect();
    const w = r.width + DOCK_PAD_X * 2;
    const h = r.height + DOCK_PAD_X * 2;
    const prev = lastFit.current;
    if (prev && Math.abs(prev.w - w) < 0.5 && Math.abs(prev.h - h) < 0.5) return;
    lastFit.current = { w, h };
    invoke("fit_dock", { w, h }).catch(() => {});
  }, [icons, transients]);

  // 禁用 WebView 默认右键菜单（透明区右键会弹出浏览器菜单）
  useEffect(() => {
    const handler = (e: MouseEvent) => e.preventDefault();
    window.addEventListener("contextmenu", handler);
    return () => window.removeEventListener("contextmenu", handler);
  }, []);

  const launch = (path: string) => {
    invoke("launch_shortcut", { path }).catch((e) => console.error("launch failed", e));
    // 任务 C：dock 图标点击属于「面板外」，开始菜单面板随之收回
    // （面板失焦复查对「新前台是 dock」不收回，交给这里显式关）
    invoke("menu_hide").catch(() => {});
  };

  // v0.8：单击临时图标 = 激活（快路径）；右键 = 固定到 dock（转正）
  const activateTransient = (t: TransientApp) => {
    invoke("transient_activate", { key: t.key, path: t.path, aumid: t.aumid })
      .catch((e) => console.error("transient activate failed", e));
    invoke("menu_hide").catch(() => {});
  };

  const pinTransient = async (t: TransientApp) => {
    try {
      await invoke("pin_transient", { key: t.key });
      // pin 已 bump 配置版本，版本轮询会校准；这里立即重拉让固定图标即刻出现
      setIcons(await invoke<Shortcut[]>("list_shortcuts"));
    } catch (e) {
      const msg = typeof e === "string" ? e : String(e);
      setNotice(msg);
      window.setTimeout(() => setNotice(null), 4000);
    }
  };

  const add = async () => {
    try {
      setIcons(await invoke<Shortcut[]>("add_shortcut"));
    } catch (e) {
      console.error("add_shortcut failed", e);
    }
  };

  const remove = async (id: string) => {
    try {
      setIcons(await invoke<Shortcut[]>("remove_shortcut", { id }));
    } catch (e) {
      console.error("remove_shortcut failed", e);
    }
  };

  // v0.5.1：dock 图标拖动排序——提起-跟随-放下（Windows 桌面式）。
  // OC-20261113-001 P1「实时化」改版：旧实现拖动期间其他图标不动、松手才按
  // 写死 base=15/pitch=50 的公式一次性 splice——图标改 52px 后公式早已失准，
  // 落点错就弹回，用户只能反复试。现改为：
  //   ① pointerdown 一次性快照全部槽位中心（此刻无任何 transform，自然位）；
  //   ② 拖动中被跨过的图标立即 translateX(±1 槽) 让位（等宽图标 = 纯算术，
  //     不做每帧 DOM 重排、不需要 FLIP）；
  //   ③ 落点 = 快照中心线性扫描 + 0.25×槽距 滞回死区（抖动整类消失）；
  //   ④ pointer capture 打在**容器**上（让位移动的是图标本身，capture 打在
  //     图标上会在重排时跟丢）；松手用滞回槽位直接落，无弹回重试。
  // 决策核心全部在 ./dragOrder（纯函数，走查脚本可脱离 WebView 驱动同一份逻辑）。
  const dragRef = useRef<{
    index: number;
    startX: number;
    startY: number;
    startT: number;
    lastX: number; // 最近一次指针位置（拖出落点用）
    lastY: number;
    active: boolean;
    out: boolean; // 已判定「拖出」（脱离区：垂直 >48px + 驻留 ≥200ms + 方向向外）
    pid: number;
    offset: number; // 按下点相对被拖图标左缘的偏移（幽灵 x 跟随用）
    grabDy: number; // 按下点相对被拖图标上缘的偏移（幽灵 y 跟随用）
    top: number; // 按下时图标上缘
    iconW: number; // 图标宽（幽灵中心 = clientX - offset + iconW/2）
    session: DragSession | null; // pointerdown 快照 + 滞回状态机
    beyondSince: number | null; // 进入脱离区 (>48px) 的时刻（驻留判定）
    lastDistY: number; // 上一帧的行外垂直距离（「方向向外」判定）
  } | null>(null);
  // 拖动结束后的短暂点击抑制：capture 在容器上时，松手那次 click 不会落到
  // 图标上（老内核无 capture 的兼容路径仍会落到）——改用时间窗双兼容。
  const suppressClickUntil = useRef(0);
  const [dragIndex, setDragIndex] = useState(-1);
  const [dragGhost, setDragGhost] = useState<{ x: number; y: number } | null>(null);
  // 夜间状态表达（OC-20261113-001 P3）：23:00–07:00 dock 降饱和（后端下发）。
  const [nightDim, setNightDim] = useState(false);
  useEffect(() => {
    const un = listen<{ on: boolean }>("night-state", (e) => setNightDim(e.payload?.on === true));
    return () => {
      un.then((f) => f());
    };
  }, []);
  // 当前让位态：drop = 滞回后的插入位（others 语义），pitch = 快照槽距。
  // null = 没有让位（未起拖 / 已拖出 / 已松手）。
  const [makeWay, setMakeWay] = useState<{ drop: number; pitch: number } | null>(null);
  // dock 行可见条目（游离图标不在行内）
  const dockIcons = useMemo(
    () => icons.filter((s) => (s.mode ?? "dock") === "dock"),
    [icons]
  );

  /** pointerdown 快照：剥掉可能残留的 FLIP inline transform 后量自然位中心。 */
  const snapshotCenters = (): number[] => {
    const out: number[] = [];
    for (const sc of dockIcons) {
      const el = rowRefs.current.get(sc.id);
      if (!el) continue;
      if (el.style.transform) {
        el.style.transition = "none";
        el.style.transform = "";
        void el.offsetWidth;
      }
      const r = el.getBoundingClientRect();
      out.push(r.left + r.width / 2);
    }
    return out;
  };

  const iconDown = (i: number) => (e: React.PointerEvent<HTMLButtonElement>) => {
    if (e.button !== 0) return;
    e.preventDefault(); // 压掉原生选择/拖拽语义
    draggingRef.current = true; // 拖动期间暂停版本轮询校准（防频闪）
    // 按下即收预览（预览窗 TOPMOST 悬在图标上方，会吃掉拖动的移动事件）
    leaveIcon();
    const r = e.currentTarget.getBoundingClientRect();
    dragRef.current = {
      index: i,
      startX: e.clientX,
      startY: e.clientY,
      startT: Date.now(),
      lastX: e.clientX,
      lastY: e.clientY,
      active: false,
      out: false,
      pid: e.pointerId,
      offset: e.clientX - r.left,
      grabDy: e.clientY - r.top,
      top: r.top,
      iconW: r.width,
      session: new DragSession(snapshotCenters(), i),
      beyondSince: null,
      lastDistY: 0,
    };
    try {
      // capture 打在容器：让位时图标自己在动，capture 在图标上会跟丢。
      pillRef.current?.setPointerCapture(e.pointerId);
    } catch {
      /* 老内核无 capture 也不至于崩 */
    }
  };

  /** 脱离区判定（卡面 P1 三区）：垂直出界 >48px、驻留 ≥200ms、方向向外。 */
  const DETACH_DIST = 48;
  const DETACH_DWELL_MS = 200;

  const iconMove = (clientX: number, clientY: number, pointerId?: number) => {
    const d = dragRef.current;
    if (!d) return;
    if (pointerId !== undefined && pointerId !== d.pid) return;
    d.lastX = clientX;
    d.lastY = clientY;
    const dx = clientX - d.startX;
    if (!d.active) {
      if (Math.abs(dx) < 6) return;
      d.active = true;
      setDragIndex(d.index);
      setMakeWay({ drop: d.index, pitch: d.session?.slotPitch() ?? 0 });
      invoke("debug_note", { msg: `active idx=${d.index} x=${Math.round(clientX)}` }).catch(() => {});
    }

    // 三区判定：重排区/灰区（clamp 不脱离）只在行内让位；只有「垂直出界
    // >48px 且驻留 ≥200ms 且没有在往回走」才转拖出意图。
    if (!d.out) {
      const row = pillRef.current?.getBoundingClientRect();
      if (row) {
        const dy =
          clientY < row.top
            ? row.top - clientY
            : clientY > row.bottom
              ? clientY - row.bottom
              : 0;
        const now = Date.now();
        if (dy > DETACH_DIST) {
          const outward = dy >= d.lastDistY - 1; // 没有明显在往回走
          if (d.beyondSince === null) d.beyondSince = now;
          if (outward && now - d.beyondSince >= DETACH_DWELL_MS) {
            d.out = true;
            setMakeWay(null); // 图标「离开」dock 行：让位复位，幽灵消失
            setDragGhost(null);
            invoke("debug_note", { msg: `detach dy=${Math.round(dy)}` }).catch(() => {});
          }
        } else {
          d.beyondSince = null; // 回到行附近：重新计驻留
        }
        d.lastDistY = dy;
      }
    }

    if (!d.out) {
      // 幽灵完全跟手（x/y 都跟随光标，垂直微调不再读作「黏」）
      setDragGhost({ x: clientX - d.offset, y: clientY - d.grabDy });
      // 让位 + 滞回：被跨过的图标立即平移，落点在死区内不抖。
      const s = d.session;
      if (s) {
        const ghostCenterX = clientX - d.offset + d.iconW / 2;
        const prev = s.current;
        const cur = s.update(ghostCenterX);
        if (cur !== prev) {
          setMakeWay({ drop: cur, pitch: s.slotPitch() });
          invoke("debug_note", { msg: `make-way ${prev}->${cur}` }).catch(() => {});
        }
      }
    }
  };

  const iconUp = (pointerId?: number) => {
    const d = dragRef.current;
    if (!d) return;
    if (pointerId !== undefined && pointerId !== d.pid) return;
    dragRef.current = null;
    draggingRef.current = false; // 拖动结束，恢复版本轮询校准
    try {
      pillRef.current?.releasePointerCapture(d.pid);
    } catch {
      /* 已释放 */
    }
    setMakeWay(null);
    if (!d.active) {
      setDragIndex(-1);
      setDragGhost(null);
      return;
    }
    suppressClickUntil.current = Date.now() + 400;
    setDragIndex(-1);
    setDragGhost(null);
    // v0.7：拖出成游离图标（落点 = 松手位置；Rust 换算成物理像素虚拟桌面坐标）
    if (d.out) {
      const sc = dockIcons[d.index];
      if (!sc) return;
      invoke("free_drag_out", { id: sc.id, cx: d.lastX, cy: d.lastY })
        .then(() => {
          // 本地立即移除 → dock 行立刻补位（Rust 侧写完 dock.json 后版本轮询再校准）
          setIcons((prev) => prev.filter((s) => s.id !== sc.id));
        })
        .catch((e) => {
          const msg = typeof e === "string" ? e : String(e);
          setNotice(msg);
          window.setTimeout(() => setNotice(null), 4000);
        });
      return;
    }
    // 落点 = 滞回状态机当前的插入位（others 语义），不再从幽灵坐标反推——
    // 快照中心扫描对 DPI/图标尺寸/间距变化天然免疫，也没有 Math.round 边界抖动。
    const drop = d.session?.drop ?? d.index;
    invoke("debug_note", { msg: `drop from=${d.index} to=${drop}` }).catch(() => {});
    if (drop !== d.index) {
      // 让位已把图标画到了最终位置：这次重排是「确认」，FLIP 不再播动画
      // （清掉基线，下一次 layout effect 只重新计量）。
      lastLeft.current.clear();
      setIcons((prev) => {
        // 只重排 **dock 行内**的条目：d.index/drop 都是行内下标，
        // 而 prev 含游离条目（不在行内）——直接对 prev 操作会串位。
        // 游离条目在数组里排到末尾（它们不占行内位置，行序无关）。
        const row = prev.filter((s) => (s.mode ?? "dock") === "dock");
        const free = prev.filter((s) => (s.mode ?? "dock") === "free");
        const next = [...row];
        const [moved] = next.splice(d.index, 1);
        next.splice(drop, 0, moved);
        const merged = [...next, ...free];
        // 持久化：服务端返回权威顺序（防并发/丢条目）
        invoke<Shortcut[]>("reorder_shortcuts", { paths: merged.map((s) => s.path) })
          .then(setIcons)
          .catch((e) => console.error("reorder failed", e));
        return merged;
      });
    }
  };

  // 双保险：capture 之外的 window 级兜底（两路都到 = 幂等，无副作用）
  useEffect(() => {
    const wm = (e: PointerEvent) => iconMove(e.clientX, e.clientY, e.pointerId);
    const wu = (e: PointerEvent) => iconUp(e.pointerId);
    const wm2 = (e: MouseEvent) => {
      // 老内核只发兼容 mouse 事件时也能走通（与 pointer 路径幂等）
      if (e.buttons === 0) return;
      iconMove(e.clientX, e.clientY);
    };
    const wu2 = () => iconUp();
    window.addEventListener("pointermove", wm);
    window.addEventListener("pointerup", wu);
    window.addEventListener("pointercancel", wu);
    window.addEventListener("mousemove", wm2);
    window.addEventListener("mouseup", wu2);
    return () => {
      window.removeEventListener("pointermove", wm);
      window.removeEventListener("pointerup", wu);
      window.removeEventListener("pointercancel", wu);
      window.removeEventListener("mousemove", wm2);
      window.removeEventListener("mouseup", wu2);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);


  // v0.7 Phase 1：dock 行补位动画（FLIP）。
  // 图标被拖出后，其余图标向左补位——用「先量旧位置 → 渲染后反向位移 → 过渡回 0」
  // 做 150ms 过渡（卡面 §1.3-5「dock 行必须自动补位（CSS transition ~150ms），不留空槽」）。
  const rowRefs = useRef(new Map<string, HTMLButtonElement>());
  const lastLeft = useRef(new Map<string, number>());
  const flipsRef = useRef(0);
  // v0.7 Phase 1：dock 行补位动画（FLIP）。
  // 图标被拖出后，其余图标向左补位——用「先量旧位置 → 渲染后反向位移 → 过渡回 0」
  // 做 150ms 过渡（卡面 §1.3-5「dock 行必须自动补位（CSS transition ~150ms），不留空槽」）。
  //
  // v0.8.1（sir 2026-11-08 反馈「图标一连串跳动」，menu.log 实证 dx ±28.6→±28.3→∓…）：
  // 本 effect 无依赖数组 → 每次 render 都跑；两条 500ms 轮询每轮都触发 re-render。
  // getBoundingClientRect() 返回的是**含 transform 的渲染位置**——若测量落在上一轮
  // 150ms 过渡进行中（400ms 版本轮询与 500ms 运行轮询节拍漂移，命中率 ~37%），
  // 记录到的就是动画中间值；下一轮（过渡已结束）算出反号 dx → 再播一次过渡 →
  // 若再被采样……形成 ±28px、~0.5s 一次、幅度减半的一连串跳动（自激振荡）。
  // 修复：测量前先剥掉 inline transform——记录的永远是「自然位」，环路切断；
  // 另：全体图标同向同幅的平移是窗口几何变化（不是行内补位），不播动画。
  useLayoutEffect(() => {
    // P1：拖动期间让位 transform 由 DragSession 独占——FLIP 若在此期间剥 transform /
    // 记基线，会把让位中间态当成「自然位」，松手后算出反号 dx 再播一遍（打架）。
    // 整段跳过；松手时 iconUp 已清空 lastLeft 基线，下一轮只重新计量、不播动画。
    if (draggingRef.current) return;
    invoke("debug_note", { msg: `FLIP tick render=${flipsRef.current++}` }).catch(() => {});
    const cur = new Map<string, number>();
    rowRefs.current.forEach((el, id) => {
      if (el.style.transform) {
        el.style.transition = "none";
        el.style.transform = "";
        void el.offsetWidth; // 强制回流，剥过渡立即生效再量（snap 到自然位）
      }
      cur.set(id, el.getBoundingClientRect().left);
    });
    const prev = lastLeft.current;
    lastLeft.current = cur;
    if (!prev.size) return;
    const dxs: Array<[string, number]> = [];
    rowRefs.current.forEach((_el, id) => {
      const old = prev.get(id);
      const now = cur.get(id);
      if (old === undefined || now === undefined) return;
      const dx = old - now;
      if (Math.abs(dx) >= 1) dxs.push([id, dx]);
    });
    if (!dxs.length) return;
    // 全员同幅平移 = fit_dock resize / 窗口重定位引起的整体位移：瞬时完成，不动画
    const first = dxs[0][1];
    if (dxs.every((d) => Math.abs(d[1] - first) < 1)) {
      invoke("debug_note", { msg: `FLIP uniform=${first.toFixed(1)} skipped` }).catch(() => {});
      return;
    }
    for (const [id, dx] of dxs) {
      const el = rowRefs.current.get(id);
      if (!el) continue;
      invoke("debug_note", { msg: `FLIP dx=${dx.toFixed(1)} id=${id}` }).catch(() => {});
      el.style.transition = "none";
      el.style.transform = `translateX(${dx}px)`;
      void el.offsetWidth; // 强制回流，让过渡真正生效
      el.style.transition = "transform 150ms ease";
      el.style.transform = "";
      window.setTimeout(() => {
        el.style.transition = "";
        el.style.transform = "";
      }, 220);
    }
  });

  // P1：让位偏移（拖动中逐图标 translateX）；松手/未拖动时为 null。
  const wayOffsets =
    dragIndex >= 0 && makeWay
      ? makeWayOffsets(dragIndex, makeWay.drop, dockIcons.length)
      : null;

  return (
    <div className={`dock${nightDim ? " night-dim" : ""}`}>
      <div
        className={`dock-icons${dragIndex >= 0 ? " is-dragging" : ""}`}
        ref={pillRef}
      >
        {/* 任务 C：开始菜单入口（⊞），点击 toggle 玻璃面板 */}
        <button
          className="dock-icon dock-start"
          title="开始（玻璃菜单）"
          onMouseDown={() => invoke("menu_toggle").catch(() => {})}
        >
          <span className="dock-start-glyph">⊞</span>
        </button>
        {dockIcons.map((sc, i) => {
          const lifted = dragIndex === i && dragGhost != null;
          const way =
            wayOffsets && !lifted ? offsetToTransform(wayOffsets[i], makeWay!.pitch) : undefined;
          return (
            <button
              key={sc.id}
              data-drag
              ref={(el) => {
                if (el) rowRefs.current.set(sc.id, el);
                else rowRefs.current.delete(sc.id);
              }}
              className={`dock-icon${lifted ? " dock-icon-lift" : ""}`}
              title={sc.name}
              style={
                lifted ? { visibility: "hidden" } : way ? { transform: way } : undefined
              }
              onPointerDown={iconDown(i)}
              onPointerMove={(e) => iconMove(e.clientX, e.clientY, e.pointerId)}
              onPointerUp={(e) => iconUp(e.pointerId)}
              onPointerCancel={(e) => iconUp(e.pointerId)}
              onMouseEnter={enterIcon(sc)}
              onMouseLeave={leaveIcon}
              onClick={() => {
                // 拖动结束后的短暂时间窗内不触发启动（capture 在容器上时
                // 松手 click 通常落在容器，这里主要防老内核兼容路径误触）
                if (Date.now() < suppressClickUntil.current) return;
                launch(sc.path);
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                void remove(sc.id);
              }}
            >
              <IconCanvas icon={sc.icon} />
              {running.has(sc.id) && <RunDot />}
            </button>
          );
        })}
        {/* v0.8：未固定的运行中应用（临时图标）。固定条目右侧、同视觉规格、
            RunDot 常亮（必然在运行）；单击激活、右键固定；**不挂任何拖动手势**，
            拖出/行内排序/悬停预览对临时图标一律不响应（卡面 §1.3-6）。 */}
        {transients.map((t) => (
          <button
            key={t.key}
            className="dock-icon dock-transient"
            title={`${t.name}（运行中，未固定）`}
            onClick={() => activateTransient(t)}
            onContextMenu={(e) => {
              e.preventDefault();
              void pinTransient(t);
            }}
          >
            {t.icon ? <IconCanvas icon={t.icon} /> : <span className="dock-transient-ph" />}
            <RunDot />
          </button>
        ))}
        <button className="dock-icon dock-add" title="添加快捷方式（右键图标为移除）" onClick={add}>
          <span className="dock-add-plus">＋</span>
        </button>
      </div>
      {/* 浮动幽灵：提起的图标跟光标走（fixed，pointer-events none 不挡任何事件） */}
      {dragGhost != null && dragIndex >= 0 && dockIcons[dragIndex] && (
        <div className="drag-ghost" style={{ left: dragGhost.x, top: dragGhost.y }}>
          <IconCanvas icon={dockIcons[dragIndex].icon} />
          <span className="drag-ghost-name">{dockIcons[dragIndex].name}</span>
        </div>
      )}
      {/* v0.7：提示条（游离图标达上限被拒等——不许静默失败） */}
      {notice != null && <div className="dock-notice">{notice}</div>}
    </div>
  );
}

export default App;

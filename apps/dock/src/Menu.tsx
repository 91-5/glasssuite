import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { IconCanvas } from "./icons";

interface IconData {
  w: number;
  h: number;
  rgba: string;
}

interface MenuApp {
  id: string;
  name: string;
  path: string;
  icon: IconData | null;
}

interface MenuSnapshot {
  ready: boolean;
  icons_done: boolean;
  apps: MenuApp[];
  pinned: string[];
}

/** 玻璃开始菜单面板（OC-20261030-003 任务 C；sir 反馈迭代：收藏制）。
 *  普通模式 = menu.json 收藏列表（可右键移除）；⊕ 进入添加模式 = 全量目录点选添加。
 *  展开不等枚举/图标：立即显示 + 占位块，400ms 轮询补齐（只在面板打开时轮询）。
 *  启动走 menu_launch：失败（Err）不关面板、提示可见——修复「点图标没反应」。 */
function MenuApp() {
  const [snap, setSnap] = useState<MenuSnapshot>({ ready: false, icons_done: false, apps: [], pinned: [] });
  const [q, setQ] = useState("");
  const [addMode, setAddMode] = useState(false);
  const [notice, setNotice] = useState("");
  const [powerOpen, setPowerOpen] = useState(false);
  // v0.5：关机/重启二次确认（3s 倒计时，超时自动取消）——sir 误触不能丢工作
  const [confirming, setConfirming] = useState<"shutdown" | "reboot" | null>(null);
  const [countdown, setCountdown] = useState(3);
  const inputRef = useRef<HTMLInputElement>(null);

  // 条目/图标拉取：打开即取一次；枚举或图标未完就 400ms 轮询，完成后停止
  useEffect(() => {
    let stopped = false;
    let timer = 0;
    const tick = () => {
      invoke<MenuSnapshot>("get_menu_apps")
        .then((r) => {
          if (stopped) return;
          setSnap(r);
          if (!r.ready || !r.icons_done) timer = window.setTimeout(tick, 400);
        })
        .catch(() => {
          if (!stopped) timer = window.setTimeout(tick, 400);
        });
    };
    tick();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, []);

  // Esc 收回（面板持焦点才有键盘，无需全局钩子）
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") invoke("menu_hide").catch(() => {});
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, []);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const catalog = useMemo(() => {
    const m = new Map<string, MenuApp>();
    for (const a of snap.apps) m.set(a.path, a);
    return m;
  }, [snap.apps]);

  const pinnedSet = useMemo(() => new Set(snap.pinned), [snap.pinned]);

  const ql = q.toLowerCase();
  const catalogFiltered = snap.apps.filter((a) => a.name.toLowerCase().includes(ql));
  const pinnedApps = snap.pinned
    .map((p) => catalog.get(p) ?? { id: p, name: p, path: p, icon: null })
    .filter((a) => a.name.toLowerCase().includes(ql));

  const flash = (msg: string) => {
    setNotice(msg);
    window.setTimeout(() => setNotice(""), 1800);
  };

  // 二次确认倒计时：3s 内不确认自动取消
  useEffect(() => {
    if (!confirming) return;
    setCountdown(3);
    const iv = window.setInterval(() => {
      setCountdown((c) => {
        if (c <= 1) {
          window.clearInterval(iv);
          setConfirming(null);
          return 0;
        }
        return c - 1;
      });
    }, 1000);
    return () => window.clearInterval(iv);
  }, [confirming]);

  const closePower = () => {
    setPowerOpen(false);
    setConfirming(null);
  };

  const power = (kind: "lock" | "sleep") => {
    invoke(kind === "lock" ? "power_lock" : "power_sleep")
      .then(() => closePower())
      .catch((e) => flash(`操作失败：${e}`));
  };

  const powerConfirm = (reboot: boolean) => {
    invoke("power_shutdown", { reboot })
      .then(() => {
        flash(reboot ? "正在重启…" : "正在关机…");
        closePower();
      })
      .catch((e) => {
        flash(`操作失败：${e}`);
        setConfirming(null);
      });
  };

  const launch = (path: string) => {
    invoke("menu_launch", { path })
      .then(() => invoke("menu_hide").catch(() => {}))
      .catch((e) => {
        console.error("launch failed", e);
        flash(`启动失败：${e}`);
      });
  };

  const add = (path: string) => {
    invoke<string[]>("menu_add", { path })
      .then((pinned) => setSnap((s) => ({ ...s, pinned })))
      .catch((e) => flash(`添加失败：${e}`));
  };

  const remove = (path: string) => {
    invoke<string[]>("menu_remove", { path })
      .then((pinned) => setSnap((s) => ({ ...s, pinned })))
      .catch((e) => flash(`移除失败：${e}`));
  };

  const list = addMode ? catalogFiltered : pinnedApps;

  return (
    <div className="menu-root">
      <div className="menu-search-row">
        <input
          ref={inputRef}
          className="menu-search"
          placeholder={addMode ? "搜索要添加的应用" : "搜索应用"}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          spellCheck={false}
        />
        <button
          className="menu-add-btn"
          title={addMode ? "完成添加" : "添加 / 移除应用"}
          onMouseDown={() => setAddMode((m) => !m)}
        >
          {addMode ? "✓" : "＋"}
        </button>
        <button
          className="menu-add-btn menu-power-btn"
          title="电源"
          onMouseDown={() => {
            setPowerOpen((o) => !o);
            setConfirming(null);
          }}
        >
          ⏻
        </button>
      </div>
      {notice && <div className="menu-notice">{notice}</div>}
      {powerOpen && (
        <div className="menu-power">
          <div className="menu-power-row">
            <button className="menu-power-opt" title="锁定（LockWorkStation）" onMouseDown={() => power("lock")}>
              🔒<span>锁定</span>
            </button>
            <button className="menu-power-opt" title="睡眠（SetSuspendState）" onMouseDown={() => power("sleep")}>
              🌙<span>睡眠</span>
            </button>
            <button
              className="menu-power-opt menu-power-danger"
              title="关机（需二次确认）"
              onMouseDown={() => setConfirming(confirming === "shutdown" ? null : "shutdown")}
            >
              ⏻<span>关机</span>
            </button>
            <button
              className="menu-power-opt menu-power-danger"
              title="重启（需二次确认）"
              onMouseDown={() => setConfirming(confirming === "reboot" ? null : "reboot")}
            >
              ↻<span>重启</span>
            </button>
          </div>
          {confirming && (
            <div className="menu-power-confirm">
              <span className="menu-power-confirm-text">
                确认{confirming === "shutdown" ? "关机" : "重启"}？（{countdown}s 后自动取消）
              </span>
              <button
                className="menu-power-cf menu-power-cf-go"
                onMouseDown={() => powerConfirm(confirming === "shutdown" ? false : true)}
              >
                确认{confirming === "shutdown" ? "关机" : "重启"}
              </button>
              <button className="menu-power-cf" onMouseDown={() => setConfirming(null)}>
                取消
              </button>
            </div>
          )}
        </div>
      )}
      <div className="menu-grid">
        {!snap.ready && <div className="menu-hint">正在读取应用列表…</div>}
        {snap.ready && list.length === 0 && (
          <div className="menu-hint">
            {addMode
              ? `没有匹配「${q}」的应用`
              : snap.pinned.length === 0
                ? "收藏是空的——点右上 ＋ 添加应用"
                : `没有匹配「${q}」的应用`}
          </div>
        )}
        {list.map((a) => {
          const isPinned = pinnedSet.has(a.path);
          return (
            <button
              key={a.id}
              className={`menu-item${addMode && isPinned ? " menu-item-in" : ""}`}
              title={addMode && isPinned ? "已收藏（右键移除）" : a.name}
              onMouseDown={() => {
                if (addMode) {
                  if (isPinned) remove(a.path);
                  else add(a.path);
                } else if (isPinned) {
                  launch(a.path);
                }
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                if (isPinned) remove(a.path);
              }}
            >
              {a.icon ? (
                <IconCanvas icon={a.icon} className="menu-icon" />
              ) : (
                <span className="menu-icon menu-icon-ph" aria-hidden />
              )}
              <span className="menu-name">{a.name}</span>
              {addMode && isPinned && <span className="menu-in-badge">已收藏</span>}
            </button>
          );
        })}
      </div>
    </div>
  );
}

export default MenuApp;

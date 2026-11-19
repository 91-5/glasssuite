import { useCallback, useEffect, useState } from "react";
import {
  controlGetState,
  controlSetDock,
  controlSetIcons,
  controlSetIsland,
  controlSetTaskbar,
  type ControlSettings,
} from "@glasssuite/ipc-types";

// OC-20261118-002：四开关接真实 IPC（写 settings.json + 生效）。
// 语义：island/dock 是「开」，icons/taskbar 是「隐藏」（on=隐藏）。
type Key = "island" | "dock" | "icons" | "taskbar";

const ROWS: { key: Key; title: string; subtitle: string }[] = [
  { key: "island", title: "灵动岛", subtitle: "顶部信息面板" },
  { key: "dock", title: "桌面 Dock", subtitle: "底部图标栏" },
  { key: "icons", title: "桌面原生图标", subtitle: "隐藏后桌面只留岛与 Dock" },
  { key: "taskbar", title: "系统任务栏", subtitle: "隐藏后底部任务栏消失" },
];

function Switch({ on, busy, onClick }: { on: boolean; busy: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      disabled={busy}
      onClick={onClick}
      className={`switch ${on ? "on" : "off"} ${busy ? "busy" : ""}`}
    >
      <span className="knob" />
    </button>
  );
}

function SwitchState(s: ControlSettings): Record<Key, boolean> {
  return {
    island: s.islandEnabled,
    dock: s.dockEnabled,
    icons: s.desktopIconsHidden,
    taskbar: s.taskbarHidden,
  };
}

export default function App() {
  const [state, setState] = useState<ControlSettings | null>(null);
  const [busy, setBusy] = useState<Key | null>(null);
  const [error, setError] = useState<string | null>(null);

  // 首次加载：从后端读真实状态（非乐观），失败时给出可见错误而非假状态。
  useEffect(() => {
    controlGetState()
      .then(setState)
      .catch((e) => setError(`读取状态失败：${String(e)}`));
  }, []);

  const apply = useCallback(async (key: Key, value: boolean) => {
    if (!state || busy) return;
    setBusy(key);
    setError(null);
    // 乐观更新，失败回滚 + 显示错误。
    const prev = state;
    setState({ ...state, ...({ [key]: value } as Partial<ControlSettings>) });
    try {
      let next: ControlSettings;
      switch (key) {
        case "island": next = await controlSetIsland(value); break;
        case "dock": next = await controlSetDock(value); break;
        case "icons": next = await controlSetIcons(value); break;
        case "taskbar": next = await controlSetTaskbar(value); break;
      }
      setState(next); // 以后端回执为准
    } catch (e) {
      setState(prev);
      setError(`切换失败：${String(e)}`);
    } finally {
      setBusy(null);
    }
  }, [state, busy]);

  // 全部恢复默认 = 产品默认沉浸态（与 ControlSettings::default 一致）：
  // 岛/Dock 开，图标/任务栏隐藏。想回系统原生请逐项关或点窗口 X 退出整个应用。
  const restoreAll = useCallback(async () => {
    setError(null);
    setBusy("island");
    try {
      let s = await controlSetIsland(true);
      s = await controlSetDock(true);
      s = await controlSetIcons(true);
      s = await controlSetTaskbar(true);
      setState(s);
    } catch (e) {
      setError(`恢复默认失败：${String(e)}`);
      controlGetState().then(setState).catch(() => {});
    } finally {
      setBusy(null);
    }
  }, []);

  const values = state ? SwitchState(state) : null;

  return (
    <div className="app">
      <header className="head">
        <div className="brand">
          <span className="dot" />
          GlassSuite 控制中心
        </div>
        <div className="status">运行中</div>
      </header>

      <main className="rows">
        {ROWS.map((r) => (
          <div className="row" key={r.key}>
            <div className="meta">
              <div className="title">{r.title}</div>
              <div className="sub">{r.subtitle}</div>
            </div>
            <Switch
              on={values ? values[r.key] : false}
              busy={busy === r.key}
              onClick={() => apply(r.key, values ? !values[r.key] : false)}
            />
          </div>
        ))}
      </main>

      <footer className="foot">
        <button type="button" className="reset" onClick={restoreAll} disabled={busy !== null}>
          全部恢复默认
        </button>
        <div className={`hint ${error ? "err" : ""}`}>
          {error ?? (state ? "点击开关即时生效" : "正在读取状态…")}
        </div>
      </footer>
    </div>
  );
}

import { useCallback, useEffect, useState } from "react";
import {
  controlGetState,
  controlHotkeyStatus,
  controlLiveStatus,
  controlSetDock,
  controlSetIcons,
  controlSetIsland,
  controlSetTaskbar,
  type ControlLiveStatus,
  type ControlSettings,
} from "@glasssuite/ipc-types";

// OC-20261118-002：四开关接真实 IPC（写 settings.json + 生效）。
// 语义：island/dock 是「开」，icons/taskbar 是「隐藏」（on=隐藏）。
// OC-20261118-003：加热键状态 + 实况回显。OC-20261121-004：裁撤冗余的全部恢复按钮。
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

/** 意图 vs 实况是否背离（守卫没跑 / 权限不足时会出现）。 */
function driftMessage(intent: ControlSettings, live: ControlLiveStatus | null): string | null {
  if (!live) return null;
  const bits: string[] = [];
  if (intent.taskbarHidden === live.taskbarVisible) {
    bits.push(live.taskbarVisible ? "任务栏仍可见" : "任务栏已隐藏");
  }
  if (intent.desktopIconsHidden !== live.iconsHidden) {
    bits.push(live.iconsHidden ? "图标已隐藏" : "图标仍可见");
  }
  if (intent.islandEnabled !== live.islandVisible) {
    bits.push(live.islandVisible ? "岛已显示" : "岛未显示");
  }
  if (intent.dockEnabled !== live.dockVisible) {
    bits.push(live.dockVisible ? "Dock 已显示" : "Dock 未显示");
  }
  return bits.length ? `守卫未运行/权限不足，实际：${bits.join("、")}` : null;
}

export default function App() {
  const [state, setState] = useState<ControlSettings | null>(null);
  const [live, setLive] = useState<ControlLiveStatus | null>(null);
  const [hotkeyOk, setHotkeyOk] = useState<boolean | null>(null);
  const [busy, setBusy] = useState<Key | null>(null);
  const [error, setError] = useState<string | null>(null);

  // 首次加载：读真实状态 + 实况 + 热键注册状态。
  useEffect(() => {
    controlGetState()
      .then(setState)
      .catch((e) => setError(`读取状态失败：${String(e)}`));
    controlLiveStatus().then(setLive).catch(() => {});
    controlHotkeyStatus().then(setHotkeyOk).catch(() => {});
  }, []);

  // 切换后 2s 用实况校验（卡 §2.3）：守卫是否真的执行了意图。
  useEffect(() => {
    if (busy !== null) return;
    const t = window.setTimeout(() => {
      controlLiveStatus().then(setLive).catch(() => {});
    }, 2000);
    return () => window.clearTimeout(t);
  }, [state, busy]);

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

  const values = state ? SwitchState(state) : null;
  const drift = state ? driftMessage(state, live) : null;
  const hotkeyWarn = hotkeyOk === false;

  return (
    <div className="app">
      <header className="head">
        <div className="brand">
          <span className="dot" />
          GlassSuite 控制中心
        </div>
        <div className="status">运行中</div>
      </header>

      {hotkeyWarn && (
        <div className="warn" role="alert">
          逃生舱不可用（逃生热键注册失败），请勿同时隐藏任务栏和桌面图标
        </div>
      )}
      {drift && <div className="warn" role="alert">{drift}</div>}

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
        <div className={`hint ${error || drift ? "err" : ""}`}>
          {error ?? (state ? "点击开关即时生效" : "正在读取状态…")}
        </div>
      </footer>
    </div>
  );
}

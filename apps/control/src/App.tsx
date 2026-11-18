import { useState } from "react";

// OC-20261118-001 首版：四开关 UI 骨架 + 本地 state（乐观更新）。
// 真实 IPC / 持久化 / 生效由 OC-20261118-002 接入（届时把 set 换成 invoke）。
type Toggle = {
  key: string;
  title: string;
  subtitle: string;
  on: boolean;
};

const INITIAL: Toggle[] = [
  { key: "island", title: "灵动岛", subtitle: "顶部信息面板", on: true },
  { key: "dock", title: "桌面 Dock", subtitle: "底部图标栏", on: true },
  { key: "icons", title: "桌面原生图标", subtitle: "隐藏后桌面只留岛与 Dock", on: false }, // on=隐藏
  { key: "taskbar", title: "系统任务栏", subtitle: "隐藏后底部任务栏消失", on: false }, // on=隐藏
];

function Switch({ on, onClick }: { on: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      onClick={onClick}
      className={`switch ${on ? "on" : "off"}`}
    >
      <span className="knob" />
    </button>
  );
}

export default function App() {
  const [rows, setRows] = useState<Toggle[]>(INITIAL);
  const toggle = (key: string) =>
    setRows((prev) => prev.map((r) => (r.key === key ? { ...r, on: !r.on } : r)));

  const toggleAll = () =>
    setRows([
      { ...INITIAL[0], on: true },
      { ...INITIAL[1], on: true },
      { ...INITIAL[2], on: true },
      { ...INITIAL[3], on: true },
    ]);

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
        {rows.map((r) => (
          <div className="row" key={r.key}>
            <div className="meta">
              <div className="title">{r.title}</div>
              <div className="sub">{r.subtitle}</div>
            </div>
            <Switch on={r.on} onClick={() => toggle(r.key)} />
          </div>
        ))}
      </main>

      <footer className="foot">
        <button type="button" className="reset" onClick={toggleAll}>
          全部恢复默认
        </button>
        <div className="hint">四开关生效逻辑将在下一版本接入</div>
      </footer>
    </div>
  );
}

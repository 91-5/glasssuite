/**
 * 开始菜单面板（第三个窗口，`dock/index.html#startmenu`）。
 *
 * 只列本机「开始菜单 / 程序」里的快捷方式与迁移来的收藏，不联网、不读云同步。
 * 面板尺寸固定 = 窗口尺寸，走的是和 island 展开面板同一套 resize 模型。
 */
import { useEffect, useState } from "react";
import {
  dockAutoHide,
  dockLaunchPath,
  dockMenuApps,
  dockMenuFavourites,
  dockMenuToggle,
  getDockState,
  type MenuApp,
} from "@glasssuite/ipc-types";

export default function StartMenu() {
  const [apps, setApps] = useState<MenuApp[]>([]);
  const [favourites, setFavourites] = useState<MenuApp[]>([]);
  const [autoHide, setAutoHide] = useState(false);

  useEffect(() => {
    dockMenuApps().then(setApps).catch(() => {});
    dockMenuFavourites().then(setFavourites).catch(() => {});
    getDockState()
      .then((s) => setAutoHide(s.autoHide))
      .catch(() => {});
  }, []);

  const launch = (path: string) => {
    dockLaunchPath(path).catch(() => {});
    dockMenuToggle(false).catch(() => {});
  };

  const toggleAutoHide = async () => {
    const next = !autoHide;
    setAutoHide(next);
    // 后端会补发一份快照，dock 前端据此更新自动隐藏设置（不需要另开事件）。
    await dockAutoHide(next).catch(() => {});
  };

  return (
    <div className="menu-panel gs-glass">
      <header className="menu-head">
        <span>开始菜单</span>
        <button
          type="button"
          className="gs-icon-button menu-close"
          onClick={() => dockMenuToggle(false).catch(() => {})}
          aria-label="关闭"
        >
          ×
        </button>
      </header>

      {favourites.length > 0 ? (
        <section className="menu-section">
          <h3>收藏（旧配置迁移）</h3>
          <ul className="menu-list">
            {favourites.map((a) => (
              <li key={`fav-${a.path}`}>
                <button type="button" onClick={() => launch(a.path)}>
                  {a.name}
                </button>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <section className="menu-section menu-section--grow">
        <h3>所有应用</h3>
        <ul className="menu-list">
          {apps.map((a) => (
            <li key={a.path}>
              <button type="button" onClick={() => launch(a.path)}>
                {a.name}
              </button>
            </li>
          ))}
          {apps.length === 0 ? <li className="menu-empty">没有找到快捷方式</li> : null}
        </ul>
      </section>

      <footer className="menu-foot">
        <label>
          <input
            type="checkbox"
            checked={autoHide}
            onChange={() => {
              toggleAutoHide().catch(() => {});
            }}
          />
          自动隐藏 dock
        </label>
        <span title="纯本地读取，不上报">本地</span>
      </footer>
    </div>
  );
}

import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import MenuApp from "./Menu";
import "./dock.css";

// 双窗口共用一份 index.html：dock 窗口渲染药丸，startmenu 窗口渲染开始菜单面板
const label = getCurrentWindow().label;
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{label === "startmenu" ? <MenuApp /> : <App />}</React.StrictMode>
);

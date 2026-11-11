import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
// 岛的外观来自 glasspill 的整套玻璃语言（993 行），与 shared-ui 的暗色 token 是两套视觉，
// 这里以 glasspill 为准 —— 复用它的样式才能和用户正在用的岛一模一样。
import "./styles/glass.css";
// island.css 保留：只承载 GlassSuite 增量的设置面板样式（.panel-* / .island-settings），
// 与 glass.css 的选择器无重叠。
import "./island.css";
import App from "./App";

const root = document.getElementById("root");
if (!root) throw new Error("island: #root 缺失");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@glasssuite/shared-ui/glass.css";
import "./dock.css";
import App from "./App";

const root = document.getElementById("root");
if (!root) throw new Error("dock: #root 缺失");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

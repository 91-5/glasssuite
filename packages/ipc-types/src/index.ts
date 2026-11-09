/**
 * GlassSuite IPC 契约。
 *
 * 纪律：前端只认这里导出的名字与类型，Rust 侧的 `#[tauri::command]` / `emit` 必须与本文件
 * 逐字一致。改名字等于破坏契约，两侧同一次提交内一起改。
 */

/** 窗口 label（与 `tauri.conf.json` / 运行时 builder 中的 label 一致）。 */
export const WINDOW_LABELS = {
  island: "island",
  dock: "dock",
  startmenu: "startmenu",
} as const;

export type WindowLabel = (typeof WINDOW_LABELS)[keyof typeof WINDOW_LABELS];

/** 前端 → 后端命令名。 */
export const COMMANDS = {
  /** 上报 dock 药丸实际尺寸，窗口收缩到内容大小（resize 与居中重定位原子完成）。 */
  fitDock: "fit_dock",
  /** 上报岛药丸实际尺寸（同上）。 */
  fitIsland: "fit_island",
} as const;

/** 后端 → 前端事件名。 */
export const EVENTS = {
  /** 运行中应用集合变化（增量或全量由 payload 的 `full` 标记）。 */
  runningChanged: "running://changed",
} as const;

/** `fit_dock` / `fit_island` 入参（CSS 逻辑像素）。 */
export interface FitRequest {
  w: number;
  h: number;
}

/** 运行状态快照：固定图标集合 + 临时（未固定但正在运行）图标集合。 */
export interface RunningState {
  /** 已固定的稳定 key 列表。 */
  pinned: string[];
  /** 临时图标 key 列表（退出即消失）。 */
  transients: string[];
}

/** 一条可启动项（开始菜单 / dock 图标共用）。 */
export interface AppEntry {
  /** 稳定标识：exe 路径哈希或 AUMID，两者互斥。 */
  key: string;
  name: string;
  /** 图标由系统 API 运行时取得，前端只拿 data URL，不打包他人图标资源。 */
  iconDataUrl?: string;
}

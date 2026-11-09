/**
 * GlassSuite IPC 契约。
 *
 * 纪律：前端只认这里导出的名字与类型，Rust 侧的 `#[tauri::command]` / `emit` 必须与本文件
 * 逐字一致。改名字等于破坏契约，两侧同一次提交内一起改。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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
  /** 读取用户配置（只读；写配置走 settings.json 文件）。 */
  settingsGet: "settings_get",
  /** 前端诊断日志。 */
  logFrontend: "log_frontend",
  /** 媒体控制：play | pause | play_pause | next | prev | seek。 */
  mediaControl: "media_control",
  /** 取最近一次 SMTC 快照（窗口重建 / 首帧补齐）。 */
  mediaState: "media_state",
  /** 上报交互热区（CSS 逻辑像素，相对窗口客户区原点）。 */
  setHotzone: "set_hotzone",
  /** 展开 / 收起岛面板（用 resize 实现，不引入固定尺寸热区窗）。 */
  expandPanel: "expand_panel",
  collapsePanel: "collapse_panel",
} as const;

/** 后端 → 前端事件名。 */
export const EVENTS = {
  /** 运行中应用集合变化（增量或全量由 payload 的 `full` 标记）。 */
  runningChanged: "running://changed",
  /** 系统指标（1s 一次）。 */
  sysMetrics: "sys-metrics",
  /** 天气（15min 一次；未配置时永不触发）。 */
  weather: "weather",
  /** 网络状态（30s 一次；未配置时永不触发）。 */
  networkStatus: "network-status",
  /** SMTC 媒体快照（事件驱动）。 */
  mediaState: "media-state",
  /** 系统通知（新通知到达）。 */
  notification: "notification",
  /** 歌词行（随播放进度推进）。 */
  lyrics: "lyrics",
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

/**
 * 上报 dock 内容尺寸，窗口收缩并对齐任务栏。
 *
 * 为什么包成函数而不是让组件直接 `invoke("fit_dock")`：命令名是契约的一部分，
 * 拼错字符串在运行时只会静默失败（窗口保持旧尺寸），集中在这里可以一处改、两处用。
 */
export async function fitDock(req: FitRequest): Promise<void> {
  await invoke(COMMANDS.fitDock, { ...req });
}

/** 上报岛内容尺寸，窗口收缩并对齐屏幕上沿。 */
export async function fitIsland(req: FitRequest): Promise<void> {
  await invoke(COMMANDS.fitIsland, { ...req });
}

// ---------------------------------------------------------------------------
// 用户配置
// ---------------------------------------------------------------------------

/** 天气配置（用户在 `settings.json` 里自己填坐标）。 */
export interface WeatherConfig {
  latitude: number;
  longitude: number;
  /** IANA 时区名；缺省按系统本地时间。 */
  timezone?: string | null;
}

/**
 * 用户配置。所有功能默认关闭：`null` = 不联网、不读隐私数据。
 * 与 Rust 侧 `glasssuite::settings::Settings` 逐字段对应。
 */
export interface Settings {
  weather?: WeatherConfig | null;
  networkProbeUrl?: string | null;
  aiBaseUrl?: string | null;
  aiApiKey?: string | null;
  aiModel?: string | null;
  /** 歌词源地址；null = 关闭（默认不联网）。 */
  lyricsSourceUrl?: string | null;
  /** 系统通知摘要读取，默认关闭。 */
  notifications?: { enabled: boolean };
  dock?: { autoHide: boolean };
}

export async function getSettings(): Promise<Settings> {
  return invoke<Settings>(COMMANDS.settingsGet);
}

/** 前端诊断日志（进 Rust 侧日志文件，便于排查「后端有数据但界面没反应」）。 */
export async function logFrontend(msg: string): Promise<void> {
  await invoke(COMMANDS.logFrontend, { msg });
}

// ---------------------------------------------------------------------------
// 后端事件订阅
// ---------------------------------------------------------------------------

/** 系统指标快照。 */
export interface SysMetrics {
  cpu: number;
  memUsed: number;
  memTotal: number;
  memPct: number;
  battery: { charge: number; state: "charging" | "discharging" } | null;
  gpu: number | null;
}

/** 天气快照（归一化后的最小集合）。 */
export interface WeatherSnapshot {
  tempC: number;
  /** WMO 天气代码。 */
  code: number;
  isDay: boolean;
}

/** 网络探测结果。 */
export interface NetworkStatus {
  online: boolean;
  latency: number;
}

/** SMTC 媒体快照。`null` 表示当前没有可控制媒体。 */
export interface MediaState {
  title: string;
  artist: string;
  album?: string | null;
  /** 封面 data URL（由 Rust 侧从缩略图转码）。 */
  artwork?: string | null;
  appId?: string | null;
  status?: string | null;
  playing: boolean;
  /** 收到该快照时播放器报告的进度（毫秒）。 */
  positionMs: number;
  /** 播放器报告 positionMs 的时刻（Unix 毫秒，FILETIME 换算）。 */
  baseUnixMs: number;
  /** 播放速率，通常为 1。 */
  rate: number;
  /** 总时长（毫秒）；0 表示未知（直播流）。 */
  durationMs: number;
  canPlay: boolean;
  canPause: boolean;
  canNext: boolean;
  canPrev: boolean;
}

export type MediaAction = "play" | "pause" | "play_pause" | "next" | "prev" | "seek";

/**
 * 系统通知**摘要条**。
 *
 * 刻意没有 `body`：通知正文不进入前端、不落盘、不进日志，原文由 Windows 横幅展示。
 * `source` 只标识事件来源类型（wpndatabase / user-notification-listener / uia-fallback）。
 */
export interface NotificationItem {
  app: string;
  /** 已按字符截断（后端 60 字，前端再做 CSS 省略）。 */
  title: string;
  /** 到达时间（Unix 毫秒）。 */
  arrivedAt: number;
  source: string;
}

/**
 * 订阅一个后端事件。返回取消订阅函数（`null` 表示非 Tauri 环境）。
 *
 * 为什么包一层：`listen` 是异步的，组件在 `useEffect` 清理时往往还没拿到 unlisten，
 * 直接写 `listen(...)` 会漏掉清理导致重复订阅；这里把「取消订阅前就已请求取消」
 * 的情况处理掉，调用方只管 `unlisten?.()`。
 */
export function subscribe<T>(
  event: string,
  cb: (payload: T) => void,
): () => void {
  let unlisten: UnlistenFn | null = null;
  let cancelled = false;

  listen<T>(event, (e) => cb(e.payload))
    .then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    })
    .catch(() => {
      /* 非 Tauri 环境（浏览器里调布局）：忽略 */
    });

  return () => {
    cancelled = true;
    unlisten?.();
  };
}

// ---------------------------------------------------------------------------
// 媒体（SMTC，事件驱动）
// ---------------------------------------------------------------------------

/** 取最近一次媒体快照；无媒体时为 `null`（前端据此回落时钟）。 */
export async function getMediaState(): Promise<MediaState | null> {
  return invoke<MediaState | null>(COMMANDS.mediaState);
}

/**
 * 下发媒体控制指令。`seek` 需要额外传 `positionMs`。
 *
 * 指令进入 SMTC 专用线程串行执行，调用方不等待 WinRT 完成——播放器回报滞后由后端
 * 延迟补一次 Refresh 兜住，前端随后会收到新的 `media-state`。
 */
export async function mediaControl(action: MediaAction, positionMs?: number): Promise<void> {
  await invoke(COMMANDS.mediaControl, { action, positionMs });
}

/** 上报当前窗口的交互热区（内容尺寸变化时重新上报）。 */
export async function setHotzone(
  x: number,
  y: number,
  w: number,
  h: number,
  target: "island" | "dock" = "island",
): Promise<void> {
  await invoke(COMMANDS.setHotzone, { x, y, w, h, target });
}

/** 展开面板（resize 到面板尺寸并取消穿透）。 */
export async function expandPanel(): Promise<void> {
  await invoke(COMMANDS.expandPanel);
}

/** 收起面板（resize 回内容尺寸并恢复穿透）。 */
export async function collapsePanel(): Promise<void> {
  await invoke(COMMANDS.collapsePanel);
}

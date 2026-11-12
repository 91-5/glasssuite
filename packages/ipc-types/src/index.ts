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
  /** 读取用户配置（只读）。 */
  settingsGet: "settings_get",
  /** 保存用户配置：写回 settings.json + 应用运行时（AI / 网络探测热加载）。 */
  settingsUpdate: "settings_update",
  /** 显式触发 AI 配置重载（幂等；settings_update 已应用，这里是兜底）。返回是否已配齐。 */
  aiReload: "ai_reload",
  /** 「测试连接」：向用户配置的端点发一次 GET /models。 */
  aiTestConnection: "ai_test_connection",
  /** 前端诊断日志。 */
  logFrontend: "log_frontend",
  /** 媒体控制：play | pause | play_pause | next | prev | seek。 */
  mediaControl: "media_control",
  /** 取最近一次 SMTC 快照（窗口重建 / 首帧补齐）。 */
  mediaState: "media_state",
  /** 取当前歌词快照（前端挂载补齐，规避 emit 早于订阅）。 */
  lyricsState: "lyrics_state",
  /** 上报交互热区（CSS 逻辑像素，相对窗口客户区原点）。 */
  setHotzone: "set_hotzone",
  /** 展开 / 收起岛面板（用 resize 实现，不引入固定尺寸热区窗）。 */
  expandPanel: "expand_panel",
  collapsePanel: "collapse_panel",
  /** dock 全量快照（固定 / 临时 / 游离 + 内容指纹）。 */
  dockState: "dock_state",
  /** 按需取一枚本机系统图标（带超时，失败返回 null）。 */
  dockIcon: "dock_icon",
  /** 启动 dock 上的应用（按稳定 key）。 */
  dockLaunch: "dock_launch",
  /** 启动开始菜单里的快捷方式（按路径）。 */
  dockLaunchPath: "dock_launch_path",
  /** 拖拽排序后持久化。 */
  dockReorder: "dock_reorder",
  /** 自动隐藏开关。 */
  dockAutoHide: "dock_auto_hide",
  /** 贴底唤出/收起时同步鼠标穿透。 */
  dockSetRevealed: "dock_set_revealed",
  /** 把固定项拖成游离图标。 */
  dockSetFree: "dock_set_free",
  /** 游离图标拖回 dock。 */
  dockUndockFree: "dock_undock_free",
  /** 移动游离图标。 */
  dockMoveFree: "dock_move_free",
  /** 开始菜单：本机快捷方式列表。 */
  dockMenuApps: "dock_menu_apps",
  /** 开始菜单：收藏（旧 menu.json 迁来）。 */
  dockMenuFavourites: "dock_menu_favourites",
  /** 开始菜单开合。 */
  dockMenuToggle: "dock_menu_toggle",
  /** 悬停注册 DWM 缩略图；false = 已降级。 */
  dockPreviewShow: "dock_preview_show",
  /** 注销 DWM 缩略图。 */
  dockPreviewHide: "dock_preview_hide",
} as const;

/** 后端 → 前端事件名。 */
export const EVENTS = {
  /** 运行中应用集合变化（增量或全量由 payload 的 `full` 标记）。 */
  runningChanged: "dock-running",
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
  /** 歌词行（整曲一次下发，前端按 positionMs 选行）。 */
  lyrics: "lyrics",
  /** AI / 本地规则产生的一行洞察（默认 AI 关闭时永不触发）。 */
  aiInsight: "ai-insight",
  /** 光标进入/离开岛热区（Rust 侧 LL 钩子判定，非 DOM 事件）。 */
  islandHover: "island-hover",
  /** dock 贴底唤出 / 收起（与岛共用同一个 LL 钩子）。 */
  dockReveal: "dock-reveal",
  /** 展开面板开/关（后端 resize 完成后才下发）。 */
  panelState: "panel-state",
  /** 全屏让位状态变化（所有窗同时显隐）。 */
  fullscreenYield: "fullscreen-yield",
  /** DWM 预览不可用/超时：前端显示纯色块。 */
  dockPreviewFallback: "dock-preview-fallback",
  /** 开始菜单开合状态（后端 show/hide 完成后下发）。 */
  dockMenuState: "dock-menu-state",
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
  /** 网络探测地址；null/空白 = 关闭（默认不联网）。 */
  networkProbeUrl?: string | null;
  /** AI 端点（OpenAI 兼容）；null = 未配置（零外联）。 */
  aiBaseUrl?: string | null;
  /** AI 密钥（用户自填时保存到本机 settings.json；也可用环境变量）。 */
  aiApiKey?: string | null;
  /** AI 模型名；null = 后端默认。 */
  aiModel?: string | null;
  /** AI 总开关；null/缺省 = 开（与基线一致），false = 用户显式关闭。 */
  aiEnabled?: boolean | null;
  /** 歌词源地址；null = 关闭（默认不联网）。 */
  lyricsSourceUrl?: string | null;
  /** 系统通知摘要读取，默认关闭。 */
  notifications?: { enabled: boolean; uiaFallback?: boolean };
  dock?: { autoHide: boolean };
}

export async function getSettings(): Promise<Settings> {
  return invoke<Settings>(COMMANDS.settingsGet);
}

/**
 * 保存用户配置。后端一次调用完成「写 settings.json → 更新内存态 → 应用运行时」，
 * AI 与网络探测随之热加载（无需重启）。写盘失败时抛错且后端不应用内存态。
 */
export async function updateSettings(next: Settings): Promise<Settings> {
  return invoke<Settings>(COMMANDS.settingsUpdate, { next });
}

/**
 * 显式触发 AI 配置重载（幂等兜底，卡片 D2 的「保存 → AI reload」链路）。
 * 返回后端视角「AI 是否已配齐」（enabled + base_url + key）。
 */
export async function aiReload(): Promise<boolean> {
  return invoke<boolean>(COMMANDS.aiReload);
}

/** 「测试连接」结果：ok=端点可达（含 4xx——端点活着但密钥/参数有问题）。 */
export interface AiTestResult {
  ok: boolean;
  status: number;
  detail: string;
}

/**
 * 显式向用户配置的端点发一次 GET {base}/models（OpenAI 兼容标准端点），
 * 让「我填的 API 到底通没通」有可见反馈。仅在按钮点击时触发，无轮询。
 */
export async function aiTestConnection(): Promise<AiTestResult> {
  return invoke<AiTestResult>(COMMANDS.aiTestConnection);
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

/** 展开面板尺寸（逻辑像素）。必须与 Rust `window::panel` 的 PANEL_W/PANEL_H 一致。 */
export const PANEL_SIZE = { w: 320, h: 268 } as const;

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

// ---------------------------------------------------------------------------
// 歌词 / AI
// ---------------------------------------------------------------------------

export interface LyricLine {
  /** 起始时间（毫秒，相对曲首）。 */
  t: number;
  text: string;
}

/** `null` = 未配置歌词源或未匹配到歌词（负缓存）。 */
export interface LyricsState {
  lines: LyricLine[];
}

export interface AiInsight {
  kind: string;
  text: string;
  source: "ai" | "local";
  at: number;
}

/** 取当前歌词快照（前端挂载补齐）。 */
export async function getLyricsState(): Promise<LyricsState | null> {
  return invoke<LyricsState | null>(COMMANDS.lyricsState);
}

/** 展开面板（resize 到面板尺寸并取消穿透）。 */
export async function expandPanel(): Promise<void> {
  await invoke(COMMANDS.expandPanel);
}

/** 收起面板（resize 回内容尺寸并恢复穿透）。 */
export async function collapsePanel(): Promise<void> {
  await invoke(COMMANDS.collapsePanel);
}

// ---------------------------------------------------------------------------
// dock（P4）
// ---------------------------------------------------------------------------

/** 游离图标的屏幕矩形（CSS 逻辑像素）。 */
export interface FreeRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** dock 上的一个图标。`icon` 为 null 时用前端自绘 SVG 占位。 */
export interface DockEntry {
  /** `exe:<规范化路径>`，跨固定 / 临时 / 预览三处稳定。 */
  key: string;
  /** 固定项的配置 id；临时项等于 key。 */
  id: string;
  name: string;
  path: string;
  /** 本机 Shell API 运行时提取的 data URL，绝不随包分发第三方图标。 */
  icon: string | null;
  running: boolean;
  pinned: boolean;
  freeRect: FreeRect | null;
}

export interface DockSnapshot {
  pinned: DockEntry[];
  transients: DockEntry[];
  free: DockEntry[];
  autoHide: boolean;
  /** 内容指纹：Rust watcher 与前端各比一次，不变就不重排。 */
  fingerprint: string;
}

export interface MenuApp {
  name: string;
  path: string;
}

/** 空快照，用于非 Tauri 环境（浏览器里调布局）。 */
export const EMPTY_DOCK: DockSnapshot = {
  pinned: [],
  transients: [],
  free: [],
  autoHide: false,
  fingerprint: "",
};

export async function getDockState(): Promise<DockSnapshot> {
  return invoke<DockSnapshot>(COMMANDS.dockState);
}

export async function getDockIcon(key: string): Promise<string | null> {
  return invoke<string | null>(COMMANDS.dockIcon, { key });
}

export async function dockLaunch(key: string): Promise<boolean> {
  return invoke<boolean>(COMMANDS.dockLaunch, { key });
}

export async function dockLaunchPath(path: string): Promise<boolean> {
  return invoke<boolean>(COMMANDS.dockLaunchPath, { path });
}

export async function dockReorder(paths: string[]): Promise<void> {
  await invoke(COMMANDS.dockReorder, { paths });
}

export async function dockAutoHide(enabled: boolean): Promise<void> {
  await invoke(COMMANDS.dockAutoHide, { enabled });
}

export async function dockSetRevealed(revealed: boolean): Promise<void> {
  await invoke(COMMANDS.dockSetRevealed, { revealed });
}

export async function dockSetFree(id: string, x: number, y: number): Promise<void> {
  await invoke(COMMANDS.dockSetFree, { id, x, y });
}

export async function dockUndockFree(id: string): Promise<void> {
  await invoke(COMMANDS.dockUndockFree, { id });
}

export async function dockMoveFree(id: string, x: number, y: number): Promise<void> {
  await invoke(COMMANDS.dockMoveFree, { id, x, y });
}

export async function dockMenuApps(): Promise<MenuApp[]> {
  return invoke<MenuApp[]>(COMMANDS.dockMenuApps);
}

export async function dockMenuFavourites(): Promise<MenuApp[]> {
  return invoke<MenuApp[]>(COMMANDS.dockMenuFavourites);
}

/** `open` 省略时切换。返回切换后的状态。 */
export async function dockMenuToggle(open?: boolean): Promise<boolean> {
  return invoke<boolean>(COMMANDS.dockMenuToggle, { open: open ?? null });
}

/** 注册 DWM 缩略图。返回 false 或收到 `dockPreviewFallback` 时显示纯色块。 */
export async function dockPreviewShow(
  key: string,
  x: number,
  y: number,
  w: number,
  h: number,
): Promise<boolean> {
  return invoke<boolean>(COMMANDS.dockPreviewShow, { key, x, y, w, h });
}

export async function dockPreviewHide(): Promise<void> {
  await invoke(COMMANDS.dockPreviewHide);
}

/**
 * FLIP 诊断日志（B4 判据）。写进 `glasssuite.log`，一行一条，只记 dx 数值与原因，
 * 不含应用名 / 路径。transient 增减时若判定为窗口几何变化，dx 必须全是 0。
 */
export async function logFlip(line: string): Promise<void> {
  await logFrontend(`[flip] ${line}`).catch(() => {
    /* 非 Tauri 环境 */
  });
}

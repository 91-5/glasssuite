/**
 * 岛的 UI 数据类型（移植自 glasspill，字段与 GlassSuite 后端契约对齐）。
 *
 * 与 `@glasssuite/ipc-types` 的差异只有一处：GlassSuite 的媒体快照用 `artwork`，
 * 这里沿用 UI 侧的 `art` 名字，由 `hooks/media.ts` 在入口做一次映射——
 * 组件层不必知道后端的字段名。
 */
export type PlaybackStatus =
  | "Closed"
  | "Opened"
  | "OpenChanging"
  | "Playing"
  | "Paused"
  | "Stopped"
  | "Ended";

/** SMTC 快照（Rust `island::media` 生成，事件驱动推送，不轮询）。 */
export interface MediaState {
  title: string;
  artist: string;
  album: string;
  appId?: string | null;
  status?: PlaybackStatus | string | null;
  playing: boolean;
  /** 快照时刻的播放位置（毫秒）。 */
  positionMs: number;
  /** 快照对应的 Unix 毫秒时间戳，前端用它插值当前位置。 */
  baseUnixMs: number;
  rate: number;
  durationMs: number;
  canPlay: boolean;
  canPause: boolean;
  canNext: boolean;
  canPrev: boolean;
  /** 专辑封面 data URL（Rust 侧读 SMTC 缩略流转 base64）。 */
  art: string | null;
}

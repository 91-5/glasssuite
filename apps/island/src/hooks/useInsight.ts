import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/**
 * AI 后台感知层推送的一句话。
 *
 * 定位（sir 2026-10-29）：岛不是聊天入口，AI 在后台把「发生了什么」压成一行推上来。
 * 后端负责聚合/去抖/降级，前端只渲染 —— 不做任何业务判断。
 */
export interface Insight {
  kind: string;
  text: string;
  /** ai = 模型重写；local = 本地按 App 计数兜底；rule = 主动建议（本地硬规则）；
   *  off = AI 层被整体关闭的一次性明示（ADV-1） */
  source: "ai" | "local" | "rule" | "off";
  count: number;
  at: number;
}

/** ADV-1：AI 关闭态的提示文案（后端 ai_disabled 命令为真时本地合成，不经事件通道） */
const AI_OFF_NOTICE = "AI 感知已关闭（GLASSPILL_AI_ENABLED=0），通知不再上岛";

/** 展示时长：够读完一句话，又不至于长期占位。 */
const SHOW_MS = 12_000;

export function useInsight(): Insight | null {
  const [insight, setInsight] = useState<Insight | null>(null);

  useEffect(() => {
    const un = listen<Insight>("ai-insight", (e) => {
      if (e.payload && e.payload.text) setInsight(e.payload);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  // ADV-1：挂载时问一次后端「AI 是否被整体关闭」。走命令而非事件，
  // 规避 spawn 时 emit 早于本监听注册的竞态（与 lyrics_state 同思路）。
  useEffect(() => {
    invoke<boolean>("ai_disabled")
      .then((off: boolean) => {
        if (off) {
          setInsight({
            kind: "ai-off",
            text: AI_OFF_NOTICE,
            source: "off",
            count: 0,
            at: Date.now(),
          });
          invoke("log_frontend", { msg: "ai-off notice shown" }).catch(
            () => {},
          );
        }
      })
      .catch(() => {});
  }, []);

  // 到点自动消失。依赖用 at（同一句话重复推送时也能重新计时）
  const at = insight?.at ?? 0;
  useEffect(() => {
    if (!at) return;
    const timer = setTimeout(() => setInsight(null), SHOW_MS);
    return () => clearTimeout(timer);
  }, [at]);

  return insight;
}

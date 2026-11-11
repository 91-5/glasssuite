import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useBallHealth } from "../../hooks/useBallHealth";

/**
 * 收起小球 —— Grok 风格，左侧，点击收起/展开主岛。
 *
 * sir 2026-10-30（OC-20261030-001/002）：小球要活起来 ——
 * - 状态灯：组件内部 useBallHealth() 仲裁四态（battery 红闪 > perf 橙红 >
 *   offline XX 眼 > normal 青色），根元素挂 data-state，配色全在 glass.css；
 *   充电中永不红闪，改在球下半部分显示闪电标（data-charging）
 * - 眼神跟随双层：全屏任意位置由 Rust 钩子量化 8 方向推 ball-look 事件，
 *   这里 ref 直写 dataset（零 setState、零重渲染）；鼠标在球上时四象限
 *   hover 热区（.ball-look）接管，CSS 特异性保证 hover 优先
 * - 断网时眼睛变 ××（data-eyes="xx"）；battery 与 offline 并发时红闪优先，
 *   眼睛仍保持 ××（卡面明确要求的组合）
 */

export function CollapseBall({ collapsed, onToggle, muted, onToggleMute }: CollapseBallProps) {
  const { state, online, charging } = useBallHealth();
  const ballRef = useRef<HTMLDivElement | null>(null);

  // 状态跃迁诊断：只在 state/charging/online 变化时进 debug.log（1Hz 推送不打日志）
  useEffect(() => {
    invoke("log_frontend", {
      msg: `ball state=${state} charging=${charging} online=${online}`,
    }).catch(() => {});
  }, [state, charging, online]);

  // 视线事件 ref 直写 —— 不经 React 状态，重渲染为 0（OC-20261030-002 任务 B）
  useEffect(() => {
    const un = listen<string>("ball-look", (e) => {
      const el = ballRef.current;
      if (el && typeof e.payload === "string") {
        el.dataset.look = e.payload;
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  return (
    <div className="collapse-ball-container">
      <div
        ref={ballRef}
        className={`collapse-ball ${collapsed ? "is-collapsed" : ""}`}
        data-state={state}
        data-eyes={online ? "open" : "xx"}
        data-charging={charging ? "true" : undefined}
        onClick={onToggle}
        title={collapsed ? "点击展开" : "点击收起"}
      >
        {/* 四象限眼神热区 —— 光标在球上时优先于 data-look（规则见 glass.css） */}
        <span className="ball-look q-tl" aria-hidden="true" />
        <span className="ball-look q-tr" aria-hidden="true" />
        <span className="ball-look q-bl" aria-hidden="true" />
        <span className="ball-look q-br" aria-hidden="true" />
        <div className="ball-glow" />
        <div className="ball-body">
          <div className="ball-eye ball-eye-left" />
          <div className="ball-eye ball-eye-right" />
        </div>
        {/* 充电闪电标：常驻 DOM 由 CSS 按 data-charging 显隐，避免插拔电时 DOM 增删 */}
        <span className="ball-bolt" aria-hidden="true" />
      </div>
      {/* 免打扰按钮 —— 小球下方 */}
      <div
        className={`mute-btn ${muted ? "on" : ""}`}
        onClick={onToggleMute}
        title={muted ? "免打扰开启" : "免打扰关闭"}
      >
        {muted ? "🔇" : "🔔"}
      </div>
    </div>
  );
}

interface CollapseBallProps {
  collapsed: boolean;
  onToggle: () => void;
  muted: boolean;
  onToggleMute: () => void;
}

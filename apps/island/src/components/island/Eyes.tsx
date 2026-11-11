import type { CSSProperties } from "react";

/**
 * GlassPill 机器人眼睛 v5 —— 荧光小球（无瞳孔）。
 *
 * sir 2026-10-26 反馈：
 * - 去掉黑色瞳孔 → 纯荧光小球，带白色高光
 * - 科技感 → 青色辉光
 * - 眼睛移动受限 → 增大移动范围
 * - 轮换信息左右对称
 *
 * 设计参考：@librarian 调研 → JARVIS 青色全息眼方向，简化为无瞳孔纯发光球
 *
 * 纯 CSS 动画（无 rAF）：
 * - 眨眼：opacity（无瞳孔，scaleY 会变形）
 * - 音乐律动：辉光脉冲（0.5s 周期，左右错峰）
 * - 跟随鼠标：CSS transform，--eye-dx/--eye-dy 驱动，±3px 范围
 */

export type EyeExpression = "idle" | "happy" | "sleepy";

interface EyesProps {
  /** 鼠标方向 -1~1 */
  dx: number;
  dy: number;
  expression: EyeExpression;
  /** 音乐播放中 → 辉光脉冲 */
  musicActive: boolean;
}

export function Eyes({ dx, dy, expression, musicActive }: EyesProps) {
  const orbStyle = {
    "--eye-dx": dx,
    "--eye-dy": dy,
  } as CSSProperties;

  return (
    <div className={`eyes eyes-${expression} ${musicActive ? "music-active" : ""}`}>
      <div className="eye-socket">
        <div className="glow-effect" />
        <div className="glow-orb" style={orbStyle}>
          <div className="glow-highlight" />
        </div>
      </div>
      <div className="eye-socket">
        <div className="glow-effect" />
        <div className="glow-orb" style={orbStyle}>
          <div className="glow-highlight" />
        </div>
      </div>
    </div>
  );
}

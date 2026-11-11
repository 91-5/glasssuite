import type { Insight } from "../../hooks/useInsight";

/**
 * AI 洞察条 —— 主岛下方，与通知岛同位置但优先级更高。
 *
 * 四种来源共用一条通道（避免两条消息互相覆盖）：
 * - ai    → ✦ 模型重写的通知摘要
 * - local → · 本地聚合兜底
 * - rule  → ! 主动建议（本地硬规则，如电量低/久坐/深夜）
 * - off   → × AI 层被整体关闭的一次性明示（ADV-1）
 */
export function InsightBar({ insight }: { insight: Insight }) {
  const badge =
    insight.source === "ai"
      ? "✦"
      : insight.source === "rule"
        ? "!"
        : insight.source === "off"
          ? "×"
          : "·";
  return (
    <div className={`insight-island ${insight.source}`} title={insight.text}>
      <span className="insight-badge" aria-hidden>
        {badge}
      </span>
      <span className="insight-text">{insight.text}</span>
    </div>
  );
}

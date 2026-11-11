import { useEffect, useState } from "react";

/**
 * GlassPill 时钟组件 —— 替换眼睛。
 *
 * sir 2026-10-26: 眼睛太丑，换成实时时间展示。
 * 水墨感字体，简洁优雅。
 * 音乐播放时时钟微脉冲（musicActive）。
 * sir 2026-11-04 (OC-20261104-001): 时间上方加日期行，收拢态不展开即见当天几号。
 */
interface ClockProps {
  musicActive: boolean;
}

export function Clock({ musicActive }: ClockProps) {
  const [now, setNow] = useState(new Date());

  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(timer);
  }, []);

  const hours = now.getHours().toString().padStart(2, "0");
  const minutes = now.getMinutes().toString().padStart(2, "0");
  // 口径：toLocaleDateString("zh-CN", { month: "long", day: "numeric", weekday: "short" })
  // 原始输出无空格（如「11月4日周三」），按卡面钉死格式 M月D日 周X 补空格。
  const dateLine = now
    .toLocaleDateString("zh-CN", { month: "long", day: "numeric", weekday: "short" })
    .replace(/(周[一二三四五六日])$/, " $1");

  return (
    <div className={`clock ${musicActive ? "music-active" : ""}`}>
      <span className="clock-date">{dateLine}</span>
      <span className="clock-time">{hours}:{minutes}</span>
    </div>
  );
}

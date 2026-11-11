import { useEffect, useState } from "react";

/**
 * FPS 测量 hook —— rAF 计数器 + 历史曲线。
 *
 * 注意：这是「测量」不是「动画」。rAF 由浏览器按显示刷新率调用，
 * 计数器只做 O(1) 加法，开销可忽略。v0.2「无常驻 rAF」红线针对的是
 * 用 rAF 驱动动画（如眼睛呼吸），不是测量帧率。
 */
export function useFps(): { fps: number; history: number[] } {
  const [fps, setFps] = useState(0);
  const [history, setHistory] = useState<number[]>([]);

  useEffect(() => {
    let frameCount = 0;
    let lastTime = performance.now();
    let rafId: number;

    const count = () => {
      frameCount++;
      const now = performance.now();
      const elapsed = now - lastTime;
      if (elapsed >= 1000) {
        const currentFps = Math.round((frameCount * 1000) / elapsed);
        setFps(currentFps);
        setHistory((prev) => {
          const next = [...prev, currentFps];
          if (next.length > 60) next.shift();
          return next;
        });
        frameCount = 0;
        lastTime = now;
      }
      rafId = requestAnimationFrame(count);
    };

    rafId = requestAnimationFrame(count);
    return () => cancelAnimationFrame(rafId);
  }, []);

  return { fps, history };
}

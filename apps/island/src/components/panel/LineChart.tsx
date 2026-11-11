import { useEffect, useRef } from "react";

const MAX_HISTORY = 60;

interface LineChartProps {
  data: number[];
  color: string;
  max: number; // Y 轴上限
  label: string;
  value: string;
}

/**
 * Canvas 2D 折线图 —— dp 大脑评审 #3.2：Canvas 优于 SVG（DOM 重排成本高）。
 * 1s 刷新，环形缓冲区 60 点，无 rAF（useEffect 在数据变化时重绘）。
 */
export function LineChart({ data, color, max, label, value }: LineChartProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const w = canvas.width;
    const h = canvas.height;
    ctx.clearRect(0, 0, w, h);

    if (data.length < 2) return;

    // 填充区域
    ctx.beginPath();
    ctx.moveTo(0, h);
    for (let i = 0; i < data.length; i++) {
      const x = (i / (MAX_HISTORY - 1)) * w;
      const y = h - (Math.min(data[i], max) / max) * h;
      ctx.lineTo(x, y);
    }
    const lastX = ((data.length - 1) / (MAX_HISTORY - 1)) * w;
    ctx.lineTo(lastX, h);
    ctx.closePath();
    ctx.fillStyle = color.replace("1)", "0.12)");
    ctx.fill();

    // 折线
    ctx.strokeStyle = color;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    for (let i = 0; i < data.length; i++) {
      const x = (i / (MAX_HISTORY - 1)) * w;
      const y = h - (Math.min(data[i], max) / max) * h;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.stroke();
  }, [data, color, max]);

  return (
    <div className="chart-widget">
      <div className="chart-header">
        <span className="chart-label">{label}</span>
        <span className="chart-value">{value}</span>
      </div>
      <canvas ref={canvasRef} width={220} height={50} />
    </div>
  );
}

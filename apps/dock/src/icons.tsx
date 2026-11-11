import { useEffect, useRef } from "react";

export interface IconData {
  w: number;
  h: number;
  /** RGBA，base64 */
  rgba: string;
}

/** RGBA base64 → canvas 绘制（dock 药丸与开始菜单面板共用）。 */
export function IconCanvas({ icon, className }: { icon: IconData; className?: string }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    try {
      const bin = atob(icon.rgba);
      const bytes = new Uint8ClampedArray(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      ctx.putImageData(new ImageData(bytes, icon.w, icon.h), 0, 0);
    } catch (e) {
      console.error("icon decode failed", e);
    }
  }, [icon]);
  return <canvas ref={ref} width={icon.w} height={icon.h} className={className ?? "dock-icon-img"} />;
}

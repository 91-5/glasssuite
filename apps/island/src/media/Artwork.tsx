/** 专辑封面。封面是 SMTC 缩略流转成的 data URL，CSP 允许 `data:`。 */
export function Artwork({ src, size = 28 }: { src?: string | null; size?: number }) {
  if (src) {
    return (
      <img
        className="island-artwork"
        src={src}
        width={size}
        height={size}
        alt=""
        draggable={false}
      />
    );
  }
  // 无封面时的占位：纯 CSS + 音符字形，不引入图片资源。
  return (
    <span
      className="island-artwork island-artwork--placeholder"
      style={{ width: size, height: size }}
      aria-hidden="true"
    >
      ♪
    </span>
  );
}

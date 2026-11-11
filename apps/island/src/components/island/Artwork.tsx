import type { MediaState } from "../../types";

const NOTE_SVG = (
  <svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor" aria-hidden>
    <path d="M9 18.5a2.5 2.5 0 1 1-2.5-2.5c.54 0 1.05.17 1.5.46V6.3l11-2.05v10.5a2.5 2.5 0 1 1-2.5-2.5c.54 0 1.05.17 1.5.46V7.1L9 8.66v9.84z" />
  </svg>
);

/** 专辑封面：无封面时画占位符（音符 + tint 渐变），有封面用 data URL。 */
export function Artwork({ media, size }: { media: MediaState | null; size: "sm" | "lg" }) {
  if (media?.art) {
    return <img className={`art ${size}`} src={media.art} alt="" draggable={false} />;
  }
  return (
    <div className={`art ${size} placeholder`} aria-hidden>
      {NOTE_SVG}
    </div>
  );
}

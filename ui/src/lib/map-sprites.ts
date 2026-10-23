// Images for the map's symbol layers, drawn on a canvas when MapLibre asks for them
// (`styleimagemissing`), because the bundled style has no glyphs or sprite sheet. An image
// id says what to draw: `n:14` a count on a marker, `l:+3` the label of an arc, and
// `a:#0072b2` an arrowhead in a colour.

export const SPRITE_RATIO = 2;

export type SpriteColours = {
  text: string;
  surface: string;
  border: string;
  onMarker: string;
  font: string;
};

export type Sprite = { width: number; height: number; data: Uint8ClampedArray };

function canvas(w: number, h: number) {
  const c = document.createElement('canvas');
  c.width = Math.ceil(w * SPRITE_RATIO);
  c.height = Math.ceil(h * SPRITE_RATIO);
  const g = c.getContext('2d', { willReadFrequently: true });
  if (!g) return null;
  g.scale(SPRITE_RATIO, SPRITE_RATIO);
  return { c, g };
}

function read(c: HTMLCanvasElement, g: CanvasRenderingContext2D): Sprite {
  const d = g.getImageData(0, 0, c.width, c.height);
  return { width: d.width, height: d.height, data: d.data };
}

function textWidth(text: string, font: string): number {
  const m = canvas(1, 1);
  if (!m) return text.length * 7;
  m.g.font = font;
  return m.g.measureText(text).width;
}

/** The image for an id, or null when the id is not one of ours. */
export function sprite(id: string, colours: SpriteColours): Sprite | null {
  const kind = id.slice(0, 2);
  const arg = id.slice(2);
  if (kind === 'n:') {
    const font = `600 11px ${colours.font}`;
    const w = Math.max(14, textWidth(arg, font) + 2);
    const k = canvas(w, 14);
    if (!k) return null;
    k.g.font = font;
    k.g.textAlign = 'center';
    k.g.textBaseline = 'middle';
    k.g.fillStyle = colours.onMarker;
    k.g.fillText(arg, w / 2, 7.5);
    return read(k.c, k.g);
  }
  if (kind === 'l:') {
    const font = `600 11px ${colours.font}`;
    const w = textWidth(arg, font) + 12;
    const k = canvas(w, 18);
    if (!k) return null;
    k.g.beginPath();
    k.g.roundRect(0.5, 0.5, w - 1, 17, 8.5);
    k.g.fillStyle = colours.surface;
    k.g.fill();
    k.g.strokeStyle = colours.border;
    k.g.lineWidth = 1;
    k.g.stroke();
    k.g.font = font;
    k.g.textAlign = 'center';
    k.g.textBaseline = 'middle';
    k.g.fillStyle = colours.text;
    k.g.fillText(arg, w / 2, 9.5);
    return read(k.c, k.g);
  }
  if (kind === 'a:') {
    const k = canvas(16, 16);
    if (!k) return null;
    k.g.beginPath();
    k.g.moveTo(8, 0.8);
    k.g.lineTo(14.6, 15);
    k.g.lineTo(8, 11.2);
    k.g.lineTo(1.4, 15);
    k.g.closePath();
    k.g.fillStyle = arg;
    k.g.fill();
    k.g.strokeStyle = colours.surface;
    k.g.lineWidth = 0.9;
    k.g.lineJoin = 'round';
    k.g.stroke();
    return read(k.c, k.g);
  }
  return null;
}

// The design tokens of app.css: every text colour meets WCAG AA (4.5:1) on the page and
// panel backgrounds in the light theme and in both dark ones, the two dark blocks agree,
// and the spacing and control tokens exist.

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

function luminance(hex: string): number {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const [r, g, b] = c.map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}
const contrast = (a: string, b: string) => {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
};

const css = readFileSync(resolve(import.meta.dirname, '../app.css'), 'utf8');
/** The hex custom properties of each block that sets `--surface`: light, dark by media, dark by attribute. */
const blocks = css
  .split('}')
  .filter((b) => /--surface:\s*#/.test(b))
  .map((b) =>
    Object.fromEntries([...b.matchAll(/(--[\w-]+):\s*(#[0-9a-f]{6})/gi)].map((m) => [m[1], m[2]])),
  );

const TEXT = [
  '--text',
  '--text-2',
  '--text-3',
  '--iri',
  '--literal',
  '--bnode',
  '--var',
  '--spark-ink',
  '--ok',
  '--warn',
  '--danger',
];
const BACKGROUNDS = ['--bg', '--surface', '--surface-2'];

describe('the theme tokens', () => {
  it('define a light theme and two identical dark ones', () => {
    expect(blocks.length).toBe(3);
    expect(blocks[1]).toEqual(blocks[2]);
  });

  it('give every text colour at least 4.5:1 on the backgrounds', () => {
    for (const [i, b] of blocks.entries())
      for (const t of TEXT)
        for (const bg of BACKGROUNDS)
          expect(
            contrast(b[t], b[bg]),
            `${i ? 'dark' : 'light'} ${t} ${b[t]} on ${bg} ${b[bg]}`,
          ).toBeGreaterThanOrEqual(4.5);
  });

  it('give the primary button text at least 4.5:1 on the accent', () => {
    for (const b of blocks)
      expect(contrast(blocks[0]['--on-spark'], b['--spark'])).toBeGreaterThanOrEqual(4.5);
  });

  it('define the spacing, control and shell tokens', () => {
    for (const t of [
      '--sp-1',
      '--sp-2',
      '--sp-3',
      '--sp-4',
      '--sp-6',
      '--gutter',
      '--gap',
      '--ctl-h',
      '--ctl-h-sm',
      '--topbar-h',
      '--rail-w',
      '--r-pill',
      '--shadow-overlay',
    ])
      expect(css, t).toMatch(new RegExp(`${t}:\\s*[^;]+;`));
  });
});

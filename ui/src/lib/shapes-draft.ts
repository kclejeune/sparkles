// Helpers of the schema browser's "Draft shapes" dialog: the request, the constraints
// that would exclude existing instances, a summary line, and the text to show or keep.

import type { DraftShape, ShapesDraft } from './api';
import { fmtInt } from './format';

export type DraftLang = 'shacl' | 'shex';

/** The support typed into the dialog, or null when it is not a number in (0, 1]. */
export function parseSupport(text: string): number | null {
  const t = text.trim();
  if (!t) return null;
  const n = Number(t);
  return Number.isFinite(n) && n > 0 && n <= 1 ? n : null;
}

/** One drafted constraint that rejects some current instances. */
export type Exclusion = { path: string; component: string; excluded: number; applicable: number };

/** The drafted constraints of a shape that exclude instances, most exclusions first. */
export function exclusions(shape: DraftShape): Exclusion[] {
  const out: Exclusion[] = [];
  for (const p of shape.properties) {
    for (const c of p.constraints) {
      if (c.excluded > 0) {
        out.push({
          path: p.path,
          component: c.component,
          excluded: c.excluded,
          applicable: c.applicable,
        });
      }
    }
  }
  return out.sort(
    (a, b) =>
      b.excluded - a.excluded ||
      a.path.localeCompare(b.path) ||
      a.component.localeCompare(b.component),
  );
}

const plural = (n: number, one: string, many = `${one}s`) => `${fmtInt(n)} ${n === 1 ? one : many}`;

/** One line about a draft: shapes, constraints and how many would reject current data. */
export function summaryLine(d: ShapesDraft): string {
  const t = d.totals;
  const excluding = d.shapes.reduce((n, s) => n + exclusions(s).length, 0);
  const parts = [
    plural(t.shapes, 'shape'),
    plural(t.propertyShapes, 'property shape'),
    plural(t.constraints, 'constraint'),
  ];
  parts.push(
    excluding
      ? `${plural(excluding, 'constraint')} would reject current data`
      : 'the current data conforms',
  );
  return parts.join(' · ');
}

/** The text of the draft in a language; ShEx carries its shape map as a comment. */
export function draftText(d: ShapesDraft, lang: DraftLang): string {
  if (lang === 'shacl') return d.shacl;
  const map = d.shapeMap
    .split('\n')
    .map((l) => `# ${l}`)
    .join('\n');
  return `${d.shex.trimEnd()}\n\n# Shape map:\n${map}\n`;
}

/** The constraint component's SHACL name: `minCount` → `sh:minCount`. */
export const componentLabel = (c: string) => `sh:${c}`;

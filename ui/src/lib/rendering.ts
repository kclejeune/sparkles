// Shared language selection for labels and the browser's prose. The language matching
// follows the ontology routes: a requested tag, its subtags, or its primary tag.
export type EntityDisplay = 'label' | 'prefixed' | 'iri';
export type Label = { '@value': string; '@language'?: string };
export type RenderingPreferences = { version: 1; mode: EntityDisplay; languages: string[] };
export const RENDERING_KEY = 'sparkles.rendering';

export function languageOrder(text: string): string[] | null {
  const tags = text
    .split(',')
    .map((s) => s.trim().toLowerCase())
    .filter(Boolean);
  if (tags.some((s) => !/^[a-z]+(?:-[a-z0-9]+)*$/.test(s))) return null;
  return [...new Set(tags)];
}

export function readRendering(raw: string | null, legacyFull = false): RenderingPreferences {
  const fallback: RenderingPreferences = {
    version: 1,
    mode: legacyFull ? 'iri' : 'label',
    languages: [],
  };
  if (!raw) return fallback;
  try {
    const p = JSON.parse(raw);
    if (
      p?.version !== 1 ||
      !['label', 'prefixed', 'iri'].includes(p.mode) ||
      !Array.isArray(p.languages) ||
      p.languages.some((s: unknown) => typeof s !== 'string')
    )
      return fallback;
    const languages = languageOrder(p.languages.join(','));
    return languages ? { version: 1, mode: p.mode, languages } : fallback;
  } catch {
    return fallback;
  }
}

/** A regional preference falls back to its primary language before the next preference. */
export function expandLanguages(langs: readonly string[]): string[] {
  return [...new Set(langs.flatMap((s) => [s.toLowerCase(), s.split('-')[0].toLowerCase()]))];
}

export function langRank(lang: string | null, langs: readonly string[]): number {
  const l = (lang ?? '').toLowerCase();
  const i = langs.findIndex((x) => {
    const w = x.toLowerCase();
    return w === l || l.startsWith(w + '-');
  });
  if (i >= 0) return i;
  if (!l) return langs.length;
  if (l === 'en' || l.startsWith('en-')) return langs.length + 1;
  return langs.length + 2;
}

export function preferredLabel(
  lits: readonly Label[] | undefined,
  langs: readonly string[],
): string | null {
  langs = expandLanguages(langs);
  let best: Label | undefined;
  let rank = Infinity;
  for (const l of lits ?? []) {
    if (!l['@value'].trim()) continue;
    const r = langRank(l['@language'] ?? null, langs);
    if (r < rank) {
      best = l;
      rank = r;
    }
  }
  return best?.['@value'] ?? null;
}

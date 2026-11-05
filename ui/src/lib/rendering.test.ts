import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  expandLanguages,
  languageOrder,
  langRank,
  preferredLabel,
  readRendering,
} from './rendering';
import { iriForm } from './iri-form.svelte';
import { displayEntity, displayTerm, toSparql } from './rdf';
import { entityName } from './ontology';
import { resultLabelsQuery, termIris } from './result-labels';
import { sortedOrder } from './table';
const EX = 'https://example.org/';
const labels = [
  { '@value': 'English', '@language': 'en' },
  { '@value': 'Français', '@language': 'fr' },
  { '@value': 'Québécois', '@language': 'fr-CA' },
];
afterEach(() => {
  iriForm.update('label', []);
  vi.unstubAllGlobals();
});
describe('rendering preferences', () => {
  it('normalizes an ordered language list and rejects invalid tags', () => {
    expect(languageOrder(' FR-ca, en, fr-CA ')).toEqual(['fr-ca', 'en']);
    expect(languageOrder('')).toEqual([]);
    expect(languageOrder('fr_CA')).toBeNull();
    expect(expandLanguages(['fr-CA', 'en-GB', 'fr'])).toEqual(['fr-ca', 'fr', 'en-gb', 'en']);
  });
  it('validates saved records and migrates the legacy full-IRI choice', () => {
    expect(readRendering(null, true).mode).toBe('iri');
    expect(readRendering(null).mode).toBe('label');
    expect(readRendering('{')).toEqual(readRendering(null));
    for (const value of [
      null,
      {},
      { version: 2, mode: 'iri', languages: [] },
      { version: 1, mode: 'bad', languages: [] },
      { version: 1, mode: 'label', languages: [23] },
      { version: 1, mode: 'label', languages: ['en_GB'] },
    ])
      expect(readRendering(JSON.stringify(value))).toEqual(readRendering(null));
    expect(
      readRendering(JSON.stringify({ version: 1, mode: 'prefixed', languages: ['FR', 'en'] })),
    ).toEqual({ version: 1, mode: 'prefixed', languages: ['fr', 'en'] });
  });
  it('keeps preferences usable when storage refuses reads and writes', () => {
    vi.stubGlobal('localStorage', {
      getItem() {
        throw Error('blocked');
      },
      setItem() {
        throw Error('blocked');
      },
      removeItem() {
        throw Error('blocked');
      },
    });
    expect(() => iriForm.update('iri', ['fr'])).not.toThrow();
    expect(iriForm.mode).toBe('iri');
    expect(iriForm.languages).toEqual(['fr']);
  });
  it('persists versioned choices and retains the quick switch', () => {
    const saved = new Map<string, string>();
    vi.stubGlobal('localStorage', {
      setItem: (k: string, v: string) => saved.set(k, v),
      removeItem: (k: string) => saved.delete(k),
    });
    iriForm.update('label', ['fr', 'en']);
    expect(readRendering(saved.get('sparkles.rendering')!)).toEqual({
      version: 1,
      mode: 'label',
      languages: ['fr', 'en'],
    });
    iriForm.set(true);
    expect(iriForm.full).toBe(true);
    iriForm.set(false);
    expect(iriForm.mode).toBe('prefixed');
    expect(iriForm.languages).toEqual(['fr', 'en']);
  });
});
it('selects regional labels, then the primary language, untagged, English and other labels', () => {
  expect(preferredLabel(labels, ['fr-CA', 'en'])).toBe('Québécois');
  expect(preferredLabel(labels, ['fr-BE', 'en'])).toBe('Français');
  expect(preferredLabel([{ '@value': 'Plain' }, ...labels], ['de'])).toBe('Plain');
  expect(preferredLabel(labels, ['de'])).toBe('English');
  expect(preferredLabel([{ '@value': 'Other', '@language': 'de' }], ['es'])).toBe('Other');
  expect(preferredLabel([{ '@value': '' }, { '@value': '  ' }], ['en'])).toBeNull();
  expect(langRank('enochian', [])).toBe(2);
});
it('uses the same display in the editor, result cells and sort keys without changing RDF', () => {
  const term = { type: 'uri' as const, value: EX + 'A' };
  const prefixes = { ex: EX };
  const all = new Map([[term.value, labels]]);
  for (const [mode, expected] of [
    ['label', 'Français'],
    ['prefixed', 'ex:A'],
    ['iri', EX + 'A'],
  ] as const) {
    iriForm.update(mode, ['fr']);
    expect(displayEntity(term.value, prefixes, labels)).toBe(expected);
    expect(entityName(term.value, all, prefixes)).toBe(expected);
    expect(displayTerm(term, prefixes, (i) => displayEntity(i, prefixes, all.get(i)))).toBe(
      expected,
    );
    expect(toSparql(term)).toBe(`<${EX}A>`);
  }
  iriForm.update('label');
  expect(displayEntity(EX + 'A', prefixes)).toBe('ex:A');
  expect(displayEntity(EX + 'unknown', {})).toBe(EX + 'unknown');
  expect(
    sortedOrder([[term], [{ type: 'uri', value: EX + 'B' }]], { col: 0, dir: 1 }, prefixes, (i) =>
      i.endsWith('A') ? 'Zulu' : 'Alpha',
    ),
  ).toEqual([1, 0]);
});
it('queries only named terms and traverses nested triple terms', () => {
  const term = { type: 'uri' as const, value: EX + 'A' };
  expect(
    termIris({
      type: 'triple',
      value: { subject: term, predicate: term, object: { type: 'literal', value: 'x' } },
    }),
  ).toEqual([EX + 'A', EX + 'A']);
  expect(resultLabelsQuery([EX + 'A'])).toContain(`VALUES ?s { <${EX}A> }`);
  expect(resultLabelsQuery([EX + 'A'])).toContain('GRAPH ?g');
});

it('uses rdfs:label before skos:prefLabel and deterministic labels within one language', async () => {
  const { labelsOfBindings } = await import('./ontology');
  const rows = [
    {
      s: { type: 'uri', value: EX + 'A' },
      p: { type: 'uri', value: 'http://www.w3.org/2004/02/skos/core#prefLabel' },
      l: { type: 'literal', value: 'Aardvark', 'xml:lang': 'en' },
    },
    {
      s: { type: 'uri', value: EX + 'A' },
      p: { type: 'uri', value: 'http://www.w3.org/2000/01/rdf-schema#label' },
      l: { type: 'literal', value: 'Zulu', 'xml:lang': 'en' },
    },
    {
      s: { type: 'uri', value: EX + 'A' },
      p: { type: 'uri', value: 'http://www.w3.org/2000/01/rdf-schema#label' },
      l: { type: 'literal', value: 'Beta', 'xml:lang': 'en' },
    },
  ];
  expect(preferredLabel(labelsOfBindings(rows).get(EX + 'A'), ['en'])).toBe('Beta');
  expect(preferredLabel(labelsOfBindings([...rows].reverse()).get(EX + 'A'), ['en'])).toBe('Beta');
});

it('writes full-IRI displays as valid Manchester names when copying or completing expressions', async () => {
  const { manchester, manchesterName } = await import('./ontology');
  expect(manchester(EX + 'A', (i) => i, {})).toBe(`<${EX}A>`);
  expect(manchesterName(EX + 'A', EX + 'A')).toBe(`<${EX}A>`);
  expect(manchesterName('a label', EX + 'A')).toBe("'a label'");
});

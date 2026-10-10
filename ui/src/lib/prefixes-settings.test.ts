import { describe, expect, it } from 'vitest';
import {
  bindPatch,
  deletePatch,
  prefixField,
  prefixIriError,
  prefixNameError,
  prefixRows,
  prefixesUrl,
  type PrefixesKind,
} from './prefixes-settings';

const KCLJ = 'https://kclj.io/sparkles/';
const NOTES = 'https://kclj.io/sparkles/memory/notes/';

/** A prefixes kind: kclj declared and overridden, notes declared and removed, locked locked, ex added, mem shadowed. */
const kind = (over: Partial<PrefixesKind> = {}): PrefixesKind => ({
  dataset: 'slurp',
  kind: 'prefixes',
  effective: {
    rdf: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#',
    kclj: 'https://kclj.io/x/',
    locked: 'https://kclj.io/locked/',
    ex: 'http://ex.org/',
    mem: 'https://example.org/mem#',
  },
  declared: {
    kclj: KCLJ,
    notes: NOTES,
    locked: 'https://kclj.io/locked/',
    mem: 'https://example.org/mem#',
  },
  runtime: { kclj: 'https://kclj.io/x/', notes: null, ex: 'http://ex.org/' },
  sources: {
    rdf: 'default',
    kclj: 'runtime',
    locked: 'locked',
    ex: 'runtime',
    mem: 'declared',
  },
  locked: ['locked'],
  overridden: [],
  overrides: [
    { path: 'kclj', declared: KCLJ, runtime: 'https://kclj.io/x/' },
    { path: 'notes', declared: NOTES, runtime: null },
  ],
  status: { valid: true },
  etag: '"e"',
  warnings: [
    {
      prefix: 'mem',
      iri: 'https://example.org/mem#',
      wellKnown: 'urn:x-sparkles:mem:',
      message: 'mem: shadows',
    },
  ],
  ...over,
});

describe('prefix rows', () => {
  it('lists effective and removed prefixes with their sources', () => {
    const rows = prefixRows(kind());
    expect(rows.map((r) => [r.name, r.source, r.iri])).toEqual([
      ['ex', 'changed', 'http://ex.org/'],
      ['kclj', 'changed', 'https://kclj.io/x/'],
      ['locked', 'locked', 'https://kclj.io/locked/'],
      ['mem', 'server config', 'https://example.org/mem#'],
      ['notes', 'removed', null],
      ['rdf', 'well-known', 'http://www.w3.org/1999/02/22-rdf-syntax-ns#'],
    ]);
    const by = Object.fromEntries(rows.map((r) => [r.name, r]));
    expect(by.kclj.info.overrides).toBe(KCLJ);
    expect(by.kclj.resettable).toBe(true);
    expect(by.notes.resettable).toBe(true);
    expect(by.notes.declared).toBe(NOTES);
    expect(by.locked.resettable).toBe(false);
    expect(by.rdf.resettable).toBe(false);
    expect(by.mem.warning?.wellKnown).toBe('urn:x-sparkles:mem:');
  });

  it('treats a lock of the whole kind as a lock of every prefix', () => {
    const rows = prefixRows(
      kind({
        locked: [''],
        sources: { rdf: 'locked', kclj: 'locked', locked: 'locked', ex: 'locked', mem: 'locked' },
      }),
    );
    expect(rows.every((r) => r.source === 'locked' && !r.resettable)).toBe(true);
  });
});

describe('prefix validation and patches', () => {
  it('checks names as PN_PREFIX', () => {
    expect(prefixNameError('kclj')).toBeNull();
    expect(prefixNameError('')).toBeNull();
    expect(prefixNameError('a.b-c_d')).toBeNull();
    expect(prefixNameError('1a')).not.toBeNull();
    expect(prefixNameError('a.')).not.toBeNull();
    expect(prefixNameError('a b')).not.toBeNull();
    expect(prefixNameError('a'.repeat(257))).not.toBeNull();
  });

  it('checks IRIs as absolute', () => {
    expect(prefixIriError(KCLJ)).toBeNull();
    expect(prefixIriError('urn:x-sparkles:mem:')).toBeNull();
    expect(prefixIriError('')).not.toBeNull();
    expect(prefixIriError('relative/path')).not.toBeNull();
    expect(prefixIriError('http://a b/')).not.toBeNull();
    expect(prefixIriError('<http://a/>')).not.toBeNull();
  });

  it('builds patches and fields', () => {
    expect(bindPatch('kclj', KCLJ)).toEqual({ kclj: KCLJ });
    expect(deletePatch('notes')).toEqual({ notes: null });
    expect(prefixField('a.b')).toBe('a\\.b');
    expect(prefixesUrl('my ds')).toBe('/$/settings/my%20ds/prefixes');
  });
});

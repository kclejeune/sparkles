import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  classificationNote,
  classifyOntology,
  consistencyInfo,
  formatTimeoutSeconds,
  inferredNodeStatus,
  parseDeadlineInput,
  type Classification,
} from './ontology';

const source = {
  boundStoreId: 'store-a',
  familyId: 'family-a',
  commit: 7,
  inputFingerprint: 'input-a',
  contentFingerprint: 'content-a',
};
function page(commit: number, offset: number, overrides: Record<string, unknown> = {}) {
  return {
    commit,
    nodes: [
      {
        node: offset,
        classes: [`http://example.org/C${offset}`],
        parents: [],
        parentClasses: [],
        children: 0,
      },
    ],
    complete: false,
    partial: false,
    excludedCount: 1,
    consistency: 'consistent',
    classificationSource: { ...source, commit },
    ...(offset === 0 ? { next: 1 } : {}),
    ...overrides,
  };
}
function mock(pages: unknown[]) {
  const calls: { url: string; body: Record<string, unknown> }[] = [];
  vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
    calls.push({ url, body: JSON.parse(String(init.body)) });
    return new Response(JSON.stringify(pages[calls.length - 1]), {
      headers: { 'Content-Type': 'application/json' },
    });
  });
  return calls;
}
afterEach(() => vi.unstubAllGlobals());
describe('explicit legacy classification tree pages', () => {
  it('never appends a page from a different commit', async () => {
    mock([page(7, 0), page(8, 1)]);
    await expect(classifyOntology('ds', { graph: null, imports: true })).rejects.toThrow(
      /changed|commit/i,
    );
  });
  it('pins subsequent requests and accepts identical content with distinct run IDs and exclusions', async () => {
    const calls = mock([
      page(7, 0, { resultId: 'first-run' }),
      page(7, 1, { resultId: 'second-run' }),
    ]);
    const result = await classifyOntology('ds', { graph: null, imports: true });
    expect(result.nodes).toHaveLength(2);
    expect(result.complete).toBe(false);
    expect(calls[1].url).toContain('at=commit%3A7');
  });
  it('rejects changed taxonomy content under the same exact input', async () => {
    mock([
      page(7, 0),
      page(7, 1, { classificationSource: { ...source, contentFingerprint: 'different' } }),
    ]);
    await expect(classifyOntology('ds', { graph: null, imports: true })).rejects.toThrow(
      /changed|content/i,
    );
  });
  it('rejects unexpected partial answers rather than assembling a tree', async () => {
    mock([page(7, 0, { partial: true }), page(7, 1, { partial: true })]);
    await expect(classifyOntology('ds', { graph: null, imports: true })).rejects.toThrow(
      /partial/i,
    );
  });
  it('sends partial, deadline timeout and expansion and accepts a requested partial hierarchy', async () => {
    const calls = mock([
      page(7, 0, {
        next: undefined,
        tier: 'disjunctive',
        partial: true,
        complete: false,
        consistency: 'unknown',
        incompleteCount: 2,
        incomplete: ['http://example.org/C0', 'http://example.org/C1'],
        excludedCount: 0,
      }),
    ]);
    const result = await classifyOntology(
      'ds',
      { graph: null, imports: true },
      undefined,
      'commit:7',
      {
        partial: true,
        timeoutMs: 1500,
        expansion: 'eager',
      },
    );
    expect(calls).toHaveLength(1);
    const url = new URL(calls[0].url, 'http://localhost');
    expect(url.searchParams.get('at')).toBe('commit:7');
    expect(url.searchParams.get('timeout')).toBe('1.5');
    expect(calls[0].body).toMatchObject({
      graph: 'default',
      imports: true,
      nodes: true,
      partial: true,
      expansion: 'eager',
    });
    expect(result.partial).toBe(true);
    expect(result.tier).toBe('disjunctive');
    expect(result.consistency).toBe('unknown');
    expect(result.incompleteCount).toBe(2);
    expect(result.incomplete).toEqual(['http://example.org/C0', 'http://example.org/C1']);
    expect(result.nodesTruncated).toBeUndefined();
  });
  it('marks a multi-page partial answer truncated instead of issuing a second unretained run', async () => {
    const calls = mock([
      page(7, 0, {
        next: 1,
        tier: 'disjunctive',
        partial: true,
        complete: false,
        consistency: 'consistent',
        incompleteCount: 1,
        incomplete: ['http://example.org/C0'],
        excludedCount: 0,
      }),
    ]);
    const result = await classifyOntology(
      'ds',
      { graph: null, imports: true },
      undefined,
      undefined,
      {
        partial: true,
        timeoutMs: 500,
        expansion: 'cautious',
      },
    );
    expect(calls).toHaveLength(1);
    expect(result.partial).toBe(true);
    expect(result.nodesTruncated).toBe(true);
    expect(result.nodes).toHaveLength(1);
  });
  it('rejects a later page that unexpectedly turns partial after a non-partial first page', async () => {
    mock([page(7, 0, { partial: false }), page(7, 1, { partial: true })]);
    await expect(
      classifyOntology('ds', { graph: null, imports: true }, undefined, undefined, {
        partial: true,
        timeoutMs: 2000,
      }),
    ).rejects.toThrow(/changed/i);
  });
});

describe('deadline parsing and timeout formatting', () => {
  it('parses blank, seconds, decimals and millisecond suffixes', () => {
    expect(parseDeadlineInput('')).toEqual({});
    expect(parseDeadlineInput('   ')).toEqual({});
    expect(parseDeadlineInput('5')).toEqual({ timeoutMs: 5000 });
    expect(parseDeadlineInput('0.5s')).toEqual({ timeoutMs: 500 });
    expect(parseDeadlineInput('250ms')).toEqual({ timeoutMs: 250 });
    expect(parseDeadlineInput('0').error).toMatch(/greater than zero/i);
    expect(parseDeadlineInput('-2').error).toMatch(/positive deadline/i);
    expect(parseDeadlineInput('abc').error).toMatch(/positive deadline/i);
  });
  it('formats positive millisecond deadlines as seconds', () => {
    expect(formatTimeoutSeconds(undefined)).toBeUndefined();
    expect(formatTimeoutSeconds(0)).toBeUndefined();
    expect(formatTimeoutSeconds(-10)).toBeUndefined();
    expect(formatTimeoutSeconds(500)).toBe('0.5');
    expect(formatTimeoutSeconds(5000)).toBe('5');
    expect(formatTimeoutSeconds(1250)).toBe('1.25');
  });
});

describe('open-class node status and consistency display helpers', () => {
  it('marks open classes in the inferred tree distinctly from closed and unsatisfiable nodes', () => {
    const classesOf = new Map<string, string[]>([
      ['n:1', ['urn:Unsat']],
      ['n:2', ['urn:Closed']],
      ['n:3', ['urn:OpenA', 'urn:ClosedB']],
    ]);
    const incomplete = new Set(['urn:OpenA']);
    expect(inferredNodeStatus('n:2', classesOf, incomplete)).toEqual({
      badge: null,
      tone: null,
      open: false,
      unsatisfiable: false,
    });
    expect(inferredNodeStatus('n:3', classesOf, incomplete)).toEqual({
      badge: 'open',
      tone: 'warn',
      open: true,
      unsatisfiable: false,
    });
    expect(inferredNodeStatus('n:1', classesOf, incomplete)).toEqual({
      badge: 'unsatisfiable',
      tone: 'danger',
      open: false,
      unsatisfiable: true,
    });
    expect(inferredNodeStatus('n:1', classesOf, new Set(['urn:Unsat']))).toEqual({
      badge: 'unsatisfiable · open',
      tone: 'danger',
      open: true,
      unsatisfiable: true,
    });
  });
  it('distinguishes unknown consistency from consistent and inconsistent', () => {
    expect(consistencyInfo('consistent', true, false)).toMatchObject({
      state: 'consistent',
      label: 'Consistent',
      tone: 'ok',
    });
    expect(consistencyInfo('inconsistent', false, false)).toMatchObject({
      state: 'inconsistent',
      label: 'Inconsistent',
      tone: 'danger',
    });
    expect(consistencyInfo('unknown', true, true)).toMatchObject({
      state: 'unknown',
      label: 'Consistency unknown',
      tone: 'warn',
    });
    expect(consistencyInfo('unknown', true, true).detail).toContain('stopped before deciding');
    expect(consistencyInfo(undefined, false, false).state).toBe('inconsistent');
  });
});

describe('classificationNote', () => {
  const base = {
    commit: 3,
    tier: 'el',
    consistent: true,
    complete: false,
    classes: 2,
    nodeCount: 2,
    unsatisfiableCount: 0,
    unsatisfiable: [],
    excludedCount: 0,
    excluded: [],
    nodes: [],
    stats: { incremental: false, saturationMs: 1 },
  } as Classification;
  it('names the tier of a complete classification', () => {
    expect(classificationNote({ ...base, complete: true })).toBe('EL · commit 3');
  });
  it('names the unmapped triples of a run that excluded no axiom', () => {
    const note = classificationNote({
      ...base,
      consistency: 'unknown',
      stats: { ...base.stats, unmapped: 1 },
    });
    expect(note).toBe('1 triple mapping to no axiom · commit 3');
    expect(note).not.toContain('omitted');
  });
  it('names both what it excluded and what maps to no axiom', () => {
    expect(
      classificationNote({
        ...base,
        excludedCount: 2,
        stats: { ...base.stats, unmapped: 3 },
      }),
    ).toBe('2 omitted or partly used axioms · 3 triples mapping to no axiom · commit 3');
  });
  it('says incomplete when it names nothing left out', () => {
    expect(classificationNote(base)).toBe('incomplete · commit 3');
  });
  it('names partial status, tier, open-class count and unknown consistency for a partial run', () => {
    expect(
      classificationNote({
        ...base,
        tier: 'disjunctive',
        partial: true,
        incompleteCount: 3,
        consistency: 'unknown',
      }),
    ).toBe('Partial · DISJUNCTIVE · 3 open classes · consistency unknown · commit 3');
    expect(
      classificationNote({
        ...base,
        tier: 'disjunctive',
        partial: true,
        incompleteCount: 1,
        consistency: 'consistent',
      }),
    ).toBe('Partial · DISJUNCTIVE · 1 open class · commit 3');
  });
});

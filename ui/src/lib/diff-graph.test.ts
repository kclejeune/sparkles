import { describe, expect, it } from 'vitest';
import type { DiffGroup, DiffQuad, GroupedDiff, Term } from './api';
import {
  applyStep,
  BUCKET_ABOVE,
  canvasElements,
  changedResources,
  classOfKey,
  countsText,
  diffCanvas,
  diffGraphHref,
  emptyCanvas,
  groupedElements,
  groupKeyOf,
  lapseGroupsStep,
  lapseSteps,
  quadIds,
  seqOf,
  stepAnnouncement,
  stepDelay,
  tooLarge,
  withContext,
  withCounts,
  type Canvas,
} from './diff-graph';
import { MAX_GHOSTS } from './explore-time';

const EX = 'http://example.org/';
const n = (l: string) => `<${EX}${l}>`;
const q = (op: '+' | '-', s: string, p: string, o: string): DiffQuad => ({
  op,
  subject: n(s),
  predicate: n(p),
  object: o.startsWith('"') ? o : n(o),
  graph: null,
});
const all = { added: true, removed: true, context: true, literals: true };
const label = (t: Term) => (typeof t.value === 'string' ? t.value.replace(EX, '') : '');
const short = (i: string) => i.replace(EX, 'ex:');
const marks = (c: Canvas) =>
  Object.fromEntries([...c.nodes.values()].map((x) => [label(x.term), x.change]));

describe('the diff graph', () => {
  it('draws added triples and removed ones as ghosts, and nothing else (A11)', () => {
    const { canvas } = diffCanvas([
      q('-', 'alice', 'knows', 'bob'),
      q('+', 'alice', 'knows', 'carol'),
    ]);
    const { nodes, edges } = canvasElements(canvas, all, label, short);
    expect(edges.map((e) => [e.label, e.change])).toEqual([
      ['ex:knows', 'removed'],
      ['ex:knows', 'added'],
    ]);
    // alice keeps an added edge; bob has only the removed one
    expect(marks(canvas)).toEqual({ alice: 'added', bob: 'removed', carol: 'added' });
    expect(nodes.find((x) => x.label === 'bob')?.change).toBe('removed');
    // hiding removals hides the ghost and its end
    const only = canvasElements(canvas, { ...all, removed: false }, label, short);
    expect(only.nodes.map((x) => x.label).sort()).toEqual(['alice', 'carol']);
  });

  it('keeps what earlier steps of a time-lapse drew without the context toggle', () => {
    const one = applyStep(emptyCanvas(), [q('+', 'alice', 'name', '"Alice"')]).canvas;
    const two = applyStep(one, [q('+', 'bob', 'name', '"Bob"')]).canvas;
    const off = { ...all, context: false };
    const drawn = canvasElements(two, off, label, short);
    expect(drawn.edges.map((e) => e.change ?? 'kept')).toEqual(['kept', 'added']);
    expect(drawn.nodes.map((x) => x.label).sort()).toEqual(['Alice', 'Bob', 'alice', 'bob']);
  });

  it('keeps literal ends apart per subject and predicate, and can hide them', () => {
    const { canvas } = diffCanvas([q('+', 'a', 'name', '"x"'), q('+', 'b', 'name', '"x"')]);
    expect(canvas.nodes.size).toBe(4);
    const hidden = canvasElements(canvas, { ...all, literals: false }, label, short);
    expect(hidden.nodes).toEqual([]);
  });

  it('adds up to 20 unchanged edges per changed resource as context', () => {
    const { canvas } = diffCanvas([q('+', 'alice', 'knows', 'carol')]);
    const ctx = new Map([
      [
        `${EX}alice`,
        Array.from({ length: 30 }, (_, i) => ({
          s: { type: 'uri' as const, value: `${EX}alice` },
          p: `${EX}likes`,
          o: { type: 'uri' as const, value: `${EX}x${i}` },
        })),
      ],
    ]);
    const c = withContext(canvas, ctx);
    expect([...c.edges.values()].filter((e) => e.change === 'kept')).toHaveLength(20);
    // context shows only when asked for
    expect(canvasElements(c, { ...all, context: false }, label, short).edges).toHaveLength(1);
    // a changed edge keeps its mark
    expect(c.edges.get(quadIds(q('+', 'alice', 'knows', 'carol')).edge)?.change).toBe('added');
  });

  it('lists the changed resources for the context reads', () => {
    expect(changedResources([q('+', 'a', 'p', 'b'), q('-', 'a', 'name', '"x"')])).toEqual([
      `${EX}a`,
      `${EX}b`,
    ]);
  });
});

describe('the time-lapse', () => {
  it('applies one step at a time and lets ghosts expire', () => {
    let s = applyStep(emptyCanvas(), [q('+', 'alice', 'knows', 'bob')]);
    expect(marks(s.canvas)).toEqual({ alice: 'added', bob: 'added' });
    s = applyStep(s.canvas, [q('+', 'bob', 'name', '"Bob"')]);
    expect(marks(s.canvas)).toMatchObject({ alice: 'kept', bob: 'kept' });
    s = applyStep(s.canvas, [q('-', 'alice', 'knows', 'bob'), q('+', 'alice', 'knows', 'carol')]);
    expect(s.canvas.edges.get(quadIds(q('+', 'alice', 'knows', 'bob')).edge)?.change).toBe(
      'removed',
    );
    // bob still has his name: he stays
    expect(marks(s.canvas).bob).toBe('kept');
    expect([s.canvas.added, s.canvas.removed, s.canvas.step]).toEqual([3, 1, 3]);
    // the ghost goes at the next step
    const next = applyStep(s.canvas, []);
    expect(next.canvas.edges.has(quadIds(q('+', 'alice', 'knows', 'bob')).edge)).toBe(false);
    // unless ghosts are kept
    const kept = applyStep(s.canvas, [], { keepGhosts: true });
    expect(kept.canvas.edges.get(quadIds(q('+', 'alice', 'knows', 'bob')).edge)?.change).toBe(
      'removed',
    );
  });

  it('turns a node whose every edge is a ghost into a ghost, and revives it', () => {
    let s = applyStep(emptyCanvas(), [q('+', 'a', 'p', 'b')]);
    s = applyStep(s.canvas, [q('-', 'a', 'p', 'b')]);
    expect(marks(s.canvas)).toEqual({ a: 'removed', b: 'removed' });
    s = applyStep(s.canvas, [q('+', 'a', 'p', 'b')], { keepGhosts: true });
    expect(marks(s.canvas)).toEqual({ a: 'added', b: 'added' });
  });

  it('keeps at most MAX_GHOSTS ghosts, the newest first', () => {
    const adds = Array.from({ length: 150 }, (_, i) => q('+', `s${i}`, 'p', `o${i}`));
    let s = applyStep(emptyCanvas(), adds);
    s = applyStep(
      s.canvas,
      adds.map((x) => ({ ...x, op: '-' as const })),
      { keepGhosts: true },
    );
    const ghosts =
      [...s.canvas.nodes.values()].filter((x) => x.change === 'removed').length +
      [...s.canvas.edges.values()].filter((x) => x.change === 'removed').length;
    expect(ghosts).toBeLessThanOrEqual(MAX_GHOSTS);
    expect(s.droppedGhosts).toBeGreaterThan(0);
    // an edge never outlives its ends
    for (const e of s.canvas.edges.values()) {
      expect(s.canvas.nodes.has(e.source) && s.canvas.nodes.has(e.target)).toBe(true);
    }
  });

  it('stops at the node cap and asks for grouping', () => {
    const s = applyStep(emptyCanvas(), [q('+', 'a', 'p', 'b')], { maxNodes: 2 });
    expect(s.overflow).toBe(false);
    const over = applyStep(s.canvas, [q('+', 'c', 'p', 'd')], { maxNodes: 2 });
    expect(over.overflow).toBe(true);
    expect(over.canvas).toBe(s.canvas);
  });

  it('counts the ends a removal draws toward the node cap', () => {
    const s = applyStep(emptyCanvas(), [q('+', 'a', 'p', 'b')], { maxNodes: 3 });
    // removing a triple that is not drawn draws its two ends as ghosts
    const over = applyStep(s.canvas, [q('-', 'c', 'p', 'd')], { maxNodes: 3, keepGhosts: true });
    expect(over.overflow).toBe(true);
    expect(over.canvas).toBe(s.canvas);
    const fits = applyStep(s.canvas, [q('-', 'a', 'p', 'b')], { maxNodes: 3 });
    expect(fits.overflow).toBe(false);
  });

  it('steps commit by commit, or by bucket over long ranges', () => {
    expect(lapseSteps(0, 3)).toEqual([
      { from: 0, to: 1 },
      { from: 1, to: 2 },
      { from: 2, to: 3 },
    ]);
    expect(lapseSteps(3, 3)).toEqual([]);
    // 100,000 commits take at most 200 steps
    const buckets = Array.from({ length: 200 }, (_, i) => ({
      firstSeq: i * 500 + 1,
      lastSeq: (i + 1) * 500,
    }));
    const steps = lapseSteps(0, 100_000, buckets);
    expect(steps).toHaveLength(200);
    expect(steps[0]).toEqual({ from: 0, to: 500 });
    expect(steps.at(-1)).toEqual({ from: 99_500, to: 100_000 });
    // a range inside the buckets is clipped to it
    expect(lapseSteps(250, 1200, buckets)).toEqual([
      { from: 250, to: 500 },
      { from: 500, to: 1000 },
      { from: 1000, to: 1200 },
    ]);
    // without buckets, even spans
    expect(lapseSteps(0, BUCKET_ABOVE * 4).length).toBeLessThanOrEqual(200);
  });

  it('waits for the step and announces it', () => {
    expect(stepDelay(10, 30)).toBe(70);
    expect(stepDelay(1, 2000)).toBe(0);
    expect(stepAnnouncement({ from: 240, to: 241 }, 12, 1)).toBe('commit 241, 12 added, 1 removed');
    expect(stepAnnouncement({ from: 0, to: 500 }, 1200, 0)).toBe(
      'commits 1 to 500, 1,200 added, 0 removed',
    );
  });
});

describe('the grouped diff as classes', () => {
  const g = (groups: DiffGroup[]): GroupedDiff => ({
    dataset: 'ds',
    datasetId: 'x',
    from: { selector: '3', commit: { seq: 3 } as never },
    to: { selector: '4', commit: { seq: 4 } as never },
    added: groups.reduce((n, x) => n + x.added, 0),
    removed: groups.reduce((n, x) => n + x.removed, 0),
    method: 'log',
    group: 'class',
    groupCount: groups.length,
    truncated: false,
    groups,
  });
  const T = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';

  it('draws one edge from Person to Org labelled worksFor +600 (A12)', () => {
    const e = groupedElements(
      g([
        {
          subjectClass: `${EX}Person`,
          predicate: `${EX}worksFor`,
          object: { class: `${EX}Org` },
          added: 600,
          removed: 0,
        },
        {
          subjectClass: `${EX}Person`,
          predicate: T,
          object: { class: `${EX}Person` },
          added: 600,
          removed: 0,
        },
        {
          subjectClass: `${EX}Person`,
          predicate: `${EX}name`,
          object: { literal: 'http://www.w3.org/2001/XMLSchema#string' },
          added: 3,
          removed: 1,
        },
        {
          subjectClass: null,
          predicate: `${EX}knows`,
          object: { iri: true },
          added: 0,
          removed: 2,
        },
      ]),
      short,
    );
    const between = e.edges.filter(
      (x) => x.source === `c:${EX}Person` && x.target === `c:${EX}Org`,
    );
    expect(between.map((x) => [x.label, x.change])).toEqual([['ex:worksFor +600', 'added']]);
    // the type changes count on the class, without an edge to itself
    const person = e.nodes.find((x) => x.id === `c:${EX}Person`);
    expect(person?.label).toBe('ex:Person +600');
    expect(e.edges.some((x) => x.isType)).toBe(false);
    expect(e.edges.find((x) => x.title === `${EX}name`)?.change).toBe('kept');
    expect(e.nodes.find((x) => x.id === 'c:')?.label).toBe('No class');
    expect(e.edges.find((x) => x.target === 'i:')?.change).toBe('removed');
    expect(classOfKey(`c:${EX}Person`)).toBe(`${EX}Person`);
    expect(classOfKey('c:')).toBeNull();
    expect(classOfKey('i:')).toBeUndefined();
  });

  it('writes counts and links', () => {
    expect(countsText(600, 0)).toBe('+600');
    expect(countsText(0, 4)).toBe('−4');
    expect(countsText(3, 1)).toBe('+3 −1');
    expect(diffGraphHref('/ui/explore', 'ds', 'commit:241', 'commit:240', { branch: 'dev' })).toBe(
      '/ui/explore?ds=ds&diff=commit%3A241&diffFrom=commit%3A240&diffBranch=dev',
    );
    expect(seqOf('commit:42')).toBe(42);
    expect(seqOf('7')).toBe(7);
    expect(seqOf('head')).toBeNull();
  });
});

describe('the sizes of steps', () => {
  it('takes the counts of commits and buckets from the catalog', () => {
    const steps = withCounts(lapseSteps(2, 4), [
      { seq: 3, inserted: 2, deleted: 1 },
      { seq: 4, inserted: 1200, deleted: 0 },
    ]);
    expect(steps).toEqual([
      { from: 2, to: 3, changes: 3 },
      { from: 3, to: 4, changes: 1200 },
    ]);
    expect(steps.map(tooLarge)).toEqual([false, true]);
    // a caller limited to some graphs gets no counts: the step is read and judged then
    expect(withCounts(lapseSteps(2, 3), [{ seq: 3 }])).toEqual([{ from: 2, to: 3 }]);
    expect(tooLarge({ from: 2, to: 3 })).toBe(false);
    const buckets = [
      { firstSeq: 1, lastSeq: 600, inserted: 10, deleted: 2 },
      { firstSeq: 601, lastSeq: 1200, inserted: 5, deleted: 0 },
    ];
    expect(lapseSteps(0, 1200, buckets)).toEqual([
      { from: 0, to: 600, changes: 12 },
      { from: 600, to: 1200, changes: 5 },
    ]);
  });
});

describe('changes between graphs', () => {
  it('does not draw a changed triple of a named graph again as context', () => {
    const t = { ...q('+', 'alice', 'knows', 'bob'), graph: `<${EX}g1>` };
    const { canvas } = diffCanvas([t]);
    const s: Term = { type: 'uri', value: `${EX}alice` };
    const o: Term = { type: 'uri', value: `${EX}bob` };
    const c = withContext(canvas, new Map([[`${EX}alice`, [{ s, p: `${EX}knows`, o }]]]));
    expect(c.edges.size).toBe(1);
  });

  it('draws a move between graphs as a removal and an addition', () => {
    const t = q('+', 'alice', 'knows', 'bob');
    const { canvas } = diffCanvas([
      { ...t, op: '-', graph: `<${EX}g1>` },
      { ...t, graph: `<${EX}g2>` },
    ]);
    const { edges } = canvasElements(canvas, all, label, short);
    expect(edges.map((e) => [e.change, e.title]).sort()).toEqual([
      ['added', `${EX}knows in <${EX}g2>`],
      ['removed', `${EX}knows in <${EX}g1>`],
    ]);
  });
});

describe('a grouped time-lapse', () => {
  const works = (added: number, removed: number): DiffGroup => ({
    subjectClass: `${EX}Person`,
    predicate: `${EX}worksFor`,
    object: { class: `${EX}Org` },
    added,
    removed,
  });

  it('keeps net counts, marks the step and lets a group go at nothing', () => {
    let s = lapseGroupsStep(new Map(), [works(600, 0)], false);
    expect(s.groups).toEqual([works(600, 0)]);
    // a step that does not touch it: drawn without a mark
    s = lapseGroupsStep(s.live, [], false);
    expect(s.groups).toEqual([works(0, 0)]);
    // all removed: a ghost for its step, then gone
    s = lapseGroupsStep(s.live, [works(0, 600)], false);
    expect(s.groups).toEqual([works(0, 600)]);
    s = lapseGroupsStep(s.live, [], false);
    expect(s.groups).toEqual([]);
    expect(s.live.size).toBe(0);
    // Keep ghosts keeps it as a ghost
    let k = lapseGroupsStep(new Map(), [works(5, 0)], true);
    k = lapseGroupsStep(k.live, [works(0, 5)], true);
    k = lapseGroupsStep(k.live, [], true);
    expect(k.groups).toEqual([works(0, 5)]);
    expect(groupKeyOf(works(1, 0))).toBe(groupKeyOf(works(0, 9)));
    // removing triples from before the start does not cancel later additions
    let r = lapseGroupsStep(new Map(), [works(0, 600)], false);
    r = lapseGroupsStep(r.live, [works(600, 0)], false);
    r = lapseGroupsStep(r.live, [], false);
    expect(r.groups).toEqual([works(0, 0)]);
  });

  it('keeps at most `max` groups, ghosts first out', () => {
    const g = (i: number, a: number, r: number): DiffGroup => ({
      ...works(a, r),
      predicate: `${EX}p${i}`,
    });
    let s = lapseGroupsStep(new Map(), [g(1, 0, 3), g(2, 4, 0), g(3, 2, 0)], true, 2);
    expect(s.live.size).toBe(2);
    expect(s.groups.map((x) => x.predicate).sort()).toEqual([`${EX}p2`, `${EX}p3`]);
    s = lapseGroupsStep(s.live, [], true, 2);
    expect(s.groups).toHaveLength(2);
  });
});

describe('the grouped picture', () => {
  it('stays within the node cap, the largest groups first', () => {
    const groups: DiffGroup[] = Array.from({ length: 1000 }, (_, i) => ({
      subjectClass: `${EX}S${i}`,
      predicate: `${EX}p`,
      object: { class: `${EX}O${i}` },
      added: 1000 - i,
      removed: 0,
    }));
    const el = groupedElements({ groups }, short);
    expect(el.nodes.length).toBeLessThanOrEqual(1500);
    expect(el.omitted).toBe(250);
    expect(el.nodes[0].label).toMatch(/S0/);
    expect(groupedElements({ groups: groups.slice(0, 3) }, short).omitted).toBe(0);
  });
});

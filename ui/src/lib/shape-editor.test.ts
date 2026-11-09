import { afterEach, describe, expect, it, vi } from 'vitest';
import type { SparklesResult, Term } from './api';
import { boundedText, query, shacl } from './api';
import { revertCommit, runPatch } from './ontology';
import { iri, WELL_KNOWN } from './rdf';
import {
  definitionProblems,
  delta,
  FIELDS,
  graphQuery,
  graphResult,
  id,
  inputTerm,
  literal,
  MAX_BYTES,
  MAX_PATH_NODES,
  MAX_TREE_REFERENCES,
  MAX_TRIPLES,
  parseIri,
  PATH_OPS,
  RDF,
  sampleNodes,
  SH,
  ShapeDraft,
  ShapeGraph,
  shapePatch,
  shapeTree,
  XSD,
  type ShapePath,
  type Triple,
} from './shape-editor';

const ex = 'http://example.org/';
const prefixes = { ...WELL_KNOWN, ex };
const s = iri(ex + 'Shape'),
  p = iri(ex + 'p');
const blank = (value: string): Term => ({ type: 'bnode', value });
const triple = (s: Term, p: string, o: Term): Triple => [s, iri(p), o];
const branchId = '01234567-89ab-cdef-0123-456789abcdef';
const result = (triples: Triple[]): SparklesResult => ({
  queryType: 'SELECT',
  vars: ['s', 'p', 'o'],
  rows: triples,
  meta: {
    totalRows: triples.length,
    sentRows: triples.length,
    commit: 7,
    timing: {} as never,
    plan: {} as never,
  },
});
afterEach(() => vi.unstubAllGlobals());

describe('complete graph reads', () => {
  it('addresses the stored default explicitly and pins named graph reads without injection', () => {
    expect(graphQuery(null)).toContain('GRAPH <urn:x-arq:DefaultGraph>');
    expect(graphQuery(ex + 'shapes')).toContain(`GRAPH <${ex}shapes>`);
    expect(graphQuery('urn:x> } #')).toContain('\\u003e');
    expect(graphQuery(null)).toContain(`LIMIT ${MAX_TRIPLES + 1}`);
  });
  it('accepts empty and complete graphs and refuses sent-row truncation or the sentinel', () => {
    expect(graphResult(result([])).triples).toEqual([]);
    const one = triple(s, RDF + 'type', iri(SH + 'NodeShape'));
    expect(graphResult(result([one])).triples).toEqual([one]);
    const partial = result([one]);
    partial.meta.totalRows = 2;
    expect(() => graphResult(partial)).toThrow('incomplete');
    partial.meta.totalRows = 1;
    partial.meta.sentRows = 0;
    expect(() => graphResult(partial)).toThrow('incomplete');
    expect(() => graphResult(result(Array.from({ length: MAX_TRIPLES + 1 }, () => one)))).toThrow(
      'exceeded',
    );
    expect(() => new ShapeGraph([triple(s, ex + 'large', literal('x'.repeat(MAX_BYTES)))])).toThrow(
      '2 MiB',
    );
  });
  it('cancels an oversized response stream before decoding JSON without relying on Content-Length', async () => {
    let canceled = false;
    const stream = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(new Uint8Array(4));
        c.enqueue(new Uint8Array(5));
      },
      cancel() {
        canceled = true;
      },
    });
    const response = new Response(stream);
    const json = vi.spyOn(response, 'json');
    await expect(boundedText(response, 8)).rejects.toThrow('8 byte');
    expect(canceled).toBe(true);
    expect(json).not.toHaveBeenCalled();
  });
  it('uses the byte bound in the real typed query request before materializing its body', async () => {
    const response = new Response('x'.repeat(50));
    const json = vi.spyOn(response, 'json');
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => response),
    );
    await expect(query('ds@edit', graphQuery(null), { maxResponseBytes: 40 })).rejects.toThrow(
      '40 byte',
    );
    expect(json).not.toHaveBeenCalled();
    await expect(boundedText(new Response('é'), 2)).resolves.toBe('é');
    await expect(boundedText(new Response('é'), 1)).rejects.toThrow('1 byte');
  });
});

describe('RDF shape projection and exact deltas', () => {
  const property = blank('b17');
  const base = new ShapeGraph([
    triple(s, RDF + 'type', iri(SH + 'NodeShape')),
    triple(s, SH + 'targetClass', iri(ex + 'Person')),
    triple(s, SH + 'property', property),
    triple(property, SH + 'path', p),
    triple(property, SH + 'minCount', literal('1', XSD + 'integer')),
    triple(s, ex + 'custom', blank('b18')),
    triple(blank('b18'), ex + 'program', literal('unchanged')),
    triple(iri(ex + 'unrelated'), ex + 'p', literal('keep', XSD + 'string')),
  ]);
  it('finds implicit and anonymous shapes and keeps custom RDF and unrelated triples exact', () => {
    expect(base.shapes().map(id)).toEqual(expect.arrayContaining([id(s), id(property)]));
    expect(base.links(s)).toEqual([{ term: property, property: 'property' }]);
    const changed = base.replace(property, SH + 'minCount', [literal('2', XSD + 'integer')]);
    const d = delta(base, changed);
    expect(d.added).toEqual([triple(property, SH + 'minCount', literal('2', XSD + 'integer'))]);
    expect(d.removed).toEqual([triple(property, SH + 'minCount', literal('1', XSD + 'integer'))]);
    expect(changed.triples.filter((t) => String(t[1].value).startsWith(ex))).toEqual(
      base.triples.filter((t) => String(t[1].value).startsWith(ex)),
    );
    expect(base.triples).toHaveLength(8);
  });
  it('keeps shared lists and unknown annotations when a field is replaced', () => {
    const list = base.withList([literal('one'), literal('two')]);
    const g = list.graph.add(
      triple(s, SH + 'in', list.root),
      triple(property, SH + 'in', list.root),
    );
    const changed = g.setList(s, SH + 'in', list.root, [literal('three')]);
    expect(changed.list(list.root).map((t) => t.value)).toEqual(['one', 'two']);
    expect(changed.values(property, SH + 'in')).toEqual([list.root]);
    const annotated = g
      .add(triple(list.root, ex + 'annotation', literal('keep')))
      .setList(property, SH + 'in', list.root, null)
      .setList(s, SH + 'in', list.root, null);
    expect(annotated.values(list.root, ex + 'annotation')).toEqual([literal('keep')]);
    expect(annotated.list(list.root)).toHaveLength(2);
  });
  it('collects only detached unannotated list/path structures and refuses destructive referenced deletion', () => {
    const a = base.withList([literal('one'), literal('two')]);
    const g = a.graph.add(triple(s, SH + 'in', a.root));
    expect(delta(base, g.setList(s, SH + 'in', a.root, null))).toEqual({ added: [], removed: [] });
    expect(() => base.remove(property)).toThrow('reference');
    const unlinked = base.replace(s, SH + 'property', []);
    expect(unlinked.remove(property).triples).toEqual(
      unlinked.triples.filter((t) => id(t[0]) !== id(property)),
    );
  });
  it('discovers logical references, standalone shapes, self cycles and implicit targets', () => {
    const logical = base.withList([s, iri(ex + 'Other')]);
    const g = logical.graph.add(
      triple(s, SH + 'or', logical.root),
      triple(iri(ex + 'Implicit'), SH + 'targetSubjectsOf', p),
      triple(property, SH + 'node', s),
    );
    expect(g.shapes().map(id)).toEqual(
      expect.arrayContaining([
        id(s),
        id(property),
        id(iri(ex + 'Other')),
        id(iri(ex + 'Implicit')),
      ]),
    );
    expect(
      g
        .links(s)
        .filter((l) => l.property === 'or')
        .map((l) => id(l.term)),
    ).toEqual([id(s), id(iri(ex + 'Other'))]);
    const tree = shapeTree(g);
    expect(tree.nodes.size).toBeLessThanOrEqual(g.shapes().length + g.triples.length);
    expect(
      [...tree.nodes].filter(([, row]) => row.reference).every(([key]) => !tree.children.has(key)),
    ).toBe(true);
  });
  it('creates fresh node/property shapes and refuses identity reuse even through an object or predicate', () => {
    const node = base.create('node');
    const child = node.graph.create('property', node.graph.fresh(), node.term);
    expect(child.graph.values(node.term, SH + 'property')).toEqual([child.term]);
    expect(() => base.create('node', p)).toThrow('already used');
    expect(() => base.create('node', iri(ex + 'custom'))).toThrow('already used');
  });
  it('pins the exact branch head and selected graph and preserves stored blank labels in both directions', () => {
    const changed = base.replace(property, SH + 'minCount', [literal('2', XSD + 'integer')]);
    const patch = shapePatch(base, changed, ex + 'shapes', branchId, 7);
    expect(patch).toContain(`H prev <urn:uuid:${branchId}#commit:7> .`);
    expect(patch).toContain('D <_:b17>');
    expect(patch).toContain('A <_:b17>');
    expect(
      patch
        .split('\n')
        .filter((l) => /^[AD] /.test(l))
        .every((l) => l.endsWith(`<${ex}shapes> .`)),
    ).toBe(true);
    expect(() => shapePatch(base, changed, null, 'wrong', 7)).toThrow('identity');
    expect(() => shapePatch(base, changed, null, branchId, NaN)).toThrow('identity');
    const created = base.create('node');
    expect(shapePatch(base, created.graph, null, branchId, 7)).toContain(
      `A _:${created.term.value}`,
    );
  });
  it('exports the whole graph as Turtle while keeping language direction and escaped IRIs', () => {
    const g = base.add(
      triple(s, SH + 'message', {
        type: 'literal',
        value: 'مرحبا',
        'xml:lang': 'ar',
        'its:dir': 'rtl',
      }),
    );
    expect(g.turtle()).toContain('"مرحبا"@ar--rtl');
    expect(g.turtle()).toContain('_:b17');
    expect(g.turtle()).toContain(`<${ex}custom>`);
  });
});

describe('structured paths', () => {
  const predicate: ShapePath = { kind: 'predicate', iri: ex + 'p' };
  it.each(PATH_OPS)('round trips %s with nested operands', (kind) => {
    const path: ShapePath =
      kind === 'predicate'
        ? predicate
        : {
            kind,
            children:
              kind === 'sequence' || kind === 'alternativePath'
                ? [predicate, { kind: 'inversePath', children: [predicate] }]
                : [{ kind: 'sequence', children: [predicate, predicate] }],
          };
    const g = new ShapeGraph().setPath(s, path);
    expect(g.path(g.one(s, SH + 'path'))).toEqual(path);
    const changed = g.setPath(s, predicate);
    expect(changed.triples).toEqual([triple(s, SH + 'path', iri(ex + 'p'))]);
  });
  it('bounds the expansion of shared acyclic path operands', () => {
    // Each level is a sequence whose two members are the same node of the level below,
    // so 18 levels describe 2^19 - 1 steps in 72 triples.
    let below: Term = p;
    const triples: Triple[] = [];
    for (let i = 0; i < 18; i++) {
      const a = blank(`l${i}a`),
        b = blank(`l${i}b`);
      triples.push(
        triple(a, RDF + 'first', below),
        triple(a, RDF + 'rest', b),
        triple(b, RDF + 'first', below),
        triple(b, RDF + 'rest', iri(RDF + 'nil')),
      );
      below = a;
    }
    const g = new ShapeGraph(triples);
    expect(() => g.path(below)).toThrow(`more than ${MAX_PATH_NODES} steps`);
    expect(g.path(blank('l2a'))).toMatchObject({ kind: 'sequence' });
  });
  it('bounds only the final graph when a path or list is replaced at the triple limit', () => {
    const seq: ShapePath = { kind: 'sequence', children: [predicate, predicate] };
    const filler = (n: number) =>
      Array.from({ length: n }, (_, i) => triple(iri(`${ex}f${i}`), ex + 'v', literal('x')));
    const withPath = new ShapeGraph().setPath(s, seq);
    const full = new ShapeGraph([
      ...withPath.triples,
      ...filler(MAX_TRIPLES - withPath.triples.length),
    ]);
    expect(full.triples).toHaveLength(MAX_TRIPLES);
    const replaced = full.setPath(s, seq);
    expect(replaced.triples).toHaveLength(MAX_TRIPLES);
    expect(replaced.path(replaced.one(s, SH + 'path'))).toEqual(seq);
    const list = new ShapeGraph().withList([literal('a'), literal('b')]);
    const listed = list.graph.add(triple(s, SH + 'in', list.root));
    const fullList = new ShapeGraph([
      ...listed.triples,
      ...filler(MAX_TRIPLES - listed.triples.length),
    ]);
    const relisted = fullList.setList(s, SH + 'in', list.root, [literal('c'), literal('d')]);
    expect(relisted.triples).toHaveLength(MAX_TRIPLES);
    expect(() =>
      fullList.setList(s, SH + 'in', list.root, [literal('c'), literal('d'), literal('e')]),
    ).toThrow(`exceeds ${MAX_TRIPLES} triples`);
  });
  it('does not remove a shared path or its annotations', () => {
    const a = new ShapeGraph().withPath({ kind: 'inversePath', children: [predicate] });
    const g = a.graph.add(triple(s, SH + 'path', a.root), triple(p, SH + 'path', a.root));
    const changed = g.setPath(s, predicate);
    expect(changed.path(a.root)).toEqual({ kind: 'inversePath', children: [predicate] });
  });
  it('refuses cycles, ambiguity, malformed lists, insufficient operands and excessive nesting', () => {
    const b = blank('cycle');
    expect(() => new ShapeGraph([triple(b, SH + 'inversePath', b)]).path(b)).toThrow('cyclic');
    expect(() =>
      new ShapeGraph([triple(b, SH + 'inversePath', p), triple(b, SH + 'oneOrMorePath', p)]).path(
        b,
      ),
    ).toThrow('competing');
    expect(() =>
      new ShapeGraph([triple(b, RDF + 'first', p), triple(b, RDF + 'rest', b)]).list(b),
    ).toThrow('cyclic');
    expect(() => new ShapeGraph().withPath({ kind: 'sequence', children: [predicate] })).toThrow(
      'at least two',
    );
    let deep = predicate;
    for (let i = 0; i < 66; i++) deep = { kind: 'inversePath', children: [deep] };
    expect(() => new ShapeGraph().withPath(deep)).toThrow('64 levels');
  });
});

describe('typed SHACL parameters and drafts', () => {
  it('covers every SHACL Recommendation Core parameter and the supported draft list constraints', () => {
    const core =
      'class datatype nodeKind minCount maxCount minExclusive minInclusive maxExclusive maxInclusive minLength maxLength pattern flags languageIn uniqueLang equals disjoint lessThan lessThanOrEquals and or xone not node qualifiedValueShape qualifiedMinCount qualifiedMaxCount qualifiedValueShapesDisjoint closed ignoredProperties hasValue in';
    expect(FIELDS.filter((f) => f.group === 'Constraints').map((f) => f.name)).toEqual(
      expect.arrayContaining([
        ...core.split(' '),
        'memberShape',
        'minListLength',
        'maxListLength',
        'uniqueMembers',
      ]),
    );
    expect(FIELDS.filter((f) => f.group === 'Targets').map((f) => f.name)).toEqual(
      expect.arrayContaining(['targetClass', 'targetNode', 'targetSubjectsOf', 'targetObjectsOf']),
    );
  });
  it('validates typed values without changing lexical content, language, datatype or direction', () => {
    expect(parseIri('ex:Person', prefixes)).toBe(ex + 'Person');
    expect(inputTerm('integer', '0002', prefixes)).toEqual(literal('0002', XSD + 'integer'));
    expect(inputTerm('decimal', '-0.50', prefixes)).toEqual(literal('-0.50', XSD + 'decimal'));
    expect(inputTerm('text', 'bonjour', prefixes, 'literal', '@fr')).toEqual({
      type: 'literal',
      value: 'bonjour',
      'xml:lang': 'fr',
    });
    expect(inputTerm('text', 'مرحبا', prefixes, 'literal', '@ar', 'rtl')).toHaveProperty(
      'its:dir',
      'rtl',
    );
    expect(inputTerm('term', '2026-01-02', prefixes, 'literal', 'xsd:date')).toEqual(
      literal('2026-01-02', XSD + 'date'),
    );
    expect(inputTerm('shape', '_:b17', prefixes, 'bnode')).toEqual(blank('b17'));
    expect(() => inputTerm('integer', '-1', prefixes)).toThrow('nonnegative');
    expect(() => inputTerm('boolean', 'yes', prefixes)).toThrow('true or false');
    expect(() => inputTerm('text', 'x', prefixes, 'literal', '@bad tag')).toThrow('language');
    expect(() => parseIri('relative', prefixes)).toThrow('absolute');
  });
  it('keeps a bounded local undo stack against the original source and clears redo after an edit', () => {
    const basis = new ShapeGraph();
    let draft = new ShapeDraft(basis);
    for (let i = 0; i < 40; i++)
      draft = draft.change(draft.graph.replace(s, SH + 'name', [literal(String(i))]));
    expect(draft.states).toHaveLength(30);
    expect(draft.basis).toBe(basis);
    expect(draft.dirty).toBe(true);
    const undone = draft.undo();
    expect(undone.redo().graph).toBe(draft.graph);
    const edited = undone.change(undone.graph.replace(s, SH + 'name', [literal('new')]));
    expect(edited.redo().graph).toBe(edited.graph);
  });
  it('accepts only bounded, explicit sample resources', () => {
    expect(sampleNodes('ex:a\n_:b17', prefixes)).toEqual([iri(ex + 'a'), blank('b17')]);
    expect(() => sampleNodes('', prefixes)).toThrow('at least one');
    expect(() => sampleNodes(Array(101).fill('ex:a').join('\n'), prefixes)).toThrow('100');
  });
});

describe('typed transport', () => {
  it('reuses the exact patch body for preview and commit and bounds the preview to 50 changes', async () => {
    const fetch = vi.fn(
      async () =>
        new Response(JSON.stringify({ inserted: 1, deleted: 0 }), {
          headers: { 'Content-Type': 'application/json' },
        }),
    );
    vi.stubGlobal('fetch', fetch);
    const g = new ShapeGraph(),
      made = g.create('node');
    const patch = shapePatch(g, made.graph, ex + 'shapes', branchId, 7);
    const ctl = new AbortController();
    await runPatch('ds@draft', patch, { dryRun: true, signal: ctl.signal });
    await runPatch('ds@draft', patch, { message: 'Edit shapes' });
    const [preview, commit] = fetch.mock.calls as unknown as [string, RequestInit][];
    expect(preview[0]).toBe(
      '/ds@draft/patch?dryRun=true&changes=50&' +
        new URLSearchParams({ expect: `urn:uuid:${branchId}#commit:7` }),
    );
    expect(commit[0]).toBe(
      '/ds@draft/patch?receipt=true&' +
        new URLSearchParams({ expect: `urn:uuid:${branchId}#commit:7` }),
    );
    expect(preview[1].body).toBe(patch);
    expect(commit[1].body).toBe(patch);
    expect(preview[1].signal).toBe(ctl.signal);
    expect(new Headers(preview[1].headers).get('Content-Type')).toBe('application/rdf-patch');
  });
  it('pins sample validation to its branch/revision/data graph and uses the inline full RDF draft', async () => {
    const fetch = vi.fn(async () => new Response('{"conforms":true,"results":[]}'));
    vi.stubGlobal('fetch', fetch);
    const text = new ShapeGraph().create('node').graph.turtle();
    await shacl('ds@draft', text, {
      at: '7',
      graph: ex + 'data',
      reasoning: false,
      targets: [iri(ex + 'a'), blank('b17')],
      storedBlankNodes: true,
      timeout: 5,
      maxResponseBytes: 1000,
    });
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
    const u = new URL(url, 'http://example.org');
    expect(u.pathname).toBe('/ds@draft/shacl');
    expect(u.searchParams.get('at')).toBe('7');
    expect(u.searchParams.get('graph')).toBe(ex + 'data');
    expect(u.searchParams.getAll('target')).toEqual([`<${ex}a>`, '_:b17']);
    expect(init.body).toBe(text);
    expect(u.searchParams.get('storedBlankNodes')).toBe('true');
  });
});

describe('preserved definition diagnostics', () => {
  it('preserves opaque stored values during unrelated edits but refuses newly invalid fields', () => {
    const expression = blank('expression');
    const basis = new ShapeGraph([
      triple(s, RDF + 'type', iri(SH + 'NodeShape')),
      triple(s, SH + 'deactivated', expression),
      triple(expression, ex + 'custom', literal('keep')),
    ]);
    const edited = basis.replace(s, SH + 'name', [literal('renamed')]);
    expect(definitionProblems(edited).some((p) => p.includes('deactivated'))).toBe(true);
    expect(definitionProblems(edited, basis)).toEqual([]);
    expect(delta(basis, edited).removed).toEqual([]);
    const invalid = edited.replace(s, SH + 'deactivated', [literal('wrong')]);
    expect(definitionProblems(invalid, basis).some((p) => p.includes('deactivated'))).toBe(true);
  });
  it('requires a path on new property shapes even when a stored graph contains warnings', () => {
    const basis = new ShapeGraph();
    const made = basis.create('property');
    expect(definitionProblems(made.graph, basis).length).toBeGreaterThan(0);
  });
  it('carries the captured source UUID and head through revert preview and commit', async () => {
    const fetch = vi.fn(async () => new Response('{"changes":{"inserted":1,"deleted":0}}'));
    vi.stubGlobal('fetch', fetch);
    await revertCommit('ds@draft', 7, { dryRun: true, head: 8, sourceId: branchId });
    await revertCommit('ds@draft', 7, { head: 8, sourceId: branchId });
    for (const [, init] of fetch.mock.calls as unknown as [string, RequestInit][]) {
      expect(JSON.parse(String(init.body)).expect).toEqual({ target: 8, targetId: branchId });
    }
  });
});

describe('shared-list tree projection bounds', () => {
  it('retains every canonical shape and all RDF while bounding repeated reference leaves', () => {
    const members = Array.from({ length: 100 }, (_, n) => iri(ex + `member${n}`));
    const list = new ShapeGraph().withList(members);
    const graph = new ShapeGraph([
      ...list.graph.triples,
      ...Array.from({ length: 100 }, (_, n) =>
        triple(iri(ex + `parent${n}`), SH + 'or', list.root),
      ),
    ]);
    const tree = shapeTree(graph);
    const canonical = [...tree.nodes.values()].filter((row) => !row.reference);
    const references = [...tree.nodes.values()].filter((row) => row.reference);
    expect(new Set(canonical.map((row) => id(row.term)))).toEqual(new Set(graph.shapes().map(id)));
    expect(references.length).toBe(MAX_TREE_REFERENCES);
    expect(tree.omitted).toBeGreaterThan(0);
    expect(tree.nodes.size).toBe(canonical.length + MAX_TREE_REFERENCES);
    expect(graph.list(list.root)).toEqual(members);
    expect(delta(graph, graph)).toEqual({ added: [], removed: [] });
  });
});

describe('authoring source selectors and report identity', () => {
  it('refuses virtual graph selectors while preserving explicit internal stored-default reads', () => {
    const before = new ShapeGraph(),
      after = before.create('node').graph;
    for (const graph of ['urn:x-arq:DefaultGraph', 'urn:x-arq:UnionGraph']) {
      expect(() => graphQuery(graph)).toThrow('not named authoring graphs');
      expect(() => shapePatch(before, after, graph, branchId, 7)).toThrow(
        'not named authoring graphs',
      );
    }
    expect(graphQuery(null)).toContain('GRAPH <urn:x-arq:DefaultGraph>');
    expect(shapePatch(before, after, null, branchId, 7)).toContain('A ');
  });
  it('verifies matching main and branch UUID/commit response headers', async () => {
    for (const headers of [
      { 'Sparkles-Dataset-Id': branchId, 'Sparkles-Commit': '7' },
      {
        'Sparkles-Dataset-Id': '00000000-0000-4000-8000-000000000002',
        'Sparkles-Branch-Id': branchId,
        'Sparkles-Commit': '7',
      },
    ] as Record<string, string>[]) {
      vi.stubGlobal(
        'fetch',
        vi.fn(async () => new Response('{"conforms":true,"results":[]}', { headers })),
      );
      await expect(
        shacl('ds@draft', '', {
          source: { id: branchId, commit: 7 },
          at: '7',
          maxResponseBytes: 1000,
        }),
      ).resolves.toEqual({ conforms: true, results: [] });
    }
  });
  it('cancels reports with missing or replaced UUID/revision before JSON materialization', async () => {
    for (const headers of [
      { 'Sparkles-Dataset-Id': '00000000-0000-4000-8000-000000000001', 'Sparkles-Commit': '7' },
      { 'Sparkles-Dataset-Id': branchId, 'Sparkles-Commit': '8' },
      { 'Sparkles-Dataset-Id': branchId },
      { 'Sparkles-Commit': '7' },
    ] as Record<string, string>[]) {
      let canceled = false;
      const response = new Response(
        new ReadableStream({
          cancel() {
            canceled = true;
          },
        }),
        { headers },
      );
      const json = vi.spyOn(response, 'json');
      vi.stubGlobal(
        'fetch',
        vi.fn(async () => response),
      );
      await expect(
        shacl('ds@draft', '', {
          source: { id: branchId, commit: 7 },
          at: '7',
          maxResponseBytes: 1000,
        }),
      ).rejects.toThrow('captured source UUID and commit');
      expect(canceled).toBe(true);
      expect(json).not.toHaveBeenCalled();
    }
  });
  it('keeps ordinary validation callers compatible without a source expectation', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('{"conforms":true,"results":[]}')),
    );
    await expect(shacl('ds', '')).resolves.toEqual({ conforms: true, results: [] });
  });
});

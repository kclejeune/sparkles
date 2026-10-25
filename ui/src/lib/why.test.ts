import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Term } from './api';
import {
  explainTriple,
  footer,
  graft,
  how,
  leaves,
  premiseTriples,
  staleNote,
  maxId,
  sameRead,
  tripleKey,
  walk,
  type Derivation,
  type Explanation,
  type WhyTriple,
} from './why';

const EX = 'http://example.org/';
const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const RDFS = 'http://www.w3.org/2000/01/rdf-schema#';
const iri = (v: string): Term => ({ type: 'uri', value: v.startsWith('http') ? v : EX + v });
const t = (s: string, p: string, o: string): WhyTriple => ({ s: iri(s), p: iri(p), o: iri(o) });

/** The tree of A7: rdfs9 over an rdfs2 subtree and an asserted subclass triple. */
const A7: Derivation = {
  id: 1,
  triple: t('alice', RDF_TYPE, 'Employee'),
  kind: 'derived',
  depth: 2,
  rule: { name: 'rdfs9', profile: 'rdfs' },
  premises: [
    {
      id: 2,
      triple: t('alice', RDF_TYPE, 'Manager'),
      kind: 'derived',
      depth: 1,
      rule: { name: 'rdfs2', profile: 'rdfs' },
      premises: [
        {
          id: 3,
          triple: t('alice', 'manages', 'team7'),
          kind: 'asserted',
          depth: 0,
          graph: `${EX}hr`,
        },
        {
          id: 4,
          triple: t('manages', `${RDFS}domain`, 'Manager'),
          kind: 'asserted',
          depth: 0,
          graph: `${EX}ontology`,
        },
      ],
    },
    {
      id: 5,
      triple: t('Manager', `${RDFS}subClassOf`, 'Employee'),
      kind: 'asserted',
      depth: 0,
      graph: `${EX}ontology`,
    },
  ],
};

const explanation = (over: Partial<Explanation> = {}): Explanation => ({
  mode: 'why',
  entailed: true,
  commit: 318,
  source: 'materialized',
  trees: [A7],
  more: false,
  stale: false,
  commitsSince: 0,
  ...over,
});

describe('derivation trees (U10 §2.4)', () => {
  it('lists the premises Show on graph draws, each once, without the explained triple', () => {
    const ps = premiseTriples(A7).map(tripleKey);
    expect(ps).toEqual(
      [
        t('alice', RDF_TYPE, 'Manager'),
        t('alice', 'manages', 'team7'),
        t('manages', `${RDFS}domain`, 'Manager'),
        t('Manager', `${RDFS}subClassOf`, 'Employee'),
      ].map(tripleKey),
    );
    expect(leaves(A7).map((d) => d.id)).toEqual([3, 4, 5]);
  });

  it('says how each node holds: its rule with a gloss, its graph, an axiom or a cut', () => {
    expect(how(A7)).toBe('derived by rdfs9 (subclass)');
    expect(how(A7.premises![0])).toBe('derived by rdfs2 (domain)');
    expect(how(A7.premises![1], () => 'ex:ontology')).toBe('asserted · graph ex:ontology');
    expect(how({ ...A7, kind: 'ref', premises: undefined })).toBe('derived deeper than shown');
    expect(how({ ...A7, kind: 'axiom', rule: { name: 'rule@line 13', profile: 'rdfs' } })).toBe(
      'axiom · rule@line 13',
    );
    expect(how({ ...A7, rule: { name: 'cax-sco', profile: 'owl-rl', owl2rl: 'cax-sco' } })).toBe(
      'derived by cax-sco (subclass)',
    );
    expect(how({ ...A7, shared: true })).toBe('derived by rdfs9 (subclass) · as above');
    expect(how({ ...A7.premises![1], graph: undefined })).toBe('asserted · RDFS schema');
  });

  it('puts an expanded ref in place', () => {
    const cut: Derivation = {
      ...A7,
      premises: [{ ...A7.premises![0], kind: 'ref', premises: undefined }, A7.premises![1]],
    };
    const back = graft(cut, 2, { ...A7.premises![0], id: 1 });
    expect(back.premises![0].kind).toBe('derived');
    expect(back.premises![0].id).toBe(2);
    expect(back.premises![0].premises).toHaveLength(2);
  });

  it("gives an expanded subtree ids of its own, so they never meet the tree's", () => {
    // the expansion's answer numbers its nodes from 1 again, and shares one of them
    const sub: Derivation = {
      ...A7.premises![0],
      id: 1,
      premises: [
        { ...A7.premises![0].premises![0], id: 2 },
        { ...A7.premises![0].premises![1], id: 3 },
        { ...A7.premises![0].premises![0], id: 2, shared: true },
      ],
    };
    const cut: Derivation = {
      ...A7,
      premises: [{ ...A7.premises![0], kind: 'ref', premises: undefined }, A7.premises![1]],
    };
    const back = graft(cut, 2, sub);
    const ids = walk(back)
      .filter((d) => !d.shared)
      .map((d) => d.id);
    expect(new Set(ids).size).toBe(ids.length);
    const g = back.premises![0];
    expect(g.id).toBe(2);
    expect(g.premises![2].id).toBe(g.premises![0].id);
    // a second expansion inside the first still lands on its own node
    const again = graft(back, g.premises![1].id, { ...A7.premises![1], id: 1 });
    expect(again.premises![0].premises![1].triple).toEqual(A7.premises![1].triple);
    expect(again.premises![1].triple).toEqual(A7.premises![1].triple);
    expect(again.premises![0].premises![0].triple).toEqual(A7.premises![0].premises![0].triple);
  });

  it('gives an expanded subtree ids past every tree of the answer', () => {
    const cut: Derivation = {
      ...A7,
      premises: [{ ...A7.premises![0], kind: 'ref', premises: undefined }, A7.premises![1]],
    };
    // the next derivation holds ids 6 to 9, beyond the first tree's
    const other: Derivation = { ...A7, id: 6, premises: [{ ...A7.premises![1], id: 7 }] };
    const floor = maxId([cut, other]);
    expect(floor).toBe(7);
    const back = graft(cut, 2, { ...A7.premises![0], id: 1 }, floor);
    const added = walk(back.premises![0])
      .slice(1)
      .map((d) => d.id);
    expect(added.every((x) => x > 7)).toBe(true);
  });

  it('tells answers that read other inferences apart', () => {
    expect(sameRead(explanation(), explanation())).toBe(true);
    expect(sameRead(explanation(), explanation({ commit: 319 }))).toBe(false);
    expect(sameRead(explanation(), explanation({ source: 'rdfsOnRead' }))).toBe(false);
    expect(sameRead(explanation(), explanation({ readAtHead: true }))).toBe(false);
  });

  it('counts the derivations and notes stale inferences', () => {
    expect(footer(explanation(), 0)).toBe('1 of 1 derivation · depth 2 · from commit 318');
    expect(footer(explanation({ trees: [A7, A7], more: true }), 1)).toBe(
      '2 of 2+ derivations · depth 2 · from commit 318',
    );
    expect(staleNote(explanation())).toBeNull();
    expect(staleNote(explanation({ stale: true, commitsSince: 3 }))).toBe(
      'These inferences are out of date. This tree shows them as computed at commit 318. 3 commits since.',
    );
    expect(staleNote(explanation({ source: 'rdfsOnRead', stale: true }))).toBeNull();
    expect(staleNote(explanation({ readAtHead: true }))).toContain('current data');
    // a stale run whose commit the history no longer has reads the head: say that
    expect(staleNote(explanation({ readAtHead: true, stale: true, commitsSince: 3 }))).toContain(
      'current data',
    );
  });
});

describe('explainTriple', () => {
  afterEach(() => vi.unstubAllGlobals());
  const answer = (status: number, body: unknown) =>
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_url: string, init: RequestInit) => {
        const sent = JSON.parse(String(init.body));
        expect(sent.mode).toBe('why');
        expect(sent.maxTrees).toBe(5);
        expect(sent.triple.s.value).toBe(`${EX}alice`);
        return new Response(JSON.stringify(body), {
          status,
          headers: { 'Content-Type': 'application/json' },
        });
      }),
    );
  const triple = t('alice', RDF_TYPE, 'Employee');

  it('returns the trees', async () => {
    answer(200, explanation());
    const r = await explainTriple('ds', triple);
    expect(r.ok && r.explanation.trees[0].rule?.name).toBe('rdfs9');
  });

  it('reads a hidden or missing derivation as the same answer', async () => {
    answer(404, { error: 'no derivation', code: 'no-visible-derivation' });
    expect(await explainTriple('ds', triple)).toEqual({
      ok: false,
      kind: 'none',
      message: 'No derivation found.',
    });
  });

  it('reads the budget and an older server', async () => {
    answer(422, { error: 'too deep', code: 'reasoning-budget' });
    const b = await explainTriple('ds', triple);
    expect(!b.ok && b.kind).toBe('budget');
    expect(!b.ok && b.message).toBe('The derivation is deeper than the search limit.');
    answer(404, { error: 'Not Found' });
    const u = await explainTriple('ds', triple);
    expect(!u.ok && u.kind).toBe('unavailable');
  });
});

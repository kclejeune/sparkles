// The plan drawn on the pattern (spec U04 §11, plan-pattern.ts): matching by terms and
// occurrence, unmatched operators, merged star joins, the misestimate rule, the width
// scale and the join order, against the built browser module.

import { existsSync, readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import type { PlanNode } from './api';
import {
  drawing,
  filtersOf,
  hash,
  normalized,
  type PatternAnswer,
  type PatternOk,
} from './pattern';
import {
  badge,
  filterLabel,
  flatten,
  label,
  literalNt,
  misestimate,
  overlay,
  pathNt,
  ratioText,
  spoken,
  width,
  type PlanPattern,
} from './plan-pattern';

const EX = 'http://example.org/';
const built = new URL('./wasm/sparkles_fmt_wasm_bg.wasm', import.meta.url);

/** A plan node with the given operator, numbers, patterns and children. */
function node(
  operator: string,
  est: number,
  rows: number,
  extra: Partial<PlanNode> & {
    patterns?: PlanPattern[];
    filterVars?: string[];
    joinVars?: string[];
  } = {},
  children: PlanNode[] = [],
): PlanNode {
  return {
    operator,
    description: '',
    columns: [],
    sortedOn: [],
    estimatedRows: est,
    estimatedCost: est,
    actualRows: rows,
    timeMs: 1,
    cached: false,
    children,
    ...extra,
  };
}

const pat = (s: string, p: string, o: string, occurrence = 0): PlanPattern => ({
  s,
  p: p.startsWith('<') || p.startsWith('?') ? p : `<${EX}${p}>`,
  o,
  occurrence,
});

describe('the misestimate rule and the scales', () => {
  it('marks a ratio of ten or more either way, and nothing it cannot judge', () => {
    expect(misestimate(node('IndexScan', 100, 1000), true)).toBe(10);
    expect(misestimate(node('IndexScan', 100, 999), true)).toBeNull();
    expect(misestimate(node('IndexScan', 400, 10), true)).toBe(40);
    expect(misestimate(node('IndexScan', -1, 1000), true)).toBeNull();
    expect(misestimate(node('IndexScan', 100, 1000, { stoppedEarly: true }), true)).toBeNull();
    expect(misestimate(node('IndexScan', 100, 1000, { complete: false }), true)).toBeNull();
    expect(misestimate(node('IndexScan', 100, 1000), false)).toBeNull();
    expect(ratioText(node('IndexScan', 900, 1_204_331), true)).toBe('×1,338');
    expect(ratioText(node('IndexScan', 400, 10), true)).toBe('÷40');
  });

  it('grows the width with the logarithm of the rows', () => {
    expect(width(0)).toBeCloseTo(1.4);
    expect(width(9)).toBeCloseTo(2.65);
    expect(width(1e6)).toBeGreaterThan(width(1e3));
    expect(width(1e12)).toBe(9);
    expect(width(-1)).toBe(1.4);
  });

  it('writes labels for the desktop, the phone, time and a hidden estimate', () => {
    const op = flatten(node('IndexScan', 900, 1_204_331))[0];
    const s = { op, est: 900, rows: 1_204_331, timeMs: 1, self: 1, ratio: '×1,338' };
    expect(label(s, { time: false, phone: false })).toBe('est 900 · rows 1,204,331 ⚠ ×1,338');
    expect(label(s, { time: false, phone: true })).toBe('900 / 1.2M ⚠');
    expect(label({ ...s, est: -1, ratio: null }, { time: false, phone: false })).toBe(
      'rows 1,204,331',
    );
    expect(label({ ...s, rows: -1, ratio: null }, { time: false, phone: false })).toBe('est 900');
    expect(label(s, { time: true, phone: false })).toBe('1.0 ms own');
    expect(spoken(s, false)).toBe(
      'estimated 900 rows, actual 1,204,331, more than ten times the estimate',
    );
  });

  it('derives the ids of a plan in format 2 and numbers the run order', () => {
    const plan = node('HashJoin', 1, 1, { planFormat: 2 }, [
      node('IndexScan', 1, 1),
      node('MergeJoin', 1, 1, {}, [node('IndexScan', 1, 1), node('IndexScan', 1, 1)]),
    ]);
    const ops = flatten(plan);
    expect(ops.map((o) => o.id)).toEqual(['0', '0.0', '0.1', '0.1.0', '0.1.1']);
    expect(ops.map((o) => o.post)).toEqual([4, 0, 3, 1, 2]);
  });
});

describe('terms in the server form', () => {
  const p = {
    prologue: {
      prefixes: [
        { prefix: 'xsd', iri: 'http://www.w3.org/2001/XMLSchema#', range: { from: 0, to: 0 } },
      ],
    },
  } as unknown as PatternOk;
  it('writes literals as N-Triples', () => {
    const X = 'http://www.w3.org/2001/XMLSchema#';
    expect(literalNt('3', p)).toBe(`"3"^^<${X}integer>`);
    expect(literalNt('-3.5', p)).toBe(`"-3.5"^^<${X}decimal>`);
    expect(literalNt('1e3', p)).toBe(`"1e3"^^<${X}double>`);
    expect(literalNt('true', p)).toBe(`"true"^^<${X}boolean>`);
    expect(literalNt("'Ana'", p)).toBe('"Ana"');
    expect(literalNt('"Ana"@EN', p)).toBe('"Ana"@en');
    expect(literalNt('"5"^^xsd:int', p)).toBe(`"5"^^<${X}int>`);
    expect(literalNt('"s"^^xsd:string', p)).toBe('"s"');
    expect(literalNt('"a \\"b\\""', p)).toBe('"a \\"b\\""');
    expect(literalNt('"x"^^nope:t', p)).toBeNull();
  });
  it('escapes controls as OxRDF does', () => {
    expect(literalNt('"a\\tb"', p)).toBe('"a\\tb"');
    expect(literalNt('"a\\bb\\fc"', p)).toBe('"a\\bb\\fc"');
    expect(literalNt('"\\u0001\\u007f"', p)).toBe('"\\u0001\\u007F"');
    expect(literalNt("'''a\nb'''", p)).toBe('"a\\nb"');
    expect(literalNt('"a\\\\b"', p)).toBe('"a\\\\b"');
  });
  it('reads boolean keywords in any case', () => {
    const X = 'http://www.w3.org/2001/XMLSchema#';
    expect(literalNt('TRUE', p)).toBe(`"true"^^<${X}boolean>`);
    expect(literalNt('False', p)).toBe(`"false"^^<${X}boolean>`);
  });
  it('resolves a datatype against BASE and undoes its escapes', () => {
    const X = 'http://www.w3.org/2001/XMLSchema#';
    const b = { prologue: { ...p.prologue, base: 'http://ex.org/a/' } } as PatternOk;
    expect(literalNt('"x"^^<dt>', b)).toBe('"x"^^<http://ex.org/a/dt>');
    expect(literalNt('"x"^^<../dt>', b)).toBe('"x"^^<http://ex.org/dt>');
    expect(literalNt('"x"^^<http://e.org/\\u0041>', b)).toBe('"x"^^<http://e.org/A>');
    expect(literalNt('"x"^^xsd:a\\.b', b)).toBe(`"x"^^<${X}a.b>`);
  });
  it('writes paths as the engine does', () => {
    const iri = (l: string) => ({ kind: 'iri' as const, text: `ex:${l}`, iri: `${EX}${l}` });
    expect(pathNt({ kind: 'mod', op: '+', item: iri('partOf') })).toBe(`<${EX}partOf>+`);
    expect(pathNt({ kind: 'mod', op: '{1, 3}', item: iri('a') })).toBe(`<${EX}a>{1,3}`);
    expect(pathNt({ kind: 'mod', op: '{,3}', item: iri('a') })).toBe(`<${EX}a>{0,3}`);
    expect(pathNt({ kind: 'mod', op: '{2}', item: iri('a') })).toBe(`<${EX}a>{2}`);
    expect(pathNt({ kind: 'mod', op: '{2,2}', item: iri('a') })).toBe(`<${EX}a>{2}`);
    expect(pathNt({ kind: 'mod', op: '{01,02}', item: iri('a') })).toBe(`<${EX}a>{1,2}`);
    expect(pathNt({ kind: 'mod', op: '{007,}', item: iri('a') })).toBe(`<${EX}a>{7,}`);
    expect(pathNt({ kind: 'mod', op: '{18446744073709551615}', item: iri('a') })).toBe(
      `<${EX}a>{18446744073709551615}`,
    );
    expect(pathNt({ kind: 'alt', items: [iri('a'), iri('b')] })).toBe(`<${EX}a>|<${EX}b>`);
    expect(pathNt({ kind: 'inv', item: { kind: 'mod', op: '*', item: iri('a') } })).toBe(
      `^(<${EX}a>*)`,
    );
  });
});

describe.skipIf(!existsSync(built))('matching a plan with the pattern', async () => {
  const bindings = existsSync(built)
    ? await import(/* @vite-ignore */ new URL('./wasm/sparkles_fmt_wasm.js', import.meta.url).href)
    : null;
  bindings?.initSync({ module: readFileSync(built) });
  const ok = (text: string) => {
    const a = JSON.parse(bindings.pattern(JSON.stringify({ text }))) as PatternAnswer;
    if (!a.ok) throw new Error(a.error);
    return a;
  };
  const P = `PREFIX ex: <${EX}>\n`;
  const edge = (d: ReturnType<typeof drawing>, l: string) => d.edges.find((e) => e.label === l)!.id;

  it('A14: puts each operator on the edge of its pattern', () => {
    const a = ok(`${P}SELECT * WHERE { ?p ex:worksFor ?o . ?p ex:name ?n . ?o ex:partOf ?u }`);
    const d = drawing(a);
    const plan = node('HashJoin', 200, 214, { joinVars: ['o'] }, [
      node('MergeJoin', 100, 120, { joinVars: ['p'] }, [
        node('IndexScan', 214, 214, { patterns: [pat('?p', 'worksFor', '?o')] }),
        node('IndexScan', 900, 1_204_331, { patterns: [pat('?p', 'name', '?n')] }),
      ]),
      node('IndexScan', 9, 4100, { patterns: [pat('?o', 'partOf', '?u')] }),
    ]);
    const o = overlay(a, d, plan, true);
    expect(o.edges.get(edge(d, 'ex:worksFor'))!.est).toBe(214);
    expect(o.edges.get(edge(d, 'ex:name'))!.ratio).toBe('×1,338');
    expect(o.edges.get(edge(d, 'ex:partOf'))!.op.id).toBe('0.1');
    expect(o.unmatched).toEqual([]);
    // the joins in the order they run, each on its variable
    expect(o.joins.map((j) => [j.n, j.op.node.operator, j.drawnVars])).toEqual([
      [1, 'MergeJoin', ['p']],
      [2, 'HashJoin', ['o']],
    ]);
    expect(badge(o.joins[0], { time: false, phone: false }, true)).toBe('① MergeJoin · 120');
  });

  it('matches when the drawing holds another copy of the answer, as reactive state does', () => {
    const a = ok(`${P}SELECT * WHERE { ?p a ex:Person ; ex:worksFor ?o }`);
    const d = drawing(a);
    const plan = node('Join', 1, 1, {}, [
      node('IndexScan', 7, 7, { patterns: [pat('?p', 'worksFor', '?o')] }),
      node('IndexScan', 9, 9, {
        patterns: [pat('?p', '<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>', `<${EX}Person>`)],
      }),
    ]);
    const o = overlay(structuredClone(a), d, plan, true);
    expect(o.edges.get(edge(d, 'ex:worksFor'))!.est).toBe(7);
    // the class stays a class, not an edge's numbers on the node
    expect(o.classes.get('v:p')!.map((s) => s.est)).toEqual([9]);
  });

  it('matches equal patterns by their occurrence', () => {
    const a = ok(
      `${P}SELECT * WHERE { ?p ex:name ?n OPTIONAL { ?p ex:knows ?q . ?q ex:name ?n } ?q ex:name ?n }`,
    );
    const d = drawing(a);
    const plan = node('Join', 1, 1, {}, [
      node('IndexScan', 10, 10, { patterns: [pat('?p', 'name', '?n', 0)] }),
      node('IndexScan', 20, 20, { patterns: [pat('?q', 'name', '?n', 0)] }),
      node('IndexScan', 30, 30, { patterns: [pat('?q', 'name', '?n', 1)] }),
      node('IndexScan', 40, 40, { patterns: [pat('?p', 'knows', '?q', 0)] }),
    ]);
    const o = overlay(a, d, plan, true);
    const named = d.edges.filter((e) => e.label === 'ex:name' && e.source === 'v:q');
    expect(named.map((e) => o.edges.get(e.id)!.est).sort()).toEqual([20, 30]);
    // the first ?q ex:name ?n in the text is the optional one
    const optional = named.find((e) => e.optional)!;
    expect(o.edges.get(optional.id)!.est).toBe(20);
  });

  it('lists the patterns that match no edge, so nothing is hidden', () => {
    const a = ok(`${P}SELECT * WHERE { ?p ex:name ?n BIND(1 AS ?x) }`);
    const d = drawing(a);
    const plan = node('Join', 1, 1, {}, [
      node('IndexScan', 10, 10, { patterns: [pat('?p', 'name', '?n')] }),
      node('IndexScan', 5, 5, { patterns: [pat('?p', 'rewritten', '?z')] }),
      node('IndexScan', 5, 5, { patterns: [{ ...pat('?p', 'name', '?n'), graph: `<${EX}g>` }] }),
    ]);
    const o = overlay(a, d, plan, true);
    // a pattern inside GRAPH draws no edge, so it is listed with its graph
    expect(o.unmatched.map((u) => [u.pattern.p, u.pattern.graph])).toEqual([
      [`<${EX}rewritten>`, undefined],
      [`<${EX}name>`, `<${EX}g>`],
    ]);
    expect(o.edges.get(edge(d, 'ex:name'))!.est).toBe(10);
  });

  it('lists the patterns of a GRAPH block, which draws no edges', () => {
    const a = ok(`${P}SELECT * WHERE { GRAPH ?g { ?s ex:p ?o OPTIONAL { ?s ex:q ?v } } }`);
    const d = drawing(a);
    const plan = node('OptionalJoin', 1, 1, { joinVars: ['s', 'g'] }, [
      node('IndexScan', 5, 5, { patterns: [{ ...pat('?s', 'p', '?o'), graph: '?g' }] }),
      node('IndexScan', 6, 6, { patterns: [{ ...pat('?s', 'q', '?v'), graph: '?g' }] }),
    ]);
    const o = overlay(a, d, plan, true);
    expect(o.unmatched.map((u) => u.pattern.p)).toEqual([`<${EX}p>`, `<${EX}q>`]);
  });

  it('matches an inverse of a modified path with its ends swapped', () => {
    const a = ok(`${P}SELECT * WHERE { ?u ^ex:partOf+ ?t . ?a ^ex:b* ?c }`);
    const d = drawing(a);
    const plan = node('Join', 1, 1, {}, [
      node('TransitivePath', 7, 7, { patterns: [pat('?t', `<${EX}partOf>+`, '?u')] }),
      node('TransitivePath', 8, 8, { patterns: [pat('?c', `<${EX}b>*`, '?a')] }),
    ]);
    const o = overlay(a, d, plan, true);
    expect(o.unmatched).toEqual([]);
    expect(o.edges.size).toBe(2);
  });

  it('leaves a chip unmapped rather than borrowing the numbers of another filter', () => {
    const a = ok(
      `${P}SELECT * WHERE { ?s ex:p ?n OPTIONAL { ?s ex:q ?m FILTER(?n > 0) } FILTER(?n < 2) }`,
    );
    const d = drawing(a);
    // the optional's filter is its join's condition: only the outer one is a Filter
    const plan = node('Filter', 3, 3, { filterVars: ['n'] }, [
      node('OptionalJoin', 9, 9, { joinVars: ['s'] }, [
        node('IndexScan', 9, 9, { patterns: [pat('?s', 'p', '?n')] }),
        node('IndexScan', 4, 4, { patterns: [pat('?s', 'q', '?m')] }),
      ]),
    ]);
    const o = overlay(a, d, plan, true);
    const outer = filtersOf(a).find((f) => f.group === a.root)!.filter;
    expect([...o.filters.keys()]).toEqual([`f:${hash(normalized(outer.text))}:0`]);
    // two chips of the root group and one operator: the second chip shows nothing
    const two = ok(`${P}SELECT * WHERE { ?s ex:p ?n FILTER(?n > 0) FILTER(?n < 2) }`);
    const one = node('Filter', 3, 3, { filterVars: ['n'] }, [
      node('IndexScan', 9, 9, { patterns: [pat('?s', 'p', '?n')] }),
    ]);
    expect(overlay(two, drawing(two), one, true).filters.size).toBe(1);
  });

  it('keeps every join on a variable, one badge each, in run order', () => {
    const a = ok(
      `${P}SELECT * WHERE { ?s ex:p ?a OPTIONAL { ?s ex:q ?b } OPTIONAL { ?s ex:r ?c } }`,
    );
    const d = drawing(a);
    const plan = node('OptionalJoin', 1, 1, { joinVars: ['s'] }, [
      node('OptionalJoin', 2, 2, { joinVars: ['s'] }, [
        node('IndexScan', 3, 3, { patterns: [pat('?s', 'p', '?a')] }),
        node('IndexScan', 4, 4, { patterns: [pat('?s', 'q', '?b')] }),
      ]),
      node('IndexScan', 5, 5, { patterns: [pat('?s', 'r', '?c')] }),
    ]);
    const o = overlay(a, d, plan, true);
    expect(o.joins.map((j) => [j.n, j.op.id, j.drawnVars])).toEqual([
      [1, '0.0', ['s']],
      [2, '0', ['s']],
    ]);
  });

  it('shares a star join between its edges and says so', () => {
    const a = ok(`${P}SELECT * WHERE { ?p a ex:Person ; ex:age ?a ; ex:name ?n }`);
    const d = drawing(a);
    const plan = node(
      'StarJoin',
      50,
      60,
      { joinVars: ['p'], patterns: [pat('?p', 'age', '?a'), pat('?p', 'name', '?n')] },
      [
        node('IndexScan', 60, 60, {
          patterns: [
            pat('?p', '<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>', `<${EX}Person>`),
          ],
        }),
      ],
    );
    const o = overlay(a, d, plan, true);
    const age = o.edges.get(edge(d, 'ex:age'))!;
    const name = o.edges.get(edge(d, 'ex:name'))!;
    expect(age.op).toBe(name.op);
    expect(age.shared).toBe(1);
    // the class is drawn under its node, not as an edge
    expect(o.classes.get('v:p')![0].est).toBe(60);
  });

  it('matches a sequence step by step and an inverse the other way round', () => {
    const a = ok(
      `${P}SELECT * WHERE { ?p ex:team/ex:partOf ?u . ?u ^ex:head ?h . ?u ex:partOf+ ?v }`,
    );
    const d = drawing(a);
    const plan = node('Join', 1, 1, {}, [
      node('IndexScan', 3, 3, { patterns: [pat('?p', 'team', '?_')] }),
      node('IndexScan', 4, 4, { patterns: [pat('?_', 'partOf', '?u')] }),
      node('IndexScan', 5, 5, { patterns: [pat('?h', 'head', '?u')] }),
      node('TransitivePath', 6, 6, { patterns: [pat('?u', `<${EX}partOf>+`, '?v')] }),
    ]);
    const o = overlay(a, d, plan, true);
    expect(o.unmatched).toEqual([]);
    const seq = o.edges.get(d.edges.find((e) => e.label.includes('/'))!.id)!;
    expect(seq.ops.length).toBe(2);
    expect(o.edges.get(d.edges.find((e) => e.label.startsWith('^'))!.id)!.est).toBe(5);
    expect(o.edges.get(d.edges.find((e) => e.label.endsWith('+'))!.id)!.op.node.operator).toBe(
      'TransitivePath',
    );
  });

  it('puts the rows that reached and passed a filter on its chip', () => {
    const a = ok(`${P}SELECT * WHERE { ?p ex:name ?n FILTER(STRSTARTS(?n, "Ana")) }`);
    const d = drawing(a);
    const plan = node('Filter', 270, 14, { filterVars: ['n'] }, [
      node('IndexScan', 900, 900, { patterns: [pat('?p', 'name', '?n')] }),
    ]);
    const o = overlay(a, d, plan, true);
    expect(o.filters.size).toBe(1);
    const f = [...o.filters.values()][0];
    expect(filterLabel(f, { time: false, phone: false })).toBe('14 of 900 passed');
    expect(filterLabel(f, { time: false, phone: true })).toBe('14 / 900');
  });

  it('puts the numbers of its one operator on a locked block, and none when it is ambiguous', () => {
    const a = ok(`${P}SELECT * WHERE { ?p ex:born ?b BIND(YEAR(?b) AS ?y) }`);
    const d = drawing(a);
    const plan = node('Bind', 10, 10, {}, [
      node('IndexScan', 10, 10, { patterns: [pat('?p', 'born', '?b')] }),
    ]);
    const o = overlay(a, d, plan, true);
    const id = d.nodes.find((n) => n.kind === 'locked')!.id;
    expect(o.locked.get(id)!.op.node.operator).toBe('Bind');
    const two = ok(`${P}SELECT * WHERE { ?p ex:born ?b BIND(YEAR(?b) AS ?y) BIND(1 AS ?z) }`);
    const both = node('Bind', 1, 1, {}, [node('Bind', 1, 1, {}, [plan.children[0]])]);
    expect(overlay(two, drawing(two), both, true).locked.size).toBe(0);
  });

  it('A17: shows actual rows only, without marks, when estimates are hidden', () => {
    const a = ok(`${P}SELECT * WHERE { ?p ex:name ?n }`);
    const d = drawing(a);
    const plan = node('IndexScan', -1, 5000, { patterns: [pat('?p', 'name', '?n')] });
    const o = overlay(a, d, plan, true);
    const s = o.edges.get(edge(d, 'ex:name'))!;
    expect(o.hiddenEstimates).toBe(true);
    expect(s.ratio).toBeNull();
    expect(label(s, { time: false, phone: false })).toBe('rows 5,000');
  });

  it('marks the operator with the largest own time', () => {
    const a = ok(`${P}SELECT * WHERE { ?p ex:name ?n . ?p ex:age ?x }`);
    const d = drawing(a);
    const plan = node('HashJoin', 1, 1, { timeMs: 12, joinVars: ['p'] }, [
      node('IndexScan', 1, 1, { timeMs: 9, patterns: [pat('?p', 'name', '?n')] }),
      node('IndexScan', 1, 1, { timeMs: 1, patterns: [pat('?p', 'age', '?x')] }),
    ]);
    const o = overlay(a, d, plan, true);
    expect(o.hot).toBe('0.0');
    expect(overlay(a, d, plan, false).hot).toBeNull();
  });

  it('A20: matches a plan of 200 patterns quickly', () => {
    const lines = Array.from({ length: 200 }, (_, i) => `?v${i} ex:p ?v${i + 1} .`);
    const a = ok(`${P}SELECT * WHERE { ${lines.join('\n')} }`);
    const d = drawing(a);
    let plan = node('IndexScan', 1, 1, { patterns: [pat('?v0', 'p', '?v1')] });
    for (let i = 1; i < 200; i++)
      plan = node('HashJoin', 1, 1, { joinVars: [`v${i}`] }, [
        plan,
        node('IndexScan', 1, 1, { patterns: [pat(`?v${i}`, 'p', `?v${i + 1}`)] }),
      ]);
    const times: number[] = [];
    let o = overlay(a, d, plan, true);
    for (let i = 0; i < 5; i++) {
      const t0 = performance.now();
      o = overlay(a, d, plan, true);
      times.push(performance.now() - t0);
    }
    times.sort((x, y) => x - y);
    expect(o.edges.size).toBe(200);
    expect(o.joins.length).toBe(199);
    expect(times[2]).toBeLessThan(50);
  });
});

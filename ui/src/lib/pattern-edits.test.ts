// The canvas edits of U08 Phase 2 (pattern-edits.ts), against the built browser module:
// each edit's exact text, that the result parses with the server's checks, and R3 for
// every kind, on fixed queries and on seeded random sequences (A5).

import { existsSync, readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
  applyChanges,
  drawing,
  EditError,
  lockedOf,
  triplesOf,
  type PatternAnswer,
  type PatternOk,
} from './pattern';
import {
  agreesWith,
  checkPredicate,
  countQuery,
  editAny,
  filterKeys,
  lockedBlocks,
  outerFilterVars,
  pathForms,
  pathPrefixes,
  patternVars,
  renameReach,
  unboundUses,
  type AnyEdit,
} from './pattern-edits';

const built = new URL('./wasm/sparkles_fmt_wasm_bg.wasm', import.meta.url);

describe.skipIf(!existsSync(built))('the edits of Phase 2', async () => {
  const bindings = existsSync(built)
    ? await import(/* @vite-ignore */ new URL('./wasm/sparkles_fmt_wasm.js', import.meta.url).href)
    : null;
  bindings?.initSync({ module: readFileSync(built) });
  const parse = (text: string, strict = false) =>
    JSON.parse(
      bindings.pattern(JSON.stringify(strict ? { text, strict } : { text })),
    ) as PatternAnswer;
  const ok = (text: string) => {
    const a = parse(text);
    if (!a.ok) throw new Error(`${a.error}\n${text}`);
    return a;
  };

  /** Make an edit, check the result parses strictly and agrees, and give its text. */
  const run = (text: string, e: AnyEdit) => {
    const p = ok(text);
    const out = applyChanges(text, editAny(text, p, e, parse));
    const strict = parse(out, true);
    expect(strict.ok, `${JSON.stringify(strict)}\n${out}`).toBe(true);
    expect(agreesWith(p, e, strict as PatternOk), out).toBe(true);
    return out;
  };
  const edgeOf = (text: string, label: string) =>
    drawing(ok(text)).edges.find((x) => x.label === label)!.id;

  const A3 = `PREFIX ex: <http://example.org/>
SELECT ?person ?org WHERE {
  ?person a ex:Person ; ex:worksFor ?org .
  ?org a ex:Org ; ex:locatedIn ex:lyon .
}
LIMIT 1000
`;

  it('changes a predicate, and one object of a `,` list leaves the list', () => {
    expect(run(A3, { kind: 'predicate', edge: edgeOf(A3, 'ex:worksFor'), p: 'ex:leads' })).toBe(
      A3.replace('ex:worksFor ?org', 'ex:leads ?org'),
    );
    const list = 'PREFIX ex: <http://x/>\nSELECT * {\n  ?s ex:p ?a, ?b .\n}\n';
    const second = drawing(ok(list)).edges.find((x) => x.target === 'v:b')!.id;
    expect(run(list, { kind: 'predicate', edge: second, p: 'ex:q' })).toBe(
      'PREFIX ex: <http://x/>\nSELECT * {\n  ?s ex:p ?a ; ex:q ?b .\n}\n',
    );
    const first = drawing(ok(list)).edges.find((x) => x.target === 'v:a')!.id;
    expect(run(list, { kind: 'predicate', edge: first, p: 'ex:q' })).toBe(
      'PREFIX ex: <http://x/>\nSELECT * {\n  ?s ex:p ?b ; ex:q ?a .\n}\n',
    );
  });

  it('writes simple paths and refuses others', () => {
    const e = edgeOf(A3, 'ex:locatedIn');
    for (const f of pathForms('ex:locatedIn').slice(1))
      expect(run(A3, { kind: 'predicate', edge: e, p: f.text })).toContain(`${f.text} ex:lyon`);
    expect(run(A3, { kind: 'predicate', edge: e, p: 'ex:locatedIn/ex:partOf' })).toContain(
      'ex:locatedIn/ex:partOf ex:lyon',
    );
    expect(checkPredicate('ex:a{1,3}')).toBe('ex:a{1,3}');
    expect(() => checkPredicate('ex:a ?x')).toThrow(EditError);
    expect(() => checkPredicate('!(ex:a)')).toThrow(EditError);
    // a path the canvas would lock does not agree (R3)
    const p = ok(A3);
    const locked = applyChanges(
      A3,
      editAny(A3, p, { kind: 'predicate', edge: e, p: '^ex:a/ex:b' }),
    );
    expect(agreesWith(p, { kind: 'predicate', edge: e, p: '^ex:a/ex:b' }, ok(locked))).toBe(false);
  });

  it('adds a filter after the last triple that binds its variable', () => {
    expect(run(A3, { kind: 'filter-add', var: 'org', expr: 'CONTAINS(STR(?org), "a")' })).toBe(
      A3.replace(
        '?org a ex:Org ; ex:locatedIn ex:lyon .\n',
        '?org a ex:Org ; ex:locatedIn ex:lyon .\n  FILTER(CONTAINS(STR(?org), "a"))\n',
      ),
    );
    // inside the OPTIONAL that binds it
    const opt = 'SELECT * {\n  ?s <http://p> ?o .\n  OPTIONAL { ?o <http://q> ?n }\n}\n';
    expect(run(opt, { kind: 'filter-add', var: 'n', expr: '?n > 2' })).toBe(
      'SELECT * {\n  ?s <http://p> ?o .\n  OPTIONAL { ?o <http://q> ?n\n    FILTER(?n > 2)\n   }\n}\n',
    );
    expect(() => editAny(A3, ok(A3), { kind: 'filter-add', var: 'zz', expr: '?zz > 1' })).toThrow(
      'No triple binds ?zz',
    );
  });

  it('edits and deletes a filter by its key', () => {
    const q = 'SELECT * {\n  ?s <http://p> ?n .\n  FILTER(?n > 2) .\n  ?s <http://q> ?m .\n}\n';
    const key = filterKeys(ok(q))[0].key;
    expect(run(q, { kind: 'filter-edit', filter: key, expr: '?n >= 3' })).toBe(
      q.replace('FILTER(?n > 2)', 'FILTER(?n >= 3)'),
    );
    expect(run(q, { kind: 'filter-delete', filter: key })).toBe(
      'SELECT * {\n  ?s <http://p> ?n .\n  ?s <http://q> ?m .\n}\n',
    );
    expect(() => editAny(q, ok(q), { kind: 'filter-delete', filter: 'f:gone:0' })).toThrow(
      'select the filter again',
    );
  });

  it('renames a variable in the subset, and in locked blocks once confirmed', () => {
    const q =
      'SELECT ?org (COUNT(?org) AS ?n) WHERE {\n  ?person <http://w> ?org .\n  FILTER(?org != <http://x>)\n  BIND(STR(?org) AS ?label)\n}\nGROUP BY ?org';
    const reach = renameReach(ok(q), 'org');
    expect(reach.editable).toHaveLength(3);
    expect(reach.locked.map((b) => b.text)).toEqual([
      'BIND(STR(?org) AS ?label)',
      'GROUP BY ?org',
      '',
    ]);
    // without the locked blocks the grouping no longer holds, which the server refuses
    const half = applyChanges(
      q,
      editAny(q, ok(q), { kind: 'rename', from: 'org', to: 'company', locked: false }),
    );
    expect(half).toBe(
      q
        .replace('SELECT ?org', 'SELECT ?company')
        .replace('<http://w> ?org', '<http://w> ?company')
        .replace('FILTER(?org', 'FILTER(?company'),
    );
    expect(parse(half, true).ok).toBe(false);
    const plain = 'SELECT ?org WHERE { ?p <http://w> ?org . FILTER(?org != <http://x>) }';
    expect(run(plain, { kind: 'rename', from: 'org', to: 'company', locked: false })).toBe(
      plain.replaceAll('?org', '?company'),
    );
    expect(run(q, { kind: 'rename', from: 'org', to: 'company', locked: true })).toBe(
      q.replaceAll('?org', '?company'),
    );
    expect(() =>
      editAny(q, ok(q), { kind: 'rename', from: 'org', to: 'person', locked: false }),
    ).toThrow('?person is already in the query');
    expect(() =>
      editAny(q, ok(q), { kind: 'rename', from: 'org', to: '1 x', locked: false }),
    ).toThrow(EditError);
  });

  it('deletes a node with its triples, class subtitles and orphaned filters', () => {
    const q = `PREFIX ex: <http://x/>
SELECT * WHERE {
  ?person a ex:Person ; ex:name ?name ; ex:worksFor ?org .
  ?org ex:size ?z .
  FILTER(?z > 10)
  BIND(?z AS ?zz)
}
`;
    const out = run(q, { kind: 'delete-node', node: 'v:org' });
    expect(out).toBe(`PREFIX ex: <http://x/>
SELECT * WHERE {
  ?person a ex:Person ; ex:name ?name .
  BIND(?z AS ?zz)
}
`);
    // the BIND now uses a variable nothing binds (§4.2 rule 4)
    const after = ok(out);
    expect(unboundUses(after, lockedOf(after)[0])).toEqual(['z']);
    expect(unboundUses(ok(q), lockedOf(ok(q))[0])).toEqual([]);
    // an IRI node, and a literal
    const lit =
      'SELECT * { ?s <http://p> "a" . ?s <http://q> <http://o> . ?t <http://q> <http://o> }';
    const d = drawing(ok(lit));
    const litNode = d.nodes.find((n) => n.kind === 'literal')!.id;
    expect(run(lit, { kind: 'delete-node', node: litNode })).toBe(
      'SELECT * {  ?s <http://q> <http://o> . ?t <http://q> <http://o> }',
    );
    expect(run(lit, { kind: 'delete-node', node: 'i:http://o' })).toBe(
      'SELECT * { ?s <http://p> "a" .   }',
    );
  });

  it('deletes a locked block by its key, comment and dot included', () => {
    const q =
      'SELECT * {\n  ?s <http://p> ?o .\n  BIND(1 AS ?x) # one\n  .\n  ?s <http://q> ?r .\n}\n';
    const key = lockedBlocks(ok(q))[0].key;
    expect(run(q, { kind: 'delete-locked', block: key })).toBe(
      'SELECT * {\n  ?s <http://p> ?o .\n  \n  ?s <http://q> ?r .\n}\n',
    );
  });

  it('adds several triples together, such as a class and an edge', () => {
    expect(
      run(A3, {
        kind: 'add-many',
        triples: [
          { s: '?city', p: 'a', o: 'ex:City' },
          { s: '?org', p: 'ex:basedIn', o: '?city' },
        ],
      }),
    ).toBe(
      A3.replace(
        '?org a ex:Org ; ex:locatedIn ex:lyon .\n',
        '?org a ex:Org ; ex:locatedIn ex:lyon .\n  ?city a ex:City .\n  ?org ex:basedIn ?city .\n',
      ),
    );
  });

  it('changes the projection, DISTINCT, LIMIT and OFFSET', () => {
    expect(run(A3, { kind: 'projection', var: 'person', on: false })).toContain(
      'SELECT ?org WHERE',
    );
    expect(run(A3, { kind: 'projection', var: 'org', on: false })).toContain(
      'SELECT ?person WHERE',
    );
    const one = A3.replace('SELECT ?person ?org', 'SELECT ?org');
    expect(run(one, { kind: 'projection', var: 'person', on: true })).toContain(
      'SELECT ?org ?person WHERE',
    );
    expect(() => editAny(one, ok(one), { kind: 'projection', var: 'org', on: false })).toThrow(
      'at least one variable',
    );
    const star = A3.replace('SELECT ?person ?org', 'SELECT *');
    expect(run(star, { kind: 'projection', var: 'org', on: false })).toContain(
      'SELECT ?person WHERE',
    );
    expect(patternVars(ok(star))).toEqual(['person', 'org']);
    expect(run(A3, { kind: 'distinct', on: true })).toContain('SELECT DISTINCT ?person ?org');
    const dis = A3.replace('SELECT ', 'SELECT DISTINCT ');
    expect(run(dis, { kind: 'distinct', on: false })).toContain('SELECT ?person ?org');
    expect(run(A3, { kind: 'limit', value: 5 })).toContain('\nLIMIT 5\n');
    expect(run(A3, { kind: 'limit', value: null })).toBe(A3.replace('\nLIMIT 1000', ''));
    expect(run(A3, { kind: 'offset', value: 10 })).toContain('LIMIT 1000\nOFFSET 10\n');
    const ordered = 'SELECT * { ?s ?p ?o } ORDER BY ?s VALUES ?s { <http://a> }';
    expect(run(ordered, { kind: 'limit', value: 3 })).toBe(
      'SELECT * { ?s ?p ?o } ORDER BY ?s\nLIMIT 3 VALUES ?s { <http://a> }',
    );
    expect(() => editAny(A3, ok(A3), { kind: 'limit', value: -1 })).toThrow(EditError);
  });

  it('counts with the prologue, FROM and the WHERE group', () => {
    const q = 'PREFIX ex: <http://x/>\nSELECT ?s FROM <http://g> WHERE { ?s ex:p ?o } LIMIT 3';
    expect(countQuery(q, ok(q))).toBe(
      'PREFIX ex: <http://x/>\nSELECT (COUNT(*) AS ?n)\nFROM <http://g>\nWHERE { ?s ex:p ?o }',
    );
    expect(parse(countQuery(q, ok(q)), true).ok).toBe(true);
  });

  it('finds the prefixes of paths and filter expressions', () => {
    expect(pathPrefixes('ex:a/^foo:b+')).toEqual(['ex', 'foo']);
    expect(pathPrefixes('CONTAINS(STR(?x), "a:b") && ?x != <http://c:d>')).toEqual([]);
    expect(pathPrefixes('?x = xsd:int')).toEqual(['xsd']);
  });

  // ------------------------------------------------ review round 1 of Phase 2 ------

  it('keeps a locked filter when its node goes, for its own confirmed deletion', () => {
    const q = 'SELECT * {\n  ?s <http://p> ?x .\n  FILTER(EXISTS { ?x <http://q> ?z })\n}\n';
    expect(run(q, { kind: 'delete-node', node: 'v:x' })).toBe(
      'SELECT * {\n  FILTER(EXISTS { ?x <http://q> ?z })\n}\n',
    );
  });

  it('puts a class-backed edge in its source’s group', () => {
    const q = 'SELECT * {\n  ?s <http://p> ?o .\n  OPTIONAL { ?o <http://q> ?x . }\n}\n';
    const out = run(q, {
      kind: 'add-many',
      triples: [
        { s: '?x', p: '<http://r>', o: '?c' },
        { s: '?c', p: 'a', o: '<http://C>' },
      ],
    });
    expect(out).toBe(
      'SELECT * {\n  ?s <http://p> ?o .\n  OPTIONAL { ?o <http://q> ?x .\n    ?x <http://r> ?c .\n    ?c a <http://C> .\n   }\n}\n',
    );
    // R3 refuses a class-backed edge that lands outside its source's OPTIONAL
    const p0 = ok(q);
    const wrong = ok(
      'SELECT * {\n  ?s <http://p> ?o .\n  OPTIONAL { ?o <http://q> ?x . }\n  ?c a <http://C> .\n  ?x <http://r> ?c .\n}\n',
    );
    expect(
      agreesWith(
        p0,
        {
          kind: 'add-many',
          triples: [
            { s: '?c', p: 'a', o: '<http://C>' },
            { s: '?x', p: '<http://r>', o: '?c' },
          ],
        },
        wrong,
      ),
    ).toBe(false);
  });

  it('adds a filter in the group that owns its variable', () => {
    const q = 'SELECT * {\n  ?s <http://p> ?x .\n  OPTIONAL { ?s <http://q> ?x }\n}\n';
    expect(run(q, { kind: 'filter-add', var: 'x', expr: '?x > 5' })).toBe(
      'SELECT * {\n  ?s <http://p> ?x .\n  FILTER(?x > 5)\n  OPTIONAL { ?s <http://q> ?x }\n}\n',
    );
  });

  it('expands SELECT * with the variables of a trailing VALUES', () => {
    const q = 'SELECT * { ?s <http://p> ?o } VALUES ?v { 1 }\n';
    expect(run(q, { kind: 'projection', var: 'o', on: false })).toBe(
      'SELECT ?s ?v { ?s <http://p> ?o } VALUES ?v { 1 }\n',
    );
    expect(countQuery(q, ok(q))).toBe(
      'SELECT (COUNT(*) AS ?n)\nWHERE { { ?s <http://p> ?o }\n  VALUES ?v { 1 } }',
    );
  });

  it('deletes one literal of a `,` list as a node', () => {
    const q = 'SELECT * { ?s <http://p> "a", "b" }\n';
    const node = drawing(ok(q)).nodes.find((n) => n.label === '"a"')!.id;
    expect(run(q, { kind: 'delete-node', node })).toBe('SELECT * { ?s <http://p> "b" }\n');
  });

  it('renames past variable-like text in strings, and refuses a name used anywhere', () => {
    const q = 'SELECT * { ?x <http://p> "?x" }\n';
    expect(run(q, { kind: 'rename', from: 'x', to: 'y', locked: false })).toBe(
      'SELECT * { ?y <http://p> "?x" }\n',
    );
    const order = 'SELECT * { ?x <http://p> ?o } ORDER BY ?y\n';
    expect(() =>
      editAny(order, ok(order), { kind: 'rename', from: 'x', to: 'y', locked: false }, parse),
    ).toThrow('?y is already in the query');
  });

  it('keeps a comment between projection items', () => {
    const q = 'SELECT ?s # keep this\n  ?o WHERE { ?s <http://p> ?o }\n';
    expect(run(q, { kind: 'projection', var: 'o', on: false })).toBe(
      'SELECT ?s # keep this\n  WHERE { ?s <http://p> ?o }\n',
    );
  });

  // ------------------------------------------------ review round 2 of Phase 2 ------

  it('scans escaped prefixed names and code points for variables', () => {
    const q = 'PREFIX ex: <http://x/>\nSELECT * { ?x ex:p ex:a\\#b . ?y ex:q ?z }\n';
    expect(() =>
      editAny(q, ok(q), { kind: 'rename', from: 'x', to: 'y', locked: false }, parse),
    ).toThrow('?y is already in the query');
    const wide = 'SELECT * { ?\u{10400} <http://p> ?o }\n';
    expect(run(wide, { kind: 'rename', from: '\u{10400}', to: 'w', locked: false })).toBe(
      'SELECT * { ?w <http://p> ?o }\n',
    );
  });

  it('counts with a trailing VALUES inside the counted group, under a free alias', () => {
    const q = 'SELECT * {} VALUES ?n { 0 }\n';
    const c = countQuery(q, ok(q));
    expect(c).toBe('SELECT (COUNT(*) AS ?n1)\nWHERE { {}\n  VALUES ?n { 0 } }');
    expect(parse(c, true).ok).toBe(true);
  });

  it('finds the added filter among identical ones by its group', () => {
    const q = 'SELECT * { OPTIONAL { ?s <http://q> ?x . FILTER(?x > 5) } ?s <http://p> ?x . }\n';
    const out = run(q, { kind: 'filter-add', var: 'x', expr: '?x > 5' });
    expect(out).toContain('} ?s <http://p> ?x .\n  FILTER(?x > 5)\n }');
  });

  it('names the variables of a filter outside its EXISTS patterns', () => {
    expect(outerFilterVars('FILTER(?a + ?b > 1 && EXISTS { ?c ?p ?d })')).toEqual(['a', 'b']);
    expect(outerFilterVars('FILTER NOT EXISTS { ?x <http://q> ?z }')).toEqual([]);
  });

  it('A5: seeded sequences of every kind agree with the parse (R3) and keep locked text', () => {
    const start = `PREFIX ex: <http://example.org/>
SELECT ?person ?name WHERE {
  ?person a ex:Person ; ex:name ?name .
  BIND(YEAR(NOW()) AS ?y) # year
  ?person ex:knows ?b .
  OPTIONAL { ?b ex:age ?age }
  FILTER(STRSTARTS(?name, "A"))
}
LIMIT 10
`;
    const rng = (seed: number) => () => {
      seed = (seed + 0x6d2b79f5) | 0;
      let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
    for (const seed of [11, 12, 13, 14, 15]) {
      const r = rng(seed);
      const pick = <T>(xs: T[]) => xs[Math.floor(r() * xs.length)];
      let text = start;
      let applied = 0;
      const bind = 'BIND(YEAR(NOW()) AS ?y) # year';
      for (let step = 0; step < 25; step++) {
        const p = ok(text);
        const d = drawing(p);
        const edges = d.edges.filter((x) => x.triple);
        const vars = patternVars(p);
        const fresh = `v${step}`;
        const kinds: (() => AnyEdit | null)[] = [
          () => ({
            kind: 'add',
            s: `?${pick(vars) ?? 'x'}`,
            p: pick(['ex:p', 'ex:q']),
            o: `?${fresh}`,
          }),
          () => (edges.length ? { kind: 'delete', edge: pick(edges).id } : null),
          () => (edges.length ? { kind: 'optional', edge: pick(edges).id } : null),
          () =>
            edges.length
              ? { kind: 'predicate', edge: pick(edges).id, p: pick(['ex:r', 'ex:s+', '^ex:t']) }
              : null,
          () =>
            vars.length
              ? { kind: 'filter-add', var: pick(vars), expr: `BOUND(?${pick(vars)})` }
              : null,
          () => {
            const f = filterKeys(p);
            return f.length ? { kind: 'filter-delete', filter: pick(f).key } : null;
          },
          () =>
            vars.length
              ? { kind: 'rename', from: pick(vars), to: `r${step}`, locked: false }
              : null,
          () => (vars.length ? { kind: 'projection', var: pick(vars), on: r() < 0.5 } : null),
          () => ({ kind: 'distinct', on: !p.projection?.distinct }),
          () => ({
            kind: pick(['limit', 'offset'] as const),
            value: r() < 0.3 ? null : Math.floor(r() * 50),
          }),
          () => {
            const n = d.nodes.filter(
              (x) => !x.fromLocked && x.kind !== 'locked' && x.kind !== 'chip',
            );
            return n.length ? { kind: 'delete-node', node: pick(n).id } : null;
          },
        ];
        const e = pick(kinds)();
        if (!e) continue;
        let changes;
        try {
          changes = editAny(text, p, e, parse);
        } catch (err) {
          if (err instanceof EditError) continue;
          throw err;
        }
        const next = applyChanges(text, changes);
        const strict = parse(next, true);
        // an edit the server would refuse, such as a scope clash, is refused by the pane
        if (!strict.ok) continue;
        expect(
          agreesWith(p, e, strict as PatternOk),
          `${JSON.stringify(e)}\n${text}\n---\n${next}`,
        ).toBe(true);
        applied++;
        if (e.kind !== 'rename' && e.kind !== 'delete-node') expect(next).toContain(bind);
        text = next;
      }
      // most steps make an edit
      expect(applied, `seed ${seed}`).toBeGreaterThan(10);
    }
  });
});

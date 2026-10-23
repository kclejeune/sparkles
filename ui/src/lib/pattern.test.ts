import { existsSync, readFileSync } from 'node:fs';
import { Store } from 'oxigraph';
import { describe, expect, it } from 'vitest';
import fixtures from './pattern-fixtures.json';
import {
  agrees,
  applyChanges,
  checkTerm,
  compact,
  describe as describeEl,
  drawing,
  edit,
  EditError,
  hash,
  lockedKeys,
  lockedOf,
  overCaps,
  removalRange,
  renamed,
  missingPrefixes,
  resolveEdit,
  resolveIri,
  resolveTerm,
  stillNames,
  textOrder,
  triplesOf,
  type Change,
  type Edit,
  type PatternAnswer,
  type PatternOk,
} from './pattern';

type Name = keyof typeof fixtures;
const fx = (name: Name) => {
  const f = fixtures[name] as { text: string; answer: PatternAnswer };
  return { text: f.text, p: f.answer as PatternOk };
};

/** Whether a query parses (oxigraph's parser, a second opinion). */
function parses(text: string): boolean {
  try {
    new Store().query(text);
    return true;
  } catch {
    return false;
  }
}

/** The id of the edge of the triple at `i` in `triplesOf`. */
const edgeAt = (p: PatternOk, i: number) => {
  const e = drawing(p).edges.find((x) => x.triple?.order === i);
  if (!e) throw new Error(`no edge for triple ${i}`);
  return e.id;
};

const run = (name: Name, e: Edit) => {
  const { text, p } = fx(name);
  const changes = edit(text, p, e);
  return { text, changes, out: applyChanges(text, changes) };
};

describe('the drawing', () => {
  it('A1: three nodes and the classes as subtitles', () => {
    const d = drawing(fx('a3').p);
    expect(d.nodes.map((n) => [n.label, n.subtitle])).toEqual([
      ['?person', ['ex:Person']],
      ['?org', ['ex:Org']],
      ['ex:lyon', []],
    ]);
    expect(d.edges.map((e) => [e.source, e.label, e.target])).toEqual([
      ['v:person', 'ex:worksFor', 'v:org'],
      ['v:org', 'ex:locatedIn', 'i:http://example.org/lyon'],
    ]);
    expect(d.parts).toBe(1);
    expect(d.triples).toBe(4);
    // the list alternative has the same rows, in text order
    expect(textOrder(d).map((el) => describeEl(el, d))).toEqual([
      'variable ?person, ex:Person',
      'edge ex:worksFor from ?person to ?org',
      'variable ?org, ex:Org',
      'edge ex:locatedIn from ?org to ex:lyon',
      'IRI ex:lyon',
    ]);
  });

  it('A2: a locked block keeps its text and comment, joined to the variable it binds', () => {
    const d = drawing(fx('bind').p);
    const block = d.nodes.find((n) => n.kind === 'locked')!;
    expect(block.label).toBe('locked · BIND');
    expect(block.locked!.text).toBe('BIND(YEAR(NOW()) AS ?y) # year');
    expect(d.nodes.some((n) => n.id === 'v:y')).toBe(true);
    expect(d.edges.filter((e) => e.source === block.id).map((e) => e.target)).toEqual(['v:y']);
    // an understood filter is a chip on its variable
    expect(d.nodes.find((n) => n.id === 'v:name')!.subtitle).toEqual(['starts with "A"']);
  });

  it('draws optional parts in a hull and a literal per occurrence', () => {
    const d = drawing(fx('optional').p);
    expect(d.hulls.map((h) => h.label)).toEqual(['OPTIONAL', 'OPTIONAL']);
    // ?org is bound outside, so it stays out of the hulls
    expect(d.nodes.find((n) => n.id === 'v:org')!.parent).toBeUndefined();
    expect(d.nodes.find((n) => n.id === 'v:c')!.parent).toBe('h:0');
    expect(d.nodes.find((n) => n.id === 'v:u')!.parent).toBe('h:1');
    expect(d.edges.filter((e) => e.optional)).toHaveLength(3);
  });

  it('keys locked blocks by kind, text hash and index', () => {
    const b = { vars: [], range: { from: 0, to: 0 } };
    const keys = lockedKeys([
      { ...b, kind: 'BIND', text: 'BIND(1 AS ?x)' },
      { ...b, kind: 'BIND', text: 'BIND(1  AS\n?x)' },
      { ...b, kind: 'BIND', text: 'BIND(2 AS ?x)' },
    ]);
    const h = hash('BIND(1 AS ?x)');
    expect(keys).toEqual([`k:BIND:${h}:0`, `k:BIND:${h}:1`, `k:BIND:${hash('BIND(2 AS ?x)')}:0`]);
  });

  it('compacts IRIs under the longest prefix', () => {
    const ps = [
      { prefix: 'ex', iri: 'http://example.org/' },
      { prefix: 'exa', iri: 'http://example.org/a/' },
    ];
    expect(compact('http://example.org/a/b', ps)).toBe('exa:b');
    expect(compact('http://example.org/x y', ps)).toBe('<http://example.org/x y>');
    expect(compact('http://other/x', ps)).toBe('<http://other/x>');
  });

  it('matches a variable renamed in the editor by its edges', () => {
    const { text, p } = fx('a3');
    const before = drawing(p);
    // the same pattern with ?org called ?company: build the answer by renaming terms
    const swap = JSON.parse(JSON.stringify(p).replaceAll('"value":"org"', '"value":"company"'));
    expect(renamed(before, drawing(swap))).toEqual([['v:org', 'v:company']]);
    // two renames at once are not guessed
    const both = JSON.parse(
      JSON.stringify(swap).replaceAll('"value":"person"', '"value":"someone"'),
    );
    expect(renamed(before, drawing(both))).toBeNull();
    expect(text).toContain('?org');
  });

  it('draws the locked clauses outside the WHERE group', () => {
    const d = drawing(fx('describe').p);
    expect(d.nodes.map((n) => n.label)).toEqual(['locked · DESCRIBE']);
    const c = drawing(fx('construct').p);
    const kinds = c.nodes.filter((n) => n.kind === 'locked').map((n) => n.locked!.kind);
    expect(kinds.sort()).toEqual(['CONSTRUCT', 'FROM']);
    const template = c.nodes.find((n) => n.locked?.kind === 'CONSTRUCT')!;
    expect(
      c.edges
        .filter((e) => e.source === template.id)
        .map((e) => e.target)
        .sort(),
    ).toEqual(['v:o', 'v:s']);
    expect(c.parts).toBe(1);
  });

  it('draws no hull without a node of its own', () => {
    const d = drawing(fx('shared-hull').p);
    expect(d.hulls).toEqual([]);
    expect(d.edges.filter((e) => e.optional).map((e) => e.label)).toEqual(['<http://q>']);
    expect(describeEl({ edge: d.edges[1] }, d)).toBe('edge <http://q> from ?s to ?o, optional');
  });

  it('keeps a filter in its OPTIONAL’s scope', () => {
    const d = drawing(fx('optional-filter').p);
    // the root filter on a required variable is its chip
    expect(d.nodes.find((n) => n.id === 'v:o')!.subtitle).toEqual(['< 9']);
    // the optional's filter on a variable of its hull is a chip there
    expect(d.nodes.find((n) => n.id === 'v:x')!.subtitle).toEqual(['> 3']);
    // the optional's filter on a required variable is a node inside the hull
    const chip = d.nodes.find((n) => n.kind === 'chip')!;
    expect([chip.title, chip.parent]).toEqual(['FILTER(?o > 2)', 'h:0']);
  });

  it('moves a renamed subject’s literals with it', () => {
    const before = drawing(fx('literal').p);
    const after = drawing(fx('literal-renamed').p);
    expect(renamed(before, after)).toEqual([
      ['v:person', 'v:someone'],
      ['l:v:person|http://name|0', 'l:v:someone|http://name|0'],
    ]);
  });

  it('draws 20,000 triples in linear time', () => {
    const { p } = fx('one-line');
    const t0 = p.root!.items[0];
    const items = Array.from({ length: 20000 }, (_, i) => ({ ...t0, stmt: i }));
    const big = { ...p, root: { ...p.root!, items } } as PatternOk;
    const start = performance.now();
    const d = drawing(big);
    expect(d.edges).toHaveLength(20000);
    expect(new Set(d.edges.map((e) => e.id)).size).toBe(20000);
    expect(performance.now() - start).toBeLessThan(1000);
  });

  it('counts disconnected parts and the caps', () => {
    const { p } = fx('crlf');
    expect(drawing(p).parts).toBe(1);
    expect(overCaps(p, drawing(p))).toBe(false);
  });
});

describe('edits as splices', () => {
  it('A6: marking ex:locatedIn optional is exactly this change', () => {
    const { p } = fx('a3');
    const i = triplesOf(p).findIndex((t) => t.triple.p.text === 'ex:locatedIn');
    const { out } = run('a3', { kind: 'optional', edge: edgeAt(p, i) });
    expect(out).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT ?person ?org WHERE {\n  ?person a ex:Person ; ex:worksFor ?org .\n  ?org a ex:Org .\n  OPTIONAL { ?org ex:locatedIn ex:lyon . }\n}\nLIMIT 1000\n',
    );
    expect(parses(out)).toBe(true);
  });

  it('adds a triple after the last triple of its subject’s group', () => {
    const { out } = run('a3', {
      kind: 'add',
      s: '?person',
      p: 'ex:age',
      o: '?age',
    });
    expect(out).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT ?person ?org WHERE {\n  ?person a ex:Person ; ex:worksFor ?org .\n  ?org a ex:Org ; ex:locatedIn ex:lyon .\n  ?person ex:age ?age .\n}\nLIMIT 1000\n',
    );
  });

  it('places a new triple after a locked block that uses a variable it binds (§4.2)', () => {
    const { out } = run('locked-after', {
      kind: 'add',
      s: '?c',
      p: 'ex:z',
      o: '?w',
    });
    expect(out).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n  ?a ex:p ?b .\n  BIND(?b + 1 AS ?c)\n  ?c ex:z ?w .\n  MINUS { ?a ex:q ?d }\n}\n',
    );
    // a triple that binds nothing the blocks use goes before them
    const { out: before } = run('locked-after', {
      kind: 'add',
      s: '?x',
      p: 'ex:z',
      o: '1',
    });
    expect(before).toContain('?a ex:p ?b .\n  ?x ex:z 1 .\n  BIND');
    expect(parses(out) && parses(before)).toBe(true);
  });

  it('adds a `.` to a statement without one and keeps a trailing comment on its line', () => {
    const { out } = run('no-dot', {
      kind: 'add',
      s: '?o',
      p: '<http://x/q>',
      o: '?r',
    });
    expect(out).toBe('SELECT * {\n  ?s <http://x/p> ?o . # last\n  ?o <http://x/q> ?r .\n}\n');
    expect(parses(out)).toBe(true);
  });

  it('adds to a query on one line and to an empty group', () => {
    const { out } = run('one-line', {
      kind: 'add',
      s: '?o',
      p: 'a',
      o: '<http://x/C>',
    });
    expect(out).toBe('SELECT * { ?s <http://x/p> ?o .\n  ?o a <http://x/C> .\n }');
    expect(parses(out)).toBe(true);
    const { out: empty } = run('empty', {
      kind: 'add',
      s: '?s',
      p: '?p',
      o: '?o',
    });
    expect(empty).toBe('SELECT * WHERE {\n  ?s ?p ?o .\n}\n');
  });

  it('adds a PREFIX line for an undeclared prefix, once', () => {
    const { out } = run('one-line', {
      kind: 'add',
      s: '?o',
      p: 'foaf:name',
      o: '?n',
      declare: [{ prefix: 'foaf', iri: 'http://xmlns.com/foaf/0.1/' }],
    });
    expect(out.startsWith('PREFIX foaf: <http://xmlns.com/foaf/0.1/>\nSELECT')).toBe(true);
    const { out: again } = run('a3', {
      kind: 'add',
      s: '?o',
      p: 'ex:name',
      o: '?n',
      declare: [{ prefix: 'ex', iri: 'http://example.org/' }],
    });
    expect(again.match(/PREFIX/g)).toHaveLength(1);
  });

  it('deletes one object of a `,` list, one entry of a `;` list, or a statement', () => {
    const { text, p } = fx('lists');
    const idx = (o: string) => triplesOf(p).findIndex((t) => t.triple.o.text === o);
    const del = (o: string) =>
      applyChanges(text, edit(text, p, { kind: 'delete', edge: edgeAt(p, idx(o)) }));
    expect(del('?a')).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * {\n  ?s ex:p ?b , ?c ;\n     ex:q ?d ;\n     ex:r ?e .\n}\n',
    );
    expect(del('?c')).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * {\n  ?s ex:p ?a, ?b ;\n     ex:q ?d ;\n     ex:r ?e .\n}\n',
    );
    expect(del('?d')).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * {\n  ?s ex:p ?a, ?b , ?c ;\n     ex:r ?e .\n}\n',
    );
    expect(del('?e')).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * {\n  ?s ex:p ?a, ?b , ?c ;\n     ex:q ?d .\n}\n',
    );
    for (const o of ['?a', '?b', '?c', '?d', '?e']) expect(parses(del(o)), o).toBe(true);
    // a statement of one triple goes with its line, CRLF included
    const crlf = fx('crlf');
    const out = applyChanges(
      crlf.text,
      edit(crlf.text, crlf.p, { kind: 'delete', edge: edgeAt(crlf.p, 0) }),
    );
    expect(out).toBe('SELECT * {\r\n  ?o <http://x/q> ?r .\r\n}\r\n');
  });

  it('moves a triple out of an OPTIONAL, deleting the OPTIONAL when it empties', () => {
    const { text, p } = fx('optional');
    const idx = (o: string) => triplesOf(p).findIndex((t) => t.triple.o.text === o);
    const lone = applyChanges(
      text,
      edit(text, p, { kind: 'optional', edge: edgeAt(p, idx('?c')) }),
    );
    // after the last editable item of the group, which is the other OPTIONAL
    expect(lone).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n  ?org ex:name ?n .\n  OPTIONAL { ?org ex:size ?z . ?z ex:unit ?u }\n  ?org ex:locatedIn ?c .\n}\n',
    );
    const shared = applyChanges(
      text,
      edit(text, p, { kind: 'optional', edge: edgeAt(p, idx('?u')) }),
    );
    expect(shared).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n  ?org ex:name ?n .\n  OPTIONAL { ?org ex:locatedIn ?c . } .\n  OPTIONAL { ?org ex:size ?z .  }\n  ?z ex:unit ?u .\n}\n',
    );
    expect(parses(lone) && parses(shared)).toBe(true);
  });

  it('R2: an edit changes only the ranges it names, plus its insertions', () => {
    const { text, p } = fx('bind');
    const placed = triplesOf(p);
    for (let i = 0; i < placed.length; i++) {
      if (!drawing(p).edges.some((x) => x.triple?.order === i)) continue;
      const r = removalRange(text, placed[i]);
      const changes = edit(text, p, { kind: 'delete', edge: edgeAt(p, i) });
      expect(changes).toEqual([{ from: r.from, to: r.to, insert: '' }]);
      expect(r.from).toBeGreaterThanOrEqual(placed[i].triple.range.from - 3);
    }
  });

  it('names a triple by its edge, so a moved text still edits the selected one', () => {
    const before = fx('two');
    const target = drawing(before.p).edges.find((x) => x.label === 'ex:q')!.id;
    // a triple typed before it: the old index would name ex:p now
    const moved = fx('two-moved');
    const out = applyChanges(
      moved.text,
      edit(moved.text, moved.p, { kind: 'delete', edge: target }),
    );
    expect(out).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * {\n  ?s ex:z ?z .\n  ?s ex:p ?a .\n}\n',
    );
    expect(resolveEdit(moved.p, target)!.triple.p.text).toBe('ex:q');
  });

  it('refuses a prefix neither the query nor the dataset declares', () => {
    const { text, p } = fx('one-line');
    expect(() => edit(text, p, { kind: 'add', s: '?s', p: 'missing:p', o: '?o' })).toThrow(
      'The prefix "missing:" is not declared',
    );
    // two undeclared prefixes the dataset knows give two PREFIX lines
    const out = applyChanges(
      text,
      edit(text, p, {
        kind: 'add',
        s: 'a:x',
        p: 'b:y',
        o: '?o',
        declare: [
          { prefix: 'a', iri: 'http://a/' },
          { prefix: 'b', iri: 'http://b/' },
          { prefix: 'a', iri: 'http://a/' },
        ],
      }),
    );
    expect(out.match(/PREFIX/g)).toHaveLength(2);
    expect(parses(out)).toBe(true);
  });

  it('resolves a prefixed or relative subject to find its group', () => {
    const { text, p } = fx('optional-iri');
    const out = applyChanges(text, edit(text, p, { kind: 'add', s: 'ex:a', p: 'ex:r', o: '?c' }));
    expect(out).toContain('OPTIONAL { ex:a ex:q ?x .\n');
    expect(out).toContain('ex:a ex:r ?c .');
    expect(out.indexOf('ex:a ex:r ?c')).toBeLessThan(out.lastIndexOf('}\n}'));
    const full = applyChanges(
      text,
      edit(text, p, {
        kind: 'add',
        s: '<http://example.org/a>',
        p: 'ex:r',
        o: '?c',
      }),
    );
    expect(full.indexOf('<http://example.org/a> ex:r ?c')).toBeLessThan(full.indexOf('}\n}'));
    expect(parses(out) && parses(full)).toBe(true);
    const base = fx('base');
    expect(resolveTerm('<thing>', base.p).value).toBe('http://x/thing');
    expect(resolveTerm('a', base.p).value).toBe('http://www.w3.org/1999/02/22-rdf-syntax-ns#type');
    // BASE gives one node for `<thing>` and `<http://x/thing>`
    expect(drawing(base.p).parts).toBe(1);
  });

  it('takes a datatype of exactly one IRI or prefixed name', () => {
    expect(checkTerm('"x"^^<http://t>', 'o')).toBe('"x"^^<http://t>');
    expect(checkTerm('"1"^^xsd:int', 'o')).toBe('"1"^^xsd:int');
    expect(() => checkTerm('"x"^^<http://t>.?x<http://p>?y', 'o')).toThrow(EditError);
    expect(() => checkTerm('"x"^^xsd:int ?y', 'o')).toThrow(EditError);
    expect(() => checkTerm('<http://a> <http://b>', 's')).toThrow(EditError);
    // dots inside a prefixed name, not at its end
    expect(checkTerm('"x"^^ex:version.1', 'o')).toBe('"x"^^ex:version.1');
    expect(checkTerm('ex:a.b', 's')).toBe('ex:a.b');
    expect(() => checkTerm('"x"^^ex:v. ?y', 'o')).toThrow(EditError);
  });

  it('R3: the gate refuses a result that does not agree with the edit', () => {
    const { text, p } = fx('two');
    const add: Edit = { kind: 'add', s: '?s', p: 'ex:r', o: '?c' };
    const after = fx('two-moved').p;
    // a different triple than the one asked for
    expect(agrees(p, add, after)).toBe(false);
    expect(agrees(p, { ...add, p: 'ex:z', o: '?z' }, after)).toBe(true);
    // deleting a triple must remove exactly it
    const del: Edit = { kind: 'delete', edge: drawing(after).edges[0].id };
    expect(agrees(after, del, p)).toBe(true);
    expect(agrees(after, { kind: 'delete', edge: drawing(after).edges[1].id }, p)).toBe(false);
    expect(agrees(p, { kind: 'optional', edge: drawing(p).edges[0].id }, p)).toBe(false);
    expect(text).toContain('ex:q');
  });

  it('adds a `.` after a locked statement, before its comment', () => {
    const { out } = run('locked-stmt', {
      kind: 'add',
      s: '?s',
      p: '<http://r>',
      o: '?x',
    });
    expect(out).toBe(
      // no byte of the locked statement and its comment changes: the `.` opens the new line
      'SELECT * {\n  ?s <http://p> [ <http://q> 1 ] # c\n  . ?s <http://r> ?x .\n}\n',
    );
    expect(parses(out)).toBe(true);
  });

  it('finds separator dots after comments and line breaks', () => {
    const { text, p } = fx('dot-comment');
    const opt = drawing(p).edges.find((x) => x.optional)!.id;
    const out = applyChanges(text, edit(text, p, { kind: 'optional', edge: opt }));
    expect(out).toBe(
      // the OPTIONAL goes with its line and trailing comment, its `.` on the next line alone
      'SELECT * {\n  ?s <http://p> ?v .\n   FILTER(?v > 1)\n  .\n  ?s <http://p> ?o .\n}\n',
    );
    expect(parses(out)).toBe(true);
    // a triple after the FILTER goes after its `.` on the next line
    const { out: added } = run('dot-comment', {
      kind: 'add',
      s: '?v',
      p: '<http://q>',
      o: '?w',
    });
    expect(added).toBe(
      'SELECT * {\n  ?s <http://p> ?v .\n  OPTIONAL { ?s <http://p> ?o } # note\n  . FILTER(?v > 1)\n  .\n  ?v <http://q> ?w .\n}\n',
    );
    expect(parses(added)).toBe(true);
  });

  it('drops an edge id that typing made ambiguous', () => {
    const before = drawing(fx('dups').p);
    const after = drawing(fx('dups-changed').p);
    const second = before.edges[1].id;
    // the second required copy's id now names the OPTIONAL copy
    expect(after.edges.find((x) => x.id === second)!.optional).toBe(true);
    expect(stillNames(before, after, second)).toBe(false);
    const a3 = drawing(fx('a3').p);
    expect(stillNames(a3, a3, a3.edges[0].id)).toBe(true);
    expect(stillNames(a3, drawing(fx('two').p), a3.edges[0].id)).toBe(false);
  });

  it('resolves relative IRIs as the parser does, without normalizing them', () => {
    expect(resolveIri('é', 'http://example.org/')).toBe('http://example.org/é');
    expect(resolveIri('x', 'http://Example.org:80/a/b')).toBe('http://Example.org:80/a/x');
    // RFC 3986 §5.4
    const b = 'http://a/b/c/d;p?q';
    expect(resolveIri('g', b)).toBe('http://a/b/c/g');
    expect(resolveIri('../g', b)).toBe('http://a/b/g');
    expect(resolveIri('../../../g', b)).toBe('http://a/g');
    expect(resolveIri('//g', b)).toBe('http://g');
    expect(resolveIri('?y', b)).toBe('http://a/b/c/d;p?y');
    expect(resolveIri('#s', b)).toBe('http://a/b/c/d;p?q#s');
    expect(resolveIri('', b)).toBe('http://a/b/c/d;p?q');
    expect(resolveIri('./g/.', b)).toBe('http://a/b/c/g/');
    expect(resolveIri('urn:x', b)).toBe('urn:x');
  });

  it('declares the dataset’s prefixes an add uses, the empty one and datatypes included', () => {
    const { p } = fx('undeclared');
    const dataset = { '': 'http://d/', ex: 'http://e/', xsd: 'http://x/' };
    expect(missingPrefixes([':s', 'ex:p', '"1"^^xsd:int', 'toString:x'], p, dataset)).toEqual([
      { prefix: '', iri: 'http://d/' },
      { prefix: 'ex', iri: 'http://e/' },
      { prefix: 'xsd', iri: 'http://x/' },
    ]);
    const { text, p: one } = fx('one-line');
    expect(() =>
      edit(text, one, { kind: 'add', s: '?o', p: '<http://x/v>', o: '"1"^^nope:t' }),
    ).toThrow('The prefix "nope:" is not declared');
  });

  it('finds the subject’s OPTIONAL by its spelling when resolution cannot', () => {
    // a prefix only the add declares, and a BASE that is not absolute
    const cases: [Name, Edit][] = [
      [
        'optional-undeclared',
        {
          kind: 'add',
          s: 'ex:a',
          p: 'ex:q',
          o: '?z',
          declare: [{ prefix: 'ex', iri: 'http://e/' }],
        },
      ],
      ['relative-base', { kind: 'add', s: '<g>', p: '<http://q>', o: '?z' }],
    ];
    for (const [name, e] of cases) {
      const { out } = run(name, e);
      const added = out.indexOf(`${(e as { s: string }).s} ${(e as { p: string }).p} ?z .`);
      expect(added, out).toBeGreaterThan(out.indexOf('OPTIONAL {'));
      expect(added, out).toBeLessThan(out.lastIndexOf('}\n}'));
    }
    expect(resolveIri('g', '../')).toBe('g');
  });

  it('refuses terms it would not write', () => {
    expect(() => checkTerm('?a b', 's')).toThrow(EditError);
    expect(() => checkTerm('"x"', 's')).toThrow(EditError);
    expect(() => checkTerm('a', 's')).toThrow(EditError);
    expect(checkTerm(' a ', 'p')).toBe('a');
    expect(checkTerm('"x"@en', 'o')).toBe('"x"@en');
    expect(checkTerm('-1.5e3', 'o')).toBe('-1.5e3');
    expect(checkTerm('ex:a', 's')).toBe('ex:a');
    expect(checkTerm('<http://x/a>', 'p')).toBe('<http://x/a>');
    const { text, p } = fx('a3');
    expect(() => edit(text, p, { kind: 'delete', edge: 'e:gone' })).toThrow(
      'The pattern changed, select the edge again',
    );
    const describe_ = fixtures.update.answer as PatternAnswer;
    expect(describe_.ok).toBe(false);
  });
});

// ------------------------------------------------- against the built module ------

const built = new URL('./wasm/sparkles_fmt_wasm_bg.wasm', import.meta.url);

/** A seeded generator (mulberry32). */
function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The triples of a pattern as text, each with whether it is optional. */
const triplesText = (p: PatternOk) =>
  triplesOf(p)
    .map(
      (t) =>
        `${t.triple.s.text} ${t.triple.p.text} ${t.triple.o.text}${t.path.length > 1 ? ' ?' : ''}`,
    )
    .sort();

describe.skipIf(!existsSync(built))('the built module', async () => {
  const bindings = existsSync(built)
    ? await import(/* @vite-ignore */ new URL('./wasm/sparkles_fmt_wasm.js', import.meta.url).href)
    : null;
  bindings?.initSync({ module: readFileSync(built) });
  const pattern = (text: string) =>
    JSON.parse(bindings.pattern(JSON.stringify({ text }))) as PatternAnswer;

  it('refuses through `strict` what the server parser refuses', () => {
    const strict = (text: string) =>
      JSON.parse(bindings.pattern(JSON.stringify({ text, strict: true }))) as PatternAnswer;
    const scope = 'SELECT * { ?s ?p ?o BIND(1 AS ?o) }';
    expect(pattern(scope).ok).toBe(true);
    expect(strict(scope).ok).toBe(false);
    expect(strict('SELECT * { ?s ?p ?o }').ok).toBe(true);
  });

  it('R3 agrees across a declared prefix and a non-ASCII relative IRI', () => {
    const cases: [string, Edit][] = [
      [
        'SELECT * { ?s ex:p ?o }',
        { kind: 'add', s: '?o', p: 'ex:q', o: '?z', declare: [{ prefix: 'ex', iri: 'http://e/' }] },
      ],
      [
        'BASE <http://example.org/> SELECT * { ?s ?p ?o }',
        { kind: 'add', s: '<é>', p: '<p>', o: '?o' },
      ],
    ];
    for (const [text, e] of cases) {
      const p = pattern(text) as PatternOk;
      const next = applyChanges(text, edit(text, p, e));
      const after = pattern(next) as PatternOk;
      expect(after.ok, next).toBe(true);
      expect(agrees(p, e, after), next).toBe(true);
    }
  });

  it('R3 checks the side of the OPTIONAL an add lands on', () => {
    const { text, p } = fx('optional-undeclared');
    const e: Edit = {
      kind: 'add',
      s: 'ex:a',
      p: 'ex:q',
      o: '?z',
      declare: [{ prefix: 'ex', iri: 'http://e/' }],
    };
    const good = pattern(applyChanges(text, edit(text, p, e))) as PatternOk;
    expect(agrees(p, e, good)).toBe(true);
    // the same triple written at the root does not agree
    const bad = pattern(
      'PREFIX ex: <http://e/>\n' +
        text.replace('?x <http://r> ?y .', '?x <http://r> ?y .\n  ex:a ex:q ?z .'),
    ) as PatternOk;
    expect(bad.ok).toBe(true);
    expect(agrees(p, e, bad)).toBe(false);
  });

  it('answers the fixtures', () => {
    for (const [name, f] of Object.entries(fixtures))
      expect(pattern(f.text), name).toEqual(f.answer);
  });

  it('A4, R2 and R3: twenty seeded edits keep locked blocks and agree with the parse', () => {
    const terms = ['?person', '?name', '?b', '?y', '?x', 'ex:a', '"lit"', '?z'];
    const preds = ['ex:knows', 'ex:p', 'a', '?pp'];
    for (const seed of [1, 2, 3, 4, 5]) {
      const r = rng(seed);
      const pick = <T>(xs: T[]) => xs[Math.floor(r() * xs.length)];
      let text = fixtures.bind.text;
      const lockedBefore = lockedOf(pattern(text) as PatternOk).map((b) => b.text);
      for (let step = 0; step < 20; step++) {
        const p = pattern(text) as PatternOk;
        expect(p.ok, text).toBe(true);
        const edges = drawing(p).edges.filter((x) => x.triple);
        const roll = r();
        let e: Edit;
        if (edges.length === 0 || roll < 0.45) {
          const s = pick(terms.filter((t) => !t.startsWith('"')));
          e = { kind: 'add', s, p: pick(preds), o: pick(terms) };
        } else
          e = {
            kind: roll < 0.7 ? 'delete' : 'optional',
            edge: pick(edges).id,
          };
        let changes: Change[];
        try {
          changes = edit(text, p, e);
        } catch (err) {
          if (err instanceof EditError) continue;
          throw err;
        }
        const next = applyChanges(text, changes);
        const q = pattern(next);
        expect(q.ok, `${JSON.stringify(e)}\n${next}`).toBe(true);
        const after = q as PatternOk;
        // R2: the locked blocks are byte-identical, and the only deletion is the range
        // the action names: the triple's, or the OPTIONAL it empties
        expect(lockedOf(after).map((b) => b.text)).toEqual(lockedBefore);
        const deletions = changes.filter((c) => c.to > c.from);
        // an emptied OPTIONAL's `.` on another line is a deletion of its own
        expect(deletions.length).toBe(e.kind === 'add' ? 0 : deletions.length > 1 ? 2 : 1);
        expect(changes.every((c) => c.to === c.from || c.insert === '')).toBe(true);
        expect(agrees(p, e, after), `${JSON.stringify(e)}\n${next}`).toBe(true);
        if (e.kind !== 'add') {
          const placed = resolveEdit(p, e.edge)!;
          const d = deletions[0];
          const named = removalRange(text, placed);
          const g = placed.path[placed.path.length - 1];
          const ownGroup =
            g.kind === 'optional' && d.from <= g.range.from && g.range.to <= d.to
              ? /^[\s.]*$/.test(text.slice(d.from, g.range.from) + text.slice(g.range.to, d.to))
              : false;
          expect(ownGroup || (d.from === named.from && d.to === named.to)).toBe(true);
        }
        // R3: the parse gives the pattern the edit describes
        const before = triplesText(p);
        const now = triplesText(after);
        if (e.kind === 'add') {
          expect(now.length).toBe(before.length + 1);
          const word = `${e.s} ${e.p} ${e.o}`;
          const count = (xs: string[]) => xs.filter((x) => x.replace(/ \?$/, '') === word).length;
          expect(count(now)).toBe(count(before) + 1);
        } else if (e.kind === 'delete') {
          expect(now.length).toBe(before.length - 1);
          const gone = resolveEdit(p, e.edge)!.triple;
          const word = `${gone.s.text} ${gone.p.text} ${gone.o.text}`;
          const count = (xs: string[]) => xs.filter((x) => x.replace(/ \?$/, '') === word).length;
          expect(count(now)).toBe(count(before) - 1);
        } else {
          expect(now.length).toBe(before.length);
          const t = resolveEdit(p, e.edge)!;
          const word = `${t.triple.s.text} ${t.triple.p.text} ${t.triple.o.text}`;
          const flipped = t.path.length > 1 ? word : `${word} ?`;
          expect(now).toContain(flipped);
        }
        text = next;
      }
      expect(parses(text), text).toBe(true);
    }
  });
});

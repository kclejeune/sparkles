import { existsSync, readFileSync } from 'node:fs';
import { Store } from 'oxigraph';
import { describe, expect, it } from 'vitest';
import fixtures from './pattern-fixtures.json';
import {
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
    expect(renamed(before, drawing(swap))).toEqual(['v:org', 'v:company']);
    // two renames at once are not guessed
    const both = JSON.parse(
      JSON.stringify(swap).replaceAll('"value":"person"', '"value":"someone"'),
    );
    expect(renamed(before, drawing(both))).toBeNull();
    expect(text).toContain('?org');
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
    const { out } = run('a3', { kind: 'optional', triple: i });
    expect(out).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT ?person ?org WHERE {\n  ?person a ex:Person ; ex:worksFor ?org .\n  ?org a ex:Org .\n  OPTIONAL { ?org ex:locatedIn ex:lyon . }\n}\nLIMIT 1000\n',
    );
    expect(parses(out)).toBe(true);
  });

  it('adds a triple after the last triple of its subject’s group', () => {
    const { out } = run('a3', { kind: 'add', s: '?person', p: 'ex:age', o: '?age' });
    expect(out).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT ?person ?org WHERE {\n  ?person a ex:Person ; ex:worksFor ?org .\n  ?org a ex:Org ; ex:locatedIn ex:lyon .\n  ?person ex:age ?age .\n}\nLIMIT 1000\n',
    );
  });

  it('places a new triple after a locked block that uses a variable it binds (§4.2)', () => {
    const { out } = run('locked-after', { kind: 'add', s: '?c', p: 'ex:z', o: '?w' });
    expect(out).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n  ?a ex:p ?b .\n  BIND(?b + 1 AS ?c)\n  ?c ex:z ?w .\n  MINUS { ?a ex:q ?d }\n}\n',
    );
    // a triple that binds nothing the blocks use goes before them
    const { out: before } = run('locked-after', { kind: 'add', s: '?x', p: 'ex:z', o: '1' });
    expect(before).toContain('?a ex:p ?b .\n  ?x ex:z 1 .\n  BIND');
    expect(parses(out) && parses(before)).toBe(true);
  });

  it('adds a `.` to a statement without one and keeps a trailing comment on its line', () => {
    const { out } = run('no-dot', { kind: 'add', s: '?o', p: '<http://x/q>', o: '?r' });
    expect(out).toBe('SELECT * {\n  ?s <http://x/p> ?o . # last\n  ?o <http://x/q> ?r .\n}\n');
    expect(parses(out)).toBe(true);
  });

  it('adds to a query on one line and to an empty group', () => {
    const { out } = run('one-line', { kind: 'add', s: '?o', p: 'a', o: '<http://x/C>' });
    expect(out).toBe('SELECT * { ?s <http://x/p> ?o .\n  ?o a <http://x/C> .\n }');
    expect(parses(out)).toBe(true);
    const { out: empty } = run('empty', { kind: 'add', s: '?s', p: '?p', o: '?o' });
    expect(empty).toBe('SELECT * WHERE {\n  ?s ?p ?o .\n}\n');
  });

  it('adds a PREFIX line for an undeclared prefix, once', () => {
    const { out } = run('one-line', {
      kind: 'add',
      s: '?o',
      p: 'foaf:name',
      o: '?n',
      declare: { prefix: 'foaf', iri: 'http://xmlns.com/foaf/0.1/' },
    });
    expect(out.startsWith('PREFIX foaf: <http://xmlns.com/foaf/0.1/>\nSELECT')).toBe(true);
    const { out: again } = run('a3', {
      kind: 'add',
      s: '?o',
      p: 'ex:name',
      o: '?n',
      declare: { prefix: 'ex', iri: 'http://example.org/' },
    });
    expect(again.match(/PREFIX/g)).toHaveLength(1);
  });

  it('deletes one object of a `,` list, one entry of a `;` list, or a statement', () => {
    const { text, p } = fx('lists');
    const idx = (o: string) => triplesOf(p).findIndex((t) => t.triple.o.text === o);
    const del = (o: string) =>
      applyChanges(text, edit(text, p, { kind: 'delete', triple: idx(o) }));
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
    const out = applyChanges(crlf.text, edit(crlf.text, crlf.p, { kind: 'delete', triple: 0 }));
    expect(out).toBe('SELECT * {\r\n  ?o <http://x/q> ?r .\r\n}\r\n');
  });

  it('moves a triple out of an OPTIONAL, deleting the OPTIONAL when it empties', () => {
    const { text, p } = fx('optional');
    const idx = (o: string) => triplesOf(p).findIndex((t) => t.triple.o.text === o);
    const lone = applyChanges(text, edit(text, p, { kind: 'optional', triple: idx('?c') }));
    // after the last editable item of the group, which is the other OPTIONAL
    expect(lone).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n  ?org ex:name ?n .\n  OPTIONAL { ?org ex:size ?z . ?z ex:unit ?u }\n  ?org ex:locatedIn ?c .\n}\n',
    );
    const shared = applyChanges(text, edit(text, p, { kind: 'optional', triple: idx('?u') }));
    expect(shared).toBe(
      'PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n  ?org ex:name ?n .\n  OPTIONAL { ?org ex:locatedIn ?c . } .\n  OPTIONAL { ?org ex:size ?z .  }\n  ?z ex:unit ?u .\n}\n',
    );
    expect(parses(lone) && parses(shared)).toBe(true);
  });

  it('R2: an edit changes only the ranges it names, plus its insertions', () => {
    const { text, p } = fx('bind');
    const placed = triplesOf(p);
    for (let i = 0; i < placed.length; i++) {
      const r = removalRange(text, placed[i]);
      const changes = edit(text, p, { kind: 'delete', triple: i });
      expect(changes).toEqual([{ from: r.from, to: r.to, insert: '' }]);
      expect(r.from).toBeGreaterThanOrEqual(placed[i].triple.range.from - 3);
    }
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
    expect(() => edit(text, p, { kind: 'delete', triple: 99 })).toThrow(EditError);
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
        const n = triplesOf(p).length;
        const roll = r();
        let e: Edit;
        if (n === 0 || roll < 0.45) {
          const s = pick(terms.filter((t) => !t.startsWith('"')));
          e = { kind: 'add', s, p: pick(preds), o: pick(terms) };
        } else e = { kind: roll < 0.7 ? 'delete' : 'optional', triple: Math.floor(r() * n) };
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
        expect(deletions.length).toBe(e.kind === 'add' ? 0 : 1);
        expect(changes.every((c) => c.to === c.from || c.insert === '')).toBe(true);
        if (e.kind !== 'add') {
          const placed = triplesOf(p)[e.triple];
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
          const gone = triplesOf(p)[e.triple].triple;
          const word = `${gone.s.text} ${gone.p.text} ${gone.o.text}`;
          const count = (xs: string[]) => xs.filter((x) => x.replace(/ \?$/, '') === word).length;
          expect(count(now)).toBe(count(before) - 1);
        } else {
          expect(now.length).toBe(before.length);
          const t = triplesOf(p)[e.triple];
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

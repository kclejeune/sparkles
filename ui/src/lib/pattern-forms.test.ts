import { existsSync, readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import type { ClassProfile, SchemaSummary } from './api';
import { effectivePrefixes, filtersOf, type PatternAnswer, type PatternOk } from './pattern';
import { constant, expression, formOf, formsFor, kindOf, XSD, type Op } from './pattern-filters';
import {
  datatypesOf,
  localName,
  narrow,
  ProfileCache,
  rankPredicates,
  SchemaSource,
  varName,
  type SchemaApi,
} from './pattern-schema';

const EX = 'http://example.org/';
const cls = (iri: string, instances: number, builtin = false) =>
  ({ iri, builtin, observed: { instances }, declared: {} }) as never;
const pred = (iri: string, triples: number) =>
  ({ iri, observed: { triples }, declared: {} }) as never;
const profile = (
  c: string,
  props: [string, number, string[]?][],
  incoming: [string, number][] = [],
): ClassProfile =>
  ({
    class: c,
    builtin: false,
    instances: 1,
    properties: props.map(([predicate, triples, dts]) => ({
      predicate,
      instances: 1,
      triples,
      minPerInstance: 1,
      maxPerInstance: 1,
      objects: dts ? { literals: dts.map((datatype) => ({ datatype, triples: 1 })) } : {},
      objectClasses: [],
    })),
    incoming: incoming.map(([predicate, triples]) => ({ predicate, triples, instances: 1 })),
  }) as ClassProfile;

function fakeApi(fail = false) {
  const calls: string[] = [];
  const api: SchemaApi = {
    async summary(ds, opts) {
      calls.push(`summary ${ds} ${opts.limit} ${opts.at ?? ''}`);
      if (fail) throw new Error('403 forbidden');
      return {
        snapshot: { commit: 'c1', generation: 1 },
        classes: {
          items: [
            cls(`${EX}Person`, 30),
            cls(`${EX}Org`, 5),
            cls(`${EX}City`, 9),
            cls('http://www.w3.org/2002/07/owl#Thing', 99, true),
          ],
          total: 5,
          next: 'k',
        },
        predicates: {
          items: [pred(`${EX}name`, 40), pred(`${EX}worksFor`, 20)],
          total: 2,
          next: null,
        },
      } as unknown as SchemaSummary;
    },
    async page(ds, list, cursor) {
      calls.push(`page ${list} ${cursor}`);
      return { items: [cls(`${EX}Team`, 2)], total: 5, next: null } as never;
    },
    async profile(ds, c) {
      calls.push(`profile ${c}`);
      return profile(c, [
        [`${EX}age`, 7, [`${XSD}integer`]],
        [`${EX}worksFor`, 3],
      ]);
    },
  };
  return { api, calls };
}

describe('the schema behind the pickers', () => {
  it('reads the first page once, pages on demand and ranks the top classes', async () => {
    const { api, calls } = fakeApi();
    const s = new SchemaSource(api, 'ds@dev', 'abc');
    await Promise.all([s.load(), s.load()]);
    expect(calls).toEqual(['summary ds@dev 1000 abc']);
    expect(s.topClasses(2).map((c) => localName(c.iri))).toEqual(['Person', 'City']);
    await s.more('classes');
    await s.more('classes');
    expect(calls.filter((c) => c.startsWith('page'))).toEqual(['page classes k']);
    expect(s.classes.map((c) => localName(c.iri))).toContain('Team');
    await s.profile(`${EX}Person`);
    await s.profile(`${EX}Person`);
    expect(calls.filter((c) => c.startsWith('profile'))).toHaveLength(1);
  });

  it('says why when the schema does not load, so the pickers take typed IRIs', async () => {
    const s = new SchemaSource(fakeApi(true).api, 'ds');
    await s.load();
    expect(s.error).toBe('403 forbidden');
    expect(s.predicates).toEqual([]);
  });

  it('ranks the source class’s predicates first, then those reaching the target', () => {
    const preds = [pred(`${EX}name`, 40), pred(`${EX}worksFor`, 20), pred(`${EX}locatedIn`, 50)];
    const src = profile(`${EX}Person`, [
      [`${EX}worksFor`, 3],
      [`${EX}name`, 9],
    ]);
    const tgt = profile(`${EX}City`, [], [[`${EX}locatedIn`, 4]]);
    expect(rankPredicates(preds, src, tgt).map((c) => [localName(c.iri), c.count, c.hint])).toEqual(
      [
        ['name', 9, 'used by the class'],
        ['worksFor', 3, 'used by the class'],
        ['locatedIn', 4, 'reaches the class'],
      ],
    );
    expect(rankPredicates(preds, null, null).map((c) => localName(c.iri))).toEqual([
      'locatedIn',
      'name',
      'worksFor',
    ]);
    expect(
      narrow(rankPredicates(preds, null, null), 'WORK', localName).map((c) => localName(c.iri)),
    ).toEqual(['worksFor']);
    expect(datatypesOf(profile('c', [[`${EX}age`, 1, [`${XSD}integer`]]]), `${EX}age`)).toEqual([
      `${XSD}integer`,
    ]);
    expect(datatypesOf(null, `${EX}age`)).toBeNull();
  });

  it('names variables after U03 §4.2', () => {
    expect(varName(`${EX}Person`, [])).toBe('person');
    expect(varName(`${EX}Person`, ['person'])).toBe('person2');
    expect(varName(`${EX}works-for`, [])).toBe('worksFor');
    expect(varName(null, ['n1'])).toBe('n2');
    expect(varName(`${EX}Timeout`, [])).toBe('timeout2');
    expect(varName(`${EX}3d`, [])).toBe('v3d');
  });
});

describe('the filter forms', () => {
  it('offers forms by datatype', () => {
    expect(kindOf(`${XSD}integer`)).toBe('number');
    expect(formsFor([`${XSD}integer`])).toEqual([
      '=',
      '!=',
      '<',
      '<=',
      '>',
      '>=',
      'bound',
      'isIRI',
      'isLiteral',
    ]);
    expect(formsFor([`${XSD}string`])).toContain('strstarts');
    expect(formsFor([`${XSD}string`])).not.toContain('<');
    expect(formsFor(['http://www.w3.org/1999/02/22-rdf-syntax-ns#langString'])).toContain(
      'langmatches',
    );
    expect(formsFor(null)).toHaveLength(14);
  });

  it('keeps regex flags, booleans of strings and prefixed names (review round 1)', () => {
    expect(expression('regex', 'x', 'a', null, 's')).toBe('REGEX(?x, "a", "s")');
    expect(expression('regex', 'x', 'a', null, '')).toBe('REGEX(?x, "a")');
    expect(formOf({ op: 'regex', var: 'x', args: ['"a"', '"s"'] })).toEqual({
      op: 'regex',
      value: 'a',
      flags: 's',
    });
    expect(formOf({ op: 'regex', var: 'x', args: ['"a"'] }).flags).toBe('');
    expect(constant('true', `${XSD}string`)).toBe('"true"');
    expect(constant('true')).toBe('true');
    expect(constant('ex:alice')).toBe('ex:alice');
    expect(constant('a b:c')).toBe('"a b:c"');
  });

  it('leaves out the dataset prefixes the query shadows (review round 1)', () => {
    expect(
      effectivePrefixes([{ prefix: 'ex', iri: 'http://query/' }], {
        ex: 'http://dataset/',
        foaf: 'http://xmlns.com/foaf/0.1/',
      }),
    ).toEqual([{ prefix: 'foaf', iri: 'http://xmlns.com/foaf/0.1/' }]);
  });

  // the pane reads at most 20 classes at a time, under the cache's bound of 50
  it('asks for each profile once while answers arrive (review round 2)', async () => {
    let asked = 0;
    const pending: (() => void)[] = [];
    const cache = new ProfileCache(
      (c) =>
        new Promise((resolve) => {
          asked++;
          pending.push(() => resolve(profile(c, [])));
        }),
    );
    const classes = Array.from({ length: 30 }, (_, i) => `${EX}C${i}`);
    for (let round = 0; round < 40; round++) {
      for (const c of classes) cache.get(c, () => {});
      pending.shift()?.();
      await Promise.resolve();
    }
    expect(asked).toBe(30);
  });

  it('pages one request at a time per list (review round 1)', async () => {
    const { api, calls } = fakeApi();
    const s = new SchemaSource(api, 'ds');
    await s.load();
    await Promise.all([s.more('classes'), s.more('classes')]);
    expect(calls.filter((c) => c.startsWith('page'))).toHaveLength(1);
    expect(s.classes.filter((c) => c.iri.endsWith('Team'))).toHaveLength(1);
  });

  it('writes constants', () => {
    expect(constant('42')).toBe('42');
    expect(constant('2020-01-01', `${XSD}date`)).toBe(`"2020-01-01"^^<${XSD}date>`);
    expect(constant('<http://x>')).toBe('<http://x>');
    expect(constant('say "hi"')).toBe('"say \\"hi\\""');
    expect(constant('42', `${XSD}string`)).toBe('"42"');
  });

  const built = new URL('./wasm/sparkles_fmt_wasm_bg.wasm', import.meta.url);
  it.skipIf(!existsSync(built))('every form reads back as a chip in the same form', async () => {
    const bindings = await import(
      /* @vite-ignore */ new URL('./wasm/sparkles_fmt_wasm.js', import.meta.url).href
    );
    bindings.initSync({ module: readFileSync(built) });
    const cases: [Op, string, string | null][] = [
      ['=', '42', null],
      ['!=', 'o"k', `${XSD}string`],
      ['<', '2020-01-01', `${XSD}date`],
      ['>=', '1.5', null],
      ['contains', 'a "b"', null],
      ['strstarts', 'A', null],
      ['strends', 'z', null],
      ['regex', '^a.*', null],
      ['langmatches', 'en', null],
      ['bound', '', null],
      ['isIRI', '', null],
      ['isLiteral', '', null],
    ];
    for (const [op, value, dt] of cases) {
      const text = `SELECT * { ?s <http://p> ?x FILTER(${expression(op, 'x', value, dt)}) }`;
      const a = JSON.parse(
        bindings.pattern(JSON.stringify({ text, strict: true })),
      ) as PatternAnswer;
      expect(a.ok, text).toBe(true);
      const f = filtersOf(a as PatternOk)[0].filter;
      expect(f.expr, text).not.toBeNull();
      expect(f.expr!.var).toBe('x');
      const back = formOf(f.expr!);
      expect(back.op).toBe(op);
      if (['contains', 'strstarts', 'strends', 'regex', 'langmatches'].includes(op))
        expect(back.value).toBe(value);
    }
  });
});

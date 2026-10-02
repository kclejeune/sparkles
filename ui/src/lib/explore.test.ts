import { describe, expect, it } from 'vitest';
import type { ConstraintsLayer, SchemaClass, SchemaPredicate, SchemaSummary, Term } from './api';
import {
  cardinalityChips,
  constraintChips,
  constraintLines,
  constraintSummary,
  enforcementText,
  hierarchyRoots,
  objectSegments,
  OWL_FUNCTIONAL,
  pickLabel,
  reduceSupers,
  schemaFromSummary,
  snapshotLine,
  type ClassInfo,
} from './explore';

/** Class map from `sub → supers` edges, with `subs` filled in as loadSchema does. */
function classesOf(edges: Record<string, string[]>, labels: Record<string, string> = {}) {
  const classes = new Map<string, ClassInfo>();
  const get = (iri: string): ClassInfo => {
    let c = classes.get(iri);
    if (!c) {
      c = {
        iri,
        supers: [],
        subs: [],
        instances: 0,
        declared: true,
        builtin: false,
        assertedSupers: [],
        equivalents: [],
        disjoint: [],
        cycle: [],
      };
      classes.set(iri, c);
    }
    return c;
  };
  for (const [c, supers] of Object.entries(edges)) {
    get(c).supers = [...supers];
    for (const s of supers) get(s).subs.push(c);
  }
  for (const [c, l] of Object.entries(labels)) get(c).label = l;
  return classes;
}

describe('reduceSupers', () => {
  it('drops supers implied by other supers', () => {
    const r = reduceSupers(
      new Map([
        ['A', ['B', 'C']],
        ['B', ['C']],
        ['C', []],
      ]),
    );
    expect(r.get('A')).toEqual(['B']);
    expect(r.get('B')).toEqual(['C']);
  });

  it('reduces a fully closed hierarchy to a chain', () => {
    const r = reduceSupers(
      new Map([
        ['D', ['C', 'B', 'A']],
        ['C', ['B', 'A']],
        ['B', ['A']],
      ]),
    );
    expect(r.get('D')).toEqual(['C']);
    expect(r.get('C')).toEqual(['B']);
  });

  it('keeps both members of an equivalence cycle', () => {
    const r = reduceSupers(
      new Map([
        ['A', ['B', 'C']],
        ['B', ['A', 'C']],
        ['C', []],
      ]),
    );
    expect(r.get('A')).toEqual(['B']);
    expect(r.get('B')).toEqual(['A']);
  });

  it('never empties a non-empty super list', () => {
    const r = reduceSupers(new Map([['A', ['A']]]));
    expect(r.get('A')).toEqual(['A']);
  });
});

describe('hierarchyRoots', () => {
  it('ranks roots by subclass count, then by label', () => {
    const classes = classesOf(
      { B1: ['B'], B2: ['B'], A1: ['A'], Z: [], Y: [] },
      { Z: 'alpha', Y: 'beta' },
    );
    expect(hierarchyRoots(classes)).toEqual(['B', 'A', 'Z', 'Y']);
  });

  // A ⊑ B ⊑ A with nothing above: both have supers, so neither is an ordinary root.
  it('promotes one member of a rootless cycle', () => {
    const roots = hierarchyRoots(classesOf({ A: ['B'], B: ['A'], C: ['A'] }));
    expect(roots).toEqual(['A']);
  });

  it('promotes a class whose only super is itself', () => {
    expect(hierarchyRoots(classesOf({ D: ['D'], E: [] }))).toEqual(['E', 'D']);
  });

  it('does not add a cycle that hangs below an ordinary root', () => {
    expect(hierarchyRoots(classesOf({ A: ['B', 'T'], B: ['A'], T: [] }))).toEqual(['T']);
  });

  it('reaches every class', () => {
    const classes = classesOf({ A: ['B'], B: ['C'], C: ['A'], X: ['Y'], Y: ['X'], Q: [] });
    const roots = hierarchyRoots(classes);
    const seen = new Set<string>();
    const walk = (iri: string) => {
      if (seen.has(iri)) return;
      seen.add(iri);
      classes.get(iri)!.subs.forEach(walk);
    };
    roots.forEach(walk);
    expect(seen.size).toBe(classes.size);
    expect(roots.length).toBe(3);
  });
});

describe('pickLabel', () => {
  const lit = (value: string, lang?: string): Term =>
    lang ? { type: 'literal', value, 'xml:lang': lang } : { type: 'literal', value };

  it('prefers English, then untagged, then anything', () => {
    expect(pickLabel([lit('Hund', 'de'), lit('dog'), lit('Dog', 'en-GB')])).toBe('Dog');
    expect(pickLabel([lit('Hund', 'de'), lit('dog')])).toBe('dog');
    expect(pickLabel([lit('Hund', 'de')])).toBe('Hund');
  });

  it('ignores non-literals and unbound values', () => {
    expect(pickLabel([undefined, { type: 'uri', value: 'http://ex.org/a' }])).toBeUndefined();
  });
});

describe('hierarchyRoots with server roots', () => {
  it('uses the given roots and still reaches every class', () => {
    // the server's roots miss X (e.g. its only super was filtered out by the client)
    const classes = classesOf({ A: [], B: ['A'], X: ['Y'], Y: ['X'] });
    expect(hierarchyRoots(classes, ['A', 'A', 'missing'])).toEqual(['A', 'X']);
  });
});

// --- schema report adapter -------------------------------------------------------

const EX = 'http://ex.org/';
const OWL = 'http://www.w3.org/2002/07/owl#';
const XSD = 'http://www.w3.org/2001/XMLSchema#';

function cls(iri: string, over: Partial<SchemaClass['declared']> = {}, instances = 0): SchemaClass {
  return {
    iri,
    builtin: !iri.startsWith(EX),
    observed: { instances },
    declared: {
      types: [],
      superClasses: [],
      equivalentClasses: [],
      disjointWith: [],
      labels: [],
      comments: [],
      ...over,
    },
  };
}

function prop(
  iri: string,
  observed: Partial<SchemaPredicate['observed']> = {},
  types: string[] = [],
): SchemaPredicate {
  return {
    iri,
    builtin: !iri.startsWith(EX),
    observed: {
      triples: 0,
      distinctSubjects: 0,
      distinctObjects: 0,
      maxPerSubject: 0,
      subjectsWithMultiple: 0,
      objects: { literals: [] },
      ...observed,
    },
    declared: {
      types,
      domains: [],
      ranges: [],
      superProperties: [],
      inverseOf: [],
      labels: [],
      comments: [],
    },
  };
}

function summary(
  classes: SchemaClass[],
  predicates: SchemaPredicate[] = [],
  hierarchy = { roots: [] as string[], cycles: [] as string[][] },
): SchemaSummary {
  return {
    schemaFormat: 1,
    dataset: 't',
    snapshot: { version: 7, generation: 'gen-0003', computedAt: '2026-01-02T12:04:00Z' },
    selection: {
      graph: 'default',
      declaredGraph: 'default',
      reasoning: true,
      declared: 'asserted',
    },
    totals: {
      triples: 0,
      classes: classes.length,
      predicates: predicates.length,
      anonymousTypeTargets: 0,
      anonymousClassExpressions: 0,
    },
    ontology: [
      {
        iri: EX + 'onto',
        labels: [{ value: 'Onto', lang: 'en' }],
        versionInfo: [{ value: '1.2' }],
        comments: [],
      },
    ],
    hierarchy,
    classes: { items: classes, total: classes.length, next: null },
    predicates: { items: predicates, total: predicates.length, next: null },
  };
}

describe('schemaFromSummary', () => {
  it('drops built-in classes outside the hierarchy and keeps owl:Thing as a super', () => {
    const s = schemaFromSummary(
      summary(
        [
          cls(EX + 'Person', { types: [OWL + 'Class'], superClasses: [OWL + 'Thing'] }, 2),
          cls(OWL + 'Class', {}, 1),
          cls(OWL + 'Thing'),
        ],
        [],
        { roots: [OWL + 'Class', OWL + 'Thing'], cycles: [] },
      ),
    );
    expect([...s.classes.keys()].sort()).toEqual([EX + 'Person', OWL + 'Thing']);
    expect(s.roots).toEqual([OWL + 'Thing']);
    expect(s.classes.get(OWL + 'Thing')!.subs).toEqual([EX + 'Person']);
    expect(s.classes.get(EX + 'Person')!.instances).toBe(2);
    expect(s.ontology).toEqual({
      iri: EX + 'onto',
      label: 'Onto',
      version: '1.2',
      comment: undefined,
    });
  });

  it('tags undeclared classes', () => {
    const s = schemaFromSummary(
      summary([cls(EX + 'Person', {}, 1), cls(EX + 'Employee', { superClasses: [EX + 'Person'] })]),
    );
    expect(s.classes.get(EX + 'Person')!.declared).toBe(false);
    expect(s.classes.get(EX + 'Employee')!.declared).toBe(false);
  });

  // A ⊑ B ⊑ A with C ⊑ A: the server names A as the cycle's root
  it('draws rootless cycles from the server root and records the other members', () => {
    const s = schemaFromSummary(
      summary(
        [
          cls(EX + 'A', { superClasses: [EX + 'B'] }),
          cls(EX + 'B', { superClasses: [EX + 'A'] }),
          cls(EX + 'C', { superClasses: [EX + 'A'] }),
        ],
        [],
        { roots: [EX + 'A'], cycles: [[EX + 'A', EX + 'B']] },
      ),
    );
    expect(s.roots).toEqual([EX + 'A']);
    expect(s.classes.get(EX + 'A')!.cycle).toEqual([EX + 'B']);
    expect(s.classes.get(EX + 'B')!.cycle).toEqual([EX + 'A']);
    expect(s.classes.get(EX + 'C')!.cycle).toEqual([]);
    expect(s.classes.get(EX + 'A')!.subs).toEqual([EX + 'B', EX + 'C']);
  });

  it('draws a transitively closed hierarchy as a tree but keeps the asserted supers', () => {
    const s = schemaFromSummary(
      summary([
        cls(EX + 'A'),
        cls(EX + 'B', { superClasses: [EX + 'A'] }),
        cls(EX + 'C', { superClasses: [EX + 'A', EX + 'B'] }),
      ]),
    );
    const c = s.classes.get(EX + 'C')!;
    expect(c.supers).toEqual([EX + 'B']);
    expect(c.assertedSupers).toEqual([EX + 'A', EX + 'B']);
    expect(s.classes.get(EX + 'A')!.subs).toEqual([EX + 'B']);
  });

  it('maps predicates with labels and observed statistics, sorted by label', () => {
    const p = prop(EX + 'zeta', { triples: 3 });
    p.declared.labels = [{ value: 'Alpha', lang: 'en' }];
    const s = schemaFromSummary(summary([], [prop(EX + 'beta'), p]));
    expect(s.properties.map((x) => x.iri)).toEqual([EX + 'zeta', EX + 'beta']);
    expect(s.properties[0].label).toBe('Alpha');
    expect(s.properties[0].observed.triples).toBe(3);
  });
});

describe('objectSegments', () => {
  it('orders kinds and datatypes by size with their share and languages', () => {
    const segs = objectSegments({
      triples: 10,
      distinctSubjects: 1,
      distinctObjects: 10,
      maxPerSubject: 10,
      subjectsWithMultiple: 1,
      objects: {
        iri: { triples: 2, distinct: 2 },
        tripleTerm: { triples: 1, distinct: 1 },
        literals: [
          {
            datatype: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#dirLangString',
            triples: 3,
            distinct: 3,
            languages: [
              { lang: 'ar', direction: 'rtl', triples: 2 },
              { lang: 'en', direction: 'ltr', triples: 1 },
            ],
          },
          { datatype: XSD + 'integer', triples: 4, distinct: 4 },
        ],
      },
    });
    expect(segs.map((s) => [s.kind, s.datatype, s.share])).toEqual([
      ['literal', XSD + 'integer', 0.4],
      ['literal', 'http://www.w3.org/1999/02/22-rdf-syntax-ns#dirLangString', 0.3],
      ['iri', undefined, 0.2],
      ['triple', undefined, 0.1],
    ]);
    expect(segs[1].languages).toEqual(['ar--rtl', 'en--ltr']);
  });
});

describe('cardinalityChips', () => {
  const info = (maxPerSubject: number, triples: number, kinds: string[] = []) => {
    const s = schemaFromSummary(summary([], [prop(EX + 'p', { maxPerSubject, triples }, kinds)]));
    return cardinalityChips(s.properties[0]);
  };

  it('reports ≤1 per subject as an observation with a warning', () => {
    const [chip, ...rest] = info(1, 5);
    expect(rest).toEqual([]);
    expect(chip.kind).toBe('observed');
    expect(chip.text).toBe('≤1 per subject (observed)');
    expect(chip.title).toContain('future write');
  });

  it('keeps a declared functional property as a separate chip, never merged', () => {
    expect(info(1, 5, [OWL_FUNCTIONAL]).map((c) => c.kind)).toEqual(['observed', 'declared']);
    // declared functional but observed with two values: only the declaration
    expect(info(2, 5, [OWL_FUNCTIONAL]).map((c) => c.kind)).toEqual(['declared']);
    // unused predicates have no observation
    expect(info(0, 0)).toEqual([]);
  });

  it('never calls the observation functional', () => {
    for (const c of info(1, 5))
      expect(c.text.toLowerCase()).not.toMatch(/functional|single|scalar/);
  });
});

describe('constraints layer', () => {
  const layer: ConstraintsLayer = {
    sources: [
      {
        kind: 'guard',
        graphs: [EX + 'shapes'],
        mode: 'reject',
        threshold: 'violation',
        shapes: 3,
        otherTargets: 0,
        classes: [
          {
            class: EX + 'Person',
            shapes: [EX + 'PersonShape'],
            closed: false,
            otherPaths: 0,
            properties: [
              {
                path: EX + 'name',
                severity: 'http://www.w3.org/ns/shacl#Violation',
                enforcement: 'reject-on-write',
                minCount: 1,
                maxCount: 1,
                datatype: XSD + 'string',
                other: ['http://www.w3.org/ns/shacl#PatternConstraintComponent'],
              },
            ],
          },
        ],
      },
      {
        kind: 'graphs',
        graphs: [EX + 'more'],
        shapes: 1,
        otherTargets: 0,
        classes: [
          {
            class: EX + 'Org',
            shapes: [],
            closed: false,
            otherPaths: 0,
            properties: [
              {
                path: EX + 'name',
                severity: 'http://www.w3.org/ns/shacl#Violation',
                enforcement: 'validated-on-request',
                class: [EX + 'Name'],
              },
            ],
          },
        ],
      },
    ],
  };
  const short = (iri: string) => iri.replace(EX, 'ex:').replace(XSD, 'xsd:');

  it('flattens the sources to one line per class and property shape', () => {
    const s = schemaFromSummary({ ...summary([], [prop(EX + 'name')]), constraints: layer });
    expect(s.constraints.map((l) => [l.class, l.source, l.constraint.path])).toEqual([
      [EX + 'Person', 'guard', EX + 'name'],
      [EX + 'Org', 'graphs', EX + 'name'],
    ]);
    expect(schemaFromSummary(summary([])).constraints).toEqual([]);
    expect(constraintLines(undefined)).toEqual([]);
  });

  it('summarizes a property shape', () => {
    const [l] = constraintLines(layer);
    expect(constraintSummary(l.constraint, short)).toBe(
      'min 1 · max 1 · datatype xsd:string · +Pattern',
    );
  });

  it('gives each constraining class its own chip, apart from the observations', () => {
    const chips = constraintChips(constraintLines(layer), EX + 'name', short);
    expect(chips.map((c) => [c.text, c.enforcement])).toEqual([
      ['SHACL min 1 · max 1 · datatype xsd:string · +Pattern', 'reject-on-write'],
      ['SHACL class ex:Name', 'validated-on-request'],
    ]);
    expect(chips[0].title).toContain('ex:Person');
    expect(chips[0].title).toContain('refuses a write');
    expect(chips[1].title).toContain('until the data is validated');
    expect(constraintChips(constraintLines(layer), EX + 'other', short)).toEqual([]);
    expect(enforcementText('warn-on-write').text).toBe('reported on write');
  });
});

describe('snapshotLine', () => {
  it('names the version and generation of the report', () => {
    const line = snapshotLine({
      version: 7,
      generation: 'gen-0003',
      computedAt: '2026-01-02T12:04:00Z',
    });
    expect(line).toMatch(/^as of version 7 · generation gen-0003 · computed \S/);
    expect(snapshotLine({ version: 1, generation: 'g', computedAt: 'soon' })).toContain(
      'computed soon',
    );
  });
});

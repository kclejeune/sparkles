import { describe, expect, it } from 'vitest';
import type { DiffQuad, Term } from './api';
import type { ClassInfo, PropertyInfo, Schema } from './explore';
import {
  candidateQuery,
  confirmPlan,
  deltaText,
  edgeIdOf,
  planDiff,
  schemaAnnouncement,
  schemaChanges,
} from './explore-time';

const EX = 'http://example.org/';
const alice = `<${EX}alice>`;
const bob = `<${EX}bob>`;
const carol = `<${EX}carol>`;
const knows = `<${EX}knows>`;
const name = `<${EX}name>`;

const quad = (op: '+' | '-', subject: string, predicate: string, object: string): DiffQuad => ({
  op,
  subject,
  predicate,
  object,
  graph: null,
});

const edge = (s: string, p: string, o: string) => `${s}|${p}|${o}`;

describe('planDiff', () => {
  // drawn: alice knows carol, alice name "Alice"
  const nodes = new Set([alice, carol, `lit:${alice}|${EX}name|"Alice"`]);
  const edges = [
    { id: edge(alice, `${EX}knows`, carol), source: alice, target: carol },
    {
      id: edge(alice, `${EX}name`, `lit:${alice}|${EX}name|"Alice"`),
      source: alice,
      target: `lit:${alice}|${EX}name|"Alice"`,
    },
  ];

  it('marks a drawn edge that was added and the node that only it reaches', () => {
    const plan = planDiff([quad('+', alice, knows, carol)], nodes, edges, alice);
    expect([...plan.addedEdges]).toEqual([edge(alice, `${EX}knows`, carol)]);
    expect([...plan.addedNodes]).toEqual([carol]);
  });

  it('never calls the focus added, and keeps a node that has an older edge', () => {
    const plan = planDiff(
      [quad('+', alice, knows, carol), quad('+', alice, name, '"Alice"')],
      nodes,
      edges,
      alice,
    );
    expect(plan.addedEdges.size).toBe(2);
    expect(plan.addedNodes.has(alice)).toBe(false);
    expect(plan.addedNodes.has(carol)).toBe(true);
  });

  it('draws a removed edge as a ghost with a ghost for the end that is not drawn', () => {
    const plan = planDiff([quad('-', alice, knows, bob)], nodes, edges, alice);
    expect(plan.ghostEdges).toEqual([
      {
        id: edge(alice, `${EX}knows`, bob),
        source: alice,
        target: bob,
        predicate: `${EX}knows`,
        lit: false,
        triple: [
          { type: 'uri', value: `${EX}alice` },
          { type: 'uri', value: `${EX}knows` },
          { type: 'uri', value: `${EX}bob` },
        ],
      },
    ]);
    expect(plan.ghostNodes.map((n) => n.id)).toEqual([bob]);
  });

  it('draws a removed edge that points at a drawn node', () => {
    const plan = planDiff([quad('-', bob, knows, carol)], nodes, edges, alice);
    expect(plan.ghostEdges.map((e) => [e.source, e.target])).toEqual([[bob, carol]]);
    expect(plan.ghostNodes.map((n) => n.id)).toEqual([bob]);
  });

  it('leaves out a change that touches nothing drawn', () => {
    const plan = planDiff(
      [quad('-', bob, knows, `<${EX}dave>`), quad('+', bob, knows, `<${EX}dave>`)],
      nodes,
      edges,
      alice,
    );
    expect(plan.ghostEdges).toEqual([]);
    expect(plan.addedEdges.size).toBe(0);
  });

  it('gives a removed literal the id the graph gives a literal node', () => {
    const plan = planDiff([quad('-', alice, name, '"Al"')], nodes, edges, alice);
    expect(plan.ghostEdges[0].lit).toBe(true);
    expect(plan.ghostEdges[0].target).toBe(`lit:${alice}|${EX}name|"Al"`);
  });

  it('caps the ghosts', () => {
    const many = Array.from({ length: 5 }, (_, i) => quad('-', alice, knows, `<${EX}p${i}>`));
    const plan = planDiff(many, nodes, edges, alice, 3);
    expect(plan.ghostEdges.length).toBe(3);
    expect(plan.truncated).toBe(true);
  });
});

describe('confirmPlan', () => {
  const nodes = new Set([alice, bob]);
  const edges = [{ id: edge(alice, `${EX}knows`, bob), source: alice, target: bob }];
  const inGraph = (op: '+' | '-', s: string, p: string, o: string): DiffQuad => ({
    ...quad(op, s, p, o),
    graph: `${EX}g`,
  });

  it('drops the mark of a drawn edge whose triple the earlier state already had', () => {
    // a copy in a named graph was added, and the drawn default-graph edge is not new
    const plan = planDiff([inGraph('+', alice, knows, bob)], nodes, edges, alice);
    expect([...plan.addedEdges]).toEqual([edge(alice, `${EX}knows`, bob)]);
    const sure = confirmPlan(
      plan,
      edges,
      alice,
      new Set([edge(alice, `${EX}knows`, bob)]),
      new Set(),
    );
    expect([...sure.addedEdges]).toEqual([]);
    expect([...sure.addedNodes]).toEqual([]);
    expect(sure.addedTriples.size).toBe(0);
  });

  it('keeps the mark of an edge the earlier state lacked, and the node only it reaches', () => {
    const plan = planDiff([quad('+', alice, knows, bob)], nodes, edges, alice);
    const sure = confirmPlan(plan, edges, alice, new Set(), new Set());
    expect([...sure.addedEdges]).toEqual([edge(alice, `${EX}knows`, bob)]);
    expect([...sure.addedNodes]).toEqual([bob]);
  });

  it('drops a ghost edge the later state still has, and the ghost node only it reached', () => {
    const plan = planDiff([inGraph('-', alice, knows, carol)], nodes, edges, alice);
    const gone = edge(alice, `${EX}knows`, carol);
    expect(plan.ghostEdges.map((g) => g.id)).toEqual([gone]);
    expect(plan.ghostNodes.map((n) => n.id)).toEqual([carol]);
    const sure = confirmPlan(plan, edges, alice, new Set(), new Set([gone]));
    expect(sure.ghostEdges).toEqual([]);
    expect(sure.ghostNodes).toEqual([]);
  });

  it('keeps a ghost the later state lacks', () => {
    const plan = planDiff([quad('-', alice, knows, carol)], nodes, edges, alice);
    const sure = confirmPlan(plan, edges, alice, new Set(), new Set());
    expect(sure.ghostEdges.length).toBe(1);
  });
});

describe('candidateQuery', () => {
  it('lists the triples as VALUES rows, a literal as its N-Triples term', () => {
    const t: [Term, Term, Term][] = [
      [
        { type: 'uri', value: `${EX}alice` },
        { type: 'uri', value: `${EX}name` },
        { type: 'literal', value: 'Al "A"', 'xml:lang': 'en' },
      ],
      [
        { type: 'uri', value: `${EX}alice` },
        { type: 'uri', value: `${EX}age` },
        { type: 'literal', value: '3', datatype: 'http://www.w3.org/2001/XMLSchema#integer' },
      ],
    ];
    const q = candidateQuery(t);
    expect(q).toBe(
      `SELECT ?s ?p ?o WHERE { VALUES (?s ?p ?o) { (<${EX}alice> <${EX}name> "Al \\"A\\""@en) (<${EX}alice> <${EX}age> "3"^^<http://www.w3.org/2001/XMLSchema#integer>) } ?s ?p ?o }`,
    );
    expect(edgeIdOf(t[0])).toBe(`${alice}|${EX}name|lit:${alice}|${EX}name|"Al \\"A\\""@en`);
  });

  it('leaves out blank nodes, and is empty when nothing is left', () => {
    const b: Term = { type: 'bnode', value: 'b0' };
    expect(candidateQuery([[b, { type: 'uri', value: `${EX}p` }, b]])).toBe('');
  });
});

// --- schema counts ---------------------------------------------------------------------------

const cls = (iri: string, instances: number): ClassInfo => ({
  iri,
  supers: [],
  subs: [],
  instances,
  declared: true,
  builtin: false,
  assertedSupers: [],
  equivalents: [],
  disjoint: [],
  cycle: [],
});

const prop = (iri: string, triples: number): PropertyInfo =>
  ({
    iri,
    kinds: [],
    domains: [],
    ranges: [],
    supers: [],
    inverseOf: [],
    builtin: false,
    observed: { triples },
  }) as unknown as PropertyInfo;

const schema = (classes: ClassInfo[], properties: PropertyInfo[]): Schema =>
  ({
    classes: new Map(classes.map((c) => [c.iri, c])),
    roots: [],
    properties,
  }) as unknown as Schema;

describe('schemaChanges', () => {
  const before = schema(
    [cls('A', 10), cls('B', 5), cls('Gone', 2)],
    [prop('p', 1), prop('q', 7), prop('old', 3)],
  );
  const after = schema(
    [cls('A', 130), cls('B', 5), cls('New', 4)],
    [prop('p', 121), prop('q', 7), prop('fresh', 9)],
  );

  it('reports the change of each count and the classes and properties that came or went', () => {
    const c = schemaChanges(before, after);
    expect(c.classes.get('A')).toEqual({ delta: 120, mark: 'kept' });
    expect(c.classes.get('New')).toEqual({ delta: 4, mark: 'added' });
    expect(c.classes.has('B')).toBe(false);
    expect(c.removedClasses.map((x) => x.iri)).toEqual(['Gone']);
    expect(c.properties.get('p')).toEqual({ delta: 120, mark: 'kept' });
    expect(c.properties.get('fresh')?.mark).toBe('added');
    expect(c.properties.has('q')).toBe(false);
    expect(c.removedProperties.map((x) => x.iri)).toEqual(['old']);
  });

  it('shows a fall as a negative count', () => {
    const c = schemaChanges(after, before);
    expect(c.classes.get('A')).toEqual({ delta: -120, mark: 'kept' });
    expect(deltaText(-4)).toBe('−4');
    expect(deltaText(120)).toBe('+120');
    expect(deltaText(1200)).toBe('+1,200');
    expect(deltaText(0)).toBe('0');
  });

  it('says what changed in words', () => {
    expect(schemaAnnouncement(schemaChanges(before, after))).toBe(
      '3 classes and 3 properties changed.',
    );
    expect(schemaAnnouncement(schemaChanges(before, before))).toBe('The schema did not change.');
  });
});

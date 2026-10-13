import { describe, expect, it } from 'vitest';
import type { DiffQuad } from './api';
import type { ClassInfo, PropertyInfo, Schema } from './explore';
import { deltaText, planDiff, schemaAnnouncement, schemaChanges } from './explore-time';

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

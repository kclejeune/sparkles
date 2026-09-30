import { describe, expect, it } from 'vitest';
import type { Term } from './api';
import { triplesToGraph, type Triple } from './graph';
import { RDF_TYPE, RDFS_LABEL, WELL_KNOWN } from './rdf';

const u = (l: string): Term => ({ type: 'uri', value: `http://ex.org/${l}` });
const lit = (v: string): Term => ({ type: 'literal', value: v });
const type: Term = { type: 'uri', value: RDF_TYPE };
const label: Term = { type: 'uri', value: RDFS_LABEL };

describe('triplesToGraph', () => {
  it('uses labels as node labels instead of separate literal nodes', () => {
    const g = triplesToGraph(
      [
        [u('a'), label, lit('Alice')],
        [u('a'), u('knows'), u('b')],
      ],
      WELL_KNOWN,
    );
    expect(g.nodes.map((n) => n.label).sort()).toEqual(['Alice', 'b']);
    expect(g.edges).toHaveLength(1);
  });

  it('gives each (subject, predicate, literal) its own node', () => {
    const g = triplesToGraph(
      [
        [u('a'), u('age'), lit('3')],
        [u('b'), u('age'), lit('3')],
      ],
      WELL_KNOWN,
    );
    expect(g.nodes.filter((n) => n.kind === 'literal')).toHaveLength(2);
  });

  it('hides rdf:type edges and literals on request', () => {
    const triples: Triple[] = [
      [u('a'), type, u('C')],
      [u('a'), u('name'), lit('x')],
    ];
    const g = triplesToGraph(triples, WELL_KNOWN, { hideTypes: true, hideLiterals: true });
    expect(g.edges).toHaveLength(0);
    expect(g.nodes.map((n) => n.id)).toEqual(['<http://ex.org/a>']);
    expect(triplesToGraph(triples, WELL_KNOWN).edges.find((e) => e.isType)).toBeTruthy();
  });

  it('reports truncation and the full node count', () => {
    const triples: Triple[] = Array.from({ length: 10 }, (_, i) => [u('hub'), u('p'), u(`n${i}`)]);
    const g = triplesToGraph(triples, WELL_KNOWN, { maxNodes: 4 });
    expect(g.truncated).toBe(true);
    expect(g.nodes).toHaveLength(4);
    expect(g.totalNodes).toBe(11);
  });

  it('keys triple-term nodes by their RDF 1.2 syntax', () => {
    const tt: Term = {
      type: 'triple',
      value: { subject: u('a'), predicate: u('p'), object: u('b') },
    };
    const g = triplesToGraph([[tt, u('source'), u('doc')]], WELL_KNOWN);
    const node = g.nodes.find((n) => n.kind === 'triple')!;
    expect(node.id).toBe('<<( <http://ex.org/a> <http://ex.org/p> <http://ex.org/b> )>>');
    expect(node.label).toBe('<<triple>>');
  });
});

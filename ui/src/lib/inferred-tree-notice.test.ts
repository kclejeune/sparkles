import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import InferredTreeView from './components/InferredTree.svelte';

const fixture = vi.hoisted(() => ({
  outcome: {
    tier: 'el' as 'el' | 'horn' | 'disjunctive',
    complete: true,
    partial: false,
    consistency: 'consistent' as 'consistent' | 'inconsistent' | 'unknown',
    excludedCount: 0,
    incompleteCount: 0,
    unmappedCount: 0,
  },
  roots: [] as string[],
  names: {} as Record<string, { label?: string; equivalenceCount?: number }>,
  incomplete: {} as Record<string, boolean>,
}));
vi.mock('./inferred-tree.svelte', () => ({
  InferredTree: class {
    reply = { commit: 7, result: { resultId: 'kept-source' }, outcome: fixture.outcome };
    tree = {
      roots: () => fixture.roots,
      children: () => [],
      more: () => false,
      clippedOf: () => null,
      counts: {} as Record<string, number>,
      loading: false,
    };
    individuals = { roots: () => [], loading: false };
    source = null;
    error = null;
    ancestryTruncated = false;
    names = fixture.names;
    incomplete = fixture.incomplete;
    canonical(iri: string) {
      return iri;
    }
  },
}));
const html = () =>
  render(InferredTreeView, {
    props: {
      target: 'ds',
      graph: null,
      commit: 7,
      kind: 'Class',
      onopen: () => {},
    },
  }).body;

beforeEach(() => {
  fixture.outcome = {
    tier: 'el',
    complete: true,
    partial: false,
    consistency: 'consistent',
    excludedCount: 0,
    incompleteCount: 0,
    unmappedCount: 0,
  };
  fixture.roots = [];
  fixture.names = {};
  fixture.incomplete = {};
});
describe('the actual inferred-tree outcome region', () => {
  it('labels completed source exclusions without claiming a stopped partial run', () => {
    fixture.outcome = {
      tier: 'el',
      complete: false,
      partial: false,
      consistency: 'consistent',
      excludedCount: 3,
      incompleteCount: 0,
      unmappedCount: 0,
    };
    const body = html();
    expect(body).toContain('omitted or partly used axioms');
    expect(body).toContain('3');
    expect(body).not.toContain('Partial classification.');
  });
  it('names the unmapped triples of an incomplete run with no excluded axiom', () => {
    fixture.outcome = {
      tier: 'el',
      complete: false,
      partial: false,
      consistency: 'unknown',
      excludedCount: 0,
      incompleteCount: 4,
      unmappedCount: 2,
    };
    const body = html();
    expect(body).toContain('triples of the input map to no axiom');
    expect(body).not.toContain('omitted or partly used axioms');
  });
  it('names both the excluded axioms and the unmapped triples when there are both', () => {
    fixture.outcome = {
      tier: 'el',
      complete: false,
      partial: false,
      consistency: 'unknown',
      excludedCount: 1,
      incompleteCount: 4,
      unmappedCount: 1,
    };
    const body = html();
    expect(body).toContain('Classification is incomplete.');
    expect(body).toContain('omitted or partly used axioms');
    expect(body).toContain('triple of the input map to no axiom');
  });
  it('retains the complete supported-source notice', () => {
    const body = html();
    expect(body).toContain('Complete for its supported source.');
    expect(body).toContain('Consistent');
  });
  it('distinguishes a stopped partial outcome, unknown consistency and open class markers', () => {
    fixture.outcome = {
      tier: 'disjunctive',
      complete: false,
      partial: true,
      consistency: 'unknown',
      excludedCount: 0,
      incompleteCount: 2,
      unmappedCount: 0,
    };
    fixture.roots = ['urn:OpenClass', 'urn:ClosedClass'];
    fixture.names = {
      'urn:OpenClass': { label: 'OpenClass', equivalenceCount: 1 },
      'urn:ClosedClass': { label: 'ClosedClass', equivalenceCount: 1 },
    };
    fixture.incomplete = { 'urn:OpenClass': true, 'urn:ClosedClass': false };
    const body = html();
    expect(body).toContain('DISJUNCTIVE');
    expect(body).toContain('Partial · 2 open classes');
    expect(body).toContain('Partial classification.');
    expect(body).toContain('Consistency unknown');
    expect(body).toContain('Consistency is unknown.');
    expect(body).toContain('>open<');
    expect(body).toContain('aria-label="OpenClass, open"');
    expect(body).toContain('aria-label="ClosedClass"');
    expect(body).toContain('warn');
  });
  it('retains excluded-axiom and unmapped-triple notices alongside a partial outcome', () => {
    fixture.outcome = {
      tier: 'disjunctive',
      complete: false,
      partial: true,
      consistency: 'unknown',
      excludedCount: 2,
      incompleteCount: 3,
      unmappedCount: 1,
    };
    const body = html().replace(/\s+/g, ' ');
    expect(body).toContain('Partial classification.');
    expect(body).toContain('2 omitted or partly used axioms.');
    expect(body).toContain('1 triple of the input map to no axiom.');
  });
});

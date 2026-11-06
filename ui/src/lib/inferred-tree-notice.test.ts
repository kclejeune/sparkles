import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import InferredTreeView from './components/InferredTree.svelte';

const fixture = vi.hoisted(() => ({
  outcome: {
    complete: true,
    partial: false,
    consistency: 'consistent',
    excludedCount: 0,
    incompleteCount: 0,
  },
}));
vi.mock('./inferred-tree.svelte', () => ({
  InferredTree: class {
    reply = { commit: 7, result: { resultId: 'kept-source' }, outcome: fixture.outcome };
    tree = { roots: () => [], loading: false };
    individuals = { roots: () => [], loading: false };
    source = null;
    error = null;
    ancestryTruncated = false;
    names = {};
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
    complete: true,
    partial: false,
    consistency: 'consistent',
    excludedCount: 0,
    incompleteCount: 0,
  };
});
describe('the actual inferred-tree outcome region', () => {
  it('labels completed source exclusions without claiming a stopped partial run', () => {
    fixture.outcome = {
      complete: false,
      partial: false,
      consistency: 'consistent',
      excludedCount: 3,
      incompleteCount: 0,
    };
    const body = html();
    expect(body).toContain('omitted or partly used axioms');
    expect(body).toContain('3');
    expect(body).not.toContain('Partial classification.');
  });
  it('retains the complete supported-source notice', () => {
    expect(html()).toContain('Complete for its supported source.');
  });
  it('distinguishes a stopped partial outcome', () => {
    fixture.outcome = {
      complete: false,
      partial: true,
      consistency: 'unknown',
      excludedCount: 0,
      incompleteCount: 2,
    };
    expect(html()).toContain('Partial classification.');
  });
});

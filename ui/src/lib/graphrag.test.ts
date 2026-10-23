import { describe, expect, it } from 'vitest';
import type { Task } from './api';
import {
  citeText,
  outcomeOf,
  outcomeText,
  tree,
  waitingEstimate,
  type CommunityRow,
} from './graphrag';

const row = (iri: string, parent: string | null, rating: number | null): CommunityRow => ({
  iri,
  level: parent ? 1 : 0,
  parent,
  memberCount: 3,
  report: rating == null ? null : { iri: `${iri}/r`, title: iri, status: 'unreviewed', rating },
});

const task = (state: Task['state'], detail: unknown): Task => ({
  id: '1',
  kind: 'graphrag',
  dataset: 'org',
  state,
  startedAt: '2026-10-23T00:00:00Z',
  detail,
});

describe('graphrag', () => {
  it('builds a forest under parents, highest rating first', () => {
    const f = tree([
      row('c', 'a', 2),
      row('a', null, 1),
      row('b', null, 9),
      row('d', 'a', 7),
      row('e', 'gone', 3),
    ]);
    expect(f.map((n) => n.iri)).toEqual(['b', 'e', 'a']);
    expect(f[2].children.map((n) => n.iri)).toEqual(['d', 'c']);
  });

  it('finds the estimate of a waiting run only', () => {
    const est = {
      communities: 2,
      requests: 2,
      model: 'p/m',
      tokens: 10,
      tokensPerBuild: 1,
      tokensPerDay: 1,
    };
    expect(
      waitingEstimate(task('running', { phase: 'awaiting-confirmation', estimate: est })),
    ).toEqual(est);
    expect(waitingEstimate(task('running', { phase: 'estimate', estimate: est }))).toBeNull();
    expect(
      waitingEstimate(task('done', { phase: 'awaiting-confirmation', estimate: est })),
    ).toBeNull();
  });

  it('describes an outcome', () => {
    const o = outcomeOf(
      task('done', {
        mode: 'build',
        communities: 3,
        written: 2,
        kept: 0,
        failed: 0,
        pending: 1,
        tokens: 1200,
        stopped: 'budget',
      }),
    );
    expect(o && outcomeText(o)).toBe(
      '2 reports written, 0 kept, 1 pending, 1,200 tokens. Stopped: budget',
    );
    expect(outcomeOf(task('failed', {}))).toBeNull();
  });

  it('shortens a cited triple term', () => {
    expect(citeText('<<( <http://ex/a> <http://ex/p> "x" )>>')).toBe(
      '<http://ex/a> <http://ex/p> "x"',
    );
    expect(citeText('urn:r#f1')).toBe('urn:r#f1');
  });
});

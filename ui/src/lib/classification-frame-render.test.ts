import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import ClassificationFrameView from './components/ClassificationFrame.svelte';
import type {
  ClassificationOutcome,
  ClassificationResult,
  KeptClassification,
} from './classification-read';

const baseResult: ClassificationResult = {
  resultId: 'res-1',
  classifiedCommit: 7,
  boundStoreId: 'branch-a',
  familyId: 'family-a',
  inputFingerprint: 'in-fp',
  sourceFingerprint: 'src-fp',
  contentFingerprint: 'cnt-fp',
  imports: 'dataset',
  mode: 'lenient',
  assertions: false,
  elOnly: false,
  partialRequested: true,
  expansion: 'eager',
  sourceCounts: { requestedGraphs: 1, resolvedGraphs: 1, imports: 0, warnings: 0 },
  capabilities: ['namedClassDirectParents'],
  justifications: 'unavailable',
};

const emptyPage = () => ({
  items: [] as unknown[],
  total: 0,
  next: null as number | null,
  loaded: true,
  loading: false,
  error: null as string | null,
  failedOffset: null as number | null,
});

const fixture = vi.hoisted(() => ({
  reply: null as KeptClassification | null,
  parts: {
    parents: {
      items: [] as unknown[],
      total: 0,
      next: null as number | null,
      loaded: true,
      loading: false,
      error: null as string | null,
      failedOffset: null as number | null,
    },
    equivalents: {
      items: [] as unknown[],
      total: 0,
      next: null as number | null,
      loaded: true,
      loading: false,
      error: null as string | null,
      failedOffset: null as number | null,
    },
    sources: {
      items: [] as unknown[],
      total: 0,
      next: null as number | null,
      loaded: true,
      loading: false,
      error: null as string | null,
      failedOffset: null as number | null,
    },
    excluded: {
      items: [] as unknown[],
      total: 0,
      next: null as number | null,
      loaded: true,
      loading: false,
      error: null as string | null,
      failedOffset: null as number | null,
    },
  },
  isolated: false,
}));

vi.mock('./classification-frame.svelte', () => ({
  ClassificationFrame: class {
    reply = fixture.reply;
    parts = fixture.parts;
    loading = false;
    computing = false;
    error = null;
    isolated = fixture.isolated;
    reset() {}
    drop() {}
    refresh() {}
    classify() {}
    more() {}
  },
}));

const renderFrame = (classifyOptions?: {
  partial?: boolean;
  timeoutMs?: number;
  expansion?: 'cautious' | 'eager';
}) =>
  render(ClassificationFrameView, {
    props: {
      target: 'uni',
      graph: null,
      iri: 'urn:OpenClass',
      commit: 7,
      items: [],
      classifyOptions,
    },
  }).body;

function availableReply(
  outcome: ClassificationOutcome,
  entity: NonNullable<KeptClassification['entity']>,
): KeptClassification {
  return {
    commit: 7,
    status: 'available',
    reason: null,
    result: baseResult,
    outcome,
    entity,
    activity: null,
    page: null,
    staleCommit: null,
  };
}

beforeEach(() => {
  fixture.reply = null;
  fixture.parts = {
    parents: emptyPage(),
    equivalents: emptyPage(),
    sources: emptyPage(),
    excluded: emptyPage(),
  };
  fixture.isolated = false;
});

describe('ClassificationFrame partial classification rendering', () => {
  it('renders deadline, partial and expansion controls when no kept classification is available', () => {
    fixture.reply = {
      commit: 7,
      status: 'unavailable',
      reason: 'notKept',
      result: null,
      outcome: null,
      entity: null,
      activity: null,
      page: null,
      staleCommit: null,
    };
    const body = renderFrame({ partial: true, timeoutMs: 2500, expansion: 'eager' });
    expect(body).toContain('Allow partial');
    expect(body).toContain('Deadline (s)');
    expect(body).toContain('Expansion');
    expect(body).toContain('Cautious');
    expect(body).toContain('Eager');
    expect(body).toContain('Classify');
  });

  it('shows partial hierarchy, tier, open-class marker and unknown consistency distinctly', () => {
    fixture.reply = availableReply(
      {
        tier: 'disjunctive',
        partial: true,
        complete: false,
        consistency: 'unknown',
        excludedCount: 0,
        incompleteCount: 3,
        unmappedCount: 0,
      },
      { iri: 'urn:OpenClass', present: true, unsatisfiable: false, incomplete: true },
    );
    fixture.parts.parents = {
      ...emptyPage(),
      items: [{ iri: 'urn:OpenParent', label: 'Open Parent', labelLang: 'en', incomplete: true }],
      total: 1,
    };
    fixture.isolated = true;
    const body = renderFrame();
    expect(body).toContain('DISJUNCTIVE');
    expect(body).toContain('Partial · 3 open classes');
    expect(body).toContain('Consistency unknown');
    expect(body).toContain(
      'Consistency is unknown because the classification stopped before deciding it.',
    );
    expect(body).toContain('Open class');
    expect(body).toContain(
      'This class is left open in this partial classification and may miss inferred clauses.',
    );
    expect(body).toContain('Open Parent');
    expect(body).toContain('>Open<');
    expect(body).toContain('Classify with deadline or partial options');
  });

  it('distinguishes consistent and inconsistent outcomes from unknown', () => {
    fixture.reply = availableReply(
      {
        tier: 'horn',
        partial: false,
        complete: true,
        consistency: 'consistent',
        excludedCount: 0,
        incompleteCount: 0,
        unmappedCount: 0,
      },
      { iri: 'urn:OpenClass', present: true, unsatisfiable: false, incomplete: false },
    );
    const consistentBody = renderFrame();
    expect(consistentBody).toContain('HORN');
    expect(consistentBody).toContain('Consistent');
    expect(consistentBody).not.toContain('Consistency unknown');
    expect(consistentBody).not.toContain('Open class');

    fixture.reply = availableReply(
      {
        tier: 'disjunctive',
        partial: false,
        complete: true,
        consistency: 'inconsistent',
        excludedCount: 0,
        incompleteCount: 0,
        unmappedCount: 0,
      },
      { iri: 'urn:OpenClass', present: true, unsatisfiable: true, incomplete: false },
    );
    const inconsistentBody = renderFrame();
    expect(inconsistentBody).toContain('Inconsistent');
    expect(inconsistentBody).toContain('The ontology is inconsistent.');
    expect(inconsistentBody).not.toContain('Consistency unknown');
  });
});

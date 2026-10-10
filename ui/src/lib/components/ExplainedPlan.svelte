<script lang="ts">
  // The plan tree with the explanation panel beside it (C18 §6.6.6). The **Explanation**
  // switch asks `/{ds}/sparql/explain` about the plan shown, so the notes and node ids
  // agree with the tree. A failed query's plan opens with the panel (**Explain why**).
  import type { CursorPlan, PlanNode } from '$lib/api';
  import { errorMessage } from '$lib/api';
  import {
    explainStream,
    marks as marksOf,
    type ExplainNote,
    type Explanation,
    type NodeFacts,
  } from '$lib/explain';
  import type { PrefixMap } from '$lib/rdf';
  import ExplainPanel from './ExplainPanel.svelte';
  import PlanView from './PlanView.svelte';

  let {
    ds,
    query = undefined,
    plan,
    commit = undefined,
    error = undefined,
    executed = true,
    prefixes = {},
    open = false,
  }: {
    /** The dataset, with `@branch` when not on `main`. */
    ds: string;
    /** The query text; without it there is no explanation. */
    query?: string;
    plan: PlanNode | CursorPlan;
    commit?: number;
    /** The error body of a run that a budget stopped. */
    error?: Record<string, unknown>;
    executed?: boolean;
    prefixes?: PrefixMap;
    /** Whether the panel starts open. */
    open?: boolean;
  } = $props();

  let shown = $state(false);
  $effect.pre(() => {
    shown = open;
  });
  let loading = $state(false);
  let failure = $state<string | null>(null);
  let explanation = $state<Explanation | null>(null);
  let notes = $state<ExplainNote[]>([]);
  let nodes = $state<NodeFacts[]>([]);
  let shownNotes = $state(12);
  let selected = $state<string | null>(null);
  let hovered = $state<string | null>(null);
  let lit = $state<Set<string> | null>(null);
  let asked: PlanNode | CursorPlan | null = null;

  const marks = $derived(marksOf(explanation?.notes ?? notes));
  const stopped = $derived(notes.some((n) => n.code === 'budget'));

  async function fetchExplanation() {
    if (!query || asked === plan) return;
    asked = plan;
    loading = true;
    failure = null;
    explanation = null;
    notes = [];
    try {
      await explainStream(
        ds,
        { query, profile: 'given', plan, commit, ...(error ? { error } : {}) },
        (e) => {
          if (e.event === 'notes') {
            notes = e.data.notes;
            nodes = e.data.nodes;
            shownNotes = e.data.shownNotes;
          } else if (e.event === 'explanation') {
            explanation = e.data;
          } else if (e.event === 'error') {
            failure = e.data.error;
          }
        },
      );
    } catch (e) {
      failure = errorMessage(e);
      asked = null;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (shown && query) void fetchExplanation();
  });
</script>

<div class="explained" class:open={shown && !!query}>
  <div class="tree-side">
    <PlanView
      {plan}
      {executed}
      {prefixes}
      {selected}
      marks={shown ? marks : undefined}
      {lit}
      onhover={(id) => (hovered = id)}
      onselect={(id) => (selected = id)}
    >
      {#snippet toolbar()}
        {#if query}
          <label class="switch">
            <input type="checkbox" bind:checked={shown} />
            Explanation
          </label>
        {/if}
      {/snippet}
    </PlanView>
  </div>
  {#if shown && query}
    <ExplainPanel
      {explanation}
      {notes}
      {nodes}
      {shownNotes}
      {executed}
      {stopped}
      {loading}
      error={failure}
      {hovered}
      onselect={(id) => {
        selected = null;
        queueMicrotask(() => (selected = id));
      }}
      onlight={(ids) => (lit = ids ? new Set(ids) : null)}
    />
  {/if}
</div>

<style>
  .explained {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    height: 100%;
    min-height: 0;
  }
  .explained.open {
    grid-template-columns: minmax(0, 1fr) minmax(260px, 34%);
  }
  .explained.open > :global(.panel) {
    border-left: 1px solid var(--border);
  }
  .tree-side {
    min-width: 0;
    min-height: 0;
  }
  .switch {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text-2);
    cursor: pointer;
    white-space: nowrap;
  }
  @media (max-width: 720px) {
    .explained.open {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: minmax(0, 1fr) auto;
    }
    .explained.open > :global(.panel) {
      border-left: none;
      border-top: 1px solid var(--border);
    }
  }
</style>

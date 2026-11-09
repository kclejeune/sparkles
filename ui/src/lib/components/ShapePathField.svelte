<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import type { Term } from '$lib/api';
  import { displayTerm, type PrefixMap } from '$lib/rdf';
  import { ShapeGraph, SH, type ShapePath } from '$lib/shape-editor';
  import ShapePathEditor from './ShapePath.svelte';
  let {
    graph,
    subject,
    prefixes,
    readonly,
    onchange,
    onpending,
  }: {
    graph: ShapeGraph;
    subject: Term;
    prefixes: PrefixMap;
    readonly: boolean;
    onchange: (g: ShapeGraph) => void;
    onpending: (pending: boolean) => void;
  } = $props();
  let malformed = $state<string | null>(null);
  const initial = untrack(() => {
    try {
      return graph.path(graph.one(subject, SH + 'path'));
    } catch (e) {
      malformed = (e as Error).message;
      return { kind: 'predicate', iri: '' } as ShapePath;
    }
  });
  let path = $state<ShapePath>(structuredClone(initial));
  let changed = $state(false);
  let replacing = $state(false);
  let error = $state<string | null>(null);
  $effect(() => {
    const p = changed;
    untrack(() => onpending(p));
  });
  onDestroy(() => onpending(false));
  function apply() {
    try {
      const next = graph.setPath(subject, path);
      error = null;
      onpending(false);
      onchange(next);
    } catch (e) {
      error = (e as Error).message;
    }
  }
</script>

<div class="path-field">
  {#if malformed && !replacing && graph.values(subject, SH + 'path').length}
    <p class="warn-text">{malformed} The stored path remains unchanged.</p>
    <code
      >{graph
        .values(subject, SH + 'path')
        .map((t) => displayTerm(t, prefixes))
        .join(', ')}</code
    >
    <button
      class="btn"
      disabled={readonly}
      onclick={() => {
        replacing = true;
        changed = true;
      }}>Replace stored path</button
    >
  {:else}
    <ShapePathEditor
      value={path}
      {prefixes}
      {readonly}
      onchange={(p) => {
        path = p;
        changed = true;
      }}
    />
    <div class="actions">
      <button class="btn" disabled={readonly || !changed} onclick={apply}>Apply path</button>
      {#if changed}<button
          class="btn ghost"
          onclick={() => {
            path = structuredClone(initial);
            changed = false;
            replacing = false;
            error = null;
          }}>Discard path edits</button
        >{/if}
    </div>
  {/if}
  {#if error}<p class="error-box" role="alert">{error}</p>{/if}
</div>

<style>
  .path-field {
    display: grid;
    gap: 8px;
    min-width: 0;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  p {
    margin: 0;
    font-size: var(--fs-sm);
  }
  code {
    overflow-wrap: anywhere;
  }
</style>

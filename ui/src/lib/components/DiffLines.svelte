<script lang="ts">
  // Changes of a commit, a merge or a preview: rendered as a change set by default, one
  // block a subject named by its label (ROADMAP item 633), with the signed N-Quads lines
  // one switch away. The History panel, the commit graph, the merge page, the editors'
  // previews and the forms use it.
  import * as api from '$lib/api';
  import { app } from '$lib/app.svelte';
  import { changeNamesQuery, parseChangeNames } from '$lib/browse';
  import { changeIris } from '$lib/changeset';
  import { fmtInt } from '$lib/format';
  import { diffLine } from '$lib/history';
  import ChangeSet from './ChangeSet.svelte';

  let {
    quads,
    total = quads.length,
    label = 'Changes',
    maxHeight = 280,
    ds = null,
  }: {
    quads: api.DiffQuad[];
    /** Every change, of which `quads` lists the first. */
    total?: number;
    /** The accessible name of the list. */
    label?: string;
    maxHeight?: number;
    /** The dataset (or `ds@branch`) whose labels name the changes; the open one when absent. */
    ds?: string | null;
  } = $props();

  let raw = $state(false);
  const target = $derived(ds ?? app.current);
  const prefixes = $derived(target ? app.prefixes(target.split('@')[0]) : {});

  let names = $state<Map<string, string>>(new Map());
  const langs = typeof navigator === 'undefined' ? ['en'] : [...(navigator.languages ?? []), 'en'];
  $effect(() => {
    const t = target;
    const iris = changeIris(quads)
      .filter((i) => /^(https?|urn):/.test(i))
      .slice(0, 300);
    if (!t || !iris.length || raw) return;
    let live = true;
    api
      .select(t, changeNamesQuery(iris, null))
      .then((rows) => {
        if (live) names = parseChangeNames(rows as never, langs);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  });
</script>

{#if quads.length}
  <div class="diff-view">
    <div class="segmented sm" role="group" aria-label="Show changes as">
      <button aria-pressed={!raw} onclick={() => (raw = false)}>Changes</button>
      <button aria-pressed={raw} onclick={() => (raw = true)}>N-Quads</button>
    </div>
    {#if raw}
      <pre
        class="diff-lines"
        aria-label={label}
        style:max-height="{maxHeight}px">{#each quads as q, i (i)}<span
            class:ins={q.op === '+'}
            class:del={q.op === '-'}>{diffLine(q)}{'\n'}</span
          >{/each}</pre>
    {:else}
      <div class="scroll" style:max-height="{maxHeight}px">
        <ChangeSet
          {quads}
          total={quads.length}
          name={(i) => names.get(i) ?? null}
          {prefixes}
          {label}
        />
      </div>
    {/if}
  </div>
{/if}
{#if total > quads.length}
  <p class="faint more">
    Showing the first {fmtInt(quads.length)} of {fmtInt(total)} changes.
  </p>
{/if}

<style>
  .diff-view {
    display: grid;
    gap: var(--sp-2);
    min-width: 0;
  }
  .diff-view > .segmented {
    justify-self: start;
  }
  .scroll {
    overflow: auto;
  }
  .diff-lines {
    margin: 0;
    overflow: auto;
    font-size: 11.5px;
    line-height: 1.45;
    white-space: pre;
  }
  .ins {
    color: var(--ok);
  }
  .del {
    color: var(--danger);
  }
  .more {
    margin: 0;
  }
</style>

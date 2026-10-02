<script lang="ts">
  // The schema browser's "Compare" dialog: what changed in the schema between an earlier
  // state (a commit, time or named snapshot) and the head or another state.
  import * as api from '$lib/api';
  import { fmtInt } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import { displayIri, type PrefixMap } from '$lib/rdf';
  import { changeText, diffSummary, parseAt } from '$lib/schema-history';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  let {
    open = $bindable(false),
    ds,
    graph = 'default',
    reasoning = undefined,
    prefixes = {},
  }: {
    open?: boolean;
    ds: string;
    graph?: string;
    reasoning?: boolean;
    prefixes?: PrefixMap;
  } = $props();

  let fromText = $state('');
  let toText = $state('');
  let diff = $state<api.SchemaDiff | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  let ctl: AbortController | null = null;

  const from = $derived(parseAt(fromText));
  const to = $derived(toText.trim() ? parseAt(toText) : 'head');
  const short = (iri: string) => shortLabel(iri, prefixes);

  // a new dataset or graph starts over
  $effect(() => {
    void ds;
    void graph;
    diff = null;
    error = null;
  });

  async function run() {
    if (!from || !to) return;
    ctl?.abort();
    ctl = new AbortController();
    loading = true;
    error = null;
    try {
      diff = await api.schemaDiff(ds, {
        from,
        to: to === 'head' ? undefined : to,
        graph,
        reasoning,
        signal: ctl.signal,
      });
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') return;
      error = api.errorMessage(e);
      diff = null;
    } finally {
      loading = false;
    }
  }
</script>

<Modal
  bind:open
  title="Compare the schema with an earlier state"
  width={760}
  onclose={() => ctl?.abort()}
>
  <p class="muted small">
    Classes and predicates added, removed and changed between two states of /{ds}, counted over the {graph ===
    'default'
      ? 'default graph'
      : graph === 'union'
        ? 'union of all graphs'
        : displayIri(graph, prefixes)}. A state is a commit number,
    <span class="mono">time:</span>&lt;RFC 3339&gt; or
    <span class="mono">snapshot:</span>&lt;name&gt;, and must still be readable.
  </p>
  <div class="opts">
    <label class="row small">
      <span class="faint">From</span>
      <input
        class="input sm"
        class:invalid={!!fromText.trim() && !from}
        aria-invalid={!!fromText.trim() && !from}
        placeholder="commit, time: or snapshot:"
        bind:value={fromText}
        onkeydown={(e) => e.key === 'Enter' && run()}
      />
    </label>
    <label class="row small">
      <span class="faint">To</span>
      <input
        class="input sm"
        class:invalid={!to}
        aria-invalid={!to}
        placeholder="head"
        bind:value={toText}
        onkeydown={(e) => e.key === 'Enter' && run()}
      />
    </label>
    <span class="spacer"></span>
    <button class="btn sm primary" onclick={run} disabled={loading || !from || !to}>
      {#if loading}<span class="spinner"></span>{:else}<Icon name="clock" size={13} />{/if}
      Compare
    </button>
  </div>

  {#if error}
    <p class="error small">{error}</p>
  {/if}

  {#if diff}
    <p class="small summary">
      Commit {diff.from.commit ?? '?'} → commit {diff.to.commit ?? '?'}: {diffSummary(diff)}
    </p>
    {#if diff.report.length}
      <ul class="changes small">
        {#each diff.report as c, i (i)}<li class="mono">{changeText(c, short)}</li>{/each}
      </ul>
    {/if}
    {#each [['Classes', diff.classes], ['Predicates', diff.predicates]] as const as [title, list] (title)}
      {#if list.added.length || list.removed.length || list.changed.length}
        <h3 class="sub">{title}</h3>
        <ul class="entries">
          {#each list.added as e (e.iri)}
            <li>
              <span class="badge add">added</span>
              <span class="mono">{displayIri(e.iri, prefixes)}</span>
            </li>
          {/each}
          {#each list.removed as e (e.iri)}
            <li>
              <span class="badge del">removed</span>
              <span class="mono">{displayIri(e.iri, prefixes)}</span>
            </li>
          {/each}
          {#each list.changed as e (e.iri)}
            <li>
              <span class="badge chg">changed</span>
              <span class="mono">{displayIri(e.iri, prefixes)}</span>
              <ul class="changes small">
                {#each e.changes as c, i (i)}<li class="mono">{changeText(c, short)}</li>{/each}
              </ul>
            </li>
          {/each}
        </ul>
      {/if}
    {/each}
    <p class="faint small">
      {fmtInt(diff.classes.changed.length + diff.predicates.changed.length)} changed entries. Counts are
      observations of each state, not constraints.
    </p>
  {/if}
</Modal>

<style>
  .opts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    margin: 8px 0;
  }
  .invalid {
    border-color: var(--danger);
  }
  .summary {
    margin: 8px 0 4px;
  }
  .entries,
  .changes {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .entries > li {
    padding: 4px 0;
    border-bottom: 1px solid var(--border);
    overflow-wrap: anywhere;
  }
  .changes {
    margin: 2px 0 0 12px;
  }
  .changes li {
    overflow-wrap: anywhere;
  }
  .badge {
    font-size: 11px;
    padding: 0 6px;
    border-radius: 8px;
    border: 1px solid var(--border);
  }
  .badge.add {
    color: var(--success, inherit);
  }
  .badge.del {
    color: var(--danger);
  }
  .sub {
    margin: 12px 0 4px;
    font-size: 13px;
  }
</style>

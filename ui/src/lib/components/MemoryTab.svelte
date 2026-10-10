<script lang="ts">
  // The Memory tab of a resource on the Explore page (C18 §8.7): what the caller's view
  // holds about it, with a citation per fact, and the history of superseded and retracted
  // facts. It reads `POST /{ds}/recall` with the resource as the only seed.
  import * as api from '$lib/api';
  import * as askApi from '$lib/ask-api';
  import { parseCompact } from '$lib/compact';
  import { fateLine, filterChoices, historyOf, principalName } from '$lib/memory';
  import type { PrefixMap } from '$lib/rdf';
  import { LatestRun } from '$lib/supersede';
  import Icon from './Icon.svelte';
  import RecallView from './RecallView.svelte';
  import TermView from './TermView.svelte';

  let {
    ds,
    iri,
    prefixes,
    onopen,
  }: {
    ds: string;
    iri: string;
    prefixes: PrefixMap;
    onopen?: (iri: string) => void;
  } = $props();

  let result = $state<askApi.RecallResult | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  let graphs = $state<string[]>([]);
  let agents = $state<string[]>([]);
  let showSuperseded = $state(false);
  /** A commit to read memory at (empty: the head). */
  let asOf = $state('');
  let commitList = $state<api.Commit[]>([]);
  const latest = new LatestRun();

  $effect(() => {
    const name = ds;
    commitList = [];
    api
      .commits(name, { limit: 50 })
      .then((p) => {
        if (ds === name) commitList = p.commits;
      })
      .catch(() => {});
  });

  async function load(name: string, seed: string, at: string) {
    const owns = latest.claim('memory');
    loading = true;
    error = null;
    try {
      const r = await askApi.recall(name, {
        seeds: [seed],
        hops: 0,
        includeSuperseded: true,
        maxTriples: 1000,
        ...(at ? { atCommit: Number(at) } : {}),
      });
      if (!owns()) return;
      result = r;
    } catch (e) {
      if (!owns()) return;
      result = null;
      error =
        e instanceof api.ApiError && e.status === 404
          ? 'This server does not answer recall requests.'
          : api.errorMessage(e);
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    void load(ds, iri, asOf);
  });

  const choices = $derived(result ? filterChoices(result) : { graphs: [], agents: [] });
  const filter = $derived({ graphs: new Set(graphs), agents: new Set(agents) });
  const all = $derived({ ...prefixes, ...(result?.prefixes ?? {}) });
  const entity = $derived(
    result?.entities.find((e) => parseCompact(e.iri, all).value === iri) ?? result?.entities[0],
  );
  const history = $derived(
    result && entity ? historyOf(result.superseded, entity.iri, filter) : [],
  );
  const citationOf = (reifier: string | undefined) =>
    reifier ? result?.citations.find((c) => c.reifier === reifier)?.id : undefined;
  const day = (t: string | undefined) => (t ? t.slice(0, 10) : '?');
</script>

<div class="memory" aria-label="Memory">
  <div class="filters">
    <label class="row small">
      <span class="faint">Graphs</span>
      <select
        class="select sm"
        multiple
        size={Math.min(Math.max(choices.graphs.length, 1), 4)}
        aria-label="Graphs"
        bind:value={graphs}
      >
        {#each choices.graphs as g (g)}<option value={g}>{g}</option>{/each}
      </select>
    </label>
    <label class="row small">
      <span class="faint">Agents</span>
      <select
        class="select sm"
        multiple
        size={Math.min(Math.max(choices.agents.length, 1), 4)}
        aria-label="Agents"
        bind:value={agents}
      >
        {#each choices.agents as a (a)}<option value={a}>{principalName(a)}</option>{/each}
      </select>
    </label>
    <label class="row small check">
      <input type="checkbox" bind:checked={showSuperseded} /> show superseded
    </label>
    <label class="row small">
      <span class="faint">as of</span>
      <select class="select sm" aria-label="As of" bind:value={asOf}>
        <option value="">now</option>
        {#each commitList as c (c.seq)}
          <option value={String(c.seq)}
            >commit {c.seq} · {c.timestamp.slice(0, 16).replace('T', ' ')}</option
          >
        {/each}
      </select>
    </label>
    {#if graphs.length || agents.length}
      <button
        class="btn ghost sm"
        onclick={() => {
          graphs = [];
          agents = [];
        }}><Icon name="x" size={11} /> Clear filters</button
      >
    {/if}
  </div>

  {#if loading && !result}
    <p class="faint row"><span class="spinner"></span> Recalling…</p>
  {:else if error}
    <div class="error-box">{error}</div>
  {:else if result && entity}
    {#if result.commit != null}
      <p class="faint small">
        Read at commit {result.commit}{result.truncated ? ', cut at 1,000 facts' : ''}.
      </p>
    {/if}
    <RecallView
      {result}
      {prefixes}
      {filter}
      entities={[entity]}
      superseded={showSuperseded ? history : []}
      {onopen}
      showHeader={false}
    />
    <h4 class="sub">History <span class="faint">{history.length}</span></h4>
    {#if history.length === 0}
      <p class="faint small">Nothing about this resource was superseded or retracted.</p>
    {:else}
      <ul class="history" aria-label="History">
        {#each history as h (h.reifier)}
          {@const by = citationOf(h.replacedBy)}
          <li>
            <span class="when mono small">{day(h.at)} → {day(h.invalidatedAt)}</span>
            <span class="mono"><TermView term={parseCompact(h.p, all)} prefixes={all} /></span>
            <span class="mono"
              ><TermView term={parseCompact(h.o, all)} prefixes={all} {onopen} /></span
            >
            <span class="faint small">{fateLine(h, by)} · {h.graph}</span>
          </li>
        {/each}
      </ul>
    {/if}
  {:else if result}
    <p class="faint">The memory you can read holds nothing about this resource.</p>
  {/if}
</div>

<style>
  .memory {
    display: grid;
    gap: 8px;
    min-width: 0;
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 12px;
    align-items: flex-start;
  }
  .filters select[multiple] {
    height: auto;
    max-width: 220px;
  }
  .check {
    gap: 5px;
    cursor: pointer;
  }
  .sub {
    margin: 6px 0 0;
    font-size: var(--fs-sm);
  }
  .history {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .history li {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
    overflow-wrap: anywhere;
  }
  .when {
    color: var(--text-2);
  }
</style>

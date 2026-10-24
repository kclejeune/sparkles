<script lang="ts">
  // The Memory tab of a resource on the Explore page (C18 §8.7): what the caller's view
  // holds about it, with a citation per fact, and the history of superseded and retracted
  // facts. It reads `POST /{ds}/recall` with the resource as the only seed.
  import { onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import * as askApi from '$lib/ask-api';
  import { parseCompact } from '$lib/compact';
  import { factChanges, fateLine, filterChoices, historyOf, principalName } from '$lib/memory';
  import { timeFailure } from '$lib/timeline';
  import type { PrefixMap } from '$lib/rdf';
  import { isAbort, LatestRun } from '$lib/supersede';
  import Icon from './Icon.svelte';
  import RecallView from './RecallView.svelte';
  import TermView from './TermView.svelte';
  import TimeSlider from './TimeSlider.svelte';
  import { toasts } from '$lib/app.svelte';

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
  /** The time slider's state: the state to read (empty: the head) and, comparing, the earlier one. */
  let timeOpen = $state(false);
  let asOf = $state('');
  let from = $state('');
  let compare = $state(false);
  let slider: TimeSlider | undefined = $state();
  /** The facts at the left handle, while comparing. */
  let before = $state<askApi.RecallResult | null>(null);
  const latest = new LatestRun();
  onDestroy(() => latest.abortAll());

  async function load(name: string, seed: string, at: string, earlier: string | null) {
    const owns = latest.claim('memory');
    // a newer read supersedes this one, which stops on the server too
    const signal = latest.signal('memory');
    loading = true;
    error = null;
    const read = (sel: string) =>
      askApi.recall(
        name,
        {
          seeds: [seed],
          hops: 0,
          includeSuperseded: true,
          maxTriples: 1000,
          ...(sel && sel !== 'head' ? { at: sel } : {}),
        },
        signal,
      );
    try {
      const [r, b] = await Promise.all([read(at), earlier ? read(earlier) : Promise.resolve(null)]);
      if (!owns()) return;
      result = r;
      before = b;
      if (timeOpen) slider?.accept();
    } catch (e) {
      if (!owns() || isAbort(e)) return;
      const gone = timeOpen ? timeFailure(e) : null;
      if (gone) {
        toasts.push('error', gone.title, gone.detail);
        slider?.reject({ seq: gone.seq, readable: gone.readable });
        if (result) return;
      }
      slider?.fail();
      result = null;
      before = null;
      error =
        e instanceof api.ApiError && e.status === 404
          ? 'Memory search is not available on this server.'
          : api.errorMessage(e);
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    // comparing reads the facts at both handles
    const earlier = timeOpen && compare && from ? from : null;
    void load(ds, iri, timeOpen ? asOf : '', earlier);
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
  /** In compare mode: the facts that differ between the two reads. */
  const changes = $derived.by(() => {
    if (!before || !timeOpen || !compare) return null;
    const pick = (r: askApi.RecallResult) =>
      r.entities.find((e) => parseCompact(e.iri, { ...prefixes, ...r.prefixes }).value === iri) ??
      r.entities[0];
    return factChanges(pick(before)?.facts, entity?.facts);
  });
  const SIGN = { added: '+', superseded: '−', retracted: '−' } as const;
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
      <input type="checkbox" bind:checked={showSuperseded} /> Show superseded
    </label>
    <button
      class="btn sm"
      aria-pressed={timeOpen}
      title="Show memory as of an earlier commit"
      onclick={() => {
        timeOpen = !timeOpen;
        if (!timeOpen) {
          compare = false;
          from = '';
          asOf = '';
        }
      }}
    >
      <Icon name="clock" size={13} /> As of{asOf ? ` ${asOf.replace('commit:', 'commit ')}` : ''}
    </button>
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

  {#if timeOpen}
    <TimeSlider
      bind:this={slider}
      {ds}
      bind:at={asOf}
      bind:from
      bind:compare
      busy={loading}
      label="Memory"
      canFollow={false}
      announcement={changes
        ? `${changes.length} fact${changes.length === 1 ? '' : 's'} changed`
        : result?.commit != null
          ? `Commit ${result.commit}`
          : ''}
      onclose={() => {
        timeOpen = false;
        compare = false;
        from = '';
        asOf = '';
      }}
    />
  {/if}

  {#if loading && !result}
    <p class="faint row"><span class="spinner"></span> Loading memory…</p>
  {:else if error}
    <div class="error-box">{error}</div>
  {:else if result && entity}
    {#if result.commit != null}
      <p class="faint small">
        Read at commit {result.commit}{result.truncated ? ', cut at 1,000 facts' : ''}.
      </p>
    {/if}
    {#if changes}
      <section class="changes" aria-label="Changed facts">
        <h4 class="sub">
          Changed since commit {before?.commit ?? ''} <span class="faint">{changes.length}</span>
        </h4>
        {#if changes.length === 0}
          <p class="faint small">No changes between these commits.</p>
        {:else}
          <ul class="history">
            {#each changes as c (c.kind + c.p + c.o)}
              <li class="change {c.kind}">
                <span class="sign" aria-label={c.kind}>{SIGN[c.kind]}</span>
                <span class="mono"><TermView term={parseCompact(c.p, all)} prefixes={all} /></span>
                <span class="mono"
                  ><TermView term={parseCompact(c.o, all)} prefixes={all} {onopen} /></span
                >
                <span class="faint small">{c.kind}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </section>
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
      <p class="faint small">No superseded or retracted facts.</p>
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
    <p class="faint">No memory about this resource yet.</p>
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
  .change {
    padding-left: 6px;
    border-left: 3px solid var(--border);
  }
  .change.added {
    border-left-color: var(--added);
  }
  .change.superseded,
  .change.retracted {
    border-left-color: var(--removed);
  }
  .change.superseded .mono,
  .change.retracted .mono {
    text-decoration: line-through;
    color: var(--text-2);
  }
  .sign {
    font-weight: 700;
    min-width: 1ch;
  }
  .change.added .sign {
    color: var(--added);
  }
  .change.superseded .sign,
  .change.retracted .sign {
    color: var(--removed);
  }
</style>

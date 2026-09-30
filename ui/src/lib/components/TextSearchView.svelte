<script lang="ts">
  import { resolve } from '$app/paths';
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import * as ex from '$lib/explore';
  import { fmtInt } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import { displayIri, type PrefixMap } from '$lib/rdf';
  import { LatestRun } from '$lib/supersede';
  import { textErrorHint, textSearch, type TextHit } from '$lib/textsearch';
  import Icon from './Icon.svelte';
  import TermView from './TermView.svelte';

  let {
    ds,
    prefixes,
    query = $bindable(''),
    onopen,
  }: {
    ds: string;
    prefixes: PrefixMap;
    /** The search text (kept in the page URL). */
    query?: string;
    /** Open a subject in the graph explorer. */
    onopen: (iri: string, label?: string) => void;
  } = $props();

  const LIMITS = [20, 50, 100, 200];

  let status = $state<api.TextStatus | null | 'unsupported' | undefined>(undefined);
  let statusError = $state<string | null>(null);
  let predicate = $state('');
  let lang = $state('');
  let limit = $state(50);
  /** Also search the named graphs (GRAPH ?g). */
  let namedGraphs = $state(false);
  let hits = $state<TextHit[] | null>(null);
  let labels = $state<Map<string, string>>(new Map());
  let error = $state<{ title: string; hint?: string } | null>(null);
  let searching = $state(false);
  let ms = $state(0);
  const runs = new LatestRun();
  let ctl: AbortController | null = null;
  let debounce: ReturnType<typeof setTimeout> | undefined;

  async function loadStatus(name: string) {
    try {
      status = await api.textStatus(name);
      statusError = null;
    } catch (e) {
      if (e instanceof api.ApiError && e.status === 501) status = 'unsupported';
      else statusError = api.errorMessage(e);
    }
  }

  $effect(() => {
    const name = ds;
    status = undefined;
    predicate = '';
    hits = null;
    void loadStatus(name);
  });

  const indexed = $derived(
    status && typeof status === 'object' && Array.isArray(status.config.predicates)
      ? status.config.predicates
      : null,
  );

  async function run() {
    clearTimeout(debounce);
    const text = query.trim();
    const owns = runs.claim('text');
    ctl?.abort();
    if (!text) {
      hits = null;
      error = null;
      searching = false;
      return;
    }
    const c = (ctl = new AbortController());
    searching = true;
    const t0 = performance.now();
    try {
      const found = await textSearch(ds, text, {
        predicates: predicate ? [predicate] : undefined,
        lang: lang.trim() || undefined,
        limit,
        namedGraphs,
        signal: c.signal,
      });
      if (!owns()) return;
      ms = performance.now() - t0;
      hits = found;
      error = null;
      const iris = [
        ...new Set(found.flatMap((h) => (h.subject.type === 'uri' ? [h.subject.value] : []))),
      ];
      const l = await ex.labels(ds, iris).catch(() => new Map<string, string>());
      if (owns()) labels = l;
    } catch (e) {
      if (!owns() || (e instanceof DOMException && e.name === 'AbortError')) return;
      hits = null;
      error = textErrorHint(e);
    } finally {
      if (owns()) searching = false;
    }
  }

  function schedule() {
    clearTimeout(debounce);
    debounce = setTimeout(run, 350);
  }

  onMount(() => {
    if (query.trim()) void run();
    return () => {
      clearTimeout(debounce);
      ctl?.abort();
    };
  });

  const datasetHref = $derived(resolve('/datasets/[name]', { name: ds }));
</script>

<div class="wrap">
  <form
    class="bar"
    onsubmit={(e) => {
      e.preventDefault();
      void run();
    }}
  >
    <div class="box">
      <Icon name="search" size={15} />
      <input
        class="q"
        placeholder={'Full-text search, e.g. brown fox, "exact phrase", +must -not'}
        bind:value={query}
        oninput={schedule}
        aria-label="Full-text query"
        spellcheck="false"
      />
      {#if searching}<span class="spinner"></span>{/if}
    </div>
    {#if indexed}
      <select class="select sm" bind:value={predicate} onchange={run} aria-label="Predicate">
        <option value="">all indexed predicates</option>
        {#each indexed as p (p)}<option value={p}>{displayIri(p, prefixes)}</option>{/each}
      </select>
    {/if}
    <input
      class="input sm lang"
      placeholder="lang"
      title="Only literals with this language tag (e.g. en)"
      bind:value={lang}
      onchange={run}
      aria-label="Language tag"
    />
    <label class="check" title="Search the named graphs too (the default graph only otherwise)">
      <input type="checkbox" bind:checked={namedGraphs} onchange={run} /> named graphs
    </label>
    <select class="select sm" bind:value={limit} onchange={run} aria-label="Results">
      {#each LIMITS as n (n)}<option value={n}>top {n}</option>{/each}
    </select>
    <button class="btn primary sm" type="submit" disabled={!query.trim()}>Search</button>
  </form>
  <p class="syntax faint">
    Terms match any word (OR); <span class="mono">"phrase"</span> (with
    <span class="mono">~2</span> slop), <span class="mono">AND</span>/<span class="mono">OR</span>,
    <span class="mono">+required</span>, <span class="mono">-excluded</span>, parentheses and
    <span class="mono">"quick bro"*</span> work as usual. Colons are searched literally (they are
    sent as <span class="mono">\:</span>), so IRIs and times can be pasted as they are.
  </p>

  <div class="body scroll">
    {#if status === 'unsupported'}
      <div class="empty">This server was built without full-text search.</div>
    {:else if statusError}
      <div class="pad"><div class="error-box">{statusError}</div></div>
    {:else if status === null}
      <div class="empty">
        <Icon name="search" size={22} />
        <p>Full-text search is off for <span class="mono">{ds}</span>.</p>
        <a class="btn" href={datasetHref}>Enable it on the dataset page</a>
      </div>
    {:else}
      {#if status && status.state !== 'ready'}
        <div class="notice">
          <Icon name="alert" size={14} /> The index is {status.state}{status.message
            ? ` (${status.message})`
            : ''}. Searches answer 503 until it is rebuilt.
        </div>
      {/if}
      {#if error}
        <div class="pad">
          <div class="error-box">
            <strong>{error.title}</strong>
            {#if error.hint}<div class="muted">{error.hint}</div>{/if}
          </div>
        </div>
      {:else if hits}
        <div class="meta faint">
          {fmtInt(hits.length)} hit{hits.length === 1 ? '' : 's'}{hits.length >= limit
            ? ` (top ${limit})`
            : ''} in {Math.round(ms)} ms, best first
        </div>
        {#if hits.length === 0}
          <div class="empty">Nothing matches “{query.trim()}”.</div>
        {:else}
          <ol class="hits">
            {#each hits as h, i (i)}
              <li>
                <span class="rank faint">{i + 1}</span>
                <div class="main">
                  <div class="subject">
                    {#if h.subject.type === 'uri'}
                      {@const iri = h.subject.value}
                      <button
                        class="open"
                        title="Open {iri} in the graph"
                        onclick={() => onopen(iri, labels.get(iri))}
                        >{labels.get(iri) ?? shortLabel(iri, prefixes)}</button
                      >
                      <span class="iri mono faint">{displayIri(iri, prefixes)}</span>
                    {:else}
                      <TermView term={h.subject} {prefixes} />
                    {/if}
                  </div>
                  <div class="lit">
                    {#if h.predicate}<span class="pred mono" title={h.predicate}
                        >{displayIri(h.predicate, prefixes)}</span
                      >{/if}
                    <TermView term={h.literal} {prefixes} />
                  </div>
                  {#if h.graph}
                    <div class="graph faint small" title={h.graph}>
                      in graph <span class="mono">{displayIri(h.graph, prefixes)}</span>
                    </div>
                  {/if}
                </div>
                <span class="score mono" title="BM25 score">{h.score.toFixed(3)}</span>
              </li>
            {/each}
          </ol>
        {/if}
      {:else if status}
        <div class="empty faint">
          <Icon name="search" size={22} />
          <p>
            Search {fmtInt(status.docs)} indexed literal{status.docs === 1 ? '' : 's'}{indexed
              ? ` of ${indexed.length} predicate${indexed.length === 1 ? '' : 's'}`
              : ''}.
          </p>
        </div>
      {/if}
    {/if}
  </div>
</div>

<style>
  .wrap {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--surface);
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    padding: 10px 14px 4px;
  }
  .box {
    flex: 1;
    min-width: 240px;
    max-width: 640px;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 32px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--bg);
    color: var(--text-3);
  }
  .box:focus-within {
    border-color: var(--iri);
    box-shadow: 0 0 0 3px rgba(36, 89, 199, 0.15);
  }
  .q {
    flex: 1;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--text);
    font: inherit;
    min-width: 0;
  }
  .select.sm,
  .input.sm {
    height: 30px;
    font-size: var(--fs-sm);
  }
  .select.sm {
    max-width: 220px;
  }
  .lang {
    width: 64px;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: var(--fs-sm);
    color: var(--text-2);
    cursor: pointer;
  }
  .check input {
    margin: 0;
    accent-color: var(--iri);
  }
  .syntax {
    margin: 0;
    padding: 2px 14px 10px;
    font-size: var(--fs-xs);
    border-bottom: 1px solid var(--border);
  }
  .body {
    flex: 1;
    min-height: 0;
  }
  .pad {
    padding: 12px 14px;
  }
  .notice {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 14px;
    font-size: var(--fs-sm);
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 8%, var(--surface));
    border-bottom: 1px solid var(--border);
  }
  .meta {
    padding: 8px 14px 4px;
    font-size: var(--fs-sm);
  }
  .hits {
    list-style: none;
    margin: 0;
    padding: 0 6px 16px;
  }
  .hits li {
    display: grid;
    grid-template-columns: 28px minmax(0, 1fr) auto;
    gap: 10px;
    align-items: baseline;
    padding: 8px;
    border-bottom: 1px solid var(--border);
  }
  .rank {
    text-align: right;
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
  }
  .main {
    display: grid;
    gap: 3px;
    min-width: 0;
  }
  .subject {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
  }
  .open {
    all: unset;
    cursor: pointer;
    color: var(--iri);
    font-weight: 500;
    white-space: nowrap;
  }
  .open:hover {
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .open:focus-visible {
    box-shadow: var(--focus);
  }
  .iri {
    font-size: 11px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .lit {
    font-size: var(--fs-sm);
    word-break: break-word;
  }
  .pred {
    font-size: 11px;
    color: var(--text-3);
    margin-right: 8px;
  }
  .small {
    font-size: 11px;
  }
  .score {
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  a.btn {
    text-decoration: none;
  }
</style>

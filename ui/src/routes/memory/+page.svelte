<script lang="ts">
  // Memory: an overview of what agents wrote into a dataset (C18 §8.7). The agents and
  // sources come from SPARQL 1.2 reifier queries over the caller's view (C17 §3.2), the
  // review branches from the branch list, and the settings from `/$/memory/{ds}`. Search
  // memory runs `recall` as an agent would. The server applies the caller's grants, so the
  // page shows whatever the queries return. The Inbox tab is the review inbox of §8.9.
  import { goto } from '$app/navigation';
  import { base, resolve } from '$app/paths';
  import { page } from '$app/state';
  import * as api from '$lib/api';
  import type { Term } from '$lib/api';
  import { app } from '$lib/app.svelte';
  import * as askApi from '$lib/ask-api';
  import { fmtInt } from '$lib/format';
  import {
    ACTIVITY_QUERY,
    AGENTS_QUERY,
    graphPrefixes,
    isReviewBranch,
    principalName,
    SOURCES_QUERY,
  } from '$lib/memory';
  import { reviewHref } from '$lib/review';
  import * as reviewApi from '$lib/review-api';
  import { LatestRun } from '$lib/supersede';
  import Icon from '$components/Icon.svelte';
  import InboxPanel from '$components/InboxPanel.svelte';
  import RecallView from '$components/RecallView.svelte';
  import TermView from '$components/TermView.svelte';

  const ds = $derived(app.current);
  const tab = $derived(page.url.searchParams.get('tab') === 'inbox' ? 'inbox' : 'overview');
  let inboxCount = $state<number | null>(null);

  function showTab(t: 'overview' | 'inbox') {
    const q = new URLSearchParams(page.url.searchParams);
    if (t === 'inbox') q.set('tab', 'inbox');
    else q.delete('tab');
    const qs = q.toString();
    goto(`${resolve('/memory')}${qs ? `?${qs}` : ''}`, {
      replaceState: true,
      keepFocus: true,
      noScroll: true,
    });
  }
  const prefixes = $derived(app.prefixes(ds));

  {
    const urlDs = page.url.searchParams.get('ds');
    if (urlDs && urlDs !== app.current) app.setDataset(urlDs);
  }

  type Row = Record<string, Term | undefined>;
  type Section<T> = { rows: T[]; error: string | null; loading: boolean };
  const empty = <T>(): Section<T> => ({ rows: [], error: null, loading: true });

  let agents = $state<Section<Row>>(empty());
  let sources = $state<Section<Row>>(empty());
  let activity = $state<Section<Row>>(empty());
  let review = $state<Section<api.Branch>>(empty());
  let settings = $state<askApi.MemorySettings | null>(null);
  let settingsError = $state<string | null>(null);
  const runs = new LatestRun();

  async function section(
    key: string,
    load: () => Promise<Row[]>,
    set: (s: Section<Row>) => void,
    name: string,
  ) {
    const owns = runs.claim(key);
    set({ rows: [], error: null, loading: true });
    try {
      const rows = await load();
      if (owns() && ds === name) set({ rows, error: null, loading: false });
    } catch (e) {
      if (owns() && ds === name) set({ rows: [], error: api.errorMessage(e), loading: false });
    }
  }

  $effect(() => {
    const name = ds;
    if (!name) return;
    void section(
      'agents',
      () => api.select(name, AGENTS_QUERY),
      (s) => (agents = s),
      name,
    );
    void section(
      'sources',
      () => api.select(name, SOURCES_QUERY),
      (s) => (sources = s),
      name,
    );
    void section(
      'activity',
      () => api.select(name, ACTIVITY_QUERY),
      (s) => (activity = s),
      name,
    );
    const owns = runs.claim('review');
    review = empty();
    api.branches(name).then(
      (l) => {
        if (owns())
          review = {
            rows: l.branches.filter((b) => isReviewBranch(b.name)),
            error: null,
            loading: false,
          };
      },
      (e) => {
        if (owns())
          review = {
            rows: [],
            error: e instanceof api.ApiError && e.status === 404 ? null : api.errorMessage(e),
            loading: false,
          };
      },
    );
    inboxCount = null;
    reviewApi.inbox(name).then(
      (r) => {
        if (ds === name) inboxCount = r.open;
      },
      () => {},
    );
    settings = null;
    settingsError = null;
    askApi.memorySettings(name).then(
      (s) => {
        if (ds === name) settings = s;
      },
      (e) => {
        if (ds === name)
          settingsError =
            e instanceof api.ApiError && e.status === 404
              ? 'This server has no memory settings.'
              : api.errorMessage(e);
      },
    );
  });

  /** Activities once each: the query gives a row per label. */
  const activities = $derived.by(() => {
    const seen = new Set<string>();
    return activity.rows.filter((r) => {
      const k = v(r.act);
      if (seen.has(k)) return false;
      seen.add(k);
      return true;
    });
  });

  // --- search ---------------------------------------------------------------------------
  let text = $state(page.url.searchParams.get('q') ?? '');
  let searching = $state(false);
  let found = $state<askApi.RecallResult | null>(null);
  let searchError = $state<string | null>(null);

  async function search() {
    const name = ds;
    const q = text.trim();
    if (!name || !q) return;
    const owns = runs.claim('search');
    searching = true;
    searchError = null;
    try {
      const r = await askApi.recall(name, { query: q });
      if (owns()) found = r;
    } catch (e) {
      if (owns()) {
        found = null;
        searchError = api.errorMessage(e);
      }
    } finally {
      if (owns()) searching = false;
    }
  }

  function openIri(iri: string) {
    goto(
      `${resolve('/explore')}?ds=${encodeURIComponent(ds ?? '')}&iri=${encodeURIComponent(iri)}&view=memory`,
    );
  }

  /** The text of a term (empty for none or a triple term). */
  const v = (t: Term | undefined) => (t && t.type !== 'triple' ? t.value : '');
  const n = (t: Term | undefined) => Number(v(t)) || 0;
  const when = (t: Term | undefined) => v(t).slice(0, 16).replace('T', ' ');
</script>

<svelte:head><title>Memory | Sparkles</title></svelte:head>

<div class="page">
  <header class="head">
    <h1><Icon name="brain" size={18} /> Memory <span class="faint">{ds ?? ''}</span></h1>
    <p class="muted small">
      What agents wrote into this dataset, as far as your grants let you read it.
    </p>
  </header>

  {#if !ds}
    <div class="empty">
      <p>No dataset selected.</p>
      <a class="btn" href={resolve('/datasets')}>Go to Datasets</a>
    </div>
  {:else}
    <div class="tabs" role="tablist" aria-label="Memory views">
      <button
        class="tab"
        role="tab"
        aria-selected={tab === 'overview'}
        onclick={() => showTab('overview')}>Overview</button
      >
      <button
        class="tab"
        role="tab"
        aria-selected={tab === 'inbox'}
        onclick={() => showTab('inbox')}
        >Inbox{#if inboxCount != null}
          <span class="count">{inboxCount}</span>{/if}</button
      >
    </div>
    {#if tab === 'inbox'}
      {#key ds}
        <InboxPanel {ds} oncount={(n) => (inboxCount = n)} />
      {/key}
    {:else}
      <section class="card" aria-label="Search memory">
        <h2>Search memory</h2>
        <form
          class="row search"
          onsubmit={(e) => {
            e.preventDefault();
            void search();
          }}
        >
          <input
            class="input"
            placeholder="payments team"
            aria-label="Search memory"
            bind:value={text}
          />
          <button class="btn primary" type="submit" disabled={!text.trim() || searching}>
            {#if searching}<span class="spinner"></span>{:else}<Icon name="search" size={13} />{/if}
            Recall
          </button>
        </form>
        {#if searchError}
          <div class="error-box">{searchError}</div>
        {:else if found}
          {#if found.entities.length === 0}
            <p class="faint">Nothing you can read matches.</p>
          {:else}
            <RecallView result={found} {prefixes} onopen={openIri} />
          {/if}
        {/if}
      </section>

      <div class="grid">
        <section class="card" aria-label="Agents">
          <h2>Agents</h2>
          {#if agents.loading}
            <p class="faint"><span class="spinner"></span></p>
          {:else if agents.error}
            <div class="error-box">{agents.error}</div>
          {:else if agents.rows.length === 0}
            <p class="faint">No agent wrote facts you can read.</p>
          {:else}
            <table class="data">
              <thead>
                <tr><th>Agent</th><th>Graphs</th><th class="num">Facts</th><th>Last write</th></tr>
              </thead>
              <tbody>
                {#each agents.rows as r (v(r.agent))}
                  <tr>
                    <td title={v(r.agent)}>{principalName(v(r.agent))}</td>
                    <td class="mono small">
                      {#each graphPrefixes(v(r.graphs).split(' ').filter(Boolean)) as g (g)}
                        <div>{g}</div>
                      {/each}
                    </td>
                    <td class="num">{fmtInt(n(r.facts))}</td>
                    <td class="small">{when(r.last)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </section>

        <section class="card" aria-label="Sources">
          <h2>Sources</h2>
          {#if sources.loading}
            <p class="faint"><span class="spinner"></span></p>
          {:else if sources.error}
            <div class="error-box">{sources.error}</div>
          {:else if sources.rows.length === 0}
            <p class="faint">No fact you can read names a source.</p>
          {:else}
            <table class="data">
              <thead><tr><th>Source</th><th class="num">Facts</th></tr></thead>
              <tbody>
                {#each sources.rows as r (v(r.source))}
                  <tr>
                    <td class="mono small"
                      ><TermView term={r.source} {prefixes} onopen={openIri} /></td
                    >
                    <td class="num">{fmtInt(n(r.facts))}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </section>

        <section class="card" aria-label="Recent activity">
          <h2>Recent activity</h2>
          {#if activity.loading}
            <p class="faint"><span class="spinner"></span></p>
          {:else if activity.error}
            <div class="error-box">{activity.error}</div>
          {:else if activities.length === 0}
            <p class="faint">No activity you can read.</p>
          {:else}
            <ul class="plain">
              {#each activities as r (v(r.act))}
                <li>
                  <span class="small mono faint">{when(r.time)}</span>
                  <span>{principalName(v(r.agent))}</span>
                  <span>{r.label ? `"${v(r.label)}"` : ''}</span>
                  <span class="faint small mono">{v(r.g)}</span>
                </li>
              {/each}
            </ul>
          {/if}
        </section>

        <section class="card" aria-label="Review branches">
          <h2>Review branches</h2>
          {#if review.loading}
            <p class="faint"><span class="spinner"></span></p>
          {:else if review.error}
            <div class="error-box">{review.error}</div>
          {:else if review.rows.length === 0}
            <p class="faint">No open proposal, ingest, review or scratch branch.</p>
          {:else}
            <ul class="plain">
              {#each review.rows as b (b.name)}
                <li>
                  <a class="mono" href={reviewHref(base, ds, b.name)}>{b.name}</a>
                  <span class="faint small"
                    >{b.ahead} commit{b.ahead === 1 ? '' : 's'} ahead · {b.modified.slice(
                      0,
                      10,
                    )}</span
                  >
                  {#if b.note}<span class="small">{b.note}</span>{/if}
                </li>
              {/each}
            </ul>
          {/if}
        </section>

        <section class="card" aria-label="Memory settings">
          <h2>Settings</h2>
          {#if settingsError}
            <p class="faint">{settingsError}</p>
          {:else if !settings}
            <p class="faint"><span class="spinner"></span></p>
          {:else}
            <dl class="kv">
              <dt>Agent graphs</dt>
              <dd>
                {#each settings.agentGraphs as g (g)}<div class="mono small">{g}</div>{:else}<span
                    class="faint">none, so no fact counts as unreviewed</span
                  >{/each}
              </dd>
              <dt>Consolidated graph</dt>
              <dd class="mono small">{settings.consolidatedGraph ?? '—'}</dd>
              <dt>Agents</dt>
              <dd>
                {#each Object.entries(settings.agents) as [a, cfg] (a)}<div>
                    {a} <span class="faint small">conversation facts {cfg.conversationFacts}</span>
                  </div>{:else}<span class="faint">no per-agent settings</span>{/each}
              </dd>
            </dl>
          {/if}
        </section>
      </div>
    {/if}
  {/if}
</div>

<style>
  .page {
    padding: 16px;
    display: grid;
    gap: 14px;
    align-content: start;
    min-width: 0;
    overflow: auto;
  }
  .head h1 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    font-size: var(--fs-lg);
  }
  .head p {
    margin: 4px 0 0;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 420px), 1fr));
    gap: 14px;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
    padding: 12px 14px;
    display: grid;
    gap: 8px;
    align-content: start;
    min-width: 0;
    overflow-x: auto;
  }
  .card h2 {
    margin: 0;
    font-size: var(--fs-md);
  }
  .search {
    display: flex;
    gap: 8px;
  }
  .search .input {
    flex: 1;
    min-width: 0;
  }
  .data {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--fs-sm);
  }
  .data th,
  .data td {
    text-align: left;
    padding: 4px 8px 4px 0;
    vertical-align: top;
    overflow-wrap: anywhere;
  }
  .data th {
    color: var(--text-2);
    font-weight: 500;
  }
  .num {
    text-align: right !important;
    font-variant-numeric: tabular-nums;
  }
  .plain {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .plain li {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
    overflow-wrap: anywhere;
  }
  .kv {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 4px 12px;
    margin: 0;
    font-size: var(--fs-sm);
  }
  .kv dt {
    color: var(--text-2);
  }
  .kv dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
</style>

<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import {
    commitFlags,
    commitKindLabel,
    fmtCommitTime,
    fmtDelta,
    historyNotes,
    mergeCommits,
    nextBefore,
  } from '$lib/commits';
  import { fmtInt, fmtRelative } from '$lib/format';
  import {
    diffSummary,
    historyBy,
    historyLine,
    historyTerm,
    normalizeAt,
    readable,
    unrecordedNote,
    validAt,
  } from '$lib/history';
  import { MAIN, onBranch } from '$lib/branches';
  import { load, save } from '$lib/storage';
  import { LatestRun } from '$lib/supersede';
  import CommitGraph from './CommitGraph.svelte';
  import DiffLines from './DiffLines.svelte';
  import Icon from './Icon.svelte';

  let {
    name,
    branch = null,
    info,
    refreshKey = 0,
  }: {
    name: string;
    /** The branch to list (null: `main`). */
    branch?: string | null;
    info: api.DatasetInfo | undefined;
    /** Bump to reload the newest commits (after a write). */
    refreshKey?: number;
  } = $props();

  const target = $derived(onBranch(name, branch));
  /** The graph of every branch, in place of the list (persistent datasets). */
  const GRAPH_KEY = 'sparkles.history.graph';
  let graphOn = $state(load(GRAPH_KEY, false));
  const canGraph = $derived(info?.type !== 'mem');
  const graph = $derived(graphOn && canGraph);
  function toggleGraph() {
    graphOn = !graphOn;
    save(GRAPH_KEY, graphOn);
  }
  const branchName = $derived(branch ?? MAIN);

  /** A link to the dataset page on another branch. */
  const branchHref = (b: string) =>
    b === MAIN
      ? resolve('/datasets/[name]', { name })
      : `${resolve('/datasets/[name]', { name })}?branch=${encodeURIComponent(b)}`;

  const PAGE = 20;

  let page = $state<api.CommitPage | null>(null);
  let shown = $state<api.Commit[]>([]);
  let error = $state<api.ApiError | Error | null>(null);
  let loading = $state(false);
  let loadingOlder = $state(false);
  let now = $state(Date.now());
  // a reload (refresh, other dataset) supersedes pages still loading
  const runs = new LatestRun();

  /** Bumped by the Reload button, for the graph. */
  let graphKick = $state(0);

  async function reload() {
    const owns = runs.claim('history');
    loading = true;
    try {
      const p = await api.commits(target, { limit: PAGE });
      if (!owns()) return;
      page = p;
      shown = p.commits;
      error = null;
    } catch (e) {
      if (owns()) error = e as Error;
    } finally {
      if (owns()) loading = false;
    }
  }

  async function loadOlder() {
    if (!page) return;
    const before = nextBefore(page, shown);
    if (before == null) return;
    const owns = runs.claim('history');
    loadingOlder = true;
    try {
      const p = await api.commits(target, { before, limit: PAGE });
      if (!owns()) return;
      shown = mergeCommits(shown, p.commits);
      page = { ...p, head: page.head };
    } catch (e) {
      if (owns()) toasts.error('Could not load older commits', e);
    } finally {
      if (owns()) loadingOlder = false;
    }
  }

  $effect(() => {
    void target;
    void refreshKey;
    void reload();
  });

  onMount(() => {
    const t = setInterval(() => (now = Date.now()), 15_000);
    return () => clearInterval(t);
  });

  const oldest = $derived(shown.length ? shown[shown.length - 1].seq : null);
  const notes = $derived(page ? historyNotes(page, oldest) : []);
  const more = $derived(page ? nextBefore(page, shown) != null : false);
  /** The dataset this one was cloned from, when it is still served here. */
  const source = $derived(
    info?.forkedFrom ? app.datasets.find((d) => d.id === info.forkedFrom?.id) : undefined,
  );
  const unsupported = $derived(error instanceof api.ApiError && error.status === 404 && !page);

  // --- point-in-time reads and diffs ---------------------------------------------

  /** Changes listed in the diff view at most. */
  const DIFF_LIMIT = 500;
  let diffForm = $state<{ from: string; to: string } | null>(null);
  let diff = $state<api.Diff | null>(null);
  let diffError = $state<api.ApiError | Error | null>(null);
  let diffLoading = $state(false);

  async function loadDiff() {
    if (!diffForm || !validAt(diffForm.from) || !validAt(diffForm.to)) return;
    const owns = runs.claim('diff');
    diffLoading = true;
    try {
      const d = await api.diff(target, {
        from: normalizeAt(diffForm.from) ?? 'head',
        to: normalizeAt(diffForm.to) ?? 'head',
        quads: true,
        limit: DIFF_LIMIT,
      });
      if (!owns()) return;
      diff = d;
      diffError = null;
    } catch (e) {
      if (owns()) {
        diff = null;
        diffError = e as Error;
      }
    } finally {
      if (owns()) diffLoading = false;
    }
  }

  /** Show what commit `c` changed (against its parent). */
  function showDiff(c: api.Commit) {
    diffForm = { from: String(Math.max(c.seq - 1, 0)), to: String(c.seq) };
    void loadDiff();
  }

  function closeDiff() {
    runs.claim('diff');
    diffForm = null;
    diff = null;
    diffError = null;
  }

  // --- history queries: the recorded changes of a resource -----------------------

  /** Changes listed by a history search at most. */
  const HISTORY_LIMIT = 200;
  let histForm = $state({ subject: '', predicate: '' });
  let hist = $state<api.HistoryChanges | null>(null);
  let histError = $state<api.ApiError | Error | null>(null);
  let histLoading = $state(false);

  async function loadHistory() {
    const subject = historyTerm(histForm.subject);
    const predicate = historyTerm(histForm.predicate);
    if (!subject && !predicate) return;
    const owns = runs.claim('changes');
    histLoading = true;
    try {
      const h = await api.historyChanges(target, {
        subject: subject ?? undefined,
        predicate: predicate ?? undefined,
        order: 'desc',
        limit: HISTORY_LIMIT,
      });
      if (!owns()) return;
      hist = h;
      histError = null;
    } catch (e) {
      if (owns()) {
        hist = null;
        histError = e as Error;
      }
    } finally {
      if (owns()) histLoading = false;
    }
  }

  function queryAt(c: api.Commit) {
    app.setDataset(name);
    app.queryBranch = branch ?? '';
    app.queryAt = c.seq === page?.head ? '' : c.ref;
    goto(resolve('/query'));
  }

  async function copyId(id: string, what: 'dataset' | 'branch') {
    try {
      await navigator.clipboard.writeText(id);
      toasts.push('success', `Copied ${what} id`, undefined, 1400);
    } catch (e) {
      toasts.error('Could not copy', e);
    }
  }
</script>

<section class="panel">
  <div class="panel-head">
    <h2>History</h2>
    {#if page}
      <span class="badge iri" title="Head commit">head {page.head}</span>
    {/if}
    <span class="spacer"></span>
    {#if canGraph}
      <button
        class="btn sm"
        class:ghost={!graph}
        aria-pressed={graph}
        title="Show the commits of every branch as a graph"
        onclick={toggleGraph}><Icon name="branch" size={12} /> Graph</button
      >
    {/if}
    <button
      class="btn ghost icon sm"
      title="Reload"
      aria-label="Reload history"
      onclick={() => {
        graphKick++;
        void reload();
      }}
      disabled={loading}
    >
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={13} />{/if}
    </button>
  </div>
  <div class="panel-body ids">
    {#if info?.id || page?.datasetId}
      {@const id = (branch ? info?.id : undefined) ?? page?.datasetId ?? info?.id ?? ''}
      <div class="idline">
        <span class="faint">Dataset id</span>
        <button class="id mono" title="Copy the dataset id" onclick={() => copyId(id, 'dataset')}
          >{id} <Icon name="copy" size={11} /></button
        >
      </div>
      {@const bid = branch ? page?.commits?.find((c) => c.branch === branch)?.branchId : undefined}
      {#if bid && bid !== id}
        <div class="idline">
          <span class="faint">Branch id</span>
          <button class="id mono" title="Copy the branch id" onclick={() => copyId(bid, 'branch')}
            >{bid} <Icon name="copy" size={11} /></button
          >
        </div>
      {/if}
    {/if}
    {#if info?.forkedFrom}
      {@const f = info.forkedFrom}
      <div class="idline">
        <span class="faint">Forked from</span>
        <span>
          {#if source}<a class="mono" href={resolve('/datasets/[name]', { name: source.name })}
              >/{source.name}</a
            >{:else}<span class="mono" title="Source dataset id">{f.id}</span>{/if}
          at commit {f.seq}
        </span>
      </div>
    {/if}
  </div>

  {#if unsupported}
    <p class="faint pad">This server does not keep a commit history.</p>
  {:else if error && !page}
    <div class="pad">
      <div class="error-box">
        <strong>Could not load the history.</strong>
        <span class="muted">{api.errorMessage(error)}</span>
      </div>
    </div>
  {:else if graph}
    <CommitGraph {name} {branch} {now} refreshKey={refreshKey + graphKick} />
  {:else if page}
    {#each notes as n (n)}
      <p class="note faint"><Icon name="info" size={13} /> {n}</p>
    {/each}
    {#if shown.length === 0}
      <p class="faint pad">No commits are retained.</p>
    {:else}
      <div class="list">
        <table class="data commits">
          <thead>
            <tr>
              <th class="num">#</th>
              <th>Kind</th>
              <th>When</th>
              <th class="num">Change</th>
              <th class="num" title="Quads in the dataset after the commit">Quads after</th>
              <th><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {#each shown as c (c.seq)}
              <tr class:head={c.seq === page.head} class:unreadable={!readable(c)}>
                <td
                  class="num mono seq"
                  title={readable(c)
                    ? `${c.ref}: a point-in-time read can see it`
                    : `${c.ref}: its data is no longer kept`}>{c.seq}</td
                >
                <td
                  ><div class="kind">
                    <span>{commitKindLabel(c.kind)}</span>
                    {#each commitFlags(c) as f (f.label)}
                      <span class="badge flag" class:warn={f.warn} title={f.title}>{f.label}</span>
                    {/each}
                    {#if c.mergedFrom}
                      {@const m = c.mergedFrom}
                      {#if m.branch}
                        <a
                          class="badge flag merged"
                          href={branchHref(m.branch)}
                          title="Merged commit {m.seq} of {m.branch}">from {m.branch}@{m.seq}</a
                        >
                      {:else}
                        <span
                          class="badge flag merged"
                          title="Merged commit {m.seq} of a deleted branch"
                          >from deleted@{m.seq}</span
                        >
                      {/if}
                    {/if}
                    {#if c.branch !== undefined && c.branch !== branchName}
                      {#if c.branch}
                        <a
                          class="badge flag inherited"
                          href={branchHref(c.branch)}
                          title="Made on {c.branch}, before {branchName} started">{c.branch}</a
                        >
                      {:else}
                        <span class="badge flag inherited" title="Made on a branch that was deleted"
                          >deleted branch</span
                        >
                      {/if}
                    {/if}
                    {#each c.snapshots ?? [] as s (s)}
                      <span class="badge flag snap" title="Pinned by the snapshot {s}">{s}</span>
                    {/each}
                  </div></td
                >
                <td title={c.timestamp}
                  ><div class="when">
                    <span>{fmtRelative(c.timestamp, now)}</span>
                    <span class="faint abs">{fmtCommitTime(c.timestamp)}</span>
                  </div></td
                >
                <td class="num delta mono">
                  {#if c.inserted || c.deleted}
                    <span class="ins">+{fmtInt(c.inserted)}</span>
                    <span class="del">−{fmtInt(c.deleted)}</span>
                  {:else}
                    <span class="faint" title={fmtDelta(c)}>—</span>
                  {/if}
                </td>
                <td class="num">{fmtInt(c.quads)}</td>
                <td class="row-actions">
                  <div class="cell-actions">
                    <button
                      class="btn ghost sm"
                      title="Show what this commit changed"
                      disabled={c.seq === 0}
                      onclick={() => showDiff(c)}>Diff</button
                    >
                    <button
                      class="btn ghost icon sm"
                      aria-label="Query at commit {c.seq}"
                      title="Query the dataset at this commit"
                      disabled={!readable(c)}
                      onclick={() => queryAt(c)}><Icon name="query" size={12} /></button
                    >
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      {#if diffForm}
        <div class="diff">
          <form
            class="diff-head"
            onsubmit={(e) => {
              e.preventDefault();
              void loadDiff();
            }}
          >
            <strong>Changes</strong>
            <label
              ><span class="faint">from</span>
              <input
                class="input sm mono"
                class:invalid={!validAt(diffForm.from)}
                size="8"
                bind:value={diffForm.from}
              /></label
            >
            <label
              ><span class="faint">to</span>
              <input
                class="input sm mono"
                class:invalid={!validAt(diffForm.to)}
                size="8"
                placeholder="head"
                bind:value={diffForm.to}
              /></label
            >
            <button class="btn sm" disabled={diffLoading}>
              {#if diffLoading}<span class="spinner"></span>{/if} Compare
            </button>
            <span class="spacer"></span>
            <button
              type="button"
              class="btn ghost icon sm"
              aria-label="Close the changes"
              onclick={closeDiff}><Icon name="x" size={12} /></button
            >
          </form>
          {#if diffError}
            <div class="error-box">
              <strong>Could not compare these commits.</strong>
              <span class="muted">{api.errorMessage(diffError)}</span>
            </div>
          {:else if diff}
            <p class="faint diff-sum">
              commit {diff.from.commit.seq} → commit {diff.to.commit.seq}:
              <span class="mono">{diffSummary(diff)}</span>
              {#if diff.method === 'compare'}· compared state against state{/if}
            </p>
            {#if diff.quads}
              <DiffLines quads={diff.quads} total={diff.added + diff.removed} />
            {/if}
          {/if}
        </div>
      {/if}
      <form
        class="diff diff-head"
        onsubmit={(e) => {
          e.preventDefault();
          void loadHistory();
        }}
      >
        <strong>Changes of</strong>
        <label
          ><span class="faint">subject</span>
          <input
            class="input sm mono"
            size="22"
            placeholder="<http://example.org/x>"
            bind:value={histForm.subject}
          /></label
        >
        <label
          ><span class="faint">predicate</span>
          <input
            class="input sm mono"
            size="18"
            placeholder="any"
            bind:value={histForm.predicate}
          /></label
        >
        <button
          class="btn sm"
          disabled={histLoading || (!histForm.subject.trim() && !histForm.predicate.trim())}
        >
          {#if histLoading}<span class="spinner"></span>{/if} Find
        </button>
      </form>
      {#if histError}
        <div class="diff">
          <div class="error-box">
            <strong>Could not read the change log.</strong>
            <span class="muted">{api.errorMessage(histError)}</span>
          </div>
        </div>
      {:else if hist}
        <div class="diff">
          <p class="faint diff-sum">
            {fmtInt(hist.changes.length)} change{hist.changes.length === 1 ? '' : 's'}, newest first{hist.truncated
              ? ` (the newest ${fmtInt(HISTORY_LIMIT)})`
              : ''}. {unrecordedNote(hist)}
          </p>
          {#if hist.changes.length}
            <pre class="diff-lines">{#each hist.changes as c, i (i)}<span
                  class="faint"
                  title={c.timestamp}
                  >{String(c.commit).padStart(6)}  </span><span
                  class:ins={c.op === 'add'}
                  class:del={c.op === 'remove'}>{historyLine(c)}</span
                >{#if historyBy(c)}<span class="faint">  # {historyBy(c)}</span
                  >{/if}{'\n'}{/each}</pre>
          {/if}
        </div>
      {/if}
      <div class="foot row">
        <span class="faint">
          {fmtInt(shown.length)} of {fmtInt(page.head - page.firstRetained + 1)} retained commits
        </span>
        <span class="spacer"></span>
        {#if more}
          <button class="btn sm" onclick={loadOlder} disabled={loadingOlder}>
            {#if loadingOlder}<span class="spinner"></span>{:else}<Icon
                name="chevronDown"
                size={13}
              />{/if} Load older
          </button>
        {/if}
      </div>
    {/if}
  {:else}
    <div class="pad faint row"><span class="spinner"></span> Loading history…</div>
  {/if}
</section>

<style>
  .ids {
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
    padding-bottom: 8px;
  }
  .ids:empty {
    display: none;
  }
  .idline {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 10px;
    align-items: baseline;
  }
  .idline > .faint {
    min-width: 76px;
  }
  .id {
    all: unset;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    color: var(--text-2);
    cursor: pointer;
    word-break: break-all;
  }
  .id:hover {
    color: var(--text);
  }
  .id:focus-visible {
    box-shadow: var(--focus);
  }
  .pad {
    padding: 12px 14px;
    font-size: var(--fs-sm);
  }
  .note {
    display: flex;
    gap: 6px;
    align-items: flex-start;
    margin: 0;
    padding: 6px 14px;
    font-size: var(--fs-sm);
    border-top: 1px solid var(--border);
  }
  .list {
    max-height: 420px;
    overflow: auto;
    border-top: 1px solid var(--border);
  }
  .commits {
    font-size: var(--fs-sm);
  }
  .commits td {
    white-space: nowrap;
  }
  .seq {
    color: var(--text-2);
    width: 1%;
  }
  tr.head .seq {
    color: var(--iri);
    font-weight: 600;
  }
  .kind {
    display: flex;
    align-items: center;
    gap: 5px;
  }
  .flag {
    height: 16px;
    font-weight: 500;
  }
  .flag.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .when {
    display: grid;
    line-height: 1.25;
  }
  .abs {
    font-size: 11px;
  }
  .delta {
    font-size: 12px;
  }
  .ins {
    color: var(--ok);
  }
  .del {
    color: var(--danger);
    margin-left: 4px;
  }
  .foot {
    padding: 8px 14px;
    border-top: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  tr.unreadable td {
    color: var(--text-2);
  }
  .snap {
    background: color-mix(in srgb, var(--iri) 14%, transparent);
    color: var(--iri);
  }
  a.flag {
    text-decoration: none;
  }
  a.flag:hover {
    text-decoration: underline;
  }
  .merged {
    background: color-mix(in srgb, var(--literal) 14%, transparent);
    color: var(--literal);
  }
  .inherited {
    font-family: var(--font-mono);
  }
  .row-actions {
    text-align: right;
    width: 1%;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  .diff {
    border-top: 1px solid var(--border);
    padding: 8px 14px;
    font-size: var(--fs-sm);
    display: grid;
    gap: 6px;
  }
  .diff-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
  }
  .diff-head label {
    display: flex;
    align-items: center;
    gap: 5px;
  }
  .diff-head input.invalid {
    border-color: var(--danger);
  }
  .diff-sum {
    margin: 0;
  }
  .diff-lines {
    margin: 0;
    max-height: 280px;
    overflow: auto;
    font-size: 11.5px;
    line-height: 1.45;
    white-space: pre;
  }
  .diff-lines .ins {
    color: var(--ok);
  }
  .diff-lines .del {
    color: var(--danger);
  }
</style>

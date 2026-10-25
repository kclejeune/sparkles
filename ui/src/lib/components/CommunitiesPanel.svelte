<script lang="ts">
  // The Communities tab of a dataset (spec C22 §2.3): the state of its GraphRAG reports,
  // a build or a refresh with its estimate to confirm, the hierarchy of communities as a
  // tree, and one community's members and report with its findings, citations and the
  // review actions. Everything here comes from routes that decide the reports graph's
  // coverage for the caller, so a caller who may not read it sees only that it is not
  // available.
  import { onDestroy, untrack } from 'svelte';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { fmtInt, fmtRelative } from '$lib/format';
  import { displayIri } from '$lib/rdf';
  import * as gr from '$lib/graphrag';
  import CommunityReport from './CommunityReport.svelte';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  let {
    ds,
    branch = null,
    canAdmin = false,
    canWrite = false,
    readOnly = false,
    prefixes = {},
    initial = null,
  }: {
    ds: string;
    /** The branch the page shows, null for main. The reports belong to main only. */
    branch?: string | null;
    canAdmin?: boolean;
    canWrite?: boolean;
    readOnly?: boolean;
    prefixes?: Record<string, string>;
    /** The community to show first (`?community=`), such as one Explore opened. */
    initial?: string | null;
  } = $props();

  let st = $state<gr.GraphragStatus | null>(null);
  let list = $state<gr.Communities | null>(null);
  let error = $state<unknown>(null);
  let loading = $state(false);
  let selected = $state<string | null>(null);
  let detail = $state<gr.Community | null>(null);
  let detailError = $state<unknown>(null);
  let detailLoading = $state(false);
  let filter = $state<gr.ReportStatus | ''>('');

  // the run this tab started or found, and its estimate dialog
  let task = $state<api.Task | null>(null);
  let starting = $state(false);
  let estimateOpen = $state(false);
  let answered = $state<string | null>(null);
  let stop = false;
  onDestroy(() => (stop = true));
  // the dataset and branch the panel shows: a new one ends every follower and answer of
  // the previous one, so a run on main is never confirmed from another branch's page
  let context = 0;

  const short = (iri: string) => displayIri(iri, prefixes);
  const running = $derived(task != null && !gr.ended(task));
  const estimate = $derived(gr.waitingEstimate(task));
  const forest = $derived(gr.tree(list?.communities ?? []));
  const shownIris = $derived.by(() => {
    if (!filter || !list) return null;
    const keep = new Set<string>();
    const byIri = new Map(list.communities.map((c) => [c.iri, c]));
    for (const c of list.communities) {
      if (c.report?.status !== filter) continue;
      // a match keeps its ancestors, so the tree stays a tree
      for (
        let n: gr.CommunityRow | undefined = c;
        n;
        n = n.parent ? byIri.get(n.parent) : undefined
      )
        keep.add(n.iri);
    }
    return keep;
  });

  async function load() {
    const ctx = context;
    loading = true;
    try {
      const [s, l] = await Promise.all([gr.status(ds), gr.communities(ds)]);
      if (ctx !== context) return;
      st = s;
      list = l;
      error = null;
      if (selected && !l.communities.some((c) => c.iri === selected)) {
        detailSeq++;
        selected = null;
        detail = null;
      }
    } catch (e) {
      error = e;
    } finally {
      loading = false;
    }
  }

  // the latest detail request: an answer to an earlier one is dropped, so a slow answer
  // never replaces the community chosen after it
  let detailSeq = 0;

  async function open(iri: string) {
    const seq = ++detailSeq;
    selected = iri;
    detailLoading = true;
    detailError = null;
    // another community's report must not stay actionable while this one loads
    if (detail?.iri !== iri) detail = null;
    try {
      const d = await gr.community(ds, iri);
      if (seq !== detailSeq) return;
      detail = d;
    } catch (e) {
      if (seq !== detailSeq) return;
      detail = null;
      detailError = e;
    } finally {
      if (seq === detailSeq) detailLoading = false;
    }
  }

  $effect(() => {
    void ds;
    const onMain = !branch;
    untrack(() => {
      context++;
      task = null;
      answered = null;
      estimateOpen = false;
      editOpen = false;
      detailSeq++;
      selected = null;
      detail = null;
      st = null;
      list = null;
      if (!onMain) return;
      void load();
      void findRunning();
      if (initial) void open(initial);
    });
  });

  /** A run of this dataset that is still going, such as one started from the CLI. */
  async function findRunning() {
    const ctx = context;
    try {
      const all = await api.listTasks();
      if (ctx !== context) return;
      const t = all.find((x) => x.kind === 'graphrag' && x.dataset === ds && !gr.ended(x));
      if (t) void follow(t);
    } catch {
      // the task list needs more than read access; the tab works without it
    }
  }

  async function follow(t: api.Task) {
    const ctx = context;
    const live = () => !stop && ctx === context;
    task = t;
    while (live() && task && !gr.ended(task)) {
      if (gr.waitingEstimate(task) && answered !== task.id) estimateOpen = true;
      await new Promise((r) => setTimeout(r, 700));
      if (!live()) return;
      try {
        const next = await gr.task(t.id);
        if (!live()) return;
        task = next;
      } catch (e) {
        if (live()) toasts.error('Could not check on the run', e);
        return;
      }
    }
    if (!live()) return;
    estimateOpen = false;
    if (!task) return;
    const o = gr.outcomeOf(task);
    if (o) toasts.push(o.stopped ? 'info' : 'success', 'GraphRAG run finished', gr.outcomeText(o));
    else if (task.state === 'failed')
      toasts.push('error', 'GraphRAG run failed', task.message ?? '');
    await load();
    if (selected) await open(selected);
  }

  async function start(mode: 'build' | 'refresh') {
    if (branch) return;
    const ctx = context;
    starting = true;
    try {
      const t = await gr.start(ds, mode, false);
      if (ctx === context) void follow(t);
    } catch (e) {
      toasts.error(`Could not start the ${mode}`, e);
    } finally {
      starting = false;
    }
  }

  async function answer(yes: boolean) {
    estimateOpen = false;
    // only a run of the dataset this panel shows on main
    if (!task || branch || task.dataset !== ds) return;
    answered = task.id;
    try {
      await gr.confirm(ds, task.id, yes);
    } catch (e) {
      toasts.error('Could not record the answer', e);
    }
  }

  async function cancel() {
    if (!task) return;
    try {
      await api.json(`/$/tasks/${encodeURIComponent(task.id)}`, { method: 'DELETE' });
    } catch (e) {
      toasts.error('Could not cancel the run', e);
    }
  }

  // reviews
  let reviewing = $state(false);
  let editOpen = $state(false);
  let editTitle = $state('');
  let editSummary = $state('');

  async function setStatus(status: 'reviewed' | 'rejected') {
    const r = detail?.report;
    if (!r) return;
    reviewing = true;
    try {
      await gr.review(ds, r.iri, { status });
      toasts.push('success', status === 'reviewed' ? 'Marked reviewed' : 'Rejected', r.title ?? '');
      await load();
      if (selected) await open(selected);
    } catch (e) {
      toasts.error('Could not save the review', e);
    } finally {
      reviewing = false;
    }
  }

  function startEdit() {
    editTitle = detail?.report?.title ?? '';
    editSummary = detail?.report?.summary ?? '';
    editOpen = true;
  }

  async function saveEdit() {
    const r = detail?.report;
    if (!r) return;
    reviewing = true;
    try {
      const change: { title?: string; summary?: string } = {};
      if (editTitle.trim() && editTitle !== r.title) change.title = editTitle.trim();
      if (editSummary.trim() && editSummary !== r.summary) change.summary = editSummary.trim();
      if (!change.title && !change.summary) {
        editOpen = false;
        return;
      }
      await gr.review(ds, r.iri, change);
      editOpen = false;
      toasts.push('success', 'Saved as a reviewed revision', change.title ?? r.title ?? '');
      await load();
      if (selected) await open(selected);
    } catch (e) {
      toasts.error('Could not save the edit', e);
    } finally {
      reviewing = false;
    }
  }

  const statusClass = gr.statusClass;
  const settingsHref = $derived(`${resolve('/datasets/[name]', { name: ds })}?tab=settings`);
</script>

{#snippet node(n: gr.TreeNode)}
  {#if !shownIris || shownIris.has(n.iri)}
    <li>
      <button
        class="node"
        class:active={selected === n.iri}
        aria-current={selected === n.iri ? 'true' : undefined}
        onclick={() => open(n.iri)}
      >
        <span class="title">{n.report?.title ?? short(n.iri)}</span>
        <span class="meta">
          <span class="faint">L{n.level} · {fmtInt(n.memberCount)}</span>
          {#if n.report?.status}
            <span class="badge {statusClass(n.report.status)}">{n.report.status}</span>
          {/if}
        </span>
      </button>
      {#if n.children.length}
        <ul>
          {#each n.children as c (c.iri)}{@render node(c)}{/each}
        </ul>
      {/if}
    </li>
  {/if}
{/snippet}

<section class="panel" data-testid="communities">
  <div class="panel-head">
    <h2>Communities</h2>
    {#if st?.enabled && list?.available}<span class="badge">{fmtInt(list.communities.length)}</span
      >{/if}
    <span class="spacer"></span>
    {#if canAdmin && st?.enabled && !branch}
      <button
        class="btn sm"
        disabled={readOnly || running || starting}
        title="Rewrite the reports of communities that changed since the last run"
        onclick={() => start('refresh')}><Icon name="cycle" size={13} /> Refresh</button
      >
      <button
        class="btn sm primary"
        disabled={readOnly || running || starting}
        title="Write a report for every community. You see the estimate before anything is sent."
        onclick={() => start('build')}
        >{#if starting}<span class="spinner"></span>{:else}<Icon name="sparkle" size={13} />{/if} Build</button
      >
    {/if}
    <button
      class="btn ghost icon sm"
      title="Reload"
      aria-label="Reload communities"
      onclick={load}
      disabled={loading || !!branch}
    >
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={13} />{/if}
    </button>
  </div>

  {#if branch}
    <p class="pad faint" data-testid="communities-off-main">
      <Icon name="branch" size={12} /> Community reports exist only on main, and you are on
      <span class="mono">{branch}</span>. Switch to main to read, build or review them.
    </p>
  {:else if error && !st}
    <div class="pad">
      <div class="error-box">
        <strong>Could not load the communities.</strong>
        <span class="muted">{api.errorMessage(error)}</span>
      </div>
    </div>
  {:else if st && !st.enabled}
    <p class="pad faint">
      GraphRAG is off for this dataset. A dataset admin can turn it on in the <a href={settingsHref}
        >GraphRAG settings</a
      > and choose which graphs to group and which user the build reads as.
    </p>
  {:else if st && !st.available}
    <p class="pad faint" data-testid="communities-unavailable">
      <Icon name="lock" size={12} /> You cannot open these reports because they draw on data you do not
      have access to.
    </p>
  {:else if st}
    <div class="panel-body summary">
      {#each gr.STATUSES as s (s)}
        {#if st.reports?.[s]}
          <span><span class="badge {statusClass(s)}">{s}</span> {fmtInt(st.reports[s] ?? 0)}</span>
        {/if}
      {/each}
      {#each st.levels ?? [] as l (l.level)}
        <span><span class="faint">Level {l.level}</span> {fmtInt(l.communities)}</span>
      {/each}
      {#if st.lastBuild}
        <span title={st.lastBuild.finishedAt}
          ><span class="faint">Last run</span>
          {fmtRelative(st.lastBuild.finishedAt)} at commit {st.lastBuild.sourceCommit},
          {fmtInt(st.lastBuild.tokens)} tokens{#if st.lastBuild.pending}, {fmtInt(
              st.lastBuild.pending,
            )} pending{/if}</span
        >
      {/if}
      {#if st.budget}
        <span
          ><span class="faint">Today</span>
          {fmtInt(st.budget.usedToday)} of {fmtInt(st.budget.tokensPerDay)} tokens</span
        >
      {/if}
    </div>
    {#if st.anomalies?.length}
      <div class="pad">
        <div class="error-box" role="alert">
          <strong>Accounting anomaly.</strong>
          A provider reported more usage than was reserved, so the run stopped. {st.anomalies
            .length} recorded.
        </div>
      </div>
    {/if}
    {#if task && running}
      <div class="pad run" data-testid="graphrag-run">
        <span class="spinner"></span>
        <span>{task.message ?? 'Running'}…</span>
        {#if estimate && answered === task.id}<span class="faint">waiting for the server</span>{/if}
        {#if estimate && answered !== task.id}
          <button class="btn sm" onclick={() => (estimateOpen = true)}>Review the estimate</button>
        {/if}
        <span class="spacer"></span>
        {#if task.cancellable}<button class="btn sm ghost" onclick={cancel}>Cancel</button>{/if}
      </div>
    {/if}

    {#if list && list.communities.length === 0 && !running}
      <p class="pad faint">
        No communities yet.{#if canAdmin}
          Build the reports to group the dataset's resources and summarize each group.{/if}
      </p>
    {:else if list}
      <div class="split">
        <nav class="tree" aria-label="Community hierarchy">
          <label class="filter">
            <span class="faint">Show</span>
            <select class="input sm" bind:value={filter}>
              <option value="">every report</option>
              {#each gr.STATUSES as s (s)}<option value={s}>{s}</option>{/each}
            </select>
          </label>
          <ul class="root">
            {#each forest as n (n.iri)}{@render node(n)}{/each}
          </ul>
        </nav>
        <div class="detail" aria-live="polite">
          {#if !selected}
            <p class="faint">Choose a community to see its members and its report.</p>
          {:else if detailLoading && !detail}
            <p class="faint"><span class="spinner"></span> Loading…</p>
          {:else if detailError}
            <div class="error-box">{api.errorMessage(detailError)}</div>
          {:else if detail}
            {@const d = detail}
            <CommunityReport
              {ds}
              detail={d}
              {prefixes}
              onopen={(iri) => open(iri)}
              review={canWrite && !readOnly
                ? { busy: reviewing, onstatus: setStatus, onedit: startEdit }
                : null}
            >
              {#snippet actions()}
                <a
                  class="btn sm"
                  href={gr.exploreHref(resolve('/explore'), ds, d.iri)}
                  title="Draw this community among the others in the Explore overview"
                  ><Icon name="graph" size={12} /> Show in Explore</a
                >
              {/snippet}
            </CommunityReport>
          {/if}
        </div>
      </div>
    {/if}
  {/if}
</section>

<Modal bind:open={estimateOpen} title="Send this build to the model?" width={520}>
  {#if estimate && !branch}
    <p>
      The run sends {fmtInt(estimate.requests)} request{estimate.requests === 1 ? '' : 's'} for {fmtInt(
        estimate.communities,
      )}
      communit{estimate.communities === 1 ? 'y' : 'ies'} to
      <span class="mono">{estimate.model}</span>.
    </p>
    <p>
      It may use up to <strong>{fmtInt(estimate.tokens)}</strong> tokens. This is an upper bound,
      and the budget allows
      {fmtInt(estimate.tokensPerBuild)} per build and {fmtInt(estimate.tokensPerDay)} per day.
    </p>
  {/if}
  {#snippet actions()}
    <button class="btn" onclick={() => answer(false)}>Don't send</button>
    <button class="btn primary solid" onclick={() => answer(true)}>Send</button>
  {/snippet}
</Modal>

<Modal bind:open={editOpen} title="Edit the report" width={600}>
  <p class="small faint">
    The edit is saved as a new reviewed report that supersedes this one. The findings and citations
    stay as they are.
  </p>
  <label class="field">
    <span class="faint">Title</span>
    <input class="input" maxlength="16384" bind:value={editTitle} />
  </label>
  <label class="field">
    <span class="faint">Summary</span>
    <textarea class="input" rows="6" maxlength="16384" bind:value={editSummary}></textarea>
  </label>
  {#snippet actions()}
    <button class="btn" onclick={() => (editOpen = false)}>Cancel</button>
    <button class="btn primary solid" disabled={reviewing} onclick={saveEdit}>Save</button>
  {/snippet}
</Modal>

<style>
  .pad {
    padding: 8px 12px;
  }
  .summary {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 16px;
    font-size: var(--fs-sm);
  }
  .run {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    font-size: var(--fs-sm);
  }
  .split {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    border-top: 1px solid var(--border);
  }
  @media (min-width: 860px) {
    .split {
      grid-template-columns: minmax(260px, 2fr) minmax(0, 3fr);
    }
    .tree {
      border-right: 1px solid var(--border);
      max-height: 72vh;
      overflow: auto;
    }
    .detail {
      max-height: 72vh;
      overflow: auto;
    }
  }
  .tree {
    padding: 8px;
    min-width: 0;
  }
  .filter {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 6px;
    font-size: var(--fs-sm);
  }
  .tree ul {
    list-style: none;
    margin: 0;
    padding: 0 0 0 14px;
  }
  .tree ul.root {
    padding: 0;
  }
  .node {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: 2px 8px;
    width: 100%;
    padding: 5px 8px;
    border: 0;
    border-radius: var(--r);
    background: none;
    color: inherit;
    text-align: left;
    font: inherit;
    font-size: var(--fs-sm);
    cursor: pointer;
  }
  .node:hover {
    background: var(--surface-2);
  }
  .node.active {
    background: var(--surface-3);
  }
  .node .title {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .node .meta {
    display: inline-flex;
    gap: 6px;
    align-items: center;
  }
  .detail {
    padding: 10px 14px;
    min-width: 0;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .field {
    display: grid;
    gap: 4px;
    margin: 10px 0;
  }
  .field textarea {
    resize: vertical;
  }
</style>

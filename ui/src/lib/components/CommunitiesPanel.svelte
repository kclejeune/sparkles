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
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  let {
    ds,
    canAdmin = false,
    canWrite = false,
    readOnly = false,
    prefixes = {},
  }: {
    ds: string;
    canAdmin?: boolean;
    canWrite?: boolean;
    readOnly?: boolean;
    prefixes?: Record<string, string>;
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
    loading = true;
    try {
      const [s, l] = await Promise.all([gr.status(ds), gr.communities(ds)]);
      st = s;
      list = l;
      error = null;
      if (selected && !l.communities.some((c) => c.iri === selected)) {
        selected = null;
        detail = null;
      }
    } catch (e) {
      error = e;
    } finally {
      loading = false;
    }
  }

  async function open(iri: string) {
    selected = iri;
    detailLoading = true;
    try {
      detail = await gr.community(ds, iri);
      detailError = null;
    } catch (e) {
      detail = null;
      detailError = e;
    } finally {
      detailLoading = false;
    }
  }

  $effect(() => {
    void ds;
    untrack(() => {
      selected = null;
      detail = null;
      void load();
      void findRunning();
    });
  });

  /** A run of this dataset that is still going, such as one started from the CLI. */
  async function findRunning() {
    try {
      const all = await api.listTasks();
      const t = all.find((x) => x.kind === 'graphrag' && x.dataset === ds && !gr.ended(x));
      if (t) void follow(t);
    } catch {
      // the task list needs more than read access; the tab works without it
    }
  }

  async function follow(t: api.Task) {
    task = t;
    while (!stop && task && !gr.ended(task)) {
      if (gr.waitingEstimate(task) && answered !== task.id) estimateOpen = true;
      await new Promise((r) => setTimeout(r, 700));
      if (stop) return;
      try {
        task = await gr.task(t.id);
      } catch (e) {
        toasts.error('Lost track of the run', e);
        return;
      }
    }
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
    starting = true;
    try {
      const t = await gr.start(ds, mode, false);
      void follow(t);
    } catch (e) {
      toasts.error(`The ${mode} did not start`, e);
    } finally {
      starting = false;
    }
  }

  async function answer(yes: boolean) {
    if (!task) return;
    answered = task.id;
    estimateOpen = false;
    try {
      await gr.confirm(ds, task.id, yes);
    } catch (e) {
      toasts.error('The answer was not recorded', e);
    }
  }

  async function cancel() {
    if (!task) return;
    try {
      await api.json(`/$/tasks/${encodeURIComponent(task.id)}`, { method: 'DELETE' });
    } catch (e) {
      toasts.error('The run was not cancelled', e);
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
      toasts.error('The review was not saved', e);
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
      toasts.error('The edit was not saved', e);
    } finally {
      reviewing = false;
    }
  }

  const statusClass = (s: gr.ReportStatus | null | undefined) =>
    s === 'reviewed'
      ? 'ok'
      : s === 'rejected' || s === 'failed'
        ? 'danger'
        : s === 'stale'
          ? 'warn'
          : 'spark';
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
    {#if canAdmin && st?.enabled}
      <button
        class="btn sm"
        disabled={readOnly || running || starting}
        title="Rewrite the reports of communities that changed since the last run"
        onclick={() => start('refresh')}><Icon name="cycle" size={13} /> Refresh</button
      >
      <button
        class="btn sm primary"
        disabled={readOnly || running || starting}
        title="Write a report for every community; the estimate is shown before anything is sent"
        onclick={() => start('build')}
        >{#if starting}<span class="spinner"></span>{:else}<Icon name="sparkle" size={13} />{/if} Build</button
      >
    {/if}
    <button
      class="btn ghost icon sm"
      title="Reload"
      aria-label="Reload communities"
      onclick={load}
      disabled={loading}
    >
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={13} />{/if}
    </button>
  </div>

  {#if error && !st}
    <div class="pad">
      <div class="error-box">
        <strong>Could not load the communities.</strong>
        <span class="muted">{api.errorMessage(error)}</span>
      </div>
    </div>
  {:else if st && !st.enabled}
    <p class="pad faint">
      GraphRAG is off for this dataset. A dataset admin turns it on in the <a href={settingsHref}
        >GraphRAG settings</a
      >, with the user whose view the build reads and the graphs it groups.
    </p>
  {:else if st && !st.available}
    <p class="pad faint" data-testid="communities-unavailable">
      <Icon name="lock" size={12} /> The community reports are not available to you, because your access
      does not cover everything the build read.
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
            {@const r = detail.report}
            <header>
              <h3>{r?.title ?? short(detail.iri)}</h3>
              <p class="faint mono small iri">{detail.iri}</p>
              <p class="small">
                Level {detail.level} · {fmtInt(detail.memberCount)} members · {fmtInt(
                  detail.factCount,
                )} facts
                {#if detail.parent}
                  · part of <button class="link" onclick={() => open(detail!.parent!)}
                    >{short(detail.parent)}</button
                  >
                {/if}
              </p>
            </header>
            {#if r}
              <div class="row status-line">
                <span class="badge {statusClass(r.status)}">{r.status}</span>
                {#if r.rating != null}<span class="faint">importance {r.rating}/10</span>{/if}
                {#if r.revisionOf}<span class="faint">edited revision</span>{/if}
                <span class="spacer"></span>
                {#if canWrite && !readOnly && r.status !== 'failed'}
                  <button
                    class="btn sm"
                    disabled={reviewing || r.status === 'reviewed'}
                    onclick={() => setStatus('reviewed')}
                    ><Icon name="check" size={12} /> Reviewed</button
                  >
                  <button
                    class="btn sm"
                    disabled={reviewing || r.status === 'rejected'}
                    onclick={() => setStatus('rejected')}><Icon name="x" size={12} /> Reject</button
                  >
                  <button class="btn sm" disabled={reviewing} onclick={startEdit}>Edit</button>
                {/if}
              </div>
              {#if r.failure}
                <div class="error-box"><strong>No report.</strong> {r.failure}</div>
              {/if}
              {#if r.summary}<p class="summary-text">{r.summary}</p>{/if}
              {#if r.findings.length}
                <ol class="findings">
                  {#each r.findings as f, i (i)}
                    <li>
                      <p>{f.text}</p>
                      {#if f.unmatchedNames.length}
                        <p class="small warn-text">
                          <Icon name="alert" size={11} /> Not found among the members: {f.unmatchedNames.join(
                            ', ',
                          )}
                        </p>
                      {/if}
                      {#if f.cites.length}
                        <details>
                          <summary class="small faint"
                            >{f.cites.length} citation{f.cites.length === 1 ? '' : 's'}</summary
                          >
                          <ul class="cites">
                            {#each f.cites as c (c)}<li class="mono small">
                                {gr.citeText(c)}
                              </li>{/each}
                          </ul>
                        </details>
                      {/if}
                    </li>
                  {/each}
                </ol>
              {/if}
              <p class="small faint">
                {#if r.activity}Written by {r.activity.model} from commit {r.sourceCommit}, {fmtInt(
                    r.activity.inputTokens,
                  )}
                  + {fmtInt(r.activity.outputTokens)} tokens.{/if}
                {#if r.droppedCitations || r.droppedUnsupported}
                  The checks dropped {r.droppedCitations ?? 0} invalid citation{r.droppedCitations ===
                  1
                    ? ''
                    : 's'} and
                  {r.droppedUnsupported ?? 0} unsupported finding{r.droppedUnsupported === 1
                    ? ''
                    : 's'}.
                {/if}
              </p>
            {:else}
              <p class="faint">
                This community has no report. It is smaller than the minimum size or still pending.
              </p>
            {/if}
            {#if detail.children.length}
              <h4>Sub-communities</h4>
              <ul class="plain">
                {#each detail.children as c (c.iri)}
                  <li>
                    <button class="link" onclick={() => open(c.iri)}
                      >{c.title ?? short(c.iri)}</button
                    >
                  </li>
                {/each}
              </ul>
            {/if}
            <h4>Members</h4>
            <ul class="members">
              {#each detail.members as m (m.iri)}
                <li>
                  <a
                    href="{resolve('/explore')}?ds={encodeURIComponent(ds)}&iri={encodeURIComponent(
                      m.iri,
                    )}"
                    title={m.iri}>{m.label ?? short(m.iri)}</a
                  >
                </li>
              {/each}
            </ul>
            {#if detail.members.length < detail.memberCount}
              <p class="small faint">
                The first {detail.members.length} of {fmtInt(detail.memberCount)} members.
              </p>
            {/if}
          {/if}
        </div>
      </div>
    {/if}
  {/if}
</section>

<Modal bind:open={estimateOpen} title="Send this build to the model?" width={520}>
  {#if estimate}
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
  .detail h3 {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .detail h4 {
    margin: 14px 0 4px;
    font-size: var(--fs-sm);
  }
  .iri {
    margin: 2px 0;
    overflow-wrap: anywhere;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .status-line {
    flex-wrap: wrap;
    gap: 6px;
    margin: 8px 0;
  }
  .summary-text {
    line-height: 1.5;
  }
  .findings {
    padding-left: 20px;
  }
  .findings li {
    margin-bottom: 8px;
  }
  .findings p {
    margin: 0 0 2px;
  }
  .cites {
    margin: 4px 0;
    padding-left: 16px;
  }
  .cites li {
    overflow-wrap: anywhere;
  }
  .warn-text {
    color: var(--warn, inherit);
  }
  .members {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    list-style: none;
    padding: 0;
    margin: 0;
    font-size: var(--fs-sm);
  }
  .members li {
    overflow-wrap: anywhere;
  }
  .plain {
    list-style: none;
    padding: 0;
    margin: 0;
    font-size: var(--fs-sm);
  }
  .link {
    border: 0;
    padding: 0;
    background: none;
    color: var(--link, var(--spark));
    font: inherit;
    cursor: pointer;
    text-align: left;
    overflow-wrap: anywhere;
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

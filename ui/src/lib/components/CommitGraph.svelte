<script lang="ts">
  // The History panel's graph view: the commits of every branch in time order, one lane
  // per branch, with fork and merge edges (lib/commit-graph.ts lays them out). Only the
  // rows in view are drawn, so thousands of commits stay cheap.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { MAIN, onBranch } from '$lib/branches';
  import {
    appendPage,
    assignLanes,
    buildRows,
    commitKey,
    edgePath,
    edgeSpan,
    headLabels,
    LANE_W,
    laneX,
    layoutEdges,
    PAD,
    ROW_H,
    rowY,
    visibleRange,
    type CommitGraphPage,
    type GraphCommit,
  } from '$lib/commit-graph';
  import { commitKindLabel, fmtCommitTime } from '$lib/commits';
  import { fmtInt, fmtRelative } from '$lib/format';
  import { diffSummary, readable } from '$lib/history';
  import { LatestRun } from '$lib/supersede';
  import DiffLines from './DiffLines.svelte';
  import Icon from './Icon.svelte';

  let {
    name,
    branch = null,
    refreshKey = 0,
    now = Date.now(),
  }: {
    name: string;
    /** The branch the page shows (null: `main`); its lane is marked. */
    branch?: string | null;
    /** Bump to reload. */
    refreshKey?: number;
    now?: number;
  } = $props();

  const PAGE = 200;
  /** Height of the scrolled graph. */
  const VIEW_H = 440;

  let page = $state<CommitGraphPage | null>(null);
  let error = $state<api.ApiError | Error | null>(null);
  let loading = $state(false);
  let loadingOlder = $state(false);
  let expanded = $state(new Set<string>());
  let selected = $state<string | null>(null);
  let scrollTop = $state(0);
  let viewH = $state(VIEW_H);
  const runs = new LatestRun();

  async function reload() {
    const owns = runs.claim('graph');
    loading = true;
    try {
      const p = await api.commitGraph(name, { limit: PAGE });
      if (!owns()) return;
      page = p;
      error = null;
    } catch (e) {
      if (owns()) error = e as Error;
    } finally {
      if (owns()) loading = false;
    }
  }

  async function loadOlder() {
    if (!page?.next) return;
    const owns = runs.claim('graph');
    loadingOlder = true;
    try {
      const p = await api.commitGraph(name, { next: page.next });
      if (!owns() || !page) return;
      page = appendPage(page, p);
    } catch (e) {
      if (owns()) toasts.error('Could not load older commits', e);
    } finally {
      if (owns()) loadingOlder = false;
    }
  }

  $effect(() => {
    void name;
    void refreshKey;
    void reload();
  });

  const branches = $derived(page?.branches ?? []);
  const lanes = $derived(assignLanes(branches));
  const laneCount = $derived(Math.max(1, lanes.size));
  const rows = $derived(page ? buildRows(page.commits, branches, lanes, expanded) : []);
  const layout = $derived(layoutEdges(rows, lanes, !!page?.next));
  const labels = $derived(headLabels(branches));
  const stubsByRow = $derived(
    layout.stubs.reduce((m, s) => m.set(s.row, [...(m.get(s.row) ?? []), s]), new Map()),
  );
  const width = $derived(PAD * 2 + (laneCount - 1) * LANE_W);
  const shownLane = $derived(
    lanes.get(branches.find((b) => b.name === (branch ?? MAIN))?.id ?? ''),
  );
  const range = $derived(visibleRange(scrollTop, viewH, rows.length));
  const bottom = $derived(rows.length * ROW_H);
  const visibleEdges = $derived(
    layout.edges.filter((e) => {
      const [a, b] = edgeSpan(e, rows.length);
      return b >= range[0] - 1 && a <= range[1];
    }),
  );
  const visibleRows = $derived(
    rows.slice(range[0], range[1]).map((r, i) => ({ r, i: i + range[0] })),
  );
  const laneColour = (lane: number) => `var(--lane-${lane % 8})`;

  const byKey = $derived(
    new Map((page?.commits ?? []).map((c) => [commitKey(c.branchId, c.seq), c] as const)),
  );
  const current = $derived(selected ? byKey.get(selected) : undefined);

  function toggleSegment(key: string) {
    const next = new Set(expanded);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expanded = next;
  }

  // --- the selected commit: its changes and a link to query it ----------------------

  const DIFF_LIMIT = 500;
  let diff = $state<api.Diff | null>(null);
  let diffError = $state<api.ApiError | Error | null>(null);
  let diffLoading = $state(false);

  async function select(c: GraphCommit) {
    const key = commitKey(c.branchId, c.seq);
    if (selected === key) {
      selected = null;
      return;
    }
    selected = key;
    diff = null;
    diffError = null;
    if (c.seq === 0) return;
    const owns = runs.claim('diff');
    diffLoading = true;
    try {
      const d = await api.diff(onBranch(name, c.branch), {
        from: String(c.seq - 1),
        to: String(c.seq),
        quads: true,
        limit: DIFF_LIMIT,
      });
      if (owns()) diff = d;
    } catch (e) {
      if (owns()) diffError = e as Error;
    } finally {
      if (owns()) diffLoading = false;
    }
  }

  function queryAt(e: MouseEvent, c: GraphCommit) {
    e.preventDefault();
    app.setDataset(name);
    app.queryBranch = c.branch === MAIN ? '' : c.branch;
    const head = branches.find((b) => b.id === c.branchId)?.head;
    app.queryAt = c.seq === head ? '' : c.ref;
    goto(resolve('/query'));
  }
</script>

<div class="cg">
  {#if error && !page}
    <div class="pad">
      <div class="error-box">
        <strong>Could not load the commit graph.</strong>
        <span class="muted">{api.errorMessage(error)}</span>
      </div>
    </div>
  {:else if !page}
    <div class="pad faint row"><span class="spinner"></span> Loading the graph…</div>
  {:else if rows.length === 0}
    <p class="faint pad">No commits are retained.</p>
  {:else}
    <div
      class="scroll"
      style:max-height="{VIEW_H}px"
      bind:clientHeight={viewH}
      onscroll={(e) => (scrollTop = (e.currentTarget as HTMLElement).scrollTop)}
    >
      <div class="space" style:height="{bottom}px">
        <svg
          class="lines"
          {width}
          height={(range[1] - range[0]) * ROW_H}
          style:top="{range[0] * ROW_H}px"
          viewBox="0 {range[0] * ROW_H} {width} {(range[1] - range[0]) * ROW_H}"
          aria-hidden="true"
        >
          {#if shownLane != null}
            <rect
              class="shown"
              x={laneX(shownLane) - LANE_W / 2}
              y={range[0] * ROW_H}
              width={LANE_W}
              height={(range[1] - range[0]) * ROW_H}
            />
          {/if}
          {#each visibleEdges as e (`${e.kind}:${e.from}:${e.to}:${e.toLane}`)}
            <path
              class="edge {e.kind}"
              d={edgePath(e, bottom)}
              style:stroke={laneColour(e.colour)}
            />
          {/each}
          {#each visibleRows as { r, i } (r.key)}
            {#if r.kind === 'segment'}
              <rect
                class="seg"
                x={laneX(r.lane) - 4}
                y={rowY(i) - 8}
                width="8"
                height="16"
                rx="4"
                style:stroke={laneColour(r.lane)}
              />
            {:else}
              {@const merge = r.commit.parents.length > 1}
              {@const head = labels.has(r.key)}
              <circle
                class="dot"
                class:merge
                class:head
                cx={laneX(r.lane)}
                cy={rowY(i)}
                r={merge ? 5 : 4}
                style:stroke={laneColour(r.lane)}
                style:fill={head ? laneColour(r.lane) : undefined}
              />
            {/if}
          {/each}
        </svg>
        <ol class="rows" style:left="{width + 4}px">
          {#each visibleRows as { r, i } (r.key)}
            <li class="rowline" style:top="{i * ROW_H}px">
              {#if r.kind === 'segment'}
                {@const n = r.commits.length}
                <button
                  class="node seg-btn"
                  aria-expanded="false"
                  title="Commits {r.commits[n - 1].seq}–{r.commits[0].seq} of {r.branch}"
                  onclick={() => toggleSegment(r.key)}
                >
                  <Icon name="chevron" size={12} />
                  <span>{fmtInt(n)} commits</span>
                  <span class="mono bname" style:color={laneColour(r.lane)}>{r.branch}</span>
                  <span class="faint mono">{r.commits[n - 1].seq}–{r.commits[0].seq}</span>
                </button>
              {:else}
                {@const c = r.commit}
                {@const segKey = [...expanded].find((k) => k === `seg:${r.key}`)}
                <button
                  class="node"
                  class:sel={selected === r.key}
                  class:unreadable={!readable(c)}
                  aria-pressed={selected === r.key}
                  aria-label="Commit {c.seq} on {c.branch}"
                  onclick={() => select(c)}
                >
                  {#each labels.get(r.key) ?? [] as l (l)}
                    <span
                      class="badge headlabel mono"
                      style:color={laneColour(r.lane)}
                      title="The head of {l}">{l}</span
                    >
                  {/each}
                  <span class="mono seq">{c.seq}</span>
                  <span class="mono bname" style:color={laneColour(r.lane)}>{c.branch}</span>
                  <span class="kind">{commitKindLabel(c.kind)}</span>
                  {#each stubsByRow.get(i) ?? [] as s (s.label)}
                    <span class="badge stub mono" title="Not in the graph">
                      {s.kind === 'merge' ? 'merged' : 'from'}
                      {s.label}</span
                    >
                  {/each}
                  {#if c.inserted || c.deleted}
                    <span class="mono delta"
                      ><span class="ins">+{fmtInt(c.inserted)}</span>
                      <span class="del">−{fmtInt(c.deleted)}</span></span
                    >
                  {/if}
                  {#if c.message}<span class="msg faint">{c.message}</span>{/if}
                  <span class="spacer"></span>
                  <span class="faint when" title={fmtCommitTime(c.timestamp)}
                    >{fmtRelative(c.timestamp, now)}</span
                  >
                </button>
                {#if segKey}
                  <button
                    class="btn ghost icon sm fold"
                    aria-label="Collapse the run of {c.branch}"
                    title="Collapse"
                    onclick={() => toggleSegment(segKey)}
                    ><Icon name="chevronDown" size={12} /></button
                  >
                {/if}
              {/if}
            </li>
          {/each}
        </ol>
      </div>
    </div>
    <div class="foot row">
      <span class="faint">
        {fmtInt(page.commits.length)} commits of {fmtInt(branches.length)} branch{branches.length ===
        1
          ? ''
          : 'es'}{page.next ? ', newest first' : ''}
      </span>
      <span class="spacer"></span>
      {#if page.next}
        <button class="btn sm" onclick={loadOlder} disabled={loadingOlder}>
          {#if loadingOlder}<span class="spinner"></span>{:else}<Icon
              name="chevronDown"
              size={13}
            />{/if} Load older
        </button>
      {/if}
    </div>
    {#if current}
      {@const c = current}
      <section class="detail" aria-label="Commit {c.seq} on {c.branch}">
        <div class="detail-head">
          <strong>Commit {c.seq}</strong>
          <span class="faint">on</span>
          <span class="mono">{c.branch}</span>
          <span class="faint">·</span>
          <span>{commitKindLabel(c.kind)}</span>
          <span class="faint" title={c.timestamp}>{fmtCommitTime(c.timestamp)}</span>
          {#if c.mergedFrom}
            <span class="badge merged mono"
              >from {c.mergedFrom.branch ?? 'deleted'}@{c.mergedFrom.seq}</span
            >
          {/if}
          <span class="spacer"></span>
          {#if readable(c)}
            <a class="btn ghost sm" href={resolve('/query')} onclick={(e) => queryAt(e, c)}
              ><Icon name="query" size={12} /> Query at commit {c.seq}</a
            >
          {/if}
          <button
            class="btn ghost icon sm"
            aria-label="Close the commit"
            onclick={() => (selected = null)}><Icon name="x" size={12} /></button
          >
        </div>
        {#if c.message}<p class="faint">{c.message}</p>{/if}
        {#if diffLoading}
          <p class="faint row"><span class="spinner"></span> Reading the changes…</p>
        {:else if diffError}
          <div class="error-box">
            <strong>Could not read the changes.</strong>
            <span class="muted">{api.errorMessage(diffError)}</span>
          </div>
        {:else if diff}
          <p class="faint">
            commit {diff.from.commit.seq} → commit {diff.to.commit.seq}:
            <span class="mono">{diffSummary(diff)}</span>
          </p>
          {#if diff.quads}<DiffLines quads={diff.quads} total={diff.added + diff.removed} />{/if}
        {/if}
      </section>
    {/if}
  {/if}
</div>

<style>
  .cg {
    /* lane colours: the term colours, which have light and dark variants */
    --lane-0: var(--iri);
    --lane-1: var(--literal);
    --lane-2: var(--bnode);
    --lane-3: var(--num);
    --lane-4: var(--var);
    --lane-5: var(--ok);
    --lane-6: var(--kw);
    --lane-7: var(--warn);
    border-top: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .pad {
    padding: 12px 14px;
  }
  .scroll {
    overflow: auto;
    position: relative;
  }
  .space {
    position: relative;
    min-width: 100%;
  }
  .lines {
    position: absolute;
    left: 0;
    display: block;
  }
  .shown {
    fill: var(--hover);
  }
  .edge {
    fill: none;
    stroke-width: 1.6;
  }
  .edge.merge {
    stroke-dasharray: 4 3;
  }
  .dot {
    fill: var(--surface);
    stroke-width: 2;
  }
  .dot.merge {
    stroke-width: 2.5;
  }
  .seg {
    fill: var(--surface);
    stroke-width: 1.6;
    stroke-dasharray: 2 2;
  }
  .rows {
    position: absolute;
    top: 0;
    right: 0;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .rowline {
    position: absolute;
    left: 0;
    right: 0;
    height: 28px;
    display: flex;
    align-items: center;
    gap: 2px;
    padding-right: 8px;
  }
  .node {
    all: unset;
    box-sizing: border-box;
    flex: 1;
    min-width: 0;
    height: 24px;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 0 8px;
    border-radius: var(--r-sm);
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
  }
  .node:hover {
    background: var(--hover);
  }
  .node.sel {
    background: var(--active);
  }
  .node:focus-visible {
    box-shadow: var(--focus);
  }
  .node.unreadable .seq {
    color: var(--text-3);
  }
  .seq {
    color: var(--text-2);
    min-width: 2.5em;
    text-align: right;
  }
  .bname {
    font-size: 11.5px;
  }
  .headlabel {
    height: 16px;
    font-weight: 600;
    background: var(--surface-2);
    border: 1px solid currentColor;
  }
  .stub {
    height: 16px;
    color: var(--text-2);
  }
  .delta {
    font-size: 11.5px;
  }
  .ins {
    color: var(--ok);
  }
  .del {
    color: var(--danger);
  }
  .msg {
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .when {
    font-size: 11px;
  }
  .seg-btn {
    color: var(--text-2);
    font-style: italic;
  }
  .foot {
    padding: 8px 14px;
    border-top: 1px solid var(--border);
  }
  .detail {
    border-top: 1px solid var(--border);
    padding: 8px 14px;
    display: grid;
    gap: 6px;
  }
  .detail p {
    margin: 0;
  }
  .detail-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .merged {
    background: color-mix(in srgb, var(--literal) 14%, transparent);
    color: var(--literal);
  }
</style>

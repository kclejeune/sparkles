<script lang="ts">
  // The entity frame's History view (ROADMAP item 620), in the manner of WebProtégé's
  // entity history: the commits of the branch that changed a triple naming the entity in the
  // editor's graph, newest first and one bounded page at a time. A commit opens to the
  // axioms and assertions mentioning the entity that it added and removed, and a caller who
  // may write reverts it through the editor's preview.
  import { onDestroy } from 'svelte';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { commitKindLabel, fmtCommitTime } from '$lib/commits';
  import { diffGraphHref } from '$lib/diff-graph';
  import {
    appendEntityPage,
    readEntityHistory,
    type EntityCommit,
    type EntityHistoryPage,
  } from '$lib/entity-history';
  import { fmtInt, fmtRelative } from '$lib/format';
  import { entityCommitDiff, type EntityDiff, type Selection } from '$lib/ontology';
  import { LatestRun } from '$lib/supersede';
  import Icon from './Icon.svelte';

  let {
    name,
    target,
    branch = null,
    sel,
    iri,
    readonly = true,
    refreshKey = 0,
    onrevert,
  }: {
    /** The dataset. */
    name: string;
    /** The dataset on its branch (`ds@branch`), as the API takes it. */
    target: string;
    branch?: string | null;
    /** The graph the editor shows. The history reads that graph alone, without imports. */
    sel: Selection;
    /** The entity. */
    iri: string;
    /** No revert is offered: the caller cannot write the branch, or its grants cover only some graphs. */
    readonly?: boolean;
    /** The head the editor shows. A new head reads the newest page again. */
    refreshKey?: number;
    /** Preview the revert of a commit. */
    onrevert?: (seq: number, label: string) => void;
  } = $props();

  /** Changes each lookup lists on one page. */
  const PAGE = 200;
  /** Axioms an opened commit lists on each side. */
  const AXIOMS = 100;

  let commits = $state<EntityCommit[]>([]);
  let next = $state<number | null>(null);
  let unrecorded = $state<EntityHistoryPage['unrecorded']>([]);
  let loading = $state(false);
  let error = $state<Error | null>(null);
  let open = $state<number | null>(null);
  let diffs = $state<Record<number, EntityDiff | Error>>({});
  let now = $state(Date.now());
  const runs = new LatestRun();
  onDestroy(() => runs.abortAll());

  /** Bumped when the entity, graph, branch or head changes, which orphans every pending read. */
  let epoch = 0;

  /** Read the page up to `to`, replacing the list for the first page. */
  async function load(to: number, replace: boolean) {
    const owns = runs.claim('page');
    loading = true;
    try {
      const p = await readEntityHistory((o) => api.historyChanges(target, o), iri, {
        graph: sel.graph ?? 'default',
        to,
        limit: PAGE,
        signal: runs.signal('page'),
      });
      if (!owns()) return;
      const all = appendEntityPage(
        replace ? { commits: [], next: null, unrecorded: [] } : { commits, next, unrecorded },
        p,
      );
      commits = all.commits;
      next = all.next;
      unrecorded = all.unrecorded;
      error = null;
      now = Date.now();
    } catch (e) {
      if (owns()) error = e as Error;
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    const head = refreshKey;
    void target;
    void iri;
    void sel.graph;
    epoch++;
    runs.abortAll();
    diffs = {};
    open = null;
    commits = [];
    next = null;
    unrecorded = [];
    void load(head, true);
  });

  async function toggle(c: EntityCommit) {
    if (open === c.seq) {
      open = null;
      return;
    }
    open = c.seq;
    if (diffs[c.seq]) return;
    const mine = epoch;
    const key = `diff:${c.seq}`;
    const owns = runs.claim(key);
    let d: EntityDiff | Error;
    try {
      d = await entityCommitDiff(target, sel, iri, c.seq, AXIOMS, runs.signal(key));
    } catch (e) {
      d = e as Error;
    }
    // an answer for an entity, graph, branch or head the view has left is dropped
    if (mine !== epoch || !owns()) return;
    diffs = { ...diffs, [c.seq]: d };
  }

  const title = (c: EntityCommit) => c.message || commitKindLabel(c.kind);
  const fullDiff = (c: EntityCommit) =>
    diffGraphHref(resolve('/explore'), name, String(c.seq), String(Math.max(c.seq - 1, 0)), {
      branch,
    });
  const range = (u: { from: number; to: number }) =>
    u.from === u.to ? `Commit ${u.from}` : `Commits ${u.from} to ${u.to}`;
  const REASONS: Record<string, string> = {
    bulk: 'a bulk load',
    'before-log': 'older than the change log',
    gap: 'missing from the change log',
  };
</script>

<div class="entity-history">
  {#if error}<p class="error-box" role="alert">{api.errorMessage(error)}</p>{/if}
  {#if !commits.length && !loading && !error}
    <p class="empty">No recorded commit changed this entity in this graph.</p>
  {/if}
  <ol class="commits" aria-label="Commits that changed the entity">
    {#each commits as c (c.seq)}
      {@const d = diffs[c.seq]}
      <li class:open={open === c.seq}>
        <button class="row" aria-expanded={open === c.seq} onclick={() => toggle(c)}>
          <Icon name="chevron" size={12} />
          <span class="msg">{title(c)}</span>
          <span class="mono faint small">#{c.seq}</span>
          <span
            class="faint small mono"
            title={c.partial ? 'This commit changed more triples than one page reads.' : undefined}
            >+{fmtInt(c.added)} −{fmtInt(c.removed)}{c.partial ? '+' : ''}</span
          >
          {#if c.author}<span class="faint small author">{c.author}</span>{/if}
          <span class="faint small" title={fmtCommitTime(c.timestamp)}
            >{fmtRelative(c.timestamp, now)}</span
          >
        </button>
        {#if open === c.seq}
          <div class="body">
            {#if !d}<p class="faint small"><span class="spinner"></span> Reading the axioms…</p>
            {:else if d instanceof Error}<p class="error-box" role="alert">
                {api.errorMessage(d)}
              </p>
            {:else}
              {#if !d.addedCount && !d.removedCount}
                <p class="faint small">
                  No axiom that mentions the entity changed. The commit changed only triples that no
                  axiom of this graph reads.
                </p>
              {/if}
              <ul class="axioms" aria-label="Axioms commit {c.seq} changed">
                {#each d.removed as a, i (`-${i}`)}
                  <li class="removed">
                    <span class="sign">−</span><span class="mono">{a.ofn}</span>
                  </li>
                {/each}
                {#each d.added as a, i (`+${i}`)}
                  <li class="added">
                    <span class="sign">+</span><span class="mono">{a.ofn}</span>
                  </li>
                {/each}
              </ul>
              {#if d.truncated}
                <p class="faint small">
                  Showing {fmtInt(d.added.length + d.removed.length)} of {fmtInt(
                    d.addedCount + d.removedCount,
                  )} axioms. The full diff lists every changed triple.
                </p>
              {/if}
            {/if}
            <div class="actions">
              <a class="btn ghost sm" href={fullDiff(c)}
                ><Icon name="external" size={13} /> Full diff of commit {c.seq}</a
              >
              {#if !readonly && onrevert}
                <button
                  class="btn sm"
                  title="Preview a commit that takes back everything commit {c.seq} changed on this branch"
                  onclick={() => onrevert(c.seq, `Revert commit ${c.seq}: ${title(c)}`)}
                  ><Icon name="cycle" size={13} /> Revert commit {c.seq}</button
                >
              {/if}
            </div>
          </div>
        {/if}
      </li>
    {/each}
  </ol>
  {#if next != null}
    <button class="btn ghost sm" disabled={loading} onclick={() => load(next!, false)}
      >{#if loading}<span class="spinner"></span>{/if} Older commits</button
    >
  {:else if loading}<p class="faint small"><span class="spinner"></span> Reading the history…</p>
  {:else if unrecorded.length}
    <ul class="unrecorded faint small" aria-label="Unrecorded commits">
      {#each unrecorded as u (`${u.from}-${u.to}`)}
        <li>
          {range(u)}
          {u.from === u.to ? 'is' : 'are'}
          {REASONS[u.reason] ?? u.reason}, so this list cannot say whether {u.from === u.to
            ? 'it'
            : 'they'} changed the entity.
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .entity-history {
    display: grid;
    gap: var(--sp-2);
    align-content: start;
  }
  .commits {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
  }
  .commits > li {
    border-bottom: 1px solid var(--border);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-height: var(--ctl-h);
    padding: 4px var(--sp-2);
    border: 0;
    background: none;
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }
  .row:hover {
    background: var(--hover);
  }
  .row:focus-visible {
    box-shadow: var(--focus);
  }
  .row :global(.icon) {
    flex: none;
    color: var(--text-3);
  }
  li.open .row :global(.icon) {
    transform: rotate(90deg);
  }
  .msg {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .author {
    max-width: 14ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .body {
    display: grid;
    gap: var(--sp-2);
    padding: 0 var(--sp-2) var(--sp-3) calc(var(--sp-2) + 20px);
  }
  .axioms {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 2px;
  }
  .axioms li {
    display: flex;
    gap: var(--sp-2);
    align-items: baseline;
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
  .sign {
    flex: none;
    width: 1ch;
    font-weight: 600;
  }
  .added .sign {
    color: var(--ok);
  }
  .removed .sign {
    color: var(--danger);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .unrecorded {
    margin: 0;
    padding-left: var(--sp-4);
  }
  .small {
    font-size: var(--fs-sm);
  }
  @media (max-width: 760px) {
    .row {
      flex-wrap: wrap;
    }
    .msg {
      flex-basis: calc(100% - 24px);
    }
    .body {
      padding-left: 0;
      padding-right: 0;
    }
  }
</style>

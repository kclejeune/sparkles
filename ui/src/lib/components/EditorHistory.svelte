<script lang="ts">
  // A Workbench editor's History tab (ROADMAP item 633), in the manner of WebProtégé's
  // Changes: the branch's commits newest first, each opening to what it changed as a change
  // set, with a link to the commit's full diff.
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { namesQuery, parseDefinitions } from '$lib/browse';
  import { changeIris } from '$lib/changeset';
  import { commitKindLabel, fmtCommitTime, fmtDelta } from '$lib/commits';
  import { diffGraphHref } from '$lib/diff-graph';
  import { fmtRelative } from '$lib/format';
  import type { PrefixMap } from '$lib/rdf';
  import { LatestRun } from '$lib/supersede';
  import ChangeSet from './ChangeSet.svelte';
  import Icon from './Icon.svelte';

  let {
    name,
    target,
    branch = null,
    graph = null,
    langs = [],
    prefixes = {},
    onopen,
    refreshKey = 0,
  }: {
    /** The dataset. */
    name: string;
    /** The dataset on its branch (`ds@branch`), as the API takes it. */
    target: string;
    branch?: string | null;
    /** The graph the editor shows; a commit's changes are read for it alone. */
    graph?: string | null;
    /** The caller's languages, best first, for the labels of the changes. */
    langs?: readonly string[];
    prefixes?: PrefixMap;
    /** Opens an entity in the editor. */
    onopen?: (iri: string) => void;
    /** Bump to reload the newest commits (after a write). */
    refreshKey?: number;
  } = $props();

  const PAGE = 25;
  /** Changes a commit lists at most; the full diff has the rest. */
  const CHANGES = 300;

  let commits = $state<api.Commit[]>([]);
  let next = $state<number | null>(null);
  let loading = $state(false);
  let error = $state<Error | null>(null);
  let open = $state<number | null>(null);
  let diffs = $state<Record<number, api.Diff | Error>>({});
  const runs = new LatestRun();
  let now = $state(Date.now());

  async function load(before?: number) {
    const owns = runs.claim('page');
    loading = true;
    try {
      const p = await api.commits(target, { limit: PAGE, before });
      if (!owns()) return;
      commits = before == null ? p.commits : [...commits, ...p.commits];
      const last = p.commits.at(-1);
      next = p.next && last ? last.seq : null;
      error = null;
      now = Date.now();
    } catch (e) {
      if (owns()) error = e as Error;
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    void refreshKey;
    void target;
    diffs = {};
    open = null;
    names = new Map();
    asked.clear();
    void load();
  });

  async function toggle(c: api.Commit) {
    if (open === c.seq) {
      open = null;
      return;
    }
    open = c.seq;
    if (diffs[c.seq]) return;
    try {
      const d = await api.diff(target, {
        from: String(Math.max(c.seq - 1, 0)),
        to: String(c.seq),
        quads: true,
        limit: CHANGES,
        graph: graph ?? undefined,
      });
      diffs = { ...diffs, [c.seq]: d };
      void loadNames(changeIris(d.quads ?? []));
    } catch (e) {
      diffs = { ...diffs, [c.seq]: e as Error };
    }
  }

  /** The labels of the IRIs the opened changes name, read once each. */
  let names = $state<Map<string, string>>(new Map());
  const asked = new Set<string>();
  async function loadNames(iris: string[]) {
    const todo = iris.filter((i) => !asked.has(i) && /^https?:|^urn:/.test(i)).slice(0, 300);
    if (!todo.length) return;
    for (const i of todo) asked.add(i);
    try {
      const rows = await api.select(target, namesQuery(todo, graph));
      names = new Map([...names, ...parseDefinitions(rows as never, langs)]);
    } catch {
      // the changes keep their short forms
    }
  }
  const label = (iri: string) => names.get(iri) ?? null;

  const fullDiff = (c: api.Commit) =>
    diffGraphHref(resolve('/explore'), name, String(c.seq), String(Math.max(c.seq - 1, 0)), {
      branch,
    });
</script>

<div class="history">
  {#if error}<p class="error-box" role="alert">{api.errorMessage(error)}</p>{/if}
  {#if !commits.length && !loading && !error}<p class="empty">No commits yet.</p>{/if}
  <ol class="commits" aria-label="Changes">
    {#each commits as c (c.seq)}
      {@const d = diffs[c.seq]}
      <li class:open={open === c.seq}>
        <button class="row" aria-expanded={open === c.seq} onclick={() => toggle(c)}>
          <Icon name="chevron" size={12} />
          <span class="msg">{c.message || commitKindLabel(c.kind)}</span>
          <span class="mono faint small">#{c.seq}</span>
          {#if c.inserted != null}<span class="faint small mono">{fmtDelta(c)}</span>{/if}
          <span class="faint small" title={fmtCommitTime(c.timestamp)}
            >{fmtRelative(c.timestamp, now)}</span
          >
        </button>
        {#if open === c.seq}
          <div class="body">
            {#if !d}<p class="faint small"><span class="spinner"></span> Reading the changes…</p>
            {:else if d instanceof Error}<p class="error-box" role="alert">
                {api.errorMessage(d)}
              </p>
            {:else}
              {#if !d.quads?.length}<p class="faint small">
                  {graph ? 'Nothing changed in this graph.' : 'Nothing changed.'}
                </p>{/if}
              <ChangeSet
                quads={d.quads ?? []}
                total={d.added + d.removed}
                name={label}
                {prefixes}
                {onopen}
                label="Changes of commit {c.seq}"
              />
            {/if}
            <a class="btn ghost sm full" href={fullDiff(c)}
              ><Icon name="external" size={13} /> Full diff of commit {c.seq}</a
            >
          </div>
        {/if}
      </li>
    {/each}
  </ol>
  {#if next != null}
    <button class="btn ghost sm" disabled={loading} onclick={() => load(next!)}
      >{#if loading}<span class="spinner"></span>{/if} Older commits</button
    >
  {:else if loading}<p class="faint small"><span class="spinner"></span> Reading commits…</p>{/if}
</div>

<style>
  .history {
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
  .body {
    display: grid;
    gap: var(--sp-2);
    padding: 0 var(--sp-2) var(--sp-3) calc(var(--sp-2) + 20px);
  }
  .full {
    justify-self: start;
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
  }
</style>

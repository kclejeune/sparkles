<script lang="ts">
  // The merge page (docs/specs/F09-branches-and-merges.md §2.9): the merge base and the
  // counts of merging `source` into `target`, the changes as signed N-Quads lines, and
  // the conflicts grouped by graph and subject with a choice per row or per group. The
  // merge sends the heads the page showed as `expect`, so a branch that moved meanwhile
  // makes the page read the new state rather than merge changes nobody saw.
  //
  // The page is laid out as a summary, the conflicts, the changes and an actions bar
  // whose options section takes further ways of merging (a squash, a revert) as more
  // controls next to the message.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { MAIN, mergeChanges, mergeCommands, ntShort, remainingConflicts } from '$lib/branches';
  import { fmtInt } from '$lib/format';
  import {
    cellKey,
    choiceOf,
    graphHeading,
    groupConflicts,
    groupKey,
    keepChoices,
    noChoices,
    resolutionsOf,
    resultObjects,
    TAKES,
    unresolved,
    type Choices,
    type Take,
  } from '$lib/merge-page';
  import { LatestRun } from '$lib/supersede';
  import DiffLines from '$components/DiffLines.svelte';
  import GuardReportView from '$components/GuardReportView.svelte';
  import Icon from '$components/Icon.svelte';

  /** Conflicts the page lists at most. */
  const CONFLICT_LIMIT = 1000;
  /** Changes the page lists at most. */
  const CHANGE_LIMIT = 500;

  const name = $derived(page.params.name ?? '');
  const source = $derived(page.url.searchParams.get('source')?.trim() ?? '');
  const target = $derived(page.url.searchParams.get('target')?.trim() || MAIN);
  const prefixes = $derived(app.prefixes(name));
  const datasetHref = (b: string) => {
    const path = resolve('/datasets/[name]', { name });
    return b === MAIN ? path : `${path}?branch=${encodeURIComponent(b)}`;
  };

  // --- the branches, for the source and target menus ------------------------------

  let branchList = $state<api.Branch[] | null>(null);
  $effect(() => {
    const ds = name;
    if (!ds) return;
    api.branches(ds).then(
      (l) => {
        if (ds === name) branchList = l.branches;
      },
      () => {},
    );
  });

  function choose(which: 'source' | 'target', value: string) {
    const q = new URLSearchParams(page.url.searchParams);
    q.set(which, value);
    goto(`${resolve('/datasets/[name]/merge', { name })}?${q}`, {
      replaceState: true,
      keepFocus: true,
      noScroll: true,
    });
  }

  let readOnly = $state(false);
  $effect(() => {
    api.cachedServerInfo().then(
      (s) => (readOnly = s?.readOnly === true),
      () => {},
    );
  });
  const canMerge = $derived(auth.can(name, 'write') && !readOnly);

  // --- the preview: base, counts and conflicts --------------------------------------

  let preview = $state<api.MergeOutcome | null>(null);
  let previewError = $state<string | null>(null);
  let loading = $state(false);
  /** Why the page read the branches again. */
  let note = $state<string | null>(null);
  let choices = $state<Choices>(noChoices());
  let onConflict = $state<'fail' | 'ours' | 'theirs' | 'union'>('fail');
  let message = $state('');
  const runs = new LatestRun();

  async function load() {
    if (!name || !source) return;
    const owns = runs.claim('preview');
    loading = true;
    try {
      const p = await api.mergePreview(name, source, target, undefined, CONFLICT_LIMIT);
      if (!owns()) return;
      preview = p;
      previewError = null;
      choices = keepChoices(p.cells ?? [], choices);
    } catch (e) {
      if (!owns()) return;
      preview = null;
      previewError = api.errorMessage(e);
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    void name;
    void source;
    void target;
    untrack(() => {
      preview = null;
      choices = noChoices();
      onConflict = 'fail';
      note = null;
      refused = null;
      error = null;
      void load();
    });
  });

  const cells = $derived(preview?.cells ?? []);
  const total = $derived(preview ? remainingConflicts(preview) : 0);
  const groups = $derived(groupConflicts(cells));
  const open = $derived(unresolved(cells, total, choices, onConflict));
  const resolutions = $derived(resolutionsOf(cells, choices));
  const expect = $derived(
    preview ? { source: preview.source.seq, target: preview.target.seq } : undefined,
  );

  function setGroup(key: string, take: Take | '') {
    const g = { ...choices.groups };
    if (take) g[key] = take;
    else delete g[key];
    // a group's choice replaces its rows' own
    const c = { ...choices.cells };
    for (const cell of cells)
      if (groupKey(cell.graph ?? null, cell.subject) === key) delete c[cellKey(cell)];
    choices = { groups: g, cells: c };
  }

  function setCell(cell: api.ConflictCell, take: Take) {
    choices = { ...choices, cells: { ...choices.cells, [cellKey(cell)]: take } };
  }

  /** The same choice for every listed conflict. */
  function chooseAll(take: Take) {
    const g: Record<string, Take> = {};
    for (const gr of groups) for (const s of gr.subjects) g[s.key] = take;
    choices = { groups: g, cells: {} };
  }

  // --- the changes: a dry run with the choices made so far ---------------------------

  let dry = $state<api.MergeDryRun | null>(null);
  let dryError = $state<string | null>(null);
  let dryLoading = $state(false);

  async function dryRun() {
    if (!preview || preview.upToDate) return;
    const owns = runs.claim('dry');
    dryLoading = true;
    try {
      const d = await api.mergeDryRun(name, {
        source,
        target,
        resolutions: resolutions.length ? resolutions : undefined,
        // conflicts without a choice keep the target's values in the list
        onConflict: onConflict !== 'fail' ? onConflict : open ? 'ours' : undefined,
        expect,
        changes: CHANGE_LIMIT,
      });
      if (!owns()) return;
      dry = d;
      dryError = null;
    } catch (e) {
      if (!owns()) return;
      dry = null;
      dryError = api.errorMessage(e);
    } finally {
      if (owns()) dryLoading = false;
    }
  }

  // read the changes again a moment after the choices change
  $effect(() => {
    void preview;
    void resolutions;
    void onConflict;
    if (!canMerge) return;
    const t = setTimeout(() => untrack(() => void dryRun()), 250);
    return () => clearTimeout(t);
  });

  // --- merging ------------------------------------------------------------------------

  let merging = $state(false);
  let error = $state<string | null>(null);
  /** The target's validation refused the merge: its report. */
  let refused = $state<api.GuardReport | null>(null);

  const ready = $derived(
    !!preview && !preview.upToDate && open === 0 && !loading && !merging && canMerge,
  );

  async function merge() {
    if (!preview || !ready) return;
    merging = true;
    error = null;
    refused = null;
    note = null;
    try {
      const r = await api.merge(name, {
        source,
        target,
        resolutions: resolutions.length ? resolutions : undefined,
        onConflict: onConflict !== 'fail' ? onConflict : undefined,
        expect,
        message: message.trim() || undefined,
      });
      if (r.upToDate) {
        preview = r;
        return;
      }
      toasts.push(
        'success',
        `Merged ${source} into ${target}`,
        r.commit ? `commit ${r.commit.seq} · ${mergeChanges(r)}` : mergeChanges(r),
      );
      goto(datasetHref(target));
    } catch (e) {
      if (e instanceof api.ApiError && e.code === 'head-moved') {
        note = `${source} or ${target} changed since the page read them. Check the conflicts and the changes again, then merge.`;
        void load();
      } else if (e instanceof api.ApiError && e.status === 422) {
        refused = (e.body?.validation as api.GuardReport | undefined) ?? {};
        error = api.errorMessage(e);
      } else if (e instanceof api.ApiError && e.code === 'merge-conflict' && e.body) {
        note = 'Some conflicts are still open. The page read them again.';
        void load();
      } else error = api.errorMessage(e);
    } finally {
      merging = false;
    }
  }

  const commands = $derived(
    mergeCommands({ server: location.origin, dataset: name, source, target }),
  );
  const branchNames = $derived(branchList?.map((b) => b.name) ?? []);
</script>

<svelte:head><title>Merge {source} into {target} | Sparkles</title></svelte:head>

<div class="page">
  <nav class="crumbs">
    <a href={resolve('/datasets')}>Datasets</a><Icon name="chevron" size={12} /><a
      href={resolve('/datasets/[name]', { name })}>{name}</a
    ><Icon name="chevron" size={12} /><span>Merge</span>
  </nav>

  <header class="head">
    <h1><Icon name="merge" size={18} /> Merge</h1>
    <label class="pick">
      <span class="faint">Source</span>
      <select
        class="select sm mono"
        aria-label="Source branch"
        value={source}
        onchange={(e) => choose('source', e.currentTarget.value)}
      >
        {#if !branchNames.includes(source)}<option value={source}>{source || '—'}</option>{/if}
        {#each branchNames as b (b)}<option value={b} disabled={b === target}>{b}</option>{/each}
      </select>
    </label>
    <Icon name="chevron" size={14} />
    <label class="pick">
      <span class="faint">Target</span>
      <select
        class="select sm mono"
        aria-label="Target branch"
        value={target}
        onchange={(e) => choose('target', e.currentTarget.value)}
      >
        {#if !branchNames.includes(target)}<option value={target}>{target}</option>{/if}
        {#each branchNames as b (b)}<option value={b} disabled={b === source}>{b}</option>{/each}
      </select>
    </label>
    <span class="spacer"></span>
    <button class="btn sm ghost" onclick={load} disabled={loading} aria-label="Compare again">
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={13} />{/if} Compare
      again
    </button>
  </header>

  {#if !source}
    <p class="faint">Choose the branch to merge.</p>
  {:else if previewError}
    <div class="error-box">
      <strong>Could not compare {source} with {target}.</strong>
      <span class="muted">{previewError}</span>
    </div>
  {:else if !preview}
    <p class="faint row"><span class="spinner"></span> Comparing the branches…</p>
  {:else}
    {@const p = preview}
    {#if note}<p class="moved" role="status"><Icon name="info" size={13} /> {note}</p>{/if}

    <section class="panel" aria-labelledby="summary-title">
      <div class="panel-head"><h2 id="summary-title">Summary</h2></div>
      <div class="panel-body">
        {#if p.upToDate}
          <p class="uptodate">
            <Icon name="check" size={14} />
            {target} already has every change of {source}. There is nothing to merge.
          </p>
        {/if}
        <dl class="facts">
          <div>
            <dt>Source</dt>
            <dd class="mono">{p.source.branch}@{p.source.seq}</dd>
          </div>
          <div>
            <dt>Target</dt>
            <dd class="mono">{p.target.branch}@{p.target.seq}</dd>
          </div>
          <div>
            <dt>Merge base</dt>
            <dd class="mono">{p.base ? `${p.base.branch}@${p.base.seq}` : 'none'}</dd>
          </div>
          <div>
            <dt>Changes</dt>
            <dd class="mono">{mergeChanges(dry?.merge ?? p)}</dd>
          </div>
          <div>
            <dt>Conflicts</dt>
            <dd>
              {fmtInt(total)}{#if total}<span class="faint"
                  >, {fmtInt(total - open)} with a choice</span
                >{/if}
            </dd>
          </div>
        </dl>
        {#if p.fastForward && !total && !p.upToDate}
          <p class="faint">
            {target} has not changed since {source} started, so this is a fast-forward.
          </p>
        {/if}
      </div>
    </section>

    {#if total}
      <section class="panel conflicts" aria-labelledby="conflicts-title">
        <div class="panel-head">
          <h2 id="conflicts-title">Conflicts</h2>
          <span class="badge danger">{fmtInt(total)}</span>
          <span class="spacer"></span>
          <span class="faint">Take for all:</span>
          {#each TAKES as t (t.take)}
            <button class="btn ghost sm" title={t.title} onclick={() => chooseAll(t.take)}
              >{t.label}</button
            >
          {/each}
        </div>
        <p class="lead">
          Both branches changed these values differently. Choose a side for each row, or for a
          subject's rows at once. <strong>Ours</strong> is {target}, and
          <strong>theirs</strong> is {source}.
        </p>
        <div class="cells">
          <table class="data">
            <thead>
              <tr>
                <th>Predicate</th>
                <th>Base</th>
                <th>Ours <span class="faint mono">({target})</span></th>
                <th>Theirs <span class="faint mono">({source})</span></th>
                <th>Take</th>
              </tr>
            </thead>
            {#each groups as g (g.graph ?? '')}
              <tbody>
                <tr class="graph-row">
                  <th colspan="5" scope="rowgroup">
                    {graphHeading(g.graph, prefixes)}
                    <span class="faint">· {fmtInt(g.count)} conflict{g.count === 1 ? '' : 's'}</span
                    >
                  </th>
                </tr>
                {#each g.subjects as s (s.key)}
                  <tr class="subject-row">
                    <th colspan="4" scope="rowgroup" class="mono">{ntShort(s.subject, prefixes)}</th
                    >
                    <td>
                      <select
                        class="select sm"
                        aria-label="Take for {ntShort(s.subject, prefixes)}"
                        value={choices.groups[s.key] ?? ''}
                        onchange={(e) => setGroup(s.key, e.currentTarget.value as Take | '')}
                      >
                        <option value="">Per row</option>
                        {#each TAKES as t (t.take)}<option value={t.take}>{t.label}</option>{/each}
                      </select>
                    </td>
                  </tr>
                  {#each s.cells as c (cellKey(c))}
                    {@const take = choiceOf(c, choices)}
                    {@const result = resultObjects(c, take)}
                    <tr class="cell" class:open={!take}>
                      <td class="mono pred">{c.predicate ? ntShort(c.predicate, prefixes) : '—'}</td
                      >
                      {#each [c.base, c.ours, c.theirs] as side, j (j)}
                        <td
                          class="mono"
                          class:taken={(take === 'base' && j === 0) ||
                            (take === 'ours' && j === 1) ||
                            (take === 'theirs' && j === 2) ||
                            (take === 'union' && j > 0)}
                          >{#each side as o, k (k)}<div>{ntShort(o, prefixes)}</div>{:else}<span
                              class="faint">none</span
                            >{/each}</td
                        >
                      {/each}
                      <td>
                        <div
                          class="takes"
                          role="radiogroup"
                          aria-label="Take for {ntShort(c.subject, prefixes)} {c.predicate
                            ? ntShort(c.predicate, prefixes)
                            : ''}"
                        >
                          {#each TAKES as t (t.take)}
                            <button
                              class="take"
                              role="radio"
                              aria-checked={take === t.take}
                              title={t.title}
                              onclick={() => setCell(c, t.take)}>{t.label}</button
                            >
                          {/each}
                        </div>
                        {#if result}
                          <div class="faint result" title="The values after the merge">
                            → {result.length
                              ? result.map((o) => ntShort(o, prefixes)).join(', ')
                              : 'none'}
                          </div>
                        {/if}
                      </td>
                    </tr>
                  {/each}
                {/each}
              </tbody>
            {/each}
          </table>
        </div>
        {#if cells.length < total}
          <p class="lead faint">
            The page lists {fmtInt(cells.length)} of {fmtInt(total)} conflicts. Choose a rule for the
            others below.
          </p>
        {/if}
        <div class="rule">
          <label>
            <span>Every conflict without a choice</span>
            <select class="select sm" aria-label="Every other conflict" bind:value={onConflict}>
              <option value="fail">stays open</option>
              <option value="ours">takes ours</option>
              <option value="theirs">takes theirs</option>
              <option value="union">keeps both</option>
            </select>
          </label>
          {#if open}
            <span class="warn-text" role="status"
              >{fmtInt(open)} conflict{open === 1 ? '' : 's'} still open</span
            >
          {:else}
            <span class="ok-text" role="status"
              ><Icon name="check" size={13} /> Every conflict has a choice</span
            >
          {/if}
        </div>
      </section>
    {/if}

    {#if !p.upToDate}
      <section class="panel" aria-labelledby="changes-title">
        <div class="panel-head">
          <h2 id="changes-title">Changes</h2>
          {#if dry?.changes}<span class="badge">{fmtInt(dry.changes.total)}</span>{/if}
          {#if dryLoading}<span class="spinner"></span>{/if}
        </div>
        <div class="panel-body changes">
          {#if !canMerge}
            <p class="faint">
              Merging into {target} needs write access on it, so the page cannot list the changes.
            </p>
          {:else if dryError}
            <div class="error-box">
              <strong>Could not compute the changes.</strong>
              <span class="muted">{dryError}</span>
            </div>
          {:else if dry}
            {#if open && onConflict === 'fail'}
              <p class="faint">Conflicts without a choice keep {target}'s values in this list.</p>
            {/if}
            {#if dry.outcome === 'rejected' && dry.validation}
              <div class="refused" role="alert">
                <p>
                  <Icon name="alert" size={14} />
                  <strong>{target}'s validation would refuse this result.</strong>
                </p>
                <GuardReportView report={dry.validation} {prefixes} />
              </div>
            {/if}
            {#if dry.changes?.quads.length}
              <DiffLines
                quads={dry.changes.quads}
                total={dry.changes.total}
                label="Changes of the merge"
                maxHeight={360}
              />
            {:else}
              <p class="faint">The merge changes no quads.</p>
            {/if}
          {:else}
            <p class="faint row"><span class="spinner"></span> Computing the changes…</p>
          {/if}
        </div>
      </section>

      <section class="panel actions" aria-label="Merge">
        <div class="panel-body">
          {#if refused}
            <div class="refused" role="alert">
              <p>
                <Icon name="alert" size={14} />
                <strong>{target}'s validation refused the merge. Nothing was written.</strong>
              </p>
              <GuardReportView report={refused} {prefixes} />
            </div>
          {:else if error}
            <div class="error-box" role="alert">
              <strong>Could not merge.</strong>
              <span class="muted">{error}</span>
            </div>
          {/if}
          <!-- the ways of merging: squash and revert controls join the message here -->
          <div class="options">
            <label class="message">
              <span class="faint">Message</span>
              <input
                class="input sm"
                placeholder="merge {source} (commit {p.source.seq}) into {target}"
                bind:value={message}
              />
            </label>
          </div>
          <div class="buttons">
            <a class="btn ghost" href={datasetHref(target)}>Cancel</a>
            <button class="btn primary" onclick={merge} disabled={!ready}>
              {#if merging}<span class="spinner"></span>{:else}<Icon name="merge" size={14} />{/if}
              Merge into {target}
            </button>
          </div>
          {#if !canMerge}
            <p class="faint">You need write access on {target} to merge into it.</p>
          {:else if open}
            <p class="faint">Choose a side for every conflict to merge.</p>
          {/if}
          {#if total}
            <details class="cli">
              <summary class="faint">The same from the command line</summary>
              {#each commands as cmd (cmd)}<pre class="mono">{cmd}</pre>{/each}
            </details>
          {/if}
        </div>
      </section>
    {/if}
  {/if}
</div>

<style>
  .page {
    padding: 18px 28px 40px;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 16px;
    max-width: 1320px;
    width: 100%;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .crumbs a {
    color: var(--text-2);
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
  }
  .head h1 {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 22px;
    margin: 0;
  }
  .pick {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
  }
  .pick .select {
    max-width: 200px;
  }
  .moved {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    margin: 0;
    color: var(--warn);
    font-size: var(--fs-sm);
  }
  .uptodate {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 0 8px;
    color: var(--ok);
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 28px;
    margin: 0;
    font-size: var(--fs-sm);
  }
  .facts dt {
    color: var(--text-2);
    font-size: var(--fs-xs);
  }
  .facts dd {
    margin: 2px 0 0;
  }
  .panel-body > p {
    margin: 8px 0 0;
    font-size: var(--fs-sm);
  }
  .lead {
    margin: 0;
    padding: 8px 14px;
    font-size: var(--fs-sm);
    border-top: 1px solid var(--border);
  }
  .cells {
    overflow: auto;
    max-height: 560px;
    border-top: 1px solid var(--border);
  }
  .cells table {
    font-size: 12px;
  }
  .cells td {
    vertical-align: top;
    overflow-wrap: anywhere;
    min-width: 90px;
  }
  .graph-row th {
    background: var(--surface-2);
    text-align: left;
    font-weight: 600;
  }
  .subject-row th {
    text-align: left;
    font-weight: 600;
    color: var(--iri);
  }
  .subject-row td {
    min-width: 0;
  }
  .cell .pred {
    padding-left: 22px;
  }
  .cell.open .pred {
    box-shadow: inset 3px 0 0 var(--warn);
  }
  td.taken {
    background: var(--ok-soft);
  }
  .takes {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    overflow: hidden;
  }
  .take {
    all: unset;
    cursor: pointer;
    padding: 2px 7px;
    font-size: 11.5px;
    color: var(--text-2);
  }
  .take + .take {
    border-left: 1px solid var(--border);
  }
  .take[aria-checked='true'] {
    background: var(--iri);
    color: var(--surface);
  }
  .take:focus-visible {
    box-shadow: var(--focus);
  }
  .result {
    margin-top: 3px;
    font-size: 11px;
  }
  .rule {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 16px;
    padding: 10px 14px;
    border-top: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .rule label {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .warn-text {
    color: var(--warn);
  }
  .ok-text {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--ok);
  }
  .changes {
    display: grid;
    gap: 8px;
    font-size: var(--fs-sm);
  }
  .changes p {
    margin: 0;
  }
  .refused {
    display: grid;
    gap: 8px;
    padding: 10px 12px;
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    border-radius: var(--r);
    background: var(--danger-soft);
    font-size: var(--fs-sm);
  }
  .refused > p {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    color: var(--danger);
  }
  .actions .panel-body {
    display: grid;
    gap: 10px;
  }
  .options {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 16px;
    align-items: flex-end;
  }
  .message {
    display: grid;
    gap: 3px;
    flex: 1 1 280px;
    font-size: var(--fs-sm);
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: flex-end;
  }
  .actions p {
    margin: 0;
    font-size: var(--fs-sm);
  }
  .cli summary {
    cursor: pointer;
    font-size: var(--fs-sm);
  }
  .cli pre {
    margin: 6px 0 0;
    padding: 6px 8px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: 11.5px;
    background: var(--surface-2);
    border-radius: var(--r);
  }
  @media (max-width: 760px) {
    .page {
      padding: 14px 16px 32px;
    }
  }
</style>

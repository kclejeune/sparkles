<script lang="ts">
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import {
    cellPlace,
    MAIN,
    mergeChanges,
    mergeCommands,
    ntShort,
    remainingConflicts,
  } from '$lib/branches';
  import { fmtInt } from '$lib/format';
  import { LatestRun } from '$lib/supersede';
  import { untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  let {
    open = $bindable(false),
    name,
    source,
    branches,
    onmerged,
  }: {
    open?: boolean;
    /** The dataset. */
    name: string;
    /** The branch to merge. */
    source: string;
    /** The dataset's branches, for the targets. */
    branches: api.Branch[];
    /** Called after a merge committed. */
    onmerged?: (r: api.MergeOutcome) => void;
  } = $props();

  let target = $state(MAIN);
  let preview = $state<api.MergeOutcome | null>(null);
  let error = $state<string | null>(null);
  /** A merge refused because of conflicts (the report of `POST`). */
  let refused = $state<api.MergeOutcome | null>(null);
  let note = $state<string | null>(null);
  let loading = $state(false);
  let merging = $state(false);
  const runs = new LatestRun();

  const targets = $derived(branches.filter((b) => b.name !== source));
  const prefixes = $derived(app.prefixes(name));

  // a new source starts at its upstream
  $effect(() => {
    if (!open) return;
    const from = source;
    untrack(() => {
      target = branches.find((b) => b.name === from)?.upstream ?? MAIN;
      note = null;
    });
  });

  async function load() {
    const owns = runs.claim('preview');
    loading = true;
    refused = null;
    try {
      const p = await api.mergePreview(name, source, target);
      if (!owns()) return;
      preview = p;
      error = null;
    } catch (e) {
      if (!owns()) return;
      preview = null;
      error = api.errorMessage(e);
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    if (open && source && target) void load();
  });

  /** The report on show: a refused merge's, or the preview's when it has conflicts. */
  const report = $derived(refused ?? (preview && remainingConflicts(preview) > 0 ? preview : null));
  const conflicts = $derived(report ? remainingConflicts(report) : 0);
  const commands = $derived(
    mergeCommands({ server: location.origin, dataset: name, source, target }),
  );
  const canMerge = $derived(!!preview && !preview.upToDate && !report && !loading && !merging);

  async function merge() {
    if (!preview) return;
    merging = true;
    note = null;
    try {
      const r = await api.merge(name, {
        source,
        target,
        expect: { source: preview.source.seq, target: preview.target.seq },
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
      open = false;
      onmerged?.(r);
    } catch (e) {
      if (e instanceof api.ApiError && e.code === 'merge-conflict' && e.body)
        refused = e.body as api.MergeOutcome;
      else if (e instanceof api.ApiError && e.code === 'head-moved') {
        note = `${source} or ${target} changed since the preview. Check the new preview and merge again.`;
        void load();
      } else error = api.errorMessage(e);
    } finally {
      merging = false;
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toasts.push('success', 'Copied the command', undefined, 1400);
    } catch (e) {
      toasts.error('Could not copy', e);
    }
  }
</script>

<Modal bind:open title="Merge {source}" width={640}>
  <div class="merge">
    <label class="target">
      <span>Merge <strong class="mono">{source}</strong> into</span>
      <select class="select sm mono" bind:value={target} aria-label="Target branch">
        {#each targets as b (b.name)}<option value={b.name}>{b.name}</option>{/each}
      </select>
    </label>

    {#if loading && !preview}
      <p class="faint row"><span class="spinner"></span> Comparing the branches…</p>
    {:else if error}
      <div class="error-box">
        <strong>Could not merge.</strong>
        <span class="muted">{error}</span>
      </div>
    {:else if preview}
      {@const p = report ?? preview}
      {#if note}<p class="moved"><Icon name="info" size={13} /> {note}</p>{/if}
      {#if preview.upToDate && !refused}
        <p class="uptodate">
          <Icon name="check" size={14} />
          {target} already has every change of {source}. There is nothing to merge.
        </p>
      {:else}
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
          {#if preview.changes}
            <div>
              <dt>Changes</dt>
              <dd class="mono">{mergeChanges(preview)}</dd>
            </div>
          {/if}
        </dl>
        {#if preview.fastForward && !report}
          <p class="faint">
            {target} has not changed since {source} started, so this is a fast-forward.
          </p>
        {/if}
      {/if}

      {#if report}
        <div class="conflicts" role="alert">
          <p>
            <Icon name="alert" size={14} />
            <strong>{fmtInt(conflicts)} conflict{conflicts === 1 ? '' : 's'}.</strong>
            Both branches changed these values differently, so nothing is merged.
          </p>
          <div class="cells">
            <table class="data">
              <thead>
                <tr>
                  <th>Where</th>
                  <th>Base</th>
                  <th>Ours <span class="faint mono">({target})</span></th>
                  <th>Theirs <span class="faint mono">({source})</span></th>
                </tr>
              </thead>
              <tbody>
                {#each report.cells ?? [] as c, i (i)}
                  <tr>
                    <td class="mono place">{cellPlace(c, prefixes)}</td>
                    <td class="mono"
                      >{#each c.base as o, j (j)}<div>{ntShort(o, prefixes)}</div>{:else}<span
                          class="faint">none</span
                        >{/each}</td
                    >
                    <td class="mono"
                      >{#each c.ours as o, j (j)}<div>{ntShort(o, prefixes)}</div>{:else}<span
                          class="faint">none</span
                        >{/each}</td
                    >
                    <td class="mono"
                      >{#each c.theirs as o, j (j)}<div>{ntShort(o, prefixes)}</div>{:else}<span
                          class="faint">none</span
                        >{/each}</td
                    >
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
          {#if report.truncated}
            <p class="faint">
              The list shows the first {fmtInt(report.cells?.length ?? 0)} conflicts.
            </p>
          {/if}
          <p>
            Resolve them from the command line, taking every value from {source}, or with a file of
            resolutions:
          </p>
          {#each commands as cmd (cmd)}
            <div class="cmd">
              <pre class="mono">{cmd}</pre>
              <button
                class="btn ghost icon sm"
                aria-label="Copy the command"
                title="Copy"
                onclick={() => copy(cmd)}><Icon name="copy" size={12} /></button
              >
            </div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>
  {#snippet actions()}
    <button class="btn" onclick={() => (open = false)}>{canMerge ? 'Cancel' : 'Close'}</button>
    {#if canMerge}
      <button class="btn primary" onclick={merge} disabled={merging}>
        {#if merging}<span class="spinner"></span>{:else}<Icon name="merge" size={14} />{/if}
        Merge into {target}
      </button>
    {/if}
  {/snippet}
</Modal>

<style>
  .merge {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
    font-size: var(--fs-sm);
  }
  .merge p {
    margin: 0;
  }
  .target {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .target .select {
    max-width: 100%;
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 20px;
    margin: 0;
  }
  .facts dt {
    color: var(--text-2);
    font-size: var(--fs-xs);
  }
  .facts dd {
    margin: 2px 0 0;
  }
  .uptodate,
  .moved {
    display: flex;
    align-items: flex-start;
    gap: 6px;
  }
  .uptodate {
    color: var(--ok);
  }
  .moved {
    color: var(--warn);
  }
  .conflicts {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 8px;
  }
  .conflicts > p:first-child {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    color: var(--danger);
  }
  .cells {
    max-height: 260px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--r);
  }
  .cells table {
    font-size: 12px;
  }
  .cells td {
    vertical-align: top;
    overflow-wrap: anywhere;
    min-width: 90px;
  }
  .cells td.place {
    min-width: 140px;
  }
  .cmd {
    display: flex;
    align-items: flex-start;
    gap: 6px;
  }
  .cmd pre {
    flex: 1;
    min-width: 0;
    margin: 0;
    padding: 6px 8px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: 11.5px;
    background: var(--surface-2);
    border-radius: var(--r);
  }
</style>

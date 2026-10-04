<script lang="ts">
  // The Branches panel's Merge button: a preview of the merge, and the merge when there are
  // no conflicts. A merge with conflicts opens the merge page, which resolves them.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { MAIN, mergeChanges, remainingConflicts } from '$lib/branches';
  import { mergeHref } from '$lib/merge-page';
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
  let note = $state<string | null>(null);
  let loading = $state(false);
  let merging = $state(false);
  const runs = new LatestRun();

  const targets = $derived(branches.filter((b) => b.name !== source));

  /** Resolve the conflicts on the merge page. */
  function openPage() {
    open = false;
    goto(mergeHref(resolve('/datasets/[name]', { name }), source, target));
  }

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
    try {
      const p = await api.mergePreview(name, source, target);
      if (!owns()) return;
      preview = p;
      error = null;
      if (remainingConflicts(p) > 0) openPage();
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

  const conflicts = $derived(preview ? remainingConflicts(preview) > 0 : false);
  const canMerge = $derived(!!preview && !preview.upToDate && !conflicts && !loading && !merging);

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
      if (e instanceof api.ApiError && e.code === 'merge-conflict') openPage();
      else if (e instanceof api.ApiError && e.code === 'head-moved') {
        note = `${source} or ${target} changed since the preview. Check the new preview and merge again.`;
        void load();
      } else error = api.errorMessage(e);
    } finally {
      merging = false;
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
      {@const p = preview}
      {#if note}<p class="moved"><Icon name="info" size={13} /> {note}</p>{/if}
      {#if conflicts}
        <p class="faint row"><span class="spinner"></span> Opening the merge page…</p>
      {:else if preview.upToDate}
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
        {#if preview.fastForward}
          <p class="faint">
            {target} has not changed since {source} started, so this is a fast-forward.
          </p>
        {/if}
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
</style>

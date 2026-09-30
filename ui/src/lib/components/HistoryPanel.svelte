<script lang="ts">
  import { onMount } from 'svelte';
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
  import { LatestRun } from '$lib/supersede';
  import Icon from './Icon.svelte';

  let {
    name,
    info,
    refreshKey = 0,
  }: {
    name: string;
    info: api.DatasetInfo | undefined;
    /** Bump to reload the newest commits (after a write). */
    refreshKey?: number;
  } = $props();

  const PAGE = 20;

  let page = $state<api.CommitPage | null>(null);
  let shown = $state<api.Commit[]>([]);
  let error = $state<api.ApiError | Error | null>(null);
  let loading = $state(false);
  let loadingOlder = $state(false);
  let now = $state(Date.now());
  // a reload (refresh, other dataset) supersedes pages still loading
  const runs = new LatestRun();

  async function reload() {
    const owns = runs.claim('history');
    loading = true;
    try {
      const p = await api.commits(name, { limit: PAGE });
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
      const p = await api.commits(name, { before, limit: PAGE });
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
    void name;
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

  async function copyId(id: string) {
    try {
      await navigator.clipboard.writeText(id);
      toasts.push('success', 'Copied dataset id', undefined, 1400);
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
    <button
      class="btn ghost icon sm"
      title="Reload"
      aria-label="Reload history"
      onclick={reload}
      disabled={loading}
    >
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={13} />{/if}
    </button>
  </div>
  <div class="panel-body ids">
    {#if info?.id || page?.datasetId}
      {@const id = page?.datasetId ?? info?.id ?? ''}
      <div class="idline">
        <span class="faint">Dataset id</span>
        <button class="id mono" title="Copy the dataset id" onclick={() => copyId(id)}
          >{id} <Icon name="copy" size={11} /></button
        >
      </div>
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
            </tr>
          </thead>
          <tbody>
            {#each shown as c (c.seq)}
              <tr class:head={c.seq === page.head}>
                <td class="num mono seq" title={c.ref}>{c.seq}</td>
                <td
                  ><div class="kind">
                    <span>{commitKindLabel(c.kind)}</span>
                    {#each commitFlags(c) as f (f.label)}
                      <span class="badge flag" class:warn={f.warn} title={f.title}>{f.label}</span>
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
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
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
</style>

<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import * as b from '$lib/backups';
  import { fmtCommitAge, fmtDedup } from '$lib/backups-format';
  import { fmtBytes, fmtRelative, fmtTime } from '$lib/format';
  import { LatestRun } from '$lib/supersede';
  import BackupDialogs from './BackupDialogs.svelte';
  import Icon from './Icon.svelte';

  let {
    repositories,
    admin,
    readOnly = false,
    dataset = $bindable(''),
    refreshKey = 0,
  }: {
    repositories: (b.Repository | b.RepositoryBrief)[];
    /** Server admin: every repository's listing; others see their datasets' backups. */
    admin: boolean;
    readOnly?: boolean;
    /** The dataset filter (kept in the URL by the page). */
    dataset?: string;
    refreshKey?: number;
  } = $props();

  const PAGE = 100;

  let backups = $state<b.BackupSummary[]>([]);
  let problems = $state<string[]>([]);
  let loading = $state(false);
  let loaded = $state(false);
  let repo = $state('');
  let policy = $state('');
  let search = $state('');
  let shown = $state(PAGE);
  let now = $state(Date.now());
  let dialogs: BackupDialogs | undefined = $state();
  const runs = new LatestRun();

  async function load() {
    const owns = runs.claim('backups');
    loading = true;
    const errors: string[] = [];
    let all: b.BackupSummary[] = [];
    if (admin) {
      const repos = repositories.filter(b.isFull);
      for (const r of repos.filter((r) => !r.status.reachable))
        errors.push(`${r.name} is unreachable${r.status.error ? `: ${r.status.error}` : ''}`);
      const got = await Promise.allSettled(
        repos.filter((r) => r.status.reachable).map((r) => b.allRepositoryBackups(r.name)),
      );
      got.forEach((g, i) => {
        if (g.status === 'fulfilled') all.push(...g.value);
        else errors.push(`${repos[i].name}: ${api.errorMessage(g.reason)}`);
      });
    } else {
      const mine = app.datasets.filter((d) => auth.can(d.name, 'admin')).map((d) => d.name);
      const got = await Promise.allSettled(mine.map((ds) => b.datasetBackups(ds)));
      const seen = new Set<string>();
      got.forEach((g, i) => {
        if (g.status === 'rejected') errors.push(`${mine[i]}: ${api.errorMessage(g.reason)}`);
        else
          for (const x of g.value.backups) {
            const key = `${x.repository}/${x.name}`;
            if (!seen.has(key)) all.push(x);
            seen.add(key);
          }
      });
    }
    if (!owns()) return;
    all = all.sort((x, y) => y.completed.localeCompare(x.completed));
    backups = all;
    problems = errors;
    loading = false;
    loaded = true;
    now = Date.now();
  }

  // reload when asked, or when a repository comes, goes or changes reachability (not on
  // every poll of the repository list)
  const repoKey = $derived(repositories.map((r) => `${r.name}:${b.reachable(r)}`).join());
  $effect(() => {
    void refreshKey;
    void repoKey;
    untrack(() => void load());
  });

  onMount(() => {
    const t = setInterval(() => (now = Date.now()), 30_000);
    return () => clearInterval(t);
  });

  const repoNames = $derived([...new Set(backups.map((x) => x.repository))].sort());
  const dsNames = $derived([...new Set(backups.map((x) => x.dataset.name))].sort());
  const policies = $derived(
    [...new Set(backups.flatMap((x) => (x.policy ? [x.policy] : [])))].sort(),
  );
  const readonlyRepos = $derived(repositories.filter(b.isReadonly).map((r) => r.name));

  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    return backups.filter(
      (x) =>
        (!repo || x.repository === repo) &&
        (!dataset || x.dataset.name === dataset) &&
        (!policy || (policy === '-' ? !x.policy : x.policy === policy)) &&
        (!q ||
          x.name.toLowerCase().includes(q) ||
          (x.note ?? '').toLowerCase().includes(q) ||
          x.dataset.name.toLowerCase().includes(q)),
    );
  });

  /** How the backup's dataset relates to the live one of that name. */
  function lineage(x: b.BackupSummary): { label: string; title: string } | null {
    const live = app.datasets.find((d) => d.name === x.dataset.name);
    if (!live)
      return { label: 'not on this server', title: 'No dataset of this name is served here' };
    if (live.id && live.id !== x.dataset.id)
      return {
        label: 'other lineage',
        title: `Dataset id ${x.dataset.id}; the live ${live.name} is ${live.id}`,
      };
    return null;
  }

  const canAdmin = (x: b.BackupSummary) => auth.can(x.dataset.name, 'admin');
</script>

<div class="bar">
  <label class="search">
    <Icon name="search" size={13} />
    <input
      class="input"
      type="search"
      bind:value={search}
      placeholder="Search names and notes"
      aria-label="Search backups"
    />
  </label>
  <select class="select" bind:value={repo} aria-label="Repository">
    <option value="">All repositories</option>
    {#each repoNames as r (r)}<option value={r}>{r}</option>{/each}
  </select>
  <select class="select" bind:value={dataset} aria-label="Dataset">
    <option value="">All datasets</option>
    {#each dsNames as d (d)}<option value={d}>{d}</option>{/each}
    {#if dataset && !dsNames.includes(dataset)}<option value={dataset}>{dataset}</option>{/if}
  </select>
  <select class="select" bind:value={policy} aria-label="Policy">
    <option value="">Any origin</option>
    <option value="-">Made by hand</option>
    {#each policies as p (p)}<option value={p}>Policy {p}</option>{/each}
  </select>
  <span class="spacer"></span>
  <span class="faint small">{filtered.length} of {backups.length}</span>
  <button class="btn ghost icon sm" onclick={load} disabled={loading} aria-label="Reload backups">
    {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={13} />{/if}
  </button>
</div>

{#each problems as p (p)}
  <p class="problem"><Icon name="alert" size={13} /> {p}</p>
{/each}

<section class="panel">
  <div class="scroll-x">
    <table class="data">
      <thead>
        <tr>
          <th>Name</th>
          <th>Dataset</th>
          <th>Commit</th>
          <th>Completed</th>
          <th class="num" title="Sum of the file sizes">Size</th>
          <th class="num" title="Bytes this backup stored first">Added</th>
          <th class="num" title="Size over added bytes">Dedup</th>
          <th>Policy</th>
          <th>Verified</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each filtered.slice(0, shown) as x (`${x.repository}/${x.name}`)}
          {@const lin = lineage(x)}
          <tr>
            <td class="name">
              <button
                class="linkish mono"
                onclick={() => dialogs?.show('details', x)}
                title="Details of {x.name}">{x.name}</button
              >
              <div class="faint small">
                {x.repository}{#if x.note}<span> · {x.note}</span>{/if}
              </div>
            </td>
            <td>
              <span class="mono">{x.dataset.name}</span>
              {#if b.fromMemory(x)}<span class="badge" title="A backup of an in-memory dataset"
                  >in-memory</span
                >{/if}
              {#if lin}<span class="badge warn" title={lin.title}>{lin.label}</span>{/if}
            </td>
            <td class="small" title={fmtTime(x.commit.timestamp)}>{fmtCommitAge(x.commit, now)}</td>
            <td class="small" title={fmtTime(x.completed)}>{fmtRelative(x.completed, now)}</td>
            <td class="num small">{fmtBytes(x.logicalBytes)}</td>
            <td class="num small">{fmtBytes(x.addedBytes)}</td>
            <td class="num small">{fmtDedup(x)}</td>
            <td class="small">
              {#if x.policy}<span class="mono">{x.policy}</span>{:else}<span class="faint"
                  >hand</span
                >{/if}
            </td>
            <td>
              {#if x.verified}
                <span
                  class="badge {x.verified.status === 'ok'
                    ? 'ok'
                    : x.verified.status === 'error'
                      ? 'danger'
                      : 'warn'}"
                  title="{x.verified.level}, {fmtTime(x.verified.at)}">{x.verified.status}</span
                >
              {:else}<span class="faint">—</span>{/if}
            </td>
            <td class="actions">
              {#if canAdmin(x)}
                <button class="btn sm" onclick={() => dialogs?.show('restore', x)}>Restore</button>
                <button class="btn sm" onclick={() => dialogs?.show('verify', x)}>Verify</button>
                {#if !readonlyRepos.includes(x.repository)}
                  <button
                    class="btn sm icon danger"
                    onclick={() => dialogs?.show('delete', x)}
                    aria-label="Delete {x.name}"
                    title="Delete {x.name}"><Icon name="trash" size={12} /></button
                  >
                {/if}
              {/if}
            </td>
          </tr>
        {:else}
          <tr>
            <td colspan="10" class="faint">
              {#if !loaded}Loading…{:else if backups.length}No backup matches the filters.{:else}No
                backups yet.{/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  {#if filtered.length > shown}
    <button class="btn ghost sm more" onclick={() => (shown += PAGE)}
      >Show more ({filtered.length - shown} hidden)</button
    >
  {/if}
</section>

<BackupDialogs bind:this={dialogs} {backups} {readonlyRepos} {readOnly} onchanged={load} />

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 6px;
    padding-left: 8px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
    color: var(--text-3);
  }
  .search input {
    border: 0;
    width: 220px;
    box-shadow: none;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .problem {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--warn);
  }
  .scroll-x {
    overflow-x: auto;
  }
  .name {
    max-width: 280px;
  }
  .name .linkish {
    overflow-wrap: anywhere;
    text-align: left;
  }
  .linkish {
    border: 0;
    background: none;
    padding: 0;
    color: var(--text);
    font-weight: 600;
    cursor: pointer;
    font-family: var(--font-mono);
    font-size: var(--fs);
  }
  .linkish:hover {
    color: var(--iri);
    text-decoration: underline;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .actions {
    text-align: right;
    white-space: nowrap;
  }
  .actions > * + * {
    margin-left: 4px;
  }
  .more {
    margin: 6px 10px 10px;
  }
</style>

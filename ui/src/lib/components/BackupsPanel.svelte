<script lang="ts">
  import { onMount } from 'svelte';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { auth } from '$lib/auth.svelte';
  import * as b from '$lib/backups';
  import { fmtCommitAge, fmtDedup } from '$lib/backups-format';
  import { fmtBytes, fmtRelative, fmtTime } from '$lib/format';
  import { LatestRun } from '$lib/supersede';
  import BackupDialogs from './BackupDialogs.svelte';
  import BackupNowDialog from './BackupNowDialog.svelte';
  import Icon from './Icon.svelte';

  let {
    name,
    branch = null,
    info,
    readOnly = false,
    refreshKey = 0,
    onstarted,
    onchanged,
  }: {
    name: string;
    branch?: string | null;
    info: api.DatasetInfo | undefined;
    readOnly?: boolean;
    /** Bump to reload (a backup task finished). */
    refreshKey?: number;
    /** A backup task was started. */
    onstarted?: (t: api.Task) => void;
    /** A restore or delete changed something. */
    onchanged?: () => void;
  } = $props();

  const SHOWN = 8;

  type Loaded = { kind: 'ok'; list: b.DatasetBackups } | { kind: 'unsupported' };
  let loaded = $state<Loaded | null>(null);
  let error = $state<string | null>(null);
  let repositories = $state<(b.Repository | b.RepositoryBrief)[]>([]);
  let backupFor = $state<string | null>(null);
  let dialogs: BackupDialogs | undefined = $state();
  let showAll = $state(false);
  let now = $state(Date.now());
  const runs = new LatestRun();

  async function load() {
    const owns = runs.claim('backups');
    try {
      const list = await b.datasetBackups(name);
      if (!owns()) return;
      loaded = { kind: 'ok', list };
      error = null;
    } catch (e) {
      if (!owns()) return;
      if (e instanceof api.ApiError && (e.status === 404 || e.status === 501) && !loaded)
        loaded = { kind: 'unsupported' };
      else error = api.errorMessage(e);
    }
  }

  $effect(() => {
    void name;
    void refreshKey;
    void load();
  });

  onMount(() => {
    const t = setInterval(() => (now = Date.now()), 30_000);
    return () => clearInterval(t);
  });

  const admin = $derived(auth.can(name, 'admin'));
  const inMemory = $derived(info?.type === 'mem');
  const list = $derived(loaded?.kind === 'ok' ? loaded.list.backups : []);
  const shown = $derived(showAll ? list : list.slice(0, SHOWN));
  const readonlyRepos = $derived(repositories.filter(b.isReadonly).map((r) => r.name));
  /** `restoredFrom` of a dataset restored from a backup (servers with backups). */
  const restoredFrom = $derived(
    (info as (api.DatasetInfo & { restoredFrom?: b.RestoredFrom }) | undefined)?.restoredFrom,
  );

  async function backupNow() {
    try {
      repositories = await b.listRepositories();
    } catch {
      repositories = [];
    }
    backupFor = name;
  }

  $effect(() => {
    // the repository list decides which delete buttons show
    if (admin && loaded?.kind === 'ok')
      b.listRepositories().then(
        (r) => (repositories = r),
        () => {},
      );
  });
</script>

{#if loaded?.kind !== 'unsupported'}
  <section class="panel" aria-labelledby="backups-h">
    <div class="panel-head">
      <h2 id="backups-h">
        Backups{#if branch}
          of main{/if}
      </h2>
      {#if list.length}<span class="faint">{list.length}</span>{/if}
      <span class="spacer"></span>
      {#if admin}
        {#if list.length}
          <button class="btn sm" onclick={() => dialogs?.show('restore', list[0])}>Restore…</button>
        {/if}
        <button
          class="btn sm primary"
          onclick={backupNow}
          title={inMemory
            ? 'Write the dataset at its current commit to a temporary copy on disk, then copy that into a backup repository'
            : 'Copy the dataset at its current commit into a backup repository'}
          ><Icon name="archive" size={13} /> Back up now</button
        >
      {/if}
      <button class="btn ghost icon sm" onclick={load} aria-label="Reload backups" title="Reload"
        ><Icon name="refresh" size={13} /></button
      >
    </div>
    {#if restoredFrom}
      <p class="restored faint">
        Restored from <span class="mono">{restoredFrom.repository}/{restoredFrom.backup}</span>
        (commit
        {restoredFrom.seq}{restoredFrom.datasetId !== info?.id ? ', another lineage' : ''}).
      </p>
    {/if}
    {#if (info?.branches ?? 1) > 1}
      <p class="restored faint">
        Backups copy the main branch only. {info!.branches! - 1 === 1
          ? 'The other branch is'
          : `The other ${info!.branches! - 1} branches are`} not backed up.
      </p>
    {/if}
    {#if error}
      <div class="panel-body"><div class="error-box">{error}</div></div>
    {:else if !loaded}
      <p class="panel-body faint"><span class="spinner"></span> Loading…</p>
    {:else if list.length === 0}
      <p class="panel-body faint">
        No backups of {name} yet{admin ? ': back it up into a repository.' : '.'}
      </p>
    {:else}
      <div class="scroll-x">
        <table class="data">
          <thead>
            <tr>
              <th>Backup</th>
              <th>Commit</th>
              <th class="num">Size</th>
              <th class="num" title="Size over the bytes it added">Dedup</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each shown as x (`${x.repository}/${x.name}`)}
              <tr>
                <td class="name">
                  <button class="linkish mono" onclick={() => dialogs?.show('details', x)}
                    >{x.name}</button
                  >
                  <div class="faint small">
                    {x.repository} · {fmtRelative(x.completed, now)}{x.policy
                      ? ` · ${x.policy}`
                      : ''}
                    {#if x.sameLineage === false}<span
                        class="badge warn"
                        title="Another dataset id ({x.dataset.id}): an earlier dataset of this name"
                        >other lineage</span
                      >{/if}
                    {#if x.verified?.status === 'error'}<span
                        class="badge danger"
                        title="Last verify ({x.verified.level}) {fmtTime(x.verified.at)}"
                        >verify failed</span
                      >{/if}
                  </div>
                </td>
                <td class="small" title={fmtTime(x.commit.timestamp)}
                  >{fmtCommitAge(x.commit, now)}</td
                >
                <td class="num small">{fmtBytes(x.logicalBytes)}</td>
                <td class="num small">{fmtDedup(x)}</td>
                <td class="actions">
                  {#if admin}
                    <button class="btn sm" onclick={() => dialogs?.show('restore', x)}
                      >Restore…</button
                    >
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <div class="foot">
        {#if list.length > SHOWN}
          <button class="btn ghost sm" onclick={() => (showAll = !showAll)}
            >{showAll ? 'Show fewer' : `Show all ${list.length}`}</button
          >
        {/if}
        <span class="spacer"></span>
        {#if auth.hasServer('server-admin') || admin}
          <a class="btn ghost sm" href="{resolve('/backups')}?dataset={encodeURIComponent(name)}"
            >Open in Backups <Icon name="chevron" size={12} /></a
          >
        {/if}
      </div>
    {/if}
  </section>
{/if}

<BackupNowDialog bind:dataset={backupFor} {repositories} {onstarted} />
<BackupDialogs
  bind:this={dialogs}
  backups={list}
  {readonlyRepos}
  {readOnly}
  onchanged={() => {
    void load();
    onchanged?.();
  }}
/>

<style>
  .small {
    font-size: var(--fs-sm);
  }
  .scroll-x {
    overflow-x: auto;
  }
  .restored {
    margin: 0;
    padding: 8px 14px 0;
    font-size: var(--fs-sm);
  }
  .name {
    max-width: 260px;
  }
  .linkish {
    border: 0;
    background: none;
    padding: 0;
    color: var(--text);
    font-weight: 600;
    cursor: pointer;
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    text-align: left;
    overflow-wrap: anywhere;
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
  .foot {
    display: flex;
    align-items: center;
    padding: 6px 10px;
    border-top: 1px solid var(--border);
  }
  a.btn {
    text-decoration: none;
  }
</style>

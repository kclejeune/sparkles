<script lang="ts">
  // The dataset page's Settings tab (C19 §10): a section for each settings kind of the
  // dataset, and in the memory section the state of memory maintenance with its Run now
  // buttons. Everyone who can read the dataset sees the tab; admins of the dataset change
  // the settings and run maintenance.
  import { onDestroy } from 'svelte';
  import {
    latestOf,
    maintenanceStatus,
    type MaintenanceJob as Job,
    type MaintenanceStatus,
  } from '$lib/maintenance';
  import * as review from '$lib/review-api';
  import { DATASET_KINDS, datasetKindUrl } from '$lib/settings';
  import { DATASET_FIELDS, KIND_TITLES } from '$lib/settings-fields';
  import Icon from './Icon.svelte';
  import MaintenanceJob from './MaintenanceJob.svelte';
  import SettingsKindPanel from './SettingsKindPanel.svelte';

  let {
    name,
    canAdmin = false,
    readOnly = false,
    branch = null,
  }: {
    name: string;
    canAdmin?: boolean;
    /** The server is read-only (settings still change; maintenance does not). */
    readOnly?: boolean;
    /** The branch the page shows: settings belong to the dataset, not a branch. */
    branch?: string | null;
  } = $props();

  let maintenance = $state<MaintenanceStatus | null>(null);
  /** The newest pass of each job, in full (a task list holds only a summary of results). */
  let last = $state<Partial<Record<Job, review.IngestTask>>>({});
  let ctl: AbortController | null = null;
  onDestroy(() => ctl?.abort());

  async function loadMaintenance() {
    ctl?.abort();
    const c = new AbortController();
    ctl = c;
    const ds = name;
    try {
      const [m, l] = await Promise.all([
        maintenanceStatus(ds, c.signal),
        review.ingestTasks(ds, c.signal).catch(() => null),
      ]);
      if (c.signal.aborted) return;
      maintenance = m;
      const tasks = l?.tasks ?? [];
      const found: Partial<Record<Job, review.IngestTask>> = {};
      for (const job of ['consolidation', 'retention'] as const) {
        // the newest pass the server still knows, else the last scheduled one
        const id = latestOf(tasks, job)?.id ?? m[job].lastTask;
        const t = id ? await review.ingestTask(ds, id, 0, c.signal).catch(() => null) : null;
        if (t) found[job] = t;
      }
      if (!c.signal.aborted) last = found;
    } catch {
      // a server without memory maintenance: the memory section shows no jobs
      if (!c.signal.aborted) maintenance = null;
    }
  }

  $effect(() => {
    void name;
    maintenance = null;
    last = {};
    void loadMaintenance();
  });
</script>

<div class="settings-tab">
  <p class="intro faint">
    {#if canAdmin}
      Each field shows where its value comes from. A change here overrides the server's settings
      file until it is reset, and fields the operator locked cannot be changed.
    {:else}
      <Icon name="lock" size={12} /> You can read these settings. Changing them needs admin on the dataset.
    {/if}
    {#if branch}
      Settings belong to the dataset and apply to every branch.
    {/if}
  </p>
  {#each DATASET_KINDS as kind (kind)}
    <SettingsKindPanel
      title={KIND_TITLES[kind].title}
      text={KIND_TITLES[kind].text}
      url={datasetKindUrl(name, kind)}
      fields={DATASET_FIELDS[kind]}
      canEdit={canAdmin}
      onchange={kind === 'memory' && maintenance ? () => void loadMaintenance() : undefined}
    >
      {#snippet groupExtra(group: string)}
        {#if kind === 'memory' && maintenance && (group === 'Consolidation' || group === 'Retention')}
          {@const job = group === 'Consolidation' ? 'consolidation' : 'retention'}
          <MaintenanceJob
            ds={name}
            {job}
            entry={maintenance[job]}
            last={last[job] ?? null}
            {canAdmin}
            {readOnly}
            onchanged={loadMaintenance}
          />
        {/if}
      {/snippet}
    </SettingsKindPanel>
  {/each}
</div>

<style>
  .settings-tab {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 16px;
    max-width: 980px;
  }
  .intro {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: var(--fs-sm);
  }
</style>

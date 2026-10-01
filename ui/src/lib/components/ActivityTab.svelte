<script lang="ts">
  import * as api from '$lib/api';
  import * as b from '$lib/backups';
  import { fmtBytes, fmtInt, fmtMs, fmtRelative, fmtTime } from '$lib/format';
  import Icon from './Icon.svelte';
  import TaskList from './TaskList.svelte';

  let {
    admin,
    repositories,
    refreshKey = 0,
    ontaskdone,
  }: {
    /** Server admin: also policy runs and GC reports. */
    admin: boolean;
    repositories: (b.Repository | b.RepositoryBrief)[];
    refreshKey?: number;
    /** A backup task finished (the page reloads its data). */
    ontaskdone?: (t: api.Task) => void;
  } = $props();

  let runs = $state<b.PolicyRun[]>([]);
  let runsError = $state<string | null>(null);
  let runsLoaded = $state(false);
  let expanded = $state<Record<string, boolean>>({});
  let shown = $state(30);

  async function loadRuns() {
    if (!admin) return;
    try {
      const policies = await b.listPolicies();
      const lists = await Promise.all(policies.map((p) => b.policyRuns(p.name, 50)));
      runs = lists.flat().sort((x, y) => y.started.localeCompare(x.started));
      runsError = null;
    } catch (e) {
      runsError = api.errorMessage(e);
    } finally {
      runsLoaded = true;
    }
  }

  $effect(() => {
    void refreshKey;
    void loadRuns();
  });

  const gcs = $derived(
    repositories
      .filter(b.isFull)
      .flatMap((r) => (r.lastGc ? [{ repo: r.name, gc: r.lastGc }] : []))
      .sort((x, y) => y.gc.finished.localeCompare(x.gc.finished)),
  );

  const counts = (r: b.PolicyRun) => {
    const c: Record<string, number> = {};
    for (const d of r.datasets) c[d.result] = (c[d.result] ?? 0) + 1;
    return Object.entries(c)
      .map(([k, n]) => `${n} ${k}`)
      .join(' · ');
  };
  const took = (r: b.PolicyRun) =>
    r.finished ? fmtMs(Date.parse(r.finished) - Date.parse(r.started)) : 'running';
  const resultClass = (r: string) =>
    r === 'ok' ? 'ok' : r === 'failed' ? 'danger' : r === 'skipped' ? '' : 'warn';
</script>

<section class="panel">
  <div class="panel-head"><h2>Backup tasks</h2></div>
  <div class="panel-body">
    <TaskList
      limit={40}
      {refreshKey}
      filter={b.isBackupTask}
      empty="No backup tasks yet. Backups, restores, verifies, garbage collections and policy runs show up here."
      ondone={ontaskdone}
    />
  </div>
</section>

{#if admin}
  <section class="panel">
    <div class="panel-head">
      <h2>Policy runs</h2>
      <span class="spacer"></span>
      <button class="btn ghost icon sm" onclick={loadRuns} aria-label="Reload policy runs"
        ><Icon name="refresh" size={13} /></button
      >
    </div>
    {#if runsError}
      <div class="panel-body faint">Policy runs are not available: {runsError}</div>
    {:else}
      <div class="scroll-x">
        <table class="data">
          <thead>
            <tr>
              <th></th><th>Started</th><th>Policy</th><th>Trigger</th><th>Result</th><th
                >Datasets</th
              ><th class="num">Took</th>
            </tr>
          </thead>
          <tbody>
            {#each runs.slice(0, shown) as r (r.id)}
              <tr>
                <td class="toggle">
                  <button
                    class="btn ghost icon sm"
                    aria-expanded={!!expanded[r.id]}
                    aria-controls="run-{r.id}"
                    aria-label="Details of run {r.id.slice(0, 8)}"
                    onclick={() => (expanded[r.id] = !expanded[r.id])}
                    ><Icon name={expanded[r.id] ? 'chevronDown' : 'chevron'} size={12} /></button
                  >
                </td>
                <td class="small" title={fmtTime(r.started)}>{fmtRelative(r.started)}</td>
                <td class="mono small">{r.policy}</td>
                <td class="small"
                  >{r.trigger}{#if r.scheduledFor}<span
                      class="faint"
                      title="Scheduled for {fmtTime(r.scheduledFor)}"
                    >
                      · {fmtTime(r.scheduledFor)}</span
                    >{/if}</td
                >
                <td
                  ><span class="badge {resultClass(r.result)}" title={r.reason}>{r.result}</span
                  ></td
                >
                <td class="small">{counts(r) || '—'}</td>
                <td class="num small">{took(r)}</td>
              </tr>
              {#if expanded[r.id]}
                <tr class="detail" id="run-{r.id}">
                  <td></td>
                  <td colspan="6">
                    {#if r.reason}<p class="small">{r.reason}</p>{/if}
                    <ul class="ds">
                      {#each r.datasets as d (d.dataset)}
                        <li>
                          <span class="badge {resultClass(d.result)}">{d.result}</span>
                          <span class="mono">{d.dataset}</span>
                          {#if d.backup}<span class="mono faint">→ {d.backup}</span>{/if}
                          {#if d.addedBytes != null}<span class="faint"
                              >{fmtBytes(d.addedBytes)} added</span
                            >{/if}
                          {#if d.millis != null}<span class="faint">{fmtMs(d.millis)}</span>{/if}
                          {#if d.reason}<span class:bad={d.result === 'failed'}>{d.reason}</span
                            >{/if}
                        </li>
                      {/each}
                    </ul>
                    <p class="small faint">
                      Retention: {#if !r.retention}not applied{:else if r.retention.error}<span
                          class="bad">{r.retention.error}</span
                        >{:else if r.retention.deleted.length}deleted <span class="mono"
                          >{r.retention.deleted.join(', ')}</span
                        >{:else}nothing to delete{/if}{#if r.gc}
                        · garbage collection in task {r.gc.task}{/if}
                    </p>
                  </td>
                </tr>
              {/if}
            {:else}
              <tr
                ><td colspan="7" class="faint">{runsLoaded ? 'No policy runs yet.' : 'Loading…'}</td
                ></tr
              >
            {/each}
          </tbody>
        </table>
      </div>
      {#if runs.length > shown}
        <button class="btn ghost sm more" onclick={() => (shown += 50)}
          >Show more ({runs.length - shown} hidden)</button
        >
      {/if}
    {/if}
  </section>

  <section class="panel">
    <div class="panel-head"><h2>Garbage collection</h2></div>
    <div class="scroll-x">
      <table class="data">
        <thead>
          <tr>
            <th>Repository</th><th>Finished</th><th class="num">Deleted</th><th class="num"
              >Freed</th
            ><th class="num" title="Unreferenced but within the grace period">Kept young</th><th
              class="num">Stored after</th
            ><th class="num">Took</th>
          </tr>
        </thead>
        <tbody>
          {#each gcs as { repo, gc } (repo)}
            <tr>
              <td class="mono">{repo}</td>
              <td class="small" title={fmtTime(gc.finished)}>{fmtRelative(gc.finished)}</td>
              <td class="num small">{fmtInt(gc.deleted)} blobs</td>
              <td class="num small">{fmtBytes(gc.deletedBytes)}</td>
              <td class="num small">{fmtInt(gc.keptYoung)}</td>
              <td class="num small">{fmtBytes(gc.storedBytesAfter)}</td>
              <td class="num small">{fmtMs(gc.millis)}</td>
            </tr>
          {:else}
            <tr
              ><td colspan="7" class="faint"
                >No garbage collection has run yet. Start one from a repository card; dry runs
                appear in the task list above.</td
              ></tr
            >
          {/each}
        </tbody>
      </table>
    </div>
  </section>
{/if}

<style>
  .small {
    font-size: var(--fs-sm);
  }
  .scroll-x {
    overflow-x: auto;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .toggle {
    width: 1%;
  }
  tr.detail td {
    background: var(--surface-2);
  }
  .ds {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .ds li {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .detail p {
    margin: 6px 0 0;
  }
  .bad {
    color: var(--danger);
  }
  .more {
    margin: 6px 10px 10px;
  }
</style>

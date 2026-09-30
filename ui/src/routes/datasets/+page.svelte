<script lang="ts">
  import { resolve } from '$app/paths';
  import { goto } from '$app/navigation';
  import { onMount } from 'svelte';
  import { app } from '$lib/app.svelte';
  import { fmtInt, fmtRelative } from '$lib/format';
  import DatasetDialogs from '$components/DatasetDialogs.svelte';
  import Icon from '$components/Icon.svelte';
  import TaskList from '$components/TaskList.svelte';

  let createOpen = $state(false);
  let deleteTarget = $state<string | null>(null);
  let refreshing = $state(false);

  async function refresh() {
    refreshing = true;
    await app.refreshDatasets();
    refreshing = false;
  }

  onMount(refresh);

  const detail = (name: string) => resolve('/datasets/[name]', { name });
  const total = $derived(app.datasets.reduce((a, d) => a + d.quads, 0));
</script>

<svelte:head><title>Datasets | Sparkles</title></svelte:head>

<div class="page">
  <header class="head">
    <div>
      <h1>Datasets</h1>
      <p class="muted">
        {app.datasets.length} dataset{app.datasets.length === 1 ? '' : 's'}, {fmtInt(total)} quads in
        total
      </p>
    </div>
    <span class="spacer"></span>
    <button class="btn" onclick={refresh} disabled={refreshing}>
      <Icon name="refresh" size={14} /> Refresh
    </button>
    <button class="btn primary" onclick={() => (createOpen = true)}
      ><Icon name="plus" size={14} /> New dataset</button
    >
  </header>

  {#if app.datasetsError}
    <div class="error-box"><strong>Could not load datasets.</strong> {app.datasetsError}</div>
  {/if}

  {#if !(app.datasetsError && app.datasets.length === 0)}
    <section class="panel">
      {#if app.datasetsLoaded && app.datasets.length === 0}
        <div class="empty">
          <Icon name="database" size={24} />
          <p>No datasets yet.</p>
          <button class="btn primary" onclick={() => (createOpen = true)}
            ><Icon name="plus" size={14} /> Create your first dataset</button
          >
        </div>
      {:else}
        <table class="data">
          <thead>
            <tr>
              <th>Name</th>
              <th>Storage</th>
              <th class="num">Quads</th>
              <th title="Head commit and when it was made">Head</th>
              <th>Reasoning</th>
              <th>Endpoint</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each app.datasets as d (d.name)}
              <tr class:current={d.name === app.current}>
                <td>
                  <a class="name" href={detail(d.name)}>{d.name}</a>
                  {#if d.name === app.current}<span class="badge spark">selected</span>{/if}
                </td>
                <td><span class="badge">{d.type === 'mem' ? 'in-memory' : 'persistent'}</span></td>
                <td class="num">{fmtInt(d.quads)}</td>
                <td>
                  {#if d.head != null}
                    <span class="mono" title={d.modified}>commit {d.head}</span>
                    {#if d.modified}<span class="faint">, {fmtRelative(d.modified)}</span>{/if}
                  {:else}
                    <span class="faint">—</span>
                  {/if}
                </td>
                <td>
                  {#if d.reasoning}
                    <span class="badge ok">{d.reasoning.profile}</span>
                    <span class="faint"
                      >+{fmtInt(d.reasoning.inferred)} inferred, {fmtRelative(d.reasoning.at)}</span
                    >
                  {:else}
                    <span class="faint">off</span>
                  {/if}
                </td>
                <td><span class="mono faint">{d.endpoints?.query ?? `/${d.name}/sparql`}</span></td>
                <td class="actions">
                  <button
                    class="btn sm"
                    onclick={() => {
                      app.setDataset(d.name);
                      goto(resolve('/query'));
                    }}><Icon name="query" size={13} /> Query</button
                  >
                  <a class="btn sm" href={detail(d.name)}>Details</a>
                  <button
                    class="btn sm icon danger"
                    aria-label="Delete {d.name}"
                    title="Delete {d.name}"
                    onclick={() => (deleteTarget = d.name)}
                  >
                    <Icon name="trash" size={13} />
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  {/if}

  <section class="panel">
    <div class="panel-head"><h2>Recent tasks</h2></div>
    <div class="panel-body"><TaskList ondone={() => app.refreshDatasets()} /></div>
  </section>
</div>

<DatasetDialogs bind:createOpen bind:deleteTarget />

<style>
  .page {
    padding: 24px 28px 40px;
    display: grid;
    gap: 18px;
    max-width: 1200px;
    width: 100%;
  }
  .head {
    display: flex;
    align-items: flex-end;
    gap: 8px;
  }
  .head p {
    margin-top: 4px;
  }
  .name {
    font-weight: 600;
    color: var(--text);
    margin-right: 6px;
  }
  .actions {
    text-align: right;
    white-space: nowrap;
  }
  .actions > * + * {
    margin-left: 4px;
  }
  a.btn {
    text-decoration: none;
  }
  table.data td {
    height: 40px;
  }
  @media (max-width: 760px) {
    .page {
      padding: 16px;
    }
    table.data th:nth-child(5),
    table.data td:nth-child(5),
    table.data th:nth-child(4),
    table.data td:nth-child(4) {
      display: none;
    }
    section.panel {
      overflow-x: auto;
    }
  }
</style>

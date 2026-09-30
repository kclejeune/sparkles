<script lang="ts">
  import { resolve } from '$app/paths';
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { app } from '$lib/app.svelte';
  import { fmtDuration, fmtInt, fmtTime } from '$lib/format';
  import Icon from '$components/Icon.svelte';
  import TaskList from '$components/TaskList.svelte';

  let info = $state<api.ServerInfo | null>(null);
  let error = $state<string | null>(null);
  let fetchedAt = $state(Date.now());
  let now = $state(Date.now());

  async function load() {
    try {
      info = await api.serverInfo();
      fetchedAt = Date.now();
      error = null;
    } catch (e) {
      error = api.errorMessage(e);
    }
  }

  onMount(() => {
    load();
    const tick = setInterval(() => (now = Date.now()), 1000);
    const poll = setInterval(load, 15000);
    return () => {
      clearInterval(tick);
      clearInterval(poll);
    };
  });

  const uptime = $derived(info ? info.uptimeSeconds + (now - fetchedAt) / 1000 : 0);
  const totalQuads = $derived(info?.datasets.reduce((a, d) => a + d.quads, 0) ?? 0);
  const origin = typeof location !== 'undefined' ? location.origin : '';
</script>

<svelte:head><title>Server | Sparkles</title></svelte:head>

<div class="page">
  <header class="head">
    <div>
      <h1>Server</h1>
      <p class="muted">{origin}</p>
    </div>
    <span class="spacer"></span>
    <button class="btn" onclick={() => { load(); app.ping(); }}><Icon name="refresh" size={14} /> Refresh</button>
  </header>

  {#if error}
    <div class="error-box">
      <strong>The server did not answer.</strong>
      <span class="muted">{error}</span>
      <pre>Check that sparkles is running and reachable from this browser.</pre>
    </div>
  {/if}

  <section class="panel status">
    <div class="pulse" class:on={app.online} class:off={app.online === false}></div>
    <div class="grow">
      <div class="big">
        {#if app.online === null}Checking…{:else if app.online}Up for {fmtDuration(uptime)}{:else}Unreachable{/if}
      </div>
      <div class="muted">
        {[info ? `Started ${fmtTime(info.startedAt)}` : '', app.lastPingMs != null ? `ping ${app.lastPingMs.toFixed(0)} ms` : '']
          .filter(Boolean)
          .join(', ')}
      </div>
    </div>
    {#if info}<span class="version mono">v{info.version}</span>{/if}
  </section>

  {#if info}
    <dl class="kv panel">
      <div><dt>Version</dt><dd class="mono">{info.version}</dd></div>
      <div><dt>Started</dt><dd>{fmtTime(info.startedAt)}</dd></div>
      <div><dt>Uptime</dt><dd>{fmtDuration(uptime)}</dd></div>
      <div><dt>Datasets</dt><dd>{info.datasets.length}</dd></div>
      <div><dt>Quads served</dt><dd>{fmtInt(totalQuads)}</dd></div>
    </dl>

    <section class="panel">
      <div class="panel-head"><h2>Endpoints</h2></div>
      <table class="data">
        <thead><tr><th>Dataset</th><th>Query</th><th>Update</th><th>Graph store</th><th>Upload</th></tr></thead>
        <tbody>
          {#each info.datasets as d (d.name)}
            <tr>
              <td><a href={resolve('/datasets/[name]', { name: d.name })}>{d.name}</a></td>
              <td class="mono">{d.endpoints.query}</td>
              <td class="mono">{d.endpoints.update}</td>
              <td class="mono">{d.endpoints.gsp}</td>
              <td class="mono">{d.endpoints.upload}</td>
            </tr>
          {:else}
            <tr><td colspan="5" class="faint">No datasets.</td></tr>
          {/each}
        </tbody>
      </table>
    </section>
  {/if}

  <section class="panel">
    <div class="panel-head"><h2>Tasks</h2></div>
    <div class="panel-body"><TaskList limit={25} /></div>
  </section>
</div>

<style>
  .page {
    padding: 24px 28px 40px;
    display: grid;
    gap: 16px;
    max-width: 1100px;
    width: 100%;
  }
  .head {
    display: flex;
    align-items: flex-end;
  }
  .head p {
    margin-top: 4px;
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
  }
  .status {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 18px 20px;
  }
  .grow {
    flex: 1;
  }
  .big {
    font-size: var(--fs-lg);
    font-weight: 600;
  }
  .pulse {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--text-3);
  }
  .pulse.on {
    background: var(--ok);
    box-shadow: 0 0 0 5px var(--ok-soft);
  }
  .pulse.off {
    background: var(--danger);
    box-shadow: 0 0 0 5px var(--danger-soft);
  }
  .version {
    font-size: var(--fs-sm);
    padding: 3px 8px;
    border-radius: 4px;
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .kv {
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    margin: 0;
  }
  .kv > div {
    padding: 12px 16px;
    border-right: 1px solid var(--border);
  }
  .kv > div:last-child {
    border-right: 0;
  }
  dt {
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  dd {
    margin: 2px 0 0;
    font-weight: 600;
    font-size: var(--fs-md);
  }
  td.mono {
    font-size: 12px;
    color: var(--text-2);
  }
  @media (max-width: 760px) {
    .page {
      padding: 16px;
    }
    .kv {
      grid-template-columns: 1fr 1fr;
    }
    section.panel {
      overflow-x: auto;
    }
  }
</style>

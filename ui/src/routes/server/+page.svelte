<script lang="ts">
  import { resolve } from '$app/paths';
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { fmtBytes, fmtDuration, fmtInt, fmtTime } from '$lib/format';
  import { fmtSeconds, hitRatio, requestRows, type RequestRow } from '$lib/metrics';
  import Icon from '$components/Icon.svelte';
  import TaskList from '$components/TaskList.svelte';

  /** Delta or write-ahead log size past which the Readiness panel offers compaction. */
  const COMPACT_DELTA_QUADS = 1_000_000;
  const COMPACT_WAL_BYTES = 256 * 1024 * 1024;
  const STATUS_POLL_MS = 5000;

  let info = $state<api.ServerInfo | null>(null);
  let error = $state<string | null>(null);
  let fetchedAt = $state(Date.now());
  let now = $state(Date.now());

  let readiness = $state<api.ReadyInfo | null>(null);
  let readyError = $state<string | null>(null);
  let snap = $state<api.MetricsSnapshot | null>(null);
  let metricsError = $state<string | null>(null);
  let rows = $state<RequestRow[]>([]);
  let prev: { snap: api.MetricsSnapshot; at: number } | null = null;
  let busy = $state<Record<string, boolean>>({});
  let taskRefresh = $state(0);

  async function load() {
    try {
      info = await api.serverInfo();
      fetchedAt = Date.now();
      error = null;
    } catch (e) {
      error = api.errorMessage(e);
    }
  }

  async function loadStatus() {
    const [r, m] = await Promise.allSettled([api.ready(), api.metricsSnapshot()]);
    if (r.status === 'fulfilled') {
      readiness = r.value;
      readyError = null;
    } else {
      readyError = api.errorMessage(r.reason);
    }
    if (m.status === 'fulfilled') {
      const at = performance.now();
      rows = requestRows(m.value, prev?.snap ?? null, prev ? (at - prev.at) / 1000 : 0);
      prev = { snap: m.value, at };
      snap = m.value;
      metricsError = null;
    } else {
      metricsError = api.errorMessage(m.reason);
    }
  }

  onMount(() => {
    load();
    loadStatus();
    const tick = setInterval(() => (now = Date.now()), 1000);
    const poll = setInterval(load, 15000);
    // readiness and metrics only while the tab is visible
    const statusPoll = setInterval(() => {
      if (document.visibilityState === 'visible') loadStatus();
    }, STATUS_POLL_MS);
    const onVisible = () => {
      if (document.visibilityState === 'visible') loadStatus();
    };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      clearInterval(tick);
      clearInterval(poll);
      clearInterval(statusPoll);
      document.removeEventListener('visibilitychange', onVisible);
    };
  });

  async function compactNow(ds: string) {
    busy[ds] = true;
    try {
      const t = await api.compact(ds);
      toasts.push('info', `Compaction of ${ds} started`, t?.id ? `Task ${t.id}` : undefined);
      taskRefresh++;
    } catch (e) {
      toasts.error(`Could not compact ${ds}`, e);
    } finally {
      busy[ds] = false;
    }
  }

  async function clearCache(ds: string) {
    busy[`cache:${ds}`] = true;
    try {
      const r = await api.clearResultCache(ds);
      toasts.push(
        'success',
        `Cleared the result cache of ${ds}`,
        `${fmtInt(r.cleared)} entries, ${fmtBytes(r.bytes)}`,
      );
      await loadStatus();
    } catch (e) {
      toasts.error(`Could not clear the result cache of ${ds}`, e);
    } finally {
      busy[`cache:${ds}`] = false;
    }
  }

  const needsCompaction = (d: api.ReadyInfo['datasets'][number]) =>
    d.deltaQuads > COMPACT_DELTA_QUADS || d.walBytes > COMPACT_WAL_BYTES;

  const dsLabel = (name: string) =>
    name === '$none' ? 'no dataset' : name === '$other' ? 'other datasets' : name;
  const isDataset = (name: string) => !name.startsWith('$');

  const pct = (x: number | null) =>
    x == null
      ? '—'
      : x === 0
        ? '0%'
        : x < 0.1
          ? '<0.1%'
          : `${x < 10 ? x.toFixed(1) : Math.round(x)}%`;
  const rate = (r: number | null) =>
    r == null
      ? '—'
      : r === 0
        ? '0/s'
        : r < 0.1
          ? '<0.1/s'
          : `${r < 10 ? r.toFixed(1) : Math.round(r)}/s`;
  const limit = (bytes: number) => (bytes ? fmtBytes(bytes) : 'unlimited');

  const outcomeTitle = (r: RequestRow) =>
    (Object.entries(r.outcomes) as [string, number][])
      .filter(([, n]) => n > 0)
      .map(([k, n]) => `${k.replace('_', ' ')}: ${fmtInt(n)}`)
      .join('\n');

  const inFlight = $derived(
    snap
      ? (Object.entries(snap.active) as [api.Operation, number][])
          .filter(([, n]) => n > 0)
          .map(([op, n]) => `${n} ${op}`)
          .join(', ') || 'none'
      : '',
  );
  const limits = $derived(snap?.limits ?? info?.limits ?? null);

  const uptime = $derived(info ? info.uptimeSeconds + (now - fetchedAt) / 1000 : 0);
  const totalQuads = $derived(info?.datasets.reduce((a, d) => a + d.quads, 0) ?? 0);
  const origin = typeof location !== 'undefined' ? location.origin : '';
</script>

<svelte:head><title>Server | Sparkles</title></svelte:head>

{#snippet meter(used: number, capacity: number)}
  {@const share = capacity ? Math.min(100, (100 * used) / capacity) : 0}
  <div class="meter-cell" title="{fmtBytes(used)} of {fmtBytes(capacity)}">
    <span class="meter"><span style:width="{share}%"></span></span>
    <span class="faint small">{fmtBytes(used)} / {fmtBytes(capacity)}</span>
  </div>
{/snippet}

<div class="page">
  <header class="head">
    <div>
      <h1>Server</h1>
      <p class="muted">{origin}</p>
    </div>
    <span class="spacer"></span>
    <button
      class="btn"
      onclick={() => {
        load();
        loadStatus();
        app.ping();
      }}><Icon name="refresh" size={14} /> Refresh</button
    >
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
        {#if app.online === null}Checking…{:else if app.online}Up for {fmtDuration(
            uptime,
          )}{:else}Unreachable{/if}
      </div>
      <div class="muted">
        {[
          info ? `Started ${fmtTime(info.startedAt)}` : '',
          app.lastPingMs != null ? `ping ${app.lastPingMs.toFixed(0)} ms` : '',
        ]
          .filter(Boolean)
          .join(', ')}
      </div>
    </div>
    {#if info}<span class="version mono">v{info.version}</span>{/if}
  </section>

  {#if info}
    <dl class="kv panel">
      <div>
        <dt>Version</dt>
        <dd class="mono">{info.version}</dd>
      </div>
      <div>
        <dt>Started</dt>
        <dd>{fmtTime(info.startedAt)}</dd>
      </div>
      <div>
        <dt>Uptime</dt>
        <dd>{fmtDuration(uptime)}</dd>
      </div>
      <div>
        <dt>Datasets</dt>
        <dd>{info.datasets.length}</dd>
      </div>
      <div>
        <dt>Quads served</dt>
        <dd>{fmtInt(totalQuads)}</dd>
      </div>
    </dl>
  {/if}

  <section class="panel">
    <div class="panel-head">
      <h2>Readiness</h2>
      {#if readiness}
        <span class="badge {readiness.ready ? 'ok' : 'danger'}">{readiness.status}</span>
      {/if}
    </div>
    {#if readyError}
      <div class="panel-body faint">Readiness is not available: {readyError}</div>
    {:else if readiness}
      <table class="data">
        <thead>
          <tr>
            <th>Dataset</th><th>Type</th><th>State</th><th>Generation</th>
            <th class="num">Uncompacted quads</th><th class="num">Write-ahead log</th><th></th>
          </tr>
        </thead>
        <tbody>
          {#each readiness.datasets as d (d.name)}
            <tr>
              <td><a href={resolve('/datasets/[name]', { name: d.name })}>{d.name}</a></td>
              <td class="muted">{d.type}</td>
              <td>
                <span class="badge {d.ready ? 'ok' : 'danger'}" title={d.error}>{d.state}</span>
              </td>
              <td class="mono small">{d.generation ?? '—'}</td>
              <td class="num">{fmtInt(d.deltaQuads)}</td>
              <td class="num">{d.type === 'mem' ? '—' : fmtBytes(d.walBytes)}</td>
              <td class="actions">
                {#if needsCompaction(d)}
                  <button
                    class="btn sm"
                    disabled={busy[d.name]}
                    title="Merge the updates into a new sorted index generation"
                    onclick={() => compactNow(d.name)}
                    ><Icon name="layers" size={13} /> Compact</button
                  >
                {/if}
              </td>
            </tr>
          {:else}
            <tr><td colspan="7" class="faint">No datasets.</td></tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <div class="panel-body faint">Loading…</div>
    {/if}
  </section>

  <section class="panel">
    <div class="panel-head">
      <h2>Requests</h2>
      <span class="spacer"></span>
      {#if snap}<span class="muted small">In flight: {inFlight}</span>{/if}
    </div>
    {#if metricsError}
      <div class="panel-body faint">Metrics are not available: {metricsError}</div>
    {:else if snap}
      <div class="scroll-x">
        <table class="data">
          <thead>
            <tr>
              <th>Dataset</th><th>Operation</th><th class="num">Rate</th>
              <th class="num">Requests</th><th class="num">Failed</th>
              <th class="num">p50</th><th class="num">p95</th>
            </tr>
          </thead>
          <tbody>
            {#each rows as r (r.key)}
              <tr>
                <td class:faint={!isDataset(r.dataset)}>{dsLabel(r.dataset)}</td>
                <td>{r.operation}</td>
                <td class="num">{rate(r.rate)}</td>
                <td class="num">{fmtInt(r.count)}</td>
                <td class="num" class:bad={r.failedPct > 0} title={outcomeTitle(r)}
                  >{pct(r.failedPct)}</td
                >
                <td class="num" title={r.windowed ? 'Last polling interval' : 'Since start'}
                  >{fmtSeconds(r.p50)}</td
                >
                <td class="num" title={r.windowed ? 'Last polling interval' : 'Since start'}
                  >{fmtSeconds(r.p95)}</td
                >
              </tr>
            {:else}
              <tr><td colspan="7" class="faint">No requests yet.</td></tr>
            {/each}
          </tbody>
        </table>
      </div>
      <p class="note faint">
        Rates cover the last {STATUS_POLL_MS / 1000} s. Latency percentiles do too when that interval
        had traffic, and otherwise the time since the server started. Health checks, metrics and the UI's
        own files are not counted.
      </p>
    {:else}
      <div class="panel-body faint">Loading…</div>
    {/if}
  </section>

  <section class="panel">
    <div class="panel-head">
      <h2>Memory and caches</h2>
      <span class="spacer"></span>
      {#if snap?.processResidentBytes != null}
        <span class="muted small"
          >Process memory <strong>{fmtBytes(snap.processResidentBytes)}</strong></span
        >
      {/if}
    </div>
    {#if snap}
      <div class="scroll-x">
        <table class="data">
          <thead>
            <tr>
              <th>Dataset</th><th>Block cache</th><th class="num">Hits</th>
              <th>Result cache</th><th class="num">Hits</th><th></th>
            </tr>
          </thead>
          <tbody>
            {#each snap.datasets as d (d.name)}
              {@const rc = d.resultCache}
              <tr>
                <td class:faint={!isDataset(d.name)}>{dsLabel(d.name)}</td>
                <td>{@render meter(d.blockCache.bytes, d.blockCache.capacityBytes)}</td>
                <td class="num">{pct(hitRatio(d.blockCache.hits, d.blockCache.misses))}</td>
                <td>
                  {#if rc.enabled}
                    {@render meter(rc.bytes, rc.capacityBytes)}
                  {:else}
                    <span class="faint">off</span>
                  {/if}
                </td>
                <td class="num">{rc.enabled ? pct(hitRatio(rc.hits, rc.misses)) : '—'}</td>
                <td class="actions">
                  {#if isDataset(d.name)}
                    <button
                      class="btn sm ghost"
                      disabled={!rc.enabled || !rc.entries || busy[`cache:${d.name}`]}
                      title="Drop this dataset's cached query results"
                      onclick={() => clearCache(d.name)}>Clear result cache</button
                    >
                  {/if}
                </td>
              </tr>
            {:else}
              <tr><td colspan="6" class="faint">No datasets.</td></tr>
            {/each}
          </tbody>
        </table>
      </div>
      <p class="note faint">
        Each dataset has its own block cache and result cache, each sized to the server-wide limit.
      </p>
    {:else if metricsError}
      <div class="panel-body faint">Metrics are not available.</div>
    {:else}
      <div class="panel-body faint">Loading…</div>
    {/if}
    {#if limits}
      <dl class="limits">
        <div>
          <dt>Query timeout</dt>
          <dd>{limits.timeoutSeconds ? `${limits.timeoutSeconds} s` : 'none'}</dd>
        </div>
        <div>
          <dt>Query memory</dt>
          <dd>{limit(limits.queryMemoryBytes)}</dd>
        </div>
        <div>
          <dt>Result size</dt>
          <dd>{limit(limits.maxResultBytes)}</dd>
        </div>
        <div>
          <dt>Intermediate rows</dt>
          <dd>{limits.maxRows ? fmtInt(limits.maxRows) : 'unlimited'}</dd>
        </div>
      </dl>
    {/if}
  </section>

  {#if info}
    <section class="panel">
      <div class="panel-head"><h2>Endpoints</h2></div>
      <table class="data">
        <thead
          ><tr><th>Dataset</th><th>Query</th><th>Update</th><th>Graph store</th><th>Upload</th></tr
          ></thead
        >
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
    <div class="panel-body"><TaskList limit={25} refreshKey={taskRefresh} /></div>
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
  .small {
    font-size: var(--fs-sm);
  }
  td.actions {
    text-align: right;
    white-space: nowrap;
    width: 1%;
  }
  td.bad {
    color: var(--danger);
  }
  .note {
    margin: 0;
    padding: 8px 14px 10px;
    font-size: var(--fs-sm);
    border-top: 1px solid var(--border);
  }
  .scroll-x {
    overflow-x: auto;
  }
  .meter-cell {
    display: flex;
    align-items: center;
    gap: 8px;
    white-space: nowrap;
  }
  .meter {
    display: inline-block;
    width: 90px;
    height: 6px;
    border-radius: 3px;
    background: var(--surface-3);
    overflow: hidden;
    flex: none;
  }
  .meter span {
    display: block;
    height: 100%;
    background: var(--spark);
  }
  .limits {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    margin: 0;
    border-top: 1px solid var(--border);
  }
  .limits > div {
    padding: 10px 14px;
    border-right: 1px solid var(--border);
  }
  .limits > div:last-child {
    border-right: 0;
  }
  .limits dd {
    font-size: var(--fs);
  }
  @media (max-width: 760px) {
    .page {
      padding: 16px;
    }
    .kv,
    .limits {
      grid-template-columns: 1fr 1fr;
    }
    section.panel {
      overflow-x: auto;
    }
  }
</style>

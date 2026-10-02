<script lang="ts">
  // The dataset page's write-time validation panel (`GET /$/validation/{ds}`): the mode
  // and language, the validation state of the head with its result counts, how shapes
  // are validated on a write, the last validated write, the decision counters and the
  // last rejected writes. Configuration stays with `sparkles validation` and
  // `PUT /$/validation/{ds}`.
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { fmtInt, fmtMs, fmtRelative } from '$lib/format';
  import { displayIri, type PrefixMap } from '$lib/rdf';
  import { LatestRun } from '$lib/supersede';
  import { baselineBadge, checkLine, fallbackText, firstResult } from '$lib/write-validation';
  import Icon from './Icon.svelte';

  let {
    name,
    prefixes,
    refreshKey = 0,
  }: {
    name: string;
    prefixes: PrefixMap;
    /** Bump to reload the status (data changed). */
    refreshKey?: number;
  } = $props();

  type Loaded = { kind: 'on'; v: api.WriteValidation } | { kind: 'off' } | { kind: 'unsupported' };

  let loaded = $state<Loaded | null>(null);
  let error = $state<string | null>(null);
  let now = $state(Date.now());
  const runs = new LatestRun();

  async function load() {
    const owns = runs.claim('validation');
    try {
      const v = await api.writeValidation(name);
      if (!owns()) return;
      loaded = v ? { kind: 'on', v } : { kind: 'off' };
      error = null;
    } catch (e) {
      if (!owns()) return;
      if (e instanceof api.ApiError && (e.status === 404 || e.status === 501)) {
        loaded = { kind: 'unsupported' };
      } else error = api.errorMessage(e);
    }
  }

  $effect(() => {
    void name;
    void refreshKey;
    void load();
  });

  onMount(() => {
    // other clients write too: follow the counters while the page is open
    const t = setInterval(() => {
      now = Date.now();
      if (loaded?.kind === 'on' && document.visibilityState === 'visible') void load();
    }, 10_000);
    return () => clearInterval(t);
  });

  const v = $derived(loaded?.kind === 'on' ? loaded.v : null);
  const st = $derived(v?.status ?? null);
  const badge = $derived(st ? baselineBadge(st.baseline) : null);
  const iri = (s: string) => {
    const x = s.startsWith('<') && s.endsWith('>') ? s.slice(1, -1) : s;
    return /^[a-z][a-z0-9+.-]*:/i.test(x) ? displayIri(x, prefixes) : x;
  };
  const counters = $derived(
    st
      ? (['passed', 'warned', 'rejected', 'skipped', 'bypassed'] as const).map(
          (k) => [k, st.counters[k] ?? 0] as const,
        )
      : [],
  );
</script>

<section class="panel">
  <div class="panel-head">
    <h2>Write-time validation</h2>
    <span class="spacer"></span>
    {#if v}
      <span class="badge {v.config.mode === 'reject' ? 'iri' : ''}" title="Mode">
        {v.config.mode}
      </span>
      <span class="badge">{v.language === 'shex' ? 'ShEx' : 'SHACL'}</span>
      {#if badge}
        <span class="badge {badge.cls}">
          <Icon name={badge.cls === 'ok' ? 'check' : 'alert'} size={12} />
          {badge.label}
        </span>
      {/if}
    {/if}
  </div>
  <div class="panel-body wv">
    {#if error}
      <p class="faint">Could not load the validation status: {error}</p>
    {:else if !loaded}
      <p class="faint"><span class="spinner"></span> Loading…</p>
    {:else if loaded.kind === 'unsupported'}
      <p class="faint">This server does not offer write-time validation.</p>
    {:else if loaded.kind === 'off'}
      <p class="faint">
        Writes are not validated. Turn validation on with <code>sparkles validation</code> or
        <code>PUT /$/validation/{name}</code>.
      </p>
    {:else if v && st}
      <dl class="facts">
        <div>
          <dt>Results at the head</dt>
          <dd>
            {#if st.baseline && st.baseline.conforms !== null}
              {fmtInt(st.baseline.bySeverity.violation)} violations ·
              {fmtInt(st.baseline.bySeverity.warning)} warnings ·
              {fmtInt(st.baseline.bySeverity.info)} infos
              <span class="faint">at commit {st.baseline.commit}</span>
            {:else}
              <span class="faint">Unknown: the next validated write runs a full validation</span>
            {/if}
          </dd>
        </div>
        <div>
          <dt>Decides on</dt>
          <dd>
            {#if v.language === 'shex'}
              every nonconformant association
            {:else}
              results at or above <strong>{v.config.threshold ?? 'violation'}</strong
              >{#if v.config.baseline === 'grandfather'}, new ones only (grandfather){/if}
            {/if}
          </dd>
        </div>
        <div>
          <dt>Shapes</dt>
          <dd>
            {fmtInt(st.shapeCount)}
            {#if st.incremental}
              <span class="faint">
                · {fmtInt(st.incremental.localShapes)} validated incrementally
                {#if st.incremental.fullShapes.length}
                  · {fmtInt(st.incremental.fullShapes.length)} in full on every write
                {/if}
              </span>
            {:else if st.associations != null}
              <span class="faint">· {fmtInt(st.associations)} associations</span>
            {/if}
          </dd>
        </div>
        <div>
          <dt>Last full validation</dt>
          <dd>{st.lastFullMillis != null ? fmtMs(st.lastFullMillis) : '—'}</dd>
        </div>
        {#if st.lastCheck}
          <div>
            <dt>Last validated write</dt>
            <dd>
              <span
                class="badge sm {st.lastCheck.status === 'rejected'
                  ? 'danger'
                  : st.lastCheck.status === 'warned'
                    ? 'warn'
                    : 'ok'}">{st.lastCheck.status}</span
              >
              {checkLine(st.lastCheck)}
              <span class="faint">· {fmtRelative(st.lastCheck.time, now)}</span>
            </dd>
          </div>
        {/if}
        <div>
          <dt>Since the server started</dt>
          <dd class="counters">
            {#each counters as [k, n] (k)}
              <span><strong>{fmtInt(n)}</strong> {k}</span>
            {/each}
          </dd>
        </div>
      </dl>
      {#if st.incremental?.fullShapes.length}
        <details class="full">
          <summary class="faint">Shapes validated in full on every write</summary>
          <ul>
            {#each st.incremental.fullShapes as f (f.shape)}
              <li>
                <code>{iri(f.shape)}</code> <span class="faint">({fallbackText(f.reason)})</span>
              </li>
            {/each}
          </ul>
        </details>
      {/if}
      {#if st.warnings.length}
        <ul class="warnings">
          {#each st.warnings as w (w)}
            <li><Icon name="alert" size={12} /> {w}</li>
          {/each}
        </ul>
      {/if}
      {#if st.recentRejections?.length}
        <div class="table-wrap">
          <table class="table">
            <caption class="faint">Recently rejected writes</caption>
            <thead>
              <tr><th>When</th><th>Write</th><th>Blocking</th><th>First result</th></tr>
            </thead>
            <tbody>
              {#each st.recentRejections as r, i (i)}
                {@const f = firstResult(r)}
                <tr>
                  <td title={r.time}>{fmtRelative(r.time, now)}</td>
                  <td>{r.kind} <span class="faint">({r.strategy})</span></td>
                  <td>
                    {fmtInt(r.introduced ?? r.blocking)}{#if r.introduced != null}
                      <span class="faint"> new</span>{/if}
                  </td>
                  <td class="first">
                    {#if f}<code>{iri(f.shape)}</code> at <code>{iri(f.node)}</code>{:else}—{/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    {/if}
  </div>
</section>

<style>
  .wv {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 10px;
    font-size: var(--fs-sm);
  }
  .facts {
    display: grid;
    gap: 6px;
    margin: 0;
  }
  .facts > div {
    display: grid;
    grid-template-columns: 11rem minmax(0, 1fr);
    gap: 8px;
  }
  .facts dt {
    color: var(--muted);
  }
  .facts dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .counters {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
  }
  .badge.sm {
    font-size: var(--fs-xs, 11px);
    padding: 0 6px;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .warnings,
  .full ul {
    margin: 0;
    padding-left: 1.1rem;
  }
  .warnings {
    list-style: none;
    padding: 0;
    color: var(--warn);
  }
  .table-wrap {
    overflow-x: auto;
  }
  caption {
    text-align: left;
    padding-bottom: 4px;
  }
  .first {
    overflow-wrap: anywhere;
  }
  @media (max-width: 760px) {
    .facts > div {
      grid-template-columns: minmax(0, 1fr);
      gap: 0;
    }
  }
</style>

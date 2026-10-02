<script lang="ts">
  // The dataset page's spatial index (GeoSPARQL) panel: state, rows, skipped literals,
  // the CRSs, memory against the budget, the configuration and the last build; Enable,
  // Configure, Rebuild and Disable (builds are `geo-index` tasks), and a map of the
  // indexed geometries in view (`GET /{ds}/geo`).
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { fmtBytes, fmtInt, fmtMs, fmtRelative, fmtTime } from '$lib/format';
  import { crsLabel, GEO } from '$lib/geo';
  import { displayIri, type PrefixMap } from '$lib/rdf';
  import { LatestRun } from '$lib/supersede';
  import { parsePredicateList } from '$lib/textsearch';
  import { poll } from '$lib/poll';
  import Icon from './Icon.svelte';
  import MapView, { type MapFeature } from './MapView.svelte';
  import Modal from './Modal.svelte';

  let {
    name,
    prefixes,
    readOnly = false,
    busy = false,
    refreshKey = 0,
    onstart,
    onstarted,
    onchanged,
  }: {
    name: string;
    prefixes: PrefixMap;
    readOnly?: boolean;
    /** Another action is starting; disables the task buttons. */
    busy?: boolean;
    /** Bump to reload the status (a task finished, data changed). */
    refreshKey?: number;
    /** Start a task with a toast (the page's task runner). */
    onstart: (label: string, fn: () => Promise<api.Task>) => Promise<void>;
    /** A task was started from the configuration dialog. */
    onstarted: (t: api.Task) => void;
    /** Something changed on the server: refresh the dataset. */
    onchanged: () => void;
  } = $props();

  const DEFAULT_PREDICATES = [`${GEO}asWKT`, `${GEO}asGeoJSON`, `${GEO}hasSerialization`];
  const DEFAULT_LINKS = [`${GEO}hasDefaultGeometry`, `${GEO}hasGeometry`];

  type Loaded =
    | { kind: 'enabled'; status: api.GeoStatus }
    | { kind: 'disabled' }
    | { kind: 'unsupported' };

  let loaded = $state<Loaded | null>(null);
  let error = $state<string | null>(null);
  let now = $state(Date.now());
  const runs = new LatestRun();

  async function load() {
    const owns = runs.claim('geo');
    try {
      const s = await api.geoStatus(name);
      if (!owns()) return;
      loaded = s ? { kind: 'enabled', status: s } : { kind: 'disabled' };
      error = null;
    } catch (e) {
      if (!owns()) return;
      if (e instanceof api.ApiError && e.status === 501) loaded = { kind: 'unsupported' };
      else error = api.errorMessage(e);
    }
  }

  $effect(() => {
    void name;
    void refreshKey;
    void load();
  });

  const status = $derived(loaded?.kind === 'enabled' ? loaded.status : null);

  let panel = $state<HTMLElement>();

  onMount(() => {
    // relative times only: no request
    const t = setInterval(() => (now = Date.now()), 2000);
    // follow a build while the panel is on screen
    const p = poll(load, {
      interval: 2000,
      when: () => status?.state === 'building',
      target: () => panel,
      immediate: false,
    });
    return () => {
      clearInterval(t);
      p.stop();
    };
  });

  const stateClass = (s: api.GeoState) =>
    s === 'ready' ? 'ok' : s === 'building' ? 'warn' : 'danger';
  const rowsTotal = $derived(
    status ? status.rows.base + status.rows.overlay + status.rows.tail : 0,
  );
  const used = $derived(
    status ? status.memory.treeBytes + status.memory.geometryBytes + status.memory.overlayBytes : 0,
  );
  const skipped = $derived(
    status
      ? (
          [
            ['malformed', status.skipped.malformed],
            ['unknown CRS', status.skipped.unknownCrs],
            ['too large', status.skipped.tooLarge],
            ['empty', status.skipped.empty],
          ] as const
        ).filter(([, n]) => n > 0)
      : [],
  );
  const crsList = $derived(
    status
      ? Object.entries(status.crs).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
      : [],
  );
  const crsMax = $derived(Math.max(1, ...crsList.map(([, n]) => n)));

  async function rebuild() {
    await onstart('Spatial index rebuild', () => api.geoRebuild(name));
    void load();
  }

  // --- configure (enable / reconfigure) -------------------------------------------
  let configOpen = $state(false);
  let predText = $state('');
  let linkText = $state('');
  let wgs84 = $state(false);
  let queryRewrite = $state(false);
  let distance = $state<'geodesic' | 'haversine'>('geodesic');
  let saving = $state(false);
  let configError = $state<string | null>(null);

  const lines = (iris: string[]) => iris.map((p) => displayIri(p, prefixes)).join('\n');

  function openConfig() {
    const c = status?.config;
    predText = lines(c?.predicates ?? DEFAULT_PREDICATES);
    linkText = lines(c?.featureLinks ?? DEFAULT_LINKS);
    wgs84 = c?.wgs84 ?? false;
    queryRewrite = c?.queryRewrite ?? false;
    distance = c?.distance ?? 'geodesic';
    configError = null;
    configOpen = true;
  }

  const preds = $derived(parsePredicateList(predText, prefixes));
  const links = $derived(parsePredicateList(linkText, prefixes));
  const configValid = $derived(
    preds.iris.length > 0 && preds.bad.length === 0 && links.bad.length === 0,
  );

  async function saveConfig(e: Event) {
    e.preventDefault();
    if (!configValid) return;
    saving = true;
    configError = null;
    // keep the settings this dialog does not edit
    const config: api.GeoConfig = {
      ...(status?.config ?? {}),
      predicates: preds.iris,
      featureLinks: links.iris,
      wgs84,
      queryRewrite,
      distance,
    };
    try {
      const t = await api.geoConfigure(name, config);
      toasts.push(
        'info',
        status ? 'Reconfiguring the spatial index' : 'Enabling the spatial index',
        t?.id ? `Building the index (task ${t.id})` : undefined,
      );
      onstarted(t);
      configOpen = false;
      void load();
    } catch (err) {
      configError = api.errorMessage(err);
    } finally {
      saving = false;
    }
  }

  // --- disable -----------------------------------------------------------------------
  let disableOpen = $state(false);
  let disabling = $state(false);

  async function disable() {
    disabling = true;
    try {
      await api.geoDisable(name);
      toasts.push('success', 'Spatial index disabled', 'The index was deleted.');
      disableOpen = false;
      mapOpen = false;
      onchanged();
      void load();
    } catch (e) {
      toasts.error('Could not disable the spatial index', e);
    } finally {
      disabling = false;
    }
  }

  // --- map of the indexed geometries -------------------------------------------------
  let mapOpen = $state(false);
  let boxed = $state<api.GeoFeatureCollection | null>(null);
  let boxError = $state<string | null>(null);
  let boxTimer: ReturnType<typeof setTimeout> | undefined;
  const boxRuns = new LatestRun();

  function view(bbox: [number, number, number, number]) {
    clearTimeout(boxTimer);
    boxTimer = setTimeout(async () => {
      const owns = boxRuns.claim('box');
      try {
        const fc = await api.geoBox(name, { bbox, limit: 2000 });
        if (!owns()) return;
        boxed = fc;
        boxError = null;
      } catch (e) {
        if (owns()) boxError = api.errorMessage(e);
      }
    }, 250);
  }

  const boxFeatures = $derived(
    (boxed?.features ?? []).map((f, i): MapFeature => ({ geometry: f.geometry, group: i })),
  );
</script>

{#snippet boxPopup(i: number)}
  {@const f = boxed?.features[i]}
  {#if f}
    <div class="popup mono">
      {#if f.properties.feature}<div title={f.properties.feature}>
          {displayIri(f.properties.feature, prefixes)}
        </div>{/if}
      <div class="faint" title={f.properties.subject}>
        {displayIri(f.properties.subject, prefixes)}
        {displayIri(f.properties.predicate, prefixes)}
      </div>
    </div>
  {/if}
{/snippet}

<section class="panel" bind:this={panel}>
  <div class="panel-head">
    <h2>Spatial index</h2>
    <span class="spacer"></span>
    {#if status}
      <span class="badge {stateClass(status.state)}" title={status.message ?? undefined}>
        <Icon name={status.state === 'ready' ? 'check' : 'alert'} size={12} />
        {status.state}{status.state === 'building' && status.progress != null
          ? ` ${Math.round(status.progress * 100)}%`
          : ''}
      </span>
    {:else if loaded?.kind === 'disabled'}
      <span class="badge">off</span>
    {/if}
  </div>
  <div class="panel-body geo">
    {#if error && !loaded}
      <div class="error-box">
        <strong>Could not load the spatial index status.</strong>
        <span class="muted">{error}</span>
      </div>
    {:else if !loaded}
      <div class="row faint"><span class="spinner"></span> Loading…</div>
    {:else if loaded.kind === 'unsupported'}
      <p class="faint">
        This server was built without GeoSPARQL (the <span class="mono">geo</span> feature).
      </p>
    {:else if loaded.kind === 'disabled'}
      <p class="faint">
        The spatial index is off for this dataset. GeoSPARQL functions and
        <span class="mono">spatial:</span> property functions still work, by scanning; the index makes
        them fast on large data.
      </p>
      <div class="row">
        <span class="spacer"></span>
        <button
          class="btn primary"
          onclick={openConfig}
          disabled={readOnly}
          title={readOnly ? 'The server is read-only' : undefined}
          ><Icon name="map" size={14} /> Enable…</button
        >
      </div>
    {:else if status}
      <dl class="facts">
        <div>
          <dt>Rows</dt>
          <dd>{fmtInt(rowsTotal)}</dd>
        </div>
        <div>
          <dt title="Distinct parsed geometries">Geometries</dt>
          <dd>{fmtInt(status.literals)}</dd>
        </div>
        <div>
          <dt>Memory</dt>
          <dd>{fmtBytes(used)}</dd>
        </div>
        <div>
          <dt title="The commit the status describes">At commit</dt>
          <dd>{status.commit}</dd>
        </div>
      </dl>
      <p class="faint small">
        base {fmtInt(status.rows.base)} · overlay {fmtInt(status.rows.overlay)} · tail {fmtInt(
          status.rows.tail,
        )}{#if status.rows.wgs84}
          · {fmtInt(status.rows.wgs84)} Basic Geo points{/if}
      </p>
      {#if status.message}<p class="faint small">{status.message}</p>{/if}
      <div class="mem" title="Tree, geometries and overlay against the index's memory budget">
        <div class="bar">
          <span
            style:width="{Math.min(100, (used / Math.max(1, status.memory.budgetBytes)) * 100)}%"
          ></span>
        </div>
        <span class="faint small"
          >{fmtBytes(used)} of {fmtBytes(status.memory.budgetBytes)}{#if status.memory.mappedBytes}
            · {fmtBytes(status.memory.mappedBytes)} read in place from the index files{/if}</span
        >
      </div>
      <div class="kv">
        <span class="faint">Skipped</span>
        <span>
          {#if skipped.length}
            {#each skipped as [what, n] (what)}<span class="chip warn">{fmtInt(n)} {what}</span
              >{/each}
          {:else}
            <span class="faint">none</span>
          {/if}
        </span>
        {#if crsList.length}
          <span class="faint">CRSs</span>
          <span class="crs">
            {#each crsList as [iri, n] (iri)}
              <span class="crs-row" title={iri}>
                <span class="crs-bar" style:width="{(n / crsMax) * 100}%"></span>
                <span class="mono">{crsLabel(iri)}</span>
                <span class="num">{fmtInt(n)}</span>
              </span>
            {/each}
          </span>
        {/if}
        <span class="faint">Predicates</span>
        <span>
          {#each status.config.predicates ?? DEFAULT_PREDICATES as p (p)}<span
              class="chip t-iri mono"
              title={p}>{displayIri(p, prefixes)}</span
            >{/each}
        </span>
        <span class="faint">Feature links</span>
        <span>
          {#each status.config.featureLinks ?? DEFAULT_LINKS as p (p)}<span
              class="chip t-iri mono"
              title={p}>{displayIri(p, prefixes)}</span
            >{/each}
        </span>
        <span class="faint">Options</span>
        <span class="small">
          {status.config.distance ?? 'geodesic'} distances{status.config.wgs84
            ? ' · Basic Geo points'
            : ''}{status.config.queryRewrite ? ' · query rewrite' : ''}
        </span>
        <span class="faint">Last build</span>
        <span>
          {#if status.lastBuild}
            <span title={fmtTime(status.lastBuild.at)}>{fmtRelative(status.lastBuild.at, now)}</span
            >
            <span class="faint"
              >· {fmtInt(status.lastBuild.rows)} rows in {fmtMs(status.lastBuild.ms)}</span
            >
          {:else}
            <span class="faint">—</span>
          {/if}
          {#if status.files}
            <span class="faint"
              >· files {fmtBytes(status.files.bytes)}{status.files.opened
                ? ', opened without a build'
                : ''}</span
            >
          {/if}
        </span>
      </div>
      <div class="row actions">
        <button
          class="btn sm"
          aria-pressed={mapOpen}
          onclick={() => (mapOpen = !mapOpen)}
          title="Show the indexed geometries on a map"><Icon name="map" size={13} /> Map</button
        >
        <span class="spacer"></span>
        <button
          class="btn sm"
          onclick={() => (disableOpen = true)}
          disabled={readOnly}
          title="Delete the index"><Icon name="trash" size={13} /> Disable</button
        >
        <button class="btn sm" onclick={openConfig} disabled={readOnly || busy}
          ><Icon name="filter" size={13} /> Configure…</button
        >
        <button
          class="btn sm"
          onclick={rebuild}
          disabled={readOnly || busy || status.state === 'building'}
          title="Rebuild the index from the current data"
          ><Icon name="refresh" size={13} /> Rebuild</button
        >
      </div>
      {#if mapOpen}
        <MapView
          features={boxFeatures}
          fit={false}
          onview={view}
          popup={boxPopup}
          height="300px"
          label="Indexed geometries"
        />
        {#if boxError}
          <div class="error-box small">{boxError}</div>
        {:else if boxed}
          <p class="faint small">
            {fmtInt(boxed.features.length)} geometries in view{boxed.truncated
              ? ' (more than shown: zoom in)'
              : ''}
          </p>
        {/if}
      {/if}
    {/if}
  </div>
</section>

<Modal
  bind:open={configOpen}
  title={status ? 'Configure the spatial index' : 'Enable the spatial index'}
  width={520}
>
  <form id="geo-config" class="form" onsubmit={saveConfig}>
    <label class="field">
      Serialization predicates <span class="faint">(one per line)</span>
      <textarea
        class="textarea mono"
        rows="3"
        bind:value={predText}
        spellcheck="false"
        aria-invalid={preds.bad.length > 0}></textarea>
    </label>
    <label class="field">
      Feature links <span class="faint">(feature → geometry)</span>
      <textarea
        class="textarea mono"
        rows="2"
        bind:value={linkText}
        spellcheck="false"
        aria-invalid={links.bad.length > 0}></textarea>
    </label>
    {#if preds.bad.length || links.bad.length}
      <p class="bad">
        Not an IRI or known prefixed name: {[...preds.bad, ...links.bad].join(', ')}
      </p>
    {/if}
    <label class="check">
      <input type="checkbox" bind:checked={wgs84} />
      <span
        >Index W3C Basic Geo points <span class="faint"
          >(<span class="mono">wgs84_pos:lat</span>/<span class="mono">long</span> on one subject)</span
        ></span
      >
    </label>
    <label class="check">
      <input type="checkbox" bind:checked={queryRewrite} />
      <span
        >Query rewrite <span class="faint"
          >(topological properties such as <span class="mono">geo:sfWithin</span> between features)</span
        ></span
      >
    </label>
    <label class="field inline">
      Distances
      <select class="select" bind:value={distance}>
        <option value="geodesic">geodesic (WGS 84 ellipsoid)</option>
        <option value="haversine">haversine (sphere)</option>
      </select>
    </label>
    <p class="faint small">
      The index is built by a background task; queries run without it (by scanning) meanwhile.
    </p>
    {#if configError}<div class="error-box">{configError}</div>{/if}
  </form>
  {#snippet actions()}
    <button class="btn" type="button" onclick={() => (configOpen = false)}>Cancel</button>
    <button class="btn primary" type="submit" form="geo-config" disabled={saving || !configValid}>
      {#if saving}<span class="spinner"></span>{/if}
      {status ? 'Save and rebuild' : 'Enable'}
    </button>
  {/snippet}
</Modal>

<Modal bind:open={disableOpen} title="Disable the spatial index?">
  <p>
    This deletes the spatial index of <span class="mono">{name}</span> and its files. GeoSPARQL queries
    keep working, by scanning. The data is not changed.
  </p>
  {#snippet actions()}
    <button class="btn" type="button" onclick={() => (disableOpen = false)}>Cancel</button>
    <button class="btn danger" type="button" onclick={disable} disabled={disabling}>
      {#if disabling}<span class="spinner"></span>{:else}<Icon name="trash" size={13} />{/if}
      Disable and delete index
    </button>
  {/snippet}
</Modal>

<style>
  .geo {
    display: grid;
    gap: 10px;
    font-size: var(--fs-sm);
  }
  .geo > p {
    margin: 0;
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 8px;
    margin: 0;
  }
  .facts dt {
    color: var(--text-2);
    font-size: var(--fs-xs);
  }
  .facts dd {
    margin: 2px 0 0;
    font-size: var(--fs-md);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .mem {
    display: grid;
    gap: 4px;
  }
  .mem .bar {
    height: 6px;
    border-radius: 3px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .mem .bar span {
    display: block;
    height: 100%;
    background: var(--iri);
  }
  .kv {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 14px;
    align-items: baseline;
  }
  .chip {
    display: inline-block;
    margin: 0 4px 3px 0;
    padding: 1px 7px;
    border-radius: 10px;
    font-size: 11px;
    background: color-mix(in srgb, var(--iri) 10%, transparent);
  }
  .chip.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .crs {
    display: grid;
    gap: 2px;
  }
  .crs-row {
    position: relative;
    display: flex;
    gap: 8px;
    padding: 1px 6px;
    font-size: 11px;
  }
  .crs-row .num {
    margin-left: auto;
    font-variant-numeric: tabular-nums;
  }
  .crs-bar {
    position: absolute;
    left: 0;
    top: 1px;
    bottom: 1px;
    border-radius: 3px;
    background: color-mix(in srgb, var(--literal) 14%, transparent);
    pointer-events: none;
  }
  .small {
    font-size: 11px;
  }
  .actions {
    flex-wrap: wrap;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .form {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
  }
  .form p {
    margin: 0;
  }
  .field.inline {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .check {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    font-size: var(--fs-sm);
  }
  .check input {
    margin-top: 2px;
    accent-color: var(--iri);
  }
  .textarea {
    font-size: 12px;
    resize: vertical;
  }
  .bad {
    color: var(--danger);
    font-size: var(--fs-sm);
  }
  .popup {
    display: grid;
    gap: 2px;
    font-size: 11px;
  }
  @media (max-width: 760px) {
    .facts {
      grid-template-columns: 1fr 1fr;
    }
  }
</style>

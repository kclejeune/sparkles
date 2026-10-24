<script lang="ts">
  // The Arcs mode of a result's Map tab (docs/specs/U11-linked-views.md, section 2.2): one
  // arc per row, from a first column to a second, labelled by a third when one is chosen.
  // The columns hold geometries or entities that have geometries.
  import * as api from '$lib/api';
  import { capPlaces, MAX_ARCS, type Link, type Place } from '$lib/arcs';
  import { fmtInt } from '$lib/format';
  import {
    arcColumns,
    defaultArcColumns,
    locateEndsCompared,
    resultLinks,
    type EndPositions,
  } from '$lib/result-arcs';
  import type { PrefixMap } from '$lib/rdf';
  import type { Mark } from '$lib/rowdiff';
  import ArcMap from './ArcMap.svelte';

  let {
    vars,
    rows,
    prefixes,
    ds,
    at = '',
    baseDs,
    baseAt = '',
    marks,
    onopen,
  }: {
    vars: string[];
    rows: (api.Term | null)[][];
    prefixes: PrefixMap;
    /** The dataset and branch the result came from (`ds@branch`). */
    ds: string;
    /** The past state the result was read at (`commit:42`), empty for the head. */
    at?: string;
    /** In compare mode, where the left side was read, for the entities of removed rows. */
    baseDs?: string;
    baseAt?: string;
    marks?: readonly Mark[];
    onopen?: (iri: string) => void;
  } = $props();

  const candidates = $derived(arcColumns(vars, rows));
  let from = $state('');
  let to = $state('');
  let label = $state('');
  $effect(() => {
    const d = defaultArcColumns(vars, rows);
    from = d?.from ?? '';
    to = d?.to ?? '';
    label = '';
  });
  /** The columns for a label: any other column. */
  const labelOptions = $derived(vars.filter((v) => v !== from && v !== to));

  const built = $derived(
    resultLinks(vars, rows, { from, to, label: label || undefined, prefixes, marks }),
  );

  let located = $state<EndPositions | null>(null);
  let progress = $state('');
  let selectedRow = $state<number | null>(null);
  let listShown = $state(100);

  $effect(() => {
    const ends = built.ends;
    const ctl = new AbortController();
    located = null;
    selectedRow = null;
    listShown = 100;
    if (!ends.size) {
      progress = '';
      return;
    }
    progress = `Locating ${fmtInt(ends.size)} ${ends.size === 1 ? 'end' : 'ends'}…`;
    const deps = (target: string, state: string) => ({
      run: (sparql: string, limit: number) =>
        api.select(target, sparql, { at: state || undefined, send: limit, signal: ctl.signal }),
      convert: async (batch: Parameters<typeof api.geoConvert>[0]) =>
        (await api.geoConvert(batch, ctl.signal)).results,
      signal: ctl.signal,
    });
    locateEndsCompared(
      ends,
      built.links,
      {
        ...deps(ds, at),
        onBatch: (_, done, total) => {
          if (!ctl.signal.aborted) progress = `Locating ${fmtInt(done)} of ${fmtInt(total)}…`;
        },
      },
      baseDs ? deps(baseDs, baseAt) : undefined,
    )
      .then((r) => {
        if (ctl.signal.aborted) return;
        located = r;
        progress = '';
      })
      .catch((e) => {
        if (ctl.signal.aborted) return;
        located = { places: [], missing: [], failed: ends.size, firstError: api.errorMessage(e) };
        progress = '';
      });
    return () => ctl.abort();
  });

  const places = $derived<Place[]>(located?.places ?? []);
  const links = $derived.by((): Link[] => (located ? built.links : []));
  const notLocated = $derived(
    (located?.missing ?? []).map((m) => ({
      id: m.id,
      label: m.why ? `${m.label} (${m.why})` : m.label,
      iri: m.iri,
    })),
  );
  /** Rows whose two ends both have a position, in order. */
  const drawnRows = $derived.by(() => {
    if (!located) return [];
    const have = new Set(capPlaces(located.places).map((p) => p.id));
    return built.links.filter((l) => have.has(l.from) && have.has(l.to));
  });
  const caption = (l: Link) => {
    const a = built.ends.get(l.from)?.label ?? '';
    const b = built.ends.get(l.to)?.label ?? '';
    return `${a} → ${b}`;
  };
</script>

<div class="result-arcs">
  <div class="opts">
    <label class="col">
      <span class="faint">From</span>
      <select bind:value={from} aria-label="Column the arcs start in">
        {#each candidates as c (c)}<option value={c}>?{c}</option>{/each}
      </select>
    </label>
    <label class="col">
      <span class="faint">To</span>
      <select bind:value={to} aria-label="Column the arcs end in">
        {#each candidates as c (c)}<option value={c}>?{c}</option>{/each}
      </select>
    </label>
    <label class="col">
      <span class="faint">Label</span>
      <select bind:value={label} aria-label="Column that labels the arcs">
        <option value="">none</option>
        {#each labelOptions as c (c)}<option value={c}>?{c}</option>{/each}
      </select>
    </label>
    <span class="spacer"></span>
    <span class="faint">
      {fmtInt(drawnRows.length)} of {fmtInt(rows.length)} rows drawn{#if built.skipped}, {fmtInt(
          built.skipped,
        )} without both ends{/if}{#if built.dropped}, the first {fmtInt(
          rows.length - built.dropped,
        )}{/if}
    </span>
  </div>
  <div class="body">
    <ArcMap
      {places}
      {links}
      {notLocated}
      loading={progress}
      fitKey={`${from}|${to}|${rows.length}`}
      {selectedRow}
      {onopen}
      height="100%"
      label="Result arcs"
      emptyText={from === to
        ? 'Choose two different columns'
        : 'None of these entities has a geometry'}
    />
    <div class="rows" aria-label="Rows with arcs">
      {#each drawnRows.slice(0, listShown) as l (l.row)}
        <button
          class="row-item"
          class:sel={selectedRow === l.row}
          aria-pressed={selectedRow === l.row}
          onclick={() => (selectedRow = selectedRow === l.row ? null : (l.row ?? null))}
        >
          <span class="n faint">{(l.row ?? 0) + 1}</span>
          {#if l.change === 'added'}
            <span class="plus" title="Added" aria-label="added">+</span>
          {:else if l.change === 'removed'}
            <span class="minus" title="Removed" aria-label="removed">−</span>
          {/if}
          <span class="mono label"
            >{#if l.label}{l.label}&ensp;<span class="faint">{caption(l)}</span>{:else}{caption(
                l,
              )}{/if}</span
          >
        </button>
      {/each}
      {#if drawnRows.length > listShown}
        <button class="btn ghost sm" onclick={() => (listShown += 200)}
          >Show more ({fmtInt(drawnRows.length - listShown)} hidden)</button
        >
      {/if}
    </div>
  </div>
  {#if built.dropped}
    <p class="warn">
      The map draws the first {fmtInt(MAX_ARCS)} of {fmtInt(rows.length)} rows.
    </p>
  {/if}
  {#if located?.failed}
    <p class="warn">
      {fmtInt(located.failed)} positions could not be read{located.firstError
        ? `: ${located.firstError}`
        : ''}.
    </p>
  {/if}
</div>

<style>
  .result-arcs {
    display: flex;
    flex-direction: column;
    gap: 8px;
    height: 100%;
    min-height: 420px;
  }
  .opts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 14px;
    font-size: var(--fs-sm);
  }
  .col {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .col select {
    font-size: var(--fs-sm);
    max-width: 180px;
  }
  .body {
    flex: 1;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 240px;
    gap: 8px;
    min-height: 360px;
  }
  .rows {
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
    display: flex;
    flex-direction: column;
    max-height: 560px;
  }
  .row-item {
    all: unset;
    display: flex;
    gap: 8px;
    align-items: baseline;
    padding: 4px 8px;
    cursor: pointer;
    font-size: 12px;
    border-bottom: 1px solid var(--border);
  }
  .row-item:hover {
    background: var(--hover);
  }
  .row-item.sel {
    background: var(--spark-soft);
  }
  .row-item:focus-visible {
    box-shadow: var(--focus);
  }
  .row-item .n {
    min-width: 28px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .row-item .plus {
    color: var(--added);
    font-weight: 600;
  }
  .row-item .minus {
    color: var(--removed);
    font-weight: 600;
  }
  .row-item .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .warn {
    margin: 0;
    color: var(--warn);
    font-size: var(--fs-sm);
  }
  @media (max-width: 760px) {
    .body {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: auto 160px;
    }
    .col select {
      min-height: 44px;
    }
    .row-item {
      min-height: 44px;
      align-items: center;
    }
  }
</style>

<script lang="ts">
  // The Map view of a SELECT result: the geometry literals of its geometry columns on a
  // map, a popup per row with its other bindings, the rows to pick from (a click
  // highlights the row's geometries), and the literals that cannot be drawn.
  import * as api from '$lib/api';
  import { fmtInt } from '$lib/format';
  import { abbreviate, geoCells, isGeoLiteral, resolveGeometries, type Resolved } from '$lib/geo';
  import type { PrefixMap } from '$lib/rdf';
  import MapView, { type MapFeature } from './MapView.svelte';
  import TermView from './TermView.svelte';

  let {
    vars,
    rows,
    columns,
    prefixes,
    onopen,
  }: {
    vars: string[];
    rows: (api.Term | null)[][];
    /** The columns holding geometry literals. */
    columns: string[];
    prefixes: PrefixMap;
    onopen?: (iri: string) => void;
  } = $props();

  /** Most literals drawn; a larger result draws its first rows. */
  const MAX_DRAWN = 20_000;

  let shownColumns = $state<string[]>([]);
  $effect(() => {
    shownColumns = [...columns];
  });

  const cells = $derived(geoCells(vars, rows, shownColumns));
  const drawnCells = $derived(cells.slice(0, MAX_DRAWN));
  let resolved = $state<Resolved[] | null>(null);
  let selected = $state<number | null>(null);
  let listShown = $state(100);

  $effect(() => {
    const todo = drawnCells;
    const ctl = new AbortController();
    resolved = null;
    selected = null;
    listShown = 100;
    resolveGeometries(
      todo.map((c) => c.literal),
      async (batch) => (await api.geoConvert(batch, ctl.signal)).results,
    ).then((r) => {
      if (!ctl.signal.aborted) resolved = r;
    });
    return () => ctl.abort();
  });

  const features = $derived.by((): MapFeature[] => {
    if (!resolved) return [];
    const out: MapFeature[] = [];
    resolved.forEach((r, i) => {
      if ('geometry' in r) out.push({ geometry: r.geometry, group: drawnCells[i].row });
    });
    return out;
  });
  const undrawn = $derived(
    resolved
      ? resolved.flatMap((r, i) => ('error' in r ? [{ cell: drawnCells[i], error: r.error }] : []))
      : [],
  );
  /** Rows with something drawn, in order. */
  const drawnRows = $derived([...new Set(features.map((f) => f.group))].sort((a, b) => a - b));
  /** The bindings of a row other than geometries, for its label and popup. */
  const others = (row: number) =>
    vars.flatMap((v, i) => (isGeoLiteral(rows[row][i]) ? [] : [[v, rows[row][i]] as const]));
</script>

{#snippet rowPopup(row: number)}
  <div class="popup">
    <div class="faint">Row {row + 1}</div>
    <table>
      <tbody>
        {#each vars as v, i (v)}
          {@const t = rows[row][i]}
          <tr>
            <th class="mono">?{v}</th>
            <td class="mono">
              {#if isGeoLiteral(t)}<span class="t-lit" title={t.value}
                  >{abbreviate(t.value, 48)}</span
                >{:else}<TermView term={t} {prefixes} {onopen} />{/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/snippet}

<div class="result-map">
  <div class="opts">
    {#if columns.length > 1}
      <span class="faint">Columns</span>
      {#each columns as c (c)}
        <label class="check">
          <input type="checkbox" bind:group={shownColumns} value={c} />
          <span class="mono">?{c}</span>
        </label>
      {/each}
    {/if}
    <span class="spacer"></span>
    <span class="faint">
      {#if !resolved}
        <span class="spinner"></span> Reading {fmtInt(drawnCells.length)} geometries…
      {:else}
        {fmtInt(features.length)} drawn{undrawn.length
          ? `, ${fmtInt(undrawn.length)} not drawn`
          : ''}
        {#if cells.length > MAX_DRAWN}
          (the first {fmtInt(MAX_DRAWN)} of {fmtInt(cells.length)})
        {/if}
      {/if}
    </span>
  </div>
  <div class="body">
    <MapView
      {features}
      {selected}
      onselect={(g) => (selected = g)}
      popup={rowPopup}
      height="100%"
      label="Result map"
    />
    <div class="rows" aria-label="Rows on the map">
      {#each drawnRows.slice(0, listShown) as row (row)}
        {@const label =
          others(row).find(([, t]) => t?.type === 'literal') ?? others(row).find(([, t]) => t)}
        <button
          class="row-item"
          class:sel={selected === row}
          aria-pressed={selected === row}
          onclick={() => (selected = selected === row ? null : row)}
        >
          <span class="n faint">{row + 1}</span>
          <span class="mono label"
            >{#if label}<TermView term={label[1]} {prefixes} />{:else}row {row + 1}{/if}</span
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
  {#if undrawn.length}
    <details class="undrawn" open={undrawn.length <= 5}>
      <summary>
        {fmtInt(undrawn.length)} literal{undrawn.length === 1 ? '' : 's'} could not be drawn
      </summary>
      <ul>
        {#each undrawn.slice(0, 200) as u, i (i)}
          <li>
            <span class="faint">row {u.cell.row + 1}, ?{u.cell.column}:</span>
            <span class="mono t-lit" title={u.cell.literal.value}
              >{abbreviate(u.cell.literal.value)}</span
            >
            <span class="why">{u.error}</span>
          </li>
        {/each}
      </ul>
    </details>
  {/if}
</div>

<style>
  .result-map {
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto;
    gap: 8px;
    height: 100%;
    min-height: 420px;
    padding: 8px 12px 12px;
  }
  .opts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 14px;
    font-size: var(--fs-sm);
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .check input {
    accent-color: var(--iri);
    margin: 0;
  }
  .body {
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
  .row-item .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .undrawn {
    font-size: var(--fs-sm);
  }
  .undrawn summary {
    cursor: pointer;
    color: var(--warn);
  }
  .undrawn ul {
    margin: 6px 0 0;
    padding-left: 18px;
    display: grid;
    gap: 3px;
  }
  .undrawn .why {
    color: var(--text-2);
  }
  .popup {
    display: grid;
    gap: 4px;
  }
  .popup table {
    border-collapse: collapse;
    font-size: 12px;
  }
  .popup th {
    text-align: left;
    font-weight: 500;
    color: var(--text-2);
    padding: 1px 8px 1px 0;
    vertical-align: top;
  }
  .popup td {
    padding: 1px 0;
    max-width: 240px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  @media (max-width: 760px) {
    .body {
      grid-template-columns: 1fr;
      grid-template-rows: 320px 160px;
    }
  }
</style>

<script lang="ts" module>
  /** A feature near the resource (`spatial:nearbyGeom`). */
  export type NearbyHit = {
    term: import('$lib/api').Term;
    label?: string;
    /** Kilometres, when the feature has a geometry literal. */
    dist?: number;
    lit?: import('$lib/geo').GeoLiteral;
  };
</script>

<script lang="ts">
  // "Nearby" on the explorer's map card: the features within a radius of the resource's
  // geometry (`spatial:nearbyGeom`), nearest first.
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { shortLabel } from '$lib/graph';
  import { isGeoLiteral, nearbyQuery, type GeoLiteral } from '$lib/geo';
  import type { PrefixMap } from '$lib/rdf';
  import { LatestRun } from '$lib/supersede';
  import Icon from './Icon.svelte';
  import TermView from './TermView.svelte';

  let {
    ds,
    iri,
    literal,
    prefixes,
    selected = null,
    onresults,
    onselect,
  }: {
    ds: string;
    iri: string;
    /** The resource's geometry, the centre of the search. */
    literal: GeoLiteral;
    prefixes: PrefixMap;
    /** The highlighted hit (its index). */
    selected?: number | null;
    onresults?: (hits: NearbyHit[]) => void;
    onselect?: (index: number | null) => void;
  } = $props();

  const RADII = [1, 5, 10, 50, 100, 500, 1000];
  const LIMIT = 50;

  let radius = $state(10);
  let hits = $state<NearbyHit[] | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  const runs = new LatestRun();

  async function search() {
    const owns = runs.claim('nearby');
    loading = true;
    error = null;
    try {
      const rows = await api.select(ds, nearbyQuery(iri, literal, radius, LIMIT), { send: LIMIT });
      if (!owns()) return;
      hits = rows.flatMap((r) => {
        if (!r.f) return [];
        const dist = r.dist?.type === 'literal' ? Number(r.dist.value) : undefined;
        return [
          {
            term: r.f,
            label: r.label?.type === 'literal' ? r.label.value : undefined,
            dist: dist != null && isFinite(dist) ? dist : undefined,
            lit: isGeoLiteral(r.lit) ? { value: r.lit.value, datatype: r.lit.datatype } : undefined,
          },
        ];
      });
      onresults?.(hits);
    } catch (e) {
      if (!owns()) return;
      hits = null;
      onresults?.([]);
      error =
        e instanceof api.ApiError && e.status === 501
          ? 'This server was built without GeoSPARQL.'
          : api.errorMessage(e);
    } finally {
      if (owns()) loading = false;
    }
  }

  const href = (target: string) =>
    `${resolve('/explore')}?ds=${encodeURIComponent(ds)}&iri=${encodeURIComponent(target)}`;
  const km = (d: number) => (d < 10 ? d.toFixed(2) : d < 100 ? d.toFixed(1) : d.toFixed(0));
</script>

<div class="nearby">
  <div class="controls">
    <label class="ctl">
      <span class="faint">within</span>
      <select class="select sm" bind:value={radius} aria-label="Radius">
        {#each RADII as r (r)}<option value={r}>{r} km</option>{/each}
      </select>
    </label>
    <button class="btn sm" onclick={search} disabled={loading}>
      {#if loading}<span class="spinner"></span>{:else}<Icon name="target" size={13} />{/if} Nearby
    </button>
  </div>
  {#if error}
    <div class="error-box small">{error}</div>
  {:else if hits}
    {#if hits.length === 0}
      <p class="faint small">No other feature within {radius} km.</p>
    {:else}
      <table class="hits">
        <thead><tr><th>Feature</th><th class="num">km</th></tr></thead>
        <tbody>
          {#each hits as h, i (i)}
            <tr
              class:sel={selected === i}
              onclick={() => onselect?.(selected === i ? null : i)}
              title={h.lit ? 'Show on the map' : undefined}
            >
              <td class="ent">
                {#if h.term.type === 'uri'}
                  {@const target = h.term.value}
                  <a
                    class="t-iri"
                    href={href(target)}
                    title={target}
                    onclick={(e) => e.stopPropagation()}
                    >{h.label ?? shortLabel(target, prefixes)}</a
                  >
                {:else}
                  <TermView term={h.term} {prefixes} />
                {/if}
              </td>
              <td class="num mono">{h.dist != null ? km(h.dist) : '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
      {#if hits.length >= LIMIT}
        <p class="faint small">The {LIMIT} nearest.</p>
      {/if}
    {/if}
  {/if}
</div>

<style>
  .nearby {
    display: grid;
    gap: 6px;
    margin-top: 8px;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--fs-sm);
  }
  .ctl {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .select.sm {
    height: 24px;
    font-size: var(--fs-sm);
  }
  .hits {
    width: 100%;
    font-size: 12px;
    border-collapse: collapse;
  }
  .hits th {
    text-align: left;
    font-weight: 500;
    color: var(--text-2);
    padding: 2px 6px;
  }
  .hits td {
    padding: 2px 6px;
    border-top: 1px solid var(--border);
  }
  .hits tr.sel td {
    background: var(--spark-soft);
  }
  .hits tbody tr {
    cursor: pointer;
  }
  .ent {
    max-width: 0;
    width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .small {
    font-size: 11px;
    margin: 0;
  }
</style>

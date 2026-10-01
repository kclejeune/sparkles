<script lang="ts">
  // The explorer's map card: a resource with a geometry (its own, through a feature
  // link, or W3C Basic Geo latitude and longitude) on a small map, with Nearby.
  import * as api from '$lib/api';
  import {
    GEO,
    geometryQuery,
    isGeoLiteral,
    resolveGeometries,
    WGS84_POS,
    type GeoLiteral,
  } from '$lib/geo';
  import type { PrefixMap } from '$lib/rdf';
  import MapView, { type MapFeature } from './MapView.svelte';
  import NearbyPanel, { type NearbyHit } from './NearbyPanel.svelte';

  let {
    ds,
    iri,
    props,
    prefixes,
  }: {
    ds: string;
    iri: string;
    /** The resource's outgoing properties: the card looks for geometries only with a hint. */
    props: { p: string; o: api.Term }[];
    prefixes: PrefixMap;
  } = $props();

  const HINTS = new Set([
    `${GEO}hasGeometry`,
    `${GEO}hasDefaultGeometry`,
    `${WGS84_POS}lat`,
    `${WGS84_POS}long`,
  ]);
  const hinted = $derived(props.some(({ p, o }) => HINTS.has(p) || isGeoLiteral(o)));

  let own = $state<MapFeature[]>([]);
  let literal = $state<GeoLiteral | null>(null);
  let near = $state<MapFeature[]>([]);
  let hits = $state<NearbyHit[]>([]);
  let selected = $state<number | null>(null);

  $effect(() => {
    own = [];
    literal = null;
    near = [];
    hits = [];
    selected = null;
    if (!hinted) return;
    let dead = false;
    void (async () => {
      try {
        const rows = await api.select(ds, geometryQuery(iri), { send: 20 });
        const lits = rows.flatMap((r) =>
          isGeoLiteral(r.lit) ? [{ value: r.lit.value, datatype: r.lit.datatype }] : [],
        );
        const geoms = await resolveGeometries(lits, async (b) => (await api.geoConvert(b)).results);
        if (dead) return;
        own = geoms.flatMap((g) =>
          'geometry' in g ? [{ geometry: g.geometry, group: -2, accent: true }] : [],
        );
        literal = lits.find((_, i) => 'geometry' in geoms[i]) ?? null;
      } catch {
        // no card when the geometries cannot be read
      }
    })();
    return () => {
      dead = true;
    };
  });

  let searches = 0;
  async function showHits(found: NearbyHit[]) {
    const mine = ++searches;
    hits = found;
    selected = null;
    near = [];
    const drawn = found.flatMap((h, i) => (h.lit ? [{ i, lit: h.lit }] : []));
    const geoms = await resolveGeometries(
      drawn.map((d) => d.lit),
      async (b) => (await api.geoConvert(b)).results,
    ).catch(() => []);
    if (mine !== searches) return;
    near = drawn.flatMap((d, k) => {
      const g = geoms[k];
      return g && 'geometry' in g ? [{ geometry: g.geometry, group: d.i }] : [];
    });
  }

  const features = $derived([...own, ...near]);
</script>

{#snippet hitPopup(i: number)}
  {@const h = hits[i]}
  {#if i < 0}
    <div class="popup"><strong>This resource</strong></div>
  {:else if h}
    <div class="popup">
      <strong>{h.label ?? (h.term.type === 'uri' ? h.term.value : '')}</strong>
      {#if h.dist != null}<span class="faint">{h.dist.toFixed(2)} km away</span>{/if}
    </div>
  {/if}
{/snippet}

{#if own.length && literal}
  <h3 class="sub">Location</h3>
  <MapView
    {features}
    {selected}
    onselect={(g) => (selected = g != null && g >= 0 ? g : null)}
    popup={hitPopup}
    height="220px"
    label="Location map"
  />
  <NearbyPanel
    {ds}
    {iri}
    {literal}
    {prefixes}
    {selected}
    onresults={showHits}
    onselect={(i) => (selected = i)}
  />
{/if}

<style>
  .sub {
    margin: 14px 0 6px;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  .popup {
    display: grid;
    gap: 2px;
    font-size: 12px;
  }
</style>

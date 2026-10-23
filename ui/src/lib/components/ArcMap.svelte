<script lang="ts">
  // The map of relations (docs/specs/U11-linked-views.md, section 2.2): located entities
  // as markers and the relations between them as great-circle arcs, coloured by predicate,
  // with markers closer than 20 px merged and the arcs joining the same two markers merged
  // into one whose width and label give the count. It reads and writes the shared
  // selection. The caller finds the positions and the links and passes what it has.
  import {
    aggregateLinks,
    clusterPlaces,
    clustersInBox,
    wrapBox,
    hotArcs,
    hotClusters,
    layoutArcs,
    MAX_ARCS,
    capPlaces,
    MAX_POINTS,
    placesBox,
    predicateColours,
    type ArcLayout,
    type Link,
    type Place,
    type WidthBy,
  } from '$lib/arcs';
  import { fmtInt } from '$lib/format';
  import { selection } from '$lib/selection.svelte';
  import MapView, { type Overlay, type Pick } from './MapView.svelte';

  let {
    places,
    links,
    notLocated = [],
    loading = '',
    fitKey = '',
    selectedRow = null,
    onopen,
    height = '100%',
    label = 'Map of relations',
    emptyText = 'None of these entities has a geometry',
  }: {
    places: Place[];
    links: Link[];
    /** Entities without a position, listed under the map; their links are not drawn. */
    notLocated?: { id: string; label: string; iri?: string }[];
    /** Progress text while positions are still being looked up; empty when done. */
    loading?: string;
    /** The view fits the places when this changes (and once they exist). */
    fitKey?: string;
    /** A result row picked outside the map: its arcs stand out. */
    selectedRow?: number | null;
    onopen?: (iri: string) => void;
    height?: string;
    label?: string;
    emptyText?: string;
  } = $props();

  let view = $state<{ box: [number, number, number, number]; zoom: number } | null>(null);
  let hidden = $state<ReadonlySet<string>>(new Set());
  let widthBy = $state<WidthBy>('count');
  let focusIdx = $state(-1);
  let detail = $state('');
  let listShown = $state(50);
  let map: MapView | undefined = $state();

  const zoom = $derived(view?.zoom ?? 0.6);
  const drawnPlaces = $derived(capPlaces(places));

  function cssToken(name: string, fallback: string): string {
    if (typeof document === 'undefined') return fallback;
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
  }

  /** The predicate colours stay put while predicates are switched off. */
  const colours = $derived(predicateColours(links));
  const predicates = $derived.by(() => {
    const n = new Map<string, { label: string; count: number }>();
    for (const l of links) {
      const e = n.get(l.predicate);
      if (e) e.count++;
      else n.set(l.predicate, { label: l.predicateLabel ?? l.predicate, count: 1 });
    }
    return [...n.entries()].sort((a, b) => b[1].count - a[1].count || (a[0] < b[0] ? -1 : 1));
  });

  /** Clusters, arcs and their GeoJSON: recomputed when the data, the zoom or the options change. */
  const scene = $derived.by(() => {
    const t0 = performance.now();
    const { clusters, of } = clusterPlaces(drawnPlaces, zoom);
    const shown = hidden.size ? links.filter((l) => !hidden.has(l.predicate)) : links;
    const agg = aggregateLinks(shown, of, clusters.length);
    const base = layoutArcs(clusters, agg, zoom, colours, widthBy, {
      added: cssToken('--added', '#1a7f37'),
      removed: cssToken('--removed', '#c0392b'),
    });
    return { clusters, of, agg, base, ms: performance.now() - t0 };
  });

  const byArc = $derived(new Map(scene.agg.arcs.map((a) => [a.id, a])));

  /** The clusters and arcs the selection (or a picked result row) stands out. */
  const hot = $derived.by((): ArcLayout => {
    const { clusters, agg, base } = scene;
    const clusterSet = hotClusters(clusters, (id) => selection.has(id));
    const arcSet = hotArcs(agg, clusterSet);
    if (selectedRow != null)
      for (const a of agg.arcs)
        if (a.rows.includes(selectedRow)) {
          arcSet.add(a.id);
          clusterSet.add(a.from);
          clusterSet.add(a.to);
        }
    return {
      lines: {
        type: 'FeatureCollection',
        features: base.lines.features.filter((f) => arcSet.has(f.properties.id)),
      },
      heads: {
        type: 'FeatureCollection',
        features: base.heads.features.filter((f) => arcSet.has(f.properties.id)),
      },
      markers: {
        type: 'FeatureCollection',
        features: base.markers.features.filter((_, i) => clusterSet.has(i)),
      },
    };
  });

  const overlay = $derived<Overlay>({
    base: scene.base,
    hot,
    dim: selection.size > 0 || selectedRow != null,
  });

  /** Clusters in view, the largest first: the order of Tab on the map and of the list. */
  const order = $derived(clustersInBox(scene.clusters, view?.box ?? null));
  const focused = $derived(focusIdx >= 0 ? order[focusIdx] : undefined);
  const focusedCluster = $derived(focused !== undefined ? scene.clusters[focused] : undefined);

  const arcsInView = $derived.by(() => {
    const inView = new Set(order);
    return scene.agg.arcs
      .filter((a) => inView.has(a.from) || inView.has(a.to))
      .sort((a, b) => b.count - a.count || (a.id < b.id ? -1 : 1));
  });

  const shownLinks = $derived(
    hidden.size ? links.filter((l) => !hidden.has(l.predicate)).length : links.length,
  );

  // fit the view to the places once they exist, and again when the caller says they changed
  let fitted = '';
  $effect(() => {
    const key = fitKey;
    const box = placesBox(drawnPlaces);
    if (!view || !box || fitted === key) return;
    fitted = key;
    map?.fitBox(box, false);
  });

  // a new set of places: no stale keyboard focus or detail
  $effect(() => {
    void places;
    focusIdx = -1;
    detail = '';
    listShown = 50;
  });

  function togglePredicate(p: string, on: boolean) {
    const next = new Set(hidden);
    if (on) next.delete(p);
    else next.add(p);
    hidden = next;
  }

  function choose(ids: string[], mods: { shift: boolean; alt: boolean }) {
    if (mods.alt) selection.remove(ids);
    else if (mods.shift) selection.add(ids, 'the map');
    else selection.set(ids, 'the map');
  }

  function pickCluster(ci: number, mods: { shift: boolean; alt: boolean }) {
    const c = scene.clusters[ci];
    if (!c) return;
    choose(c.members, mods);
    detail = `${c.label}: ${fmtInt(c.members.length)} place${c.members.length === 1 ? '' : 's'}${
      c.members.length > 1 ? ' within 20 px' : ''
    }${scene.agg.local[ci] ? `, ${fmtInt(scene.agg.local[ci])} link${scene.agg.local[ci] === 1 ? '' : 's'} among them` : ''}`;
  }

  function pickArc(id: string, mods: { shift: boolean; alt: boolean }) {
    const a = byArc.get(id);
    if (!a) return;
    const from = scene.clusters[a.from];
    const to = scene.clusters[a.to];
    choose([...from.members, ...to.members], mods);
    const what =
      a.labels.length && a.count === 1 ? `${a.predicateLabel}: ${a.labels[0]}` : a.predicateLabel;
    detail = `${from.label} to ${to.label}, ${what}, ${fmtInt(a.count)} link${a.count === 1 ? '' : 's'}`;
  }

  function onpick(hit: Pick | null, mods: { shift: boolean; alt: boolean }) {
    if (!hit) {
      if (!mods.shift && !mods.alt) {
        selection.clear();
        detail = '';
      }
      return;
    }
    if (hit.kind === 'marker') {
      const ci = scene.clusters.findIndex((c) => c.id === hit.id);
      if (ci >= 0) pickCluster(ci, mods);
    } else pickArc(hit.id, mods);
  }

  // Tab walks the markers in view, largest first; Enter selects, Shift+Enter adds
  function onkeydown(e: KeyboardEvent) {
    // only the map itself walks the markers; its zoom buttons and the links keep their keys
    const t = e.target as Element | null;
    if (!t?.closest?.('.maplibregl-canvas, .maplibregl-canvas-container')) return;
    if (e.key === 'Tab' && !e.ctrlKey && !e.altKey && !e.metaKey) {
      const next = focusIdx + (e.shiftKey ? -1 : 1);
      detail = '';
      if (next >= 0 && next < order.length) {
        e.preventDefault();
        focusIdx = next;
      } else {
        focusIdx = -1;
      }
      return;
    }
    if ((e.key === 'Enter' || e.key === ' ') && focused !== undefined) {
      e.preventDefault();
      e.stopPropagation();
      pickCluster(focused, { shift: e.shiftKey, alt: e.altKey });
      return;
    }
    if (e.key === 'Escape' && selection.size) {
      e.preventDefault();
      selection.clear();
      detail = '';
    }
  }

  const spoken = $derived(
    focusedCluster
      ? `${focusedCluster.label}, ${fmtInt(focusedCluster.members.length)} place${focusedCluster.members.length === 1 ? '' : 's'}, ${focusIdx + 1} of ${order.length}`
      : '',
  );
</script>

<div
  class="arcmap"
  style:height
  data-places={places.length}
  data-drawn={drawnPlaces.length}
  data-clusters={scene.clusters.length}
  data-arcs={scene.agg.arcs.length}
  data-links={shownLinks}
  data-unlocated-links={scene.agg.unlocated}
  data-zoom={zoom.toFixed(2)}
  data-layout-ms={scene.ms.toFixed(1)}
  data-loading={loading ? 'true' : 'false'}
  data-arc-summary={scene.agg.arcs.length <= 60
    ? scene.agg.arcs
        .map(
          (a) =>
            `${scene.clusters[a.from].label}>${scene.clusters[a.to].label}:${a.predicateLabel}:${a.count}`,
        )
        .join('|')
    : ''}
>
  <div class="bar">
    {#if predicates.length}
      <span class="faint">Arcs</span>
      <div class="preds" role="group" aria-label="Predicates">
        {#each predicates as [p, info] (p)}
          <label class="pred">
            <input
              type="checkbox"
              checked={!hidden.has(p)}
              onchange={(e) => togglePredicate(p, e.currentTarget.checked)}
            />
            <span class="swatch" style:background={colours.get(p)} aria-hidden="true"></span>
            <span class="mono" title={p}>{info.label}</span>
            <span class="faint">{fmtInt(info.count)}</span>
          </label>
        {/each}
      </div>
      <label class="width">
        <span class="faint">Width by</span>
        <select bind:value={widthBy} aria-label="Width of arcs">
          <option value="count">count</option>
          <option value="uniform">one width</option>
        </select>
      </label>
    {/if}
    <span class="spacer"></span>
    <span class="faint tally">
      {#if loading}<span class="spinner"></span> {loading}{:else}{fmtInt(places.length)} located
        {#if notLocated.length}&middot; {fmtInt(notLocated.length)} not{/if}{/if}
    </span>
  </div>

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="stage"
    {onkeydown}
    onfocusout={(e) => {
      if (!e.currentTarget.contains(e.relatedTarget as Node | null)) focusIdx = -1;
    }}
    aria-describedby="arcmap-hint"
  >
    <MapView
      bind:this={map}
      features={[]}
      fit={false}
      height="100%"
      {label}
      {overlay}
      focusId={focusedCluster?.id ?? null}
      {onpick}
      onview={(_, z, drawn) => (view = { box: wrapBox(drawn), zoom: z })}
    />
    {#if !places.length && !loading}
      <div class="empty">
        <strong>{emptyText}</strong>
        <span class="faint">
          The map looks for a geometry literal on the entity, one reached through
          <span class="mono">geo:hasGeometry</span> or
          <span class="mono">geo:hasDefaultGeometry</span>, or WGS84 latitude and longitude.
        </span>
      </div>
    {/if}
  </div>
  <p id="arcmap-hint" class="hint faint">
    Tab moves through the markers in view, largest first. Enter selects one, Shift+Enter adds it.
    Escape clears the selection.
  </p>
  <p class="detail" aria-live="polite">{detail || spoken}</p>
  <p class="sr-only" aria-live="polite">{selection.announcement}</p>
  {#if scene.agg.dropped || places.length > MAX_POINTS}
    <p class="warn small">
      {#if places.length > MAX_POINTS}The map draws the first {fmtInt(MAX_POINTS)} of {fmtInt(
          places.length,
        )} places, and leaves out {fmtInt(scene.agg.unlocated)} links that end in the others.{/if}
      {#if scene.agg.dropped}It draws the first {fmtInt(MAX_ARCS)} of {fmtInt(links.length)} links.{/if}
    </p>
  {/if}

  <details class="list">
    <summary
      >List of places and links in view ({fmtInt(order.length)} markers, {fmtInt(arcsInView.length)} arcs)</summary
    >
    <div class="cols">
      <div>
        <h4>Markers</h4>
        <ul aria-label="Markers in view">
          {#each order.slice(0, listShown) as ci (ci)}
            {@const c = scene.clusters[ci]}
            <li>
              <button
                class="item"
                aria-pressed={c.members.every((m) => selection.has(m))}
                onclick={(e) => pickCluster(ci, { shift: e.shiftKey, alt: e.altKey })}
              >
                <span class="mono">{c.label}</span>
                <span class="faint">{fmtInt(c.members.length)}</span>
              </button>
            </li>
          {/each}
        </ul>
      </div>
      <div>
        <h4>Arcs</h4>
        <ul aria-label="Arcs in view">
          {#each arcsInView.slice(0, listShown) as a (a.id)}
            <li>
              <button
                class="item"
                onclick={(e) => pickArc(a.id, { shift: e.shiftKey, alt: e.altKey })}
              >
                <span class="mono"
                  >{scene.clusters[a.from].label} &rarr; {scene.clusters[a.to].label}</span
                >
                <span class="faint">{a.predicateLabel} &middot; {fmtInt(a.count)}</span>
              </button>
            </li>
          {/each}
        </ul>
      </div>
    </div>
    {#if order.length > listShown || arcsInView.length > listShown}
      <button class="btn ghost sm" onclick={() => (listShown += 200)}>Show more</button>
    {/if}
  </details>

  {#if notLocated.length}
    <details class="undrawn">
      <summary>
        Not located: {notLocated
          .slice(0, 4)
          .map((n) => n.label)
          .join(', ')}{notLocated.length > 4 ? ', …' : ''} ({fmtInt(notLocated.length)})
      </summary>
      <ul>
        {#each notLocated.slice(0, 200) as n (n.id)}
          <li class="mono">
            {#if n.iri && onopen}
              <button class="link" onclick={() => onopen?.(n.iri!)}>{n.label}</button>
            {:else}{n.label}{/if}
          </li>
        {/each}
      </ul>
      <p class="faint small">Links to these entities are not drawn.</p>
    </details>
  {/if}
</div>

<style>
  .arcmap {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 0;
    min-width: 0;
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 14px;
    font-size: var(--fs-sm);
  }
  .preds {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
  }
  .pred {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-height: 24px;
  }
  .pred input {
    margin: 0;
    accent-color: var(--iri);
  }
  .swatch {
    width: 14px;
    height: 4px;
    border-radius: 2px;
  }
  .width {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .width select {
    font-size: var(--fs-sm);
  }
  .stage {
    position: relative;
    flex: 1;
    min-height: 300px;
  }
  .empty {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    padding: 16px;
    text-align: center;
    font-size: var(--fs-sm);
    background: color-mix(in srgb, var(--surface) 78%, transparent);
    pointer-events: none;
  }
  .hint,
  .detail,
  .small {
    margin: 0;
    font-size: var(--fs-xs);
  }
  .detail {
    min-height: 1.3em;
    font-size: var(--fs-sm);
  }
  .warn {
    margin: 0;
    color: var(--warn);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  details {
    font-size: var(--fs-sm);
  }
  summary {
    cursor: pointer;
    min-height: 24px;
  }
  .undrawn summary {
    color: var(--warn);
  }
  .cols {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px 16px;
    max-height: 220px;
    overflow: auto;
  }
  h4 {
    margin: 4px 0;
    font-size: var(--fs-xs);
    color: var(--text-2);
  }
  ul {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 2px;
  }
  .undrawn ul {
    padding-left: 18px;
    list-style: disc;
    max-height: 160px;
    overflow: auto;
  }
  .item {
    all: unset;
    box-sizing: border-box;
    display: flex;
    gap: 8px;
    justify-content: space-between;
    width: 100%;
    padding: 3px 6px;
    cursor: pointer;
    border-radius: var(--r);
    min-height: 24px;
  }
  .item:hover {
    background: var(--hover);
  }
  .item[aria-pressed='true'] {
    background: var(--spark-soft);
  }
  .item:focus-visible {
    box-shadow: var(--focus);
  }
  .link {
    all: unset;
    cursor: pointer;
    color: var(--iri);
    text-decoration: underline;
  }
  .link:focus-visible {
    box-shadow: var(--focus);
  }
  @media (max-width: 760px) {
    .stage {
      min-height: max(320px, 55vh);
    }
    .cols {
      grid-template-columns: minmax(0, 1fr);
    }
    /* 44 px targets for taps */
    .pred {
      min-height: 44px;
    }
    .pred input {
      width: 22px;
      height: 22px;
    }
    .item,
    summary {
      min-height: 44px;
      align-items: center;
    }
    .hint {
      display: none;
    }
  }
</style>

<script lang="ts" module>
  import type { GeoJsonGeometry } from '$lib/api';

  /** A geometry to draw; features of one `group` (a result row) highlight together. */
  export type MapFeature = {
    geometry: GeoJsonGeometry;
    group: number;
    /** Drawn in the accent colour (the resource a map is about). */
    accent?: boolean;
  };
</script>

<script lang="ts">
  // A MapLibre map of geometries (CRS84) over the basemap. MapLibre is imported when the
  // map mounts; without WebGL the map says so and the page around it keeps working.
  import { mount, onMount, unmount, type Snippet } from 'svelte';
  import { bounds } from '$lib/geo';
  import SnippetHost from './SnippetHost.svelte';

  let {
    features,
    selected = null,
    onselect,
    popup,
    fit = true,
    onview,
    height = '360px',
    label = 'Map',
  }: {
    features: MapFeature[];
    /** The highlighted group (its popup opens). */
    selected?: number | null;
    /** A feature was clicked (null: the map outside the features). */
    onselect?: (group: number | null) => void;
    /** The popup of a group. */
    popup?: Snippet<[number]>;
    /** Fit the view to the features whenever they change. */
    fit?: boolean;
    /** The visible box after every move, [west, south, east, north] in degrees. */
    onview?: (bbox: [number, number, number, number]) => void;
    height?: string;
    label?: string;
  } = $props();

  type Lib = typeof import('$lib/maplibre');
  type MlMap = import('maplibre-gl').Map;
  type MlPopup = import('maplibre-gl').Popup;

  let host: HTMLDivElement;
  let status = $state<'loading' | 'ready' | 'failed'>('loading');
  let failure = $state('');
  let lib: Lib | undefined;
  let map: MlMap | undefined;
  let open: { popup: MlPopup; content: ReturnType<typeof mount> | null } | null = null;
  /** The group whose popup was opened last (by a click or by `selected`). */
  let shown: number | null = null;

  const LAYERS = ['f-fill', 'f-line', 'f-point'];
  const POLY = ['in', ['geometry-type'], ['literal', ['Polygon', 'MultiPolygon']]];
  const POINT = ['in', ['geometry-type'], ['literal', ['Point', 'MultiPoint']]];

  function token(name: string, fallback: string): string {
    const v = getComputedStyle(host).getPropertyValue(name).trim();
    return v || fallback;
  }

  function collection(fs: MapFeature[]) {
    return {
      type: 'FeatureCollection' as const,
      features: fs.map((f, i) => ({
        type: 'Feature' as const,
        id: i,
        geometry: f.geometry,
        properties: { group: f.group, accent: f.accent === true },
      })),
    };
  }

  function addLayers(m: MlMap) {
    const ink = [
      'case',
      ['get', 'accent'],
      token('--literal', '#0b7d69'),
      token('--iri', '#2459c7'),
    ];
    const hot = token('--spark', '#e5a117');
    const halo = token('--surface', '#ffffff');
    m.addSource('features', { type: 'geojson', data: collection(features) as never });
    const sel = ['==', ['get', 'group'], -1];
    const layer = (id: string, type: string, filter: unknown, paint: object) =>
      m.addLayer({ id, type, source: 'features', filter, paint } as never);
    layer('f-fill', 'fill', POLY, { 'fill-color': ink, 'fill-opacity': 0.18 });
    layer('f-line', 'line', ['!', POINT], { 'line-color': ink, 'line-width': 1.6 });
    layer('f-point', 'circle', POINT, {
      'circle-color': ink,
      'circle-radius': 5,
      'circle-stroke-color': halo,
      'circle-stroke-width': 1.5,
    });
    layer('h-fill', 'fill', ['all', POLY, sel], { 'fill-color': hot, 'fill-opacity': 0.35 });
    layer('h-line', 'line', ['all', ['!', POINT], sel], { 'line-color': hot, 'line-width': 3 });
    layer('h-point', 'circle', ['all', POINT, sel], {
      'circle-color': hot,
      'circle-radius': 7,
      'circle-stroke-color': halo,
      'circle-stroke-width': 2,
    });
  }

  function highlight(m: MlMap, group: number | null) {
    const sel = ['==', ['get', 'group'], group ?? -1];
    m.setFilter('h-fill', ['all', POLY, sel] as never);
    m.setFilter('h-line', ['all', ['!', POINT], sel] as never);
    m.setFilter('h-point', ['all', POINT, sel] as never);
  }

  function closePopup() {
    const o = open;
    open = null;
    if (!o) return;
    o.popup.remove();
    if (o.content) void unmount(o.content);
  }

  function showPopup(group: number, at: [number, number]) {
    if (!map || !lib || !popup) return;
    closePopup();
    const el = document.createElement('div');
    el.className = 'map-popup';
    const content = mount(SnippetHost<number>, {
      target: el,
      props: { snippet: popup, arg: group },
    });
    const p = new lib.Popup({ closeButton: true, maxWidth: '340px', focusAfterOpen: false })
      .setLngLat(at)
      .setDOMContent(el)
      .addTo(map);
    const mine = { popup: p, content };
    p.on('close', () => {
      if (open === mine) {
        open = null;
        void unmount(content);
      }
    });
    open = mine;
  }

  /** Report the visible box (`onview`), in range for the server. */
  function view(m: MlMap) {
    if (!onview) return;
    const b = m.getBounds();
    const clamp = (x: number, lim: number) => Math.max(-lim, Math.min(lim, x));
    onview([
      clamp(b.getWest(), 180),
      clamp(b.getSouth(), 90),
      clamp(b.getEast(), 180),
      clamp(b.getNorth(), 90),
    ]);
  }

  /** The box of a group's geometries. */
  const groupBox = (group: number) =>
    bounds(features.filter((f) => f.group === group).map((f) => f.geometry));

  function fitTo(box: [number, number, number, number] | null, animate: boolean) {
    if (!map || !box) return;
    map.fitBounds(
      [
        [box[0], box[1]],
        [box[2], box[3]],
      ],
      { padding: 40, maxZoom: 12, duration: animate ? 400 : 0 },
    );
  }

  onMount(() => {
    let dead = false;
    void (async () => {
      try {
        lib = await import('$lib/maplibre');
        const style = await lib.mapStyle();
        if (dead) return;
        const m = new lib.Map({
          container: host,
          style,
          center: [0, 20],
          zoom: 0.6,
          attributionControl: { compact: true },
          dragRotate: false,
          pitchWithRotate: false,
          touchPitch: false,
        });
        map = m;
        m.addControl(new lib.NavigationControl({ showCompass: false }), 'top-right');
        m.on('load', () => {
          if (dead) return;
          addLayers(m);
          status = 'ready';
          if (fit) fitTo(bounds(features.map((f) => f.geometry)), false);
          if (selected != null) highlight(m, selected);
          view(m);
        });
        m.on('click', (e) => {
          const hit = m.queryRenderedFeatures(e.point, { layers: LAYERS })[0];
          const group = hit ? Number(hit.properties?.group) : null;
          // already in view, with its popup at the click
          shown = group;
          onselect?.(group);
          if (group != null) showPopup(group, [e.lngLat.lng, e.lngLat.lat]);
        });
        for (const l of LAYERS) {
          m.on('mouseenter', l, () => (m.getCanvas().style.cursor = 'pointer'));
          m.on('mouseleave', l, () => (m.getCanvas().style.cursor = ''));
        }
        m.on('moveend', () => view(m));
      } catch (e) {
        if (dead) return;
        status = 'failed';
        failure = e instanceof Error ? e.message : String(e);
      }
    })();
    return () => {
      dead = true;
      closePopup();
      map?.remove();
      map = undefined;
    };
  });

  // new features: redraw, and fit the view to them
  $effect(() => {
    const fc = collection(features);
    if (status !== 'ready' || !map) return;
    const src = map.getSource('features') as { setData(d: unknown): void } | undefined;
    src?.setData(fc);
    closePopup();
    shown = null;
    if (fit) fitTo(bounds(features.map((f) => f.geometry)), false);
  });

  // a group picked outside the map (a table row): highlight it, show it, open its popup
  $effect(() => {
    const group = selected;
    if (status !== 'ready' || !map) return;
    highlight(map, group);
    if (group === shown) return;
    shown = group;
    if (group == null) return closePopup();
    const box = groupBox(group);
    if (!box) return;
    fitTo(box, true);
    showPopup(group, [(box[0] + box[2]) / 2, (box[1] + box[3]) / 2]);
  });
</script>

<div
  class="map"
  style:height
  role="region"
  aria-label={label}
  data-state={status}
  data-features={features.length}
  data-selected={selected ?? ''}
>
  <div class="canvas" bind:this={host}></div>
  {#if status === 'loading'}
    <div class="overlay faint"><span class="spinner"></span> Loading the map…</div>
  {:else if status === 'failed'}
    <div class="overlay faint">This browser cannot draw maps: {failure}</div>
  {/if}
</div>

<style>
  .map {
    position: relative;
    min-height: 160px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    overflow: hidden;
    background: var(--surface-2);
  }
  .canvas {
    position: absolute;
    inset: 0;
  }
  .overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 16px;
    text-align: center;
    font-size: var(--fs-sm);
    pointer-events: none;
  }
  .map :global(.maplibregl-popup-content) {
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--r);
    box-shadow: var(--shadow-pop);
    padding: 8px 26px 8px 10px;
    font-size: var(--fs-sm);
  }
  .map :global(.maplibregl-popup-tip) {
    display: none;
  }
  .map :global(.maplibregl-popup-close-button) {
    color: var(--text-2);
  }
  .map :global(.maplibregl-ctrl-attrib) {
    font-size: 10px;
  }
</style>

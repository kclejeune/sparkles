<script lang="ts" module>
  import type { GeoJsonGeometry } from '$lib/api';
  import type { ArcLayout } from '$lib/arcs';

  /** A geometry to draw; features of one `group` (a result row) highlight together. */
  export type MapFeature = {
    geometry: GeoJsonGeometry;
    group: number;
    /** Drawn in the accent colour (the resource a map is about). */
    accent?: boolean;
    /** In compare mode, a geometry only the later state has (added) or only the earlier one (removed). */
    change?: 'added' | 'removed';
  };

  /** Arcs and markers drawn over the features (the map of relations, `ArcMap`). */
  export type Overlay = {
    base: ArcLayout;
    /** The arcs and markers of the selection, drawn over the rest in the accent colour. */
    hot: ArcLayout;
    /** Draw the base muted, because a selection stands out against it. */
    dim: boolean;
  };

  /** What a click on the overlay hit. */
  export type Pick = { kind: 'marker' | 'arc'; id: string };
</script>

<script lang="ts">
  // A MapLibre map of geometries (CRS84) over the basemap. MapLibre is imported when the
  // map mounts; without WebGL the map says so and the page around it keeps working.
  import { mount, onMount, unmount, type Snippet } from 'svelte';
  import { bounds } from '$lib/geo';
  import { sprite, SPRITE_RATIO, type SpriteColours } from '$lib/map-sprites';
  import SnippetHost from './SnippetHost.svelte';

  let {
    features,
    selected = null,
    onselect,
    popup,
    fit = true,
    onview,
    overlay = null,
    focusId = null,
    onpick,
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
    /** The visible box after every move, [west, south, east, north] in degrees, and the zoom. */
    onview?: (bbox: [number, number, number, number], zoom: number) => void;
    /** Arcs and markers over the features. */
    overlay?: Overlay | null;
    /** The marker with a focus ring (keyboard focus on the map of relations). */
    focusId?: string | null;
    /** A click on the overlay: a marker, an arc, or nothing (null) with the modifier keys. */
    onpick?: (hit: Pick | null, mods: { shift: boolean; alt: boolean }) => void;
    height?: string;
    label?: string;
  } = $props();

  type Lib = typeof import('$lib/maplibre');
  type MlMap = import('maplibre-gl').Map;
  type MlPopup = import('maplibre-gl').Popup;

  let host: HTMLDivElement;
  let region: HTMLDivElement;
  let status = $state<'loading' | 'ready' | 'failed'>('loading');
  let failure = $state('');
  let lib: Lib | undefined;
  let map: MlMap | undefined;
  let open: { popup: MlPopup; content: ReturnType<typeof mount> | null } | null = null;
  /** The group whose popup was opened last (by a click or by `selected`). */
  let shown: number | null = null;

  const LAYERS = ['f-fill', 'f-line', 'f-point'];
  const HIT_LAYERS = ['o-hot-marker', 'o-marker', 'o-hot-arc', 'o-arc', 'o-arc-removed'];
  const EMPTY = { type: 'FeatureCollection' as const, features: [] };
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
        properties: { group: f.group, accent: f.accent === true, change: f.change ?? '' },
      })),
    };
  }

  function addLayers(m: MlMap) {
    const ink = [
      'case',
      ['==', ['get', 'change'], 'added'],
      token('--added', '#1a7f37'),
      ['==', ['get', 'change'], 'removed'],
      token('--removed', '#c0392b'),
      ['get', 'accent'],
      token('--literal', '#0b7d69'),
      token('--iri', '#2459c7'),
    ];
    const gone = ['==', ['get', 'change'], 'removed'];
    const fresh = ['==', ['get', 'change'], 'added'];
    const hot = token('--spark', '#e5a117');
    const halo = token('--surface', '#ffffff');
    m.addSource('features', { type: 'geojson', data: collection(features) as never });
    const sel = ['==', ['get', 'group'], -1];
    const layer = (id: string, type: string, filter: unknown, paint: object) =>
      m.addLayer({ id, type, source: 'features', filter, paint } as never);
    layer('f-fill', 'fill', POLY, {
      'fill-color': ink,
      'fill-opacity': ['case', gone, 0.06, fresh, 0.28, 0.18],
    });
    layer('f-line', 'line', ['!', POINT], {
      'line-color': ink,
      'line-width': ['case', fresh, 2.4, 1.6],
    });
    // removed geometries are outlines, so they read as gone without colour
    layer('f-point', 'circle', POINT, {
      'circle-color': ink,
      'circle-opacity': ['case', gone, 0.15, 1],
      'circle-radius': ['case', fresh, 7, 5],
      'circle-stroke-color': ['case', gone, ink, halo],
      'circle-stroke-width': ['case', gone, 2, 1.5],
    });
    layer('h-fill', 'fill', ['all', POLY, sel], { 'fill-color': hot, 'fill-opacity': 0.35 });
    layer('h-line', 'line', ['all', ['!', POINT], sel], { 'line-color': hot, 'line-width': 3 });
    layer('h-point', 'circle', ['all', POINT, sel], {
      'circle-color': hot,
      'circle-radius': 7,
      'circle-stroke-color': halo,
      'circle-stroke-width': 2,
    });
    addOverlayLayers(m, hot, halo);
  }

  /** The sources and layers of the overlay, empty until `overlay` has something. */
  function addOverlayLayers(m: MlMap, hot: string, halo: string) {
    for (const id of ['arcs', 'heads', 'markers', 'hot-arcs', 'hot-heads', 'hot-markers'])
      m.addSource(`o-${id}`, { type: 'geojson', data: EMPTY as never });
    const add = (spec: object) => m.addLayer(spec as never);
    const gone = ['==', ['get', 'change'], 'removed'];
    add({
      id: 'o-arc',
      type: 'line',
      source: 'o-arcs',
      filter: ['!', gone],
      layout: { 'line-cap': 'round', 'line-join': 'round' },
      paint: {
        'line-color': ['get', 'color'],
        'line-width': ['get', 'width'],
        'line-opacity': 0.82,
      },
    });
    // a removed arc is dashed, so that it reads as gone without colour
    add({
      id: 'o-arc-removed',
      type: 'line',
      source: 'o-arcs',
      filter: gone,
      paint: {
        'line-color': ['get', 'color'],
        'line-width': ['get', 'width'],
        'line-dasharray': [2, 2],
        'line-opacity': 0.7,
      },
    });
    add({
      id: 'o-hot-halo',
      type: 'line',
      source: 'o-hot-arcs',
      layout: { 'line-cap': 'round' },
      paint: { 'line-color': hot, 'line-width': ['+', ['get', 'width'], 5], 'line-opacity': 0.55 },
    });
    add({
      id: 'o-hot-arc',
      type: 'line',
      source: 'o-hot-arcs',
      layout: { 'line-cap': 'round' },
      paint: { 'line-color': ['get', 'color'], 'line-width': ['get', 'width'] },
    });
    const head = {
      type: 'symbol',
      layout: {
        'icon-image': ['concat', 'a:', ['get', 'color']],
        'icon-rotate': ['get', 'bearing'],
        'icon-rotation-alignment': 'map',
        'icon-allow-overlap': true,
        'icon-ignore-placement': true,
      },
    };
    add({ ...head, id: 'o-head', source: 'o-heads' });
    add({ ...head, id: 'o-hot-head', source: 'o-hot-heads' });
    add({
      id: 'o-marker',
      type: 'circle',
      source: 'o-markers',
      paint: {
        'circle-color': token('--iri', '#2459c7'),
        'circle-radius': ['get', 'radius'],
        'circle-stroke-color': halo,
        'circle-stroke-width': 1.5,
      },
    });
    add({
      id: 'o-hot-marker',
      type: 'circle',
      source: 'o-hot-markers',
      paint: {
        'circle-color': hot,
        'circle-radius': ['+', ['get', 'radius'], 2],
        'circle-stroke-color': halo,
        'circle-stroke-width': 2,
      },
    });
    add({
      id: 'o-focus',
      type: 'circle',
      source: 'o-markers',
      filter: ['==', ['get', 'id'], ''],
      paint: {
        'circle-color': 'rgba(0,0,0,0)',
        'circle-radius': ['+', ['get', 'radius'], 5],
        'circle-stroke-color': token('--text', '#1d2330'),
        'circle-stroke-width': 2.5,
      },
    });
    const count = {
      type: 'symbol',
      layout: {
        'icon-image': ['case', ['>', ['get', 'size'], 1], ['concat', 'n:', ['get', 'label']], ''],
        'icon-allow-overlap': true,
        'icon-ignore-placement': true,
      },
    };
    add({ ...count, id: 'o-count', source: 'o-markers' });
    add({ ...count, id: 'o-hot-count', source: 'o-hot-markers' });
    add({
      id: 'o-label',
      type: 'symbol',
      source: 'o-arcs',
      filter: ['!=', ['get', 'label'], ''],
      layout: {
        'symbol-placement': 'line-center',
        'icon-image': ['concat', 'l:', ['get', 'label']],
        'icon-rotation-alignment': 'viewport',
        'icon-padding': 3,
        'symbol-sort-key': ['-', 0, ['get', 'count']],
      },
    });
  }

  /** Put the overlay's data and dimming on the map. */
  function setOverlay(m: MlMap, o: Overlay | null) {
    const put = (id: string, data: unknown) =>
      (m.getSource(`o-${id}`) as { setData(d: unknown): void } | undefined)?.setData(data ?? EMPTY);
    put('arcs', o?.base.lines);
    put('heads', o?.base.heads);
    put('markers', o?.base.markers);
    put('hot-arcs', o?.hot.lines);
    put('hot-heads', o?.hot.heads);
    put('hot-markers', o?.hot.markers);
    const dim = !!o?.dim;
    m.setPaintProperty('o-arc', 'line-opacity', dim ? 0.2 : 0.82);
    m.setPaintProperty('o-arc-removed', 'line-opacity', dim ? 0.15 : 0.7);
    m.setPaintProperty('o-head', 'icon-opacity', dim ? 0.25 : 1);
    m.setPaintProperty('o-marker', 'circle-opacity', dim ? 0.3 : 1);
    m.setPaintProperty('o-marker', 'circle-stroke-opacity', dim ? 0.3 : 1);
    m.setPaintProperty('o-count', 'icon-opacity', dim ? 0.3 : 1);
    m.setPaintProperty('o-label', 'icon-opacity', dim ? 0.4 : 1);
  }

  function spriteColours(): SpriteColours {
    return {
      text: token('--text', '#1d2330'),
      surface: token('--surface', '#ffffff'),
      border: token('--border-strong', '#c5cad4'),
      onMarker: '#ffffff',
      font: token('--font-ui', 'system-ui, sans-serif').replace(/["']/g, ''),
    };
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
    onview(
      [
        clamp(b.getWest(), 180),
        clamp(b.getSouth(), 90),
        clamp(b.getEast(), 180),
        clamp(b.getNorth(), 90),
      ],
      m.getZoom(),
    );
  }

  /** Fit the view to a box, [west, south, east, north] (the map of relations fits itself). */
  export function fitBox(box: [number, number, number, number] | null, animate = false) {
    fitTo(box, animate);
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
        // tests reach the map through its region
        (region as unknown as { __map?: MlMap }).__map = m;
        // the images of counts, labels and arrowheads are drawn when a layer asks for one
        m.setMissingStyleImageResolver((id) => {
          if (!id || m.hasImage(id)) return;
          const img = sprite(id, spriteColours());
          if (img) m.addImage(id, img, { pixelRatio: SPRITE_RATIO });
        });
        m.addControl(new lib.NavigationControl({ showCompass: false }), 'top-right');
        m.on('load', () => {
          if (dead) return;
          addLayers(m);
          status = 'ready';
          setOverlay(m, overlay);
          if (fit) fitTo(bounds(features.map((f) => f.geometry)), false);
          if (selected != null) highlight(m, selected);
          view(m);
        });
        m.on('click', (e) => {
          if (onpick) {
            const pad = 5;
            const box: [[number, number], [number, number]] = [
              [e.point.x - pad, e.point.y - pad],
              [e.point.x + pad, e.point.y + pad],
            ];
            // a marker wins over the arcs that end in it
            const found =
              m.queryRenderedFeatures(box, { layers: ['o-hot-marker', 'o-marker'] })[0] ??
              m.queryRenderedFeatures(box, { layers: HIT_LAYERS })[0];
            const mods = { shift: e.originalEvent.shiftKey, alt: e.originalEvent.altKey };
            onpick(
              found
                ? {
                    kind: found.layer.id.includes('marker') ? 'marker' : 'arc',
                    id: String(found.properties?.id),
                  }
                : null,
              mods,
            );
            if (found) return;
          }
          const hit = m.queryRenderedFeatures(e.point, { layers: LAYERS })[0];
          const group = hit ? Number(hit.properties?.group) : null;
          // already in view, with its popup at the click
          shown = group;
          onselect?.(group);
          if (group != null) showPopup(group, [e.lngLat.lng, e.lngLat.lat]);
        });
        for (const l of [...LAYERS, ...HIT_LAYERS]) {
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

  // arcs and markers
  $effect(() => {
    const o = overlay;
    if (status !== 'ready' || !map) return;
    setOverlay(map, o);
  });

  // the marker with the keyboard focus
  $effect(() => {
    const id = focusId;
    if (status !== 'ready' || !map) return;
    map.setFilter('o-focus', ['==', ['get', 'id'], id ?? ''] as never);
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
  bind:this={region}
  style:height
  role="region"
  aria-label={label}
  data-state={status}
  data-features={features.length}
  data-selected={selected ?? ''}
  data-added={features.filter((f) => f.change === 'added').length}
  data-removed={features.filter((f) => f.change === 'removed').length}
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

<script lang="ts">
  /**
   * The embedding map's picture (docs/specs/U11-linked-views.md, section 2.4): the points
   * of a projection drawn with WebGL through sigma, over a density shading of every point
   * of the region, with a lasso and a box that select points. A plain drag pans and the
   * wheel zooms. In lasso or box mode a drag draws instead, `Shift` adds to the selection
   * and `Alt` removes from it. A point's colour comes from `colourIndex`, a palette index
   * per point, and points outside an active selection are dimmed.
   */
  import { onDestroy, onMount, untrack } from 'svelte';
  import type Graph from 'graphology';
  import type Sigma from 'sigma';
  import { boxed, lassoed, type Points, type Pt } from '$lib/embedding';
  import { readPalette } from '$lib/lens';

  let {
    points,
    colourIndex,
    density = null,
    densityBox = null,
    bounds = null,
    selected = null,
    mode = 'pan',
    label = 'Embedding map',
    onselect,
    onhover,
    onopen,
    onview,
    onfail,
  }: {
    points: Points;
    /** the palette index of each point */
    colourIndex: Uint32Array;
    /** the density grid's counts, row by row from the minimum y */
    density?: Uint32Array | null;
    densityBox?: [number, number, number, number] | null;
    /**
     * the whole projection's bounds, which fix the camera's frame so that a region's
     * points replace the sample without the view jumping
     */
    bounds?: [number, number, number, number] | null;
    /** the indexes of the selected points, or null when no selection is active */
    selected?: ReadonlySet<number> | null;
    mode?: 'pan' | 'lasso' | 'box';
    label?: string;
    /** the points inside a drawn lasso or box, and the shape in the points' coordinates */
    onselect?: (
      points: number[],
      how: 'set' | 'add' | 'remove',
      shape: { kind: 'lasso'; poly: Pt[] } | { kind: 'box'; a: Pt; b: Pt },
    ) => void;
    onhover?: (point: number | null, at: { x: number; y: number } | null) => void;
    onopen?: (point: number) => void;
    /** the region in view, in the points' coordinates, after the camera stops */
    onview?: (bbox: [number, number, number, number]) => void;
    onfail?: (reason: string) => void;
  } = $props();

  let host: HTMLDivElement;
  let under: HTMLCanvasElement;
  let over: HTMLCanvasElement;
  let wrap: HTMLDivElement;
  let graph: Graph | undefined;
  let renderer: Sigma | undefined;
  let palette: string[] = [];
  let dim = '#888';
  let spark = '#e5a117';
  let surface = '';
  let destroyed = false;
  let viewTimer: ReturnType<typeof setTimeout> | undefined;
  let sizeObserver: ResizeObserver | undefined;
  /** the path of the lasso or the corners of the box being drawn, in viewport pixels */
  let drawing: Pt[] | null = null;

  function cssVar(name: string) {
    return getComputedStyle(host).getPropertyValue(name).trim() || '#888';
  }

  function readColors() {
    palette = readPalette(host);
    dim = cssVar('--border-strong');
    spark = cssVar('--spark');
    surface = cssVar('--surface');
  }

  /**
   * `colour` mixed a third of the way from the map's background, so that the map keeps its
   * shape around a selection. WebGL draws no transparency over the page, so the mix is made
   * here; the border colour stands in when either colour is not a hex value.
   */
  function fade(colour: string): string {
    const a = /^#([0-9a-f]{6})$/i.exec(colour);
    const b = /^#([0-9a-f]{6})$/i.exec(surface);
    if (!a || !b) return dim;
    const ch = (h: string, i: number) => parseInt(h.slice(i, i + 2), 16);
    let out = '#';
    for (const i of [0, 2, 4]) {
      const v = Math.round(ch(b[1], i) + (ch(a[1], i) - ch(b[1], i)) * 0.3);
      out += v.toString(16).padStart(2, '0');
    }
    return out;
  }

  function colourOf(i: number): string {
    return palette[colourIndex[i]] ?? palette[palette.length - 1] ?? '#888';
  }

  /** Put the points in the graph, keyed by their index. */
  function fill() {
    if (!graph) return;
    graph.clear();
    const n = points.x.length;
    const size = n > 20000 ? 1.5 : n > 2000 ? 2.5 : 4;
    for (let i = 0; i < n; i++) {
      const color = colourOf(i);
      graph.addNode(String(i), {
        x: points.x[i],
        y: points.y[i],
        size,
        color,
        faded: fade(color),
        label: '',
      });
    }
  }

  /** viewport = a × graph + b, for the density shading, the lasso and the tests */
  function transform() {
    if (!renderer) return null;
    const p0 = renderer.graphToViewport({ x: 0, y: 0 });
    const p1 = renderer.graphToViewport({ x: 1, y: 1 });
    return { ax: p1.x - p0.x, bx: p0.x, ay: p1.y - p0.y, by: p0.y };
  }

  function drawUnder() {
    if (!under || !renderer) return;
    const dpr = window.devicePixelRatio || 1;
    const w = host.clientWidth;
    const h = host.clientHeight;
    if (under.width !== Math.round(w * dpr) || under.height !== Math.round(h * dpr)) {
      under.width = Math.round(w * dpr);
      under.height = Math.round(h * dpr);
    }
    const ctx = under.getContext('2d');
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const t = transform();
    if (t) wrap.dataset.transform = [t.ax, t.bx, t.ay, t.by].map((v) => v.toFixed(4)).join(',');
    if (!density || !densityBox || !t) return;
    const img = shading();
    if (!img) return;
    // the grid drawn in one call, mapped from the points' coordinates to the viewport
    const [x0, y0, x1, y1] = densityBox;
    ctx.setTransform(dpr * t.ax, 0, 0, dpr * t.ay, dpr * t.bx, dpr * t.by);
    ctx.imageSmoothingEnabled = true;
    ctx.drawImage(img, x0, y0, x1 - x0, y1 - y0);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }

  /** The density grid as an image of one pixel per cell, made again when the grid changes. */
  let shaded: { of: Uint32Array; colour: string; img: HTMLCanvasElement } | null = null;
  function shading(): HTMLCanvasElement | null {
    if (!density) return null;
    if (shaded && shaded.of === density && shaded.colour === spark) return shaded.img;
    const size = Math.round(Math.sqrt(density.length));
    let max = 0;
    for (const c of density) max = Math.max(max, c);
    if (!max || !size) return null;
    const img = document.createElement('canvas');
    img.width = size;
    img.height = size;
    const g = img.getContext('2d');
    if (!g) return null;
    g.fillStyle = spark;
    const lmax = Math.log1p(max);
    for (let r = 0; r < size; r++)
      for (let c = 0; c < size; c++) {
        const n = density[r * size + c];
        if (!n) continue;
        g.globalAlpha = 0.05 + 0.35 * (Math.log1p(n) / lmax);
        g.fillRect(c, r, 1, 1);
      }
    shaded = { of: density, colour: spark, img };
    return img;
  }

  function drawOver() {
    if (!over) return;
    const dpr = window.devicePixelRatio || 1;
    const w = host.clientWidth;
    const h = host.clientHeight;
    if (over.width !== Math.round(w * dpr) || over.height !== Math.round(h * dpr)) {
      over.width = Math.round(w * dpr);
      over.height = Math.round(h * dpr);
    }
    const ctx = over.getContext('2d');
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    if (!drawing || drawing.length < 2) return;
    ctx.strokeStyle = spark;
    ctx.fillStyle = spark;
    ctx.lineWidth = 2;
    ctx.setLineDash([6, 4]);
    ctx.beginPath();
    if (mode === 'box') {
      const [a, b] = [drawing[0], drawing[drawing.length - 1]];
      ctx.rect(Math.min(a.x, b.x), Math.min(a.y, b.y), Math.abs(b.x - a.x), Math.abs(b.y - a.y));
    } else {
      ctx.moveTo(drawing[0].x, drawing[0].y);
      for (const p of drawing.slice(1)) ctx.lineTo(p.x, p.y);
      ctx.closePath();
    }
    ctx.globalAlpha = 0.12;
    ctx.fill();
    ctx.globalAlpha = 1;
    ctx.stroke();
  }

  function viewBox(): [number, number, number, number] | null {
    if (!renderer) return null;
    const w = host.clientWidth;
    const h = host.clientHeight;
    const a = renderer.viewportToGraph({ x: 0, y: 0 });
    const b = renderer.viewportToGraph({ x: w, y: h });
    return [Math.min(a.x, b.x), Math.min(a.y, b.y), Math.max(a.x, b.x), Math.max(a.y, b.y)];
  }

  function frame() {
    if (!renderer) return;
    if (!bounds) {
      renderer.setCustomBBox(null);
      return;
    }
    const [x0, y0, x1, y1] = bounds;
    // a flat projection still needs an extent
    const pad = (a: number, b: number) => (b > a ? [a, b] : [a - 1, b + 1]) as [number, number];
    renderer.setCustomBBox({ x: pad(x0, x1), y: pad(y0, y1) });
  }

  async function mount() {
    const [{ default: GraphCtor }, { default: SigmaCtor }] = await Promise.all([
      import('graphology'),
      import('sigma'),
    ]);
    if (!host || destroyed) return;
    readColors();
    graph = new GraphCtor({ type: 'undirected', multi: false, allowSelfLoops: false });
    fill();
    try {
      renderer = new SigmaCtor(graph, host, {
        allowInvalidContainer: true,
        renderLabels: false,
        enableEdgeEvents: false,
        zIndex: true,
        nodeReducer: (key, data) => {
          const res = { ...data };
          const sel = selected;
          if (sel && !sel.has(Number(key))) {
            // the point's own colour, faded, so the map keeps its shape around a selection
            res.color = data.faded as string;
            res.zIndex = 0;
          } else if (sel) res.zIndex = 1;
          return res;
        },
      });
    } catch (e) {
      onfail?.(`WebGL is not available in this browser (${e instanceof Error ? e.message : e})`);
      return;
    }
    frame();
    renderer.on('enterNode', ({ node, event }) => {
      host.style.cursor = 'pointer';
      onhover?.(Number(node), { x: event.x, y: event.y });
    });
    renderer.on('leaveNode', () => {
      host.style.cursor = '';
      onhover?.(null, null);
    });
    renderer.on('clickNode', ({ node }) => onopen?.(Number(node)));
    renderer.on('afterRender', drawUnder);
    renderer.getCamera().on('updated', () => {
      clearTimeout(viewTimer);
      viewTimer = setTimeout(() => {
        const b = viewBox();
        if (b) onview?.(b);
      }, 300);
    });
    if (typeof ResizeObserver === 'function') {
      sizeObserver = new ResizeObserver(() => {
        if (renderer && !destroyed) {
          renderer.resize();
          renderer.refresh();
        }
      });
      sizeObserver.observe(host);
    }
    wrap.dataset.ready = 'true';
  }

  onMount(() => {
    void mount();
  });

  onDestroy(() => {
    destroyed = true;
    clearTimeout(viewTimer);
    sizeObserver?.disconnect();
    renderer?.kill();
    renderer = undefined;
  });

  // new points: a new graph, keeping the camera so that a region's points replace the
  // sample without a jump
  $effect(() => {
    void points;
    untrack(() => {
      if (!graph || !renderer) return;
      fill();
      renderer.refresh();
    });
  });

  $effect(() => {
    void bounds;
    untrack(() => {
      if (!renderer) return;
      frame();
      renderer.refresh();
    });
  });

  $effect(() => {
    void colourIndex;
    untrack(() => {
      if (!graph || !renderer) return;
      graph.forEachNode((k) => {
        const color = colourOf(Number(k));
        graph!.mergeNodeAttributes(k, { color, faded: fade(color) });
      });
      renderer.refresh({ skipIndexation: true });
    });
  });

  $effect(() => {
    void selected;
    void density;
    untrack(() => {
      renderer?.refresh({ skipIndexation: true });
      if (wrap) wrap.dataset.selected = String(selected?.size ?? '');
    });
  });

  /** Fit the whole projection in view. */
  export function fit() {
    void renderer?.getCamera().animatedReset({ duration: 200 });
  }

  /** Zoom by `factor` (above 1 zooms in). */
  export function zoom(factor: number) {
    const cam = renderer?.getCamera();
    if (!cam) return;
    void (factor > 1 ? cam.animatedZoom({ duration: 150 }) : cam.animatedUnzoom({ duration: 150 }));
  }

  // --- drawing a lasso or a box -------------------------------------------------

  function local(e: PointerEvent): Pt {
    const r = over.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  function down(e: PointerEvent) {
    if (mode === 'pan' || e.button !== 0) return;
    e.preventDefault();
    over.setPointerCapture(e.pointerId);
    drawing = [local(e)];
    drawOver();
  }

  function move(e: PointerEvent) {
    if (!drawing) return;
    const p = local(e);
    if (mode === 'box') drawing = [drawing[0], p];
    else {
      const last = drawing[drawing.length - 1];
      if (Math.hypot(p.x - last.x, p.y - last.y) >= 3) drawing = [...drawing, p];
    }
    drawOver();
  }

  function up(e: PointerEvent) {
    if (!drawing || !renderer) return;
    const path = drawing;
    drawing = null;
    drawOver();
    const how = e.shiftKey ? 'add' : e.altKey ? 'remove' : 'set';
    const toGraph = (p: Pt) => renderer!.viewportToGraph(p);
    if (mode === 'box') {
      if (path.length < 2) return;
      const a = toGraph(path[0]);
      const b = toGraph(path[path.length - 1]);
      onselect?.(boxed(points, a, b), how, { kind: 'box', a, b });
    } else {
      if (path.length < 3) return;
      const poly = path.map(toGraph);
      onselect?.(lassoed(points, poly), how, { kind: 'lasso', poly });
    }
  }
</script>

<div class="emap" bind:this={wrap} data-points={points.x.length} data-mode={mode}>
  <canvas class="under" bind:this={under} aria-hidden="true"></canvas>
  <div class="host" bind:this={host} role="img" aria-label={label}></div>
  <canvas
    class="over"
    class:drawing={mode !== 'pan'}
    bind:this={over}
    aria-hidden="true"
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={() => {
      drawing = null;
      drawOver();
    }}
  ></canvas>
</div>

<style>
  .emap {
    position: relative;
    width: 100%;
    height: 100%;
    min-height: 320px;
    overflow: hidden;
    background: var(--surface);
  }
  .under,
  .over,
  .host {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
  .under {
    pointer-events: none;
  }
  .over {
    pointer-events: none;
    touch-action: none;
  }
  .over.drawing {
    pointer-events: auto;
    cursor: crosshair;
  }
</style>

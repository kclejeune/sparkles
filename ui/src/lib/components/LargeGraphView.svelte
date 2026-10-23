<script lang="ts" module>
  /** A node of the WebGL view: its radius, colour and starting position. */
  export type LNode = {
    key: string;
    label: string;
    /** Radius in pixels at the default zoom. */
    size: number;
    color: string;
    x: number;
    y: number;
    /** Kept in place while the layout runs (it was laid out before). */
    fixed?: boolean;
  };
  export type LEdge = {
    key: string;
    source: string;
    target: string;
    size?: number;
    color?: string;
  };
</script>

<script lang="ts">
  /**
   * The WebGL graph view (spec U06 §4.1): sigma.js on a graphology graph, with
   * ForceAtlas2 in a web worker. Sigma draws labels for the largest nodes on screen and
   * for the hovered and selected ones without overlap; an overlay canvas draws the ring
   * of the selected node and of the keyboard focus. A click or tap on a node calls
   * `onopen`, a click on the background clears the selection. With the canvas focused,
   * Tab and Shift+Tab move through the largest nodes on screen, Enter opens the focused
   * one, the arrow keys pan and + and − zoom.
   */
  import { onDestroy, onMount, untrack } from 'svelte';
  import type Graph from 'graphology';
  import type Sigma from 'sigma';
  import type { LayoutStart, LayoutTick } from '$lib/layout.worker';
  import { NO_ENCODING, OTHER, readPalette, sizeFactor, type Encoding } from '$lib/lens';

  let {
    nodes,
    edges,
    selected = null,
    onselect,
    onopen,
    onlayout,
    onfail,
    iterations = 300,
    label = 'Graph',
    marked = null,
    still = false,
    lens = NO_ENCODING,
  }: {
    nodes: LNode[];
    edges: LEdge[];
    selected?: string | null;
    /** Nodes to mark with a ring and keep in colour, such as the entities of a finding. */
    marked?: string[] | null;
    onselect?: (key: string | null) => void;
    onopen?: (key: string) => void;
    /** Positions after the layout ends or is paused. */
    onlayout?: (positions: Map<string, { x: number; y: number }>) => void;
    /** WebGL failed for good (no context, or lost twice). */
    onfail?: (reason: string) => void;
    iterations?: number;
    label?: string;
    /**
     * Update sizes, colours and labels in place without a new layout, so the picture
     * holds still while a time-lapse changes its counts (U01 Phase 4).
     */
    still?: boolean;
    /**
     * What the active lenses make each element look like (docs/specs/U10-canvas-lenses.md):
     * sizes and colours go through the node reducer, badges and the dashed inferred edges are
     * drawn on the overlay, since sigma's edge programs have no dashes.
     */
    lens?: Encoding;
  } = $props();

  let host: HTMLDivElement;
  let overlay: HTMLCanvasElement;
  let renderer: Sigma | undefined;
  let graph: Graph | undefined;
  let worker: Worker | undefined;
  let layoutId = 0;
  let order: string[] = [];
  let progress = $state(1);
  let running = $state(false);
  let hovered: string | null = null;
  let focusKey = $state<string | null>(null);
  let lost = 0;
  let themeObserver: MutationObserver | undefined;
  let media: MediaQueryList | undefined;
  let colors = {
    text: '#888',
    text2: '#888',
    surface: '#fff',
    edge: '#ccc',
    spark: '#e5a117',
    onSpark: '#1a1300',
  };
  let palette: string[] = [];
  let ready = $state(false);

  const reduced = () =>
    typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;

  function cssVar(name: string) {
    return getComputedStyle(host).getPropertyValue(name).trim() || '#888';
  }

  function readColors() {
    colors = {
      text: cssVar('--text'),
      text2: cssVar('--text-2'),
      surface: cssVar('--surface'),
      edge: cssVar('--border-strong'),
      spark: cssVar('--spark'),
      onSpark: cssVar('--on-spark'),
    };
    palette = readPalette(host);
    renderer?.setSetting('labelColor', { color: colors.text });
    renderer?.setSetting('defaultEdgeColor', colors.edge);
    renderer?.refresh();
  }

  /** The hover box of a node: its label on the surface colour, which follows the theme. */
  function drawHover(
    ctx: CanvasRenderingContext2D,
    data: { x: number; y: number; size: number; label?: string | null },
    settings: { labelSize: number; labelFont: string; labelWeight: string },
  ) {
    const size = settings.labelSize;
    ctx.font = `${settings.labelWeight} ${size}px ${settings.labelFont}`;
    const text = data.label ?? '';
    const w = ctx.measureText(text).width + 10;
    const h = size + 8;
    const x = data.x + data.size + 4;
    const y = data.y - h / 2;
    ctx.fillStyle = colors.surface;
    ctx.strokeStyle = colors.edge;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.roundRect(x, y, w, h, 4);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = colors.text;
    ctx.fillText(text, x + 5, data.y + size / 3);
  }

  function neighbours(key: string): Set<string> {
    const out = new Set<string>([key]);
    if (graph?.hasNode(key)) graph.forEachNeighbor(key, (n) => out.add(n));
    return out;
  }

  let near: Set<string> | null = null;
  const markSet = $derived(new Set(marked ?? []));

  function setFocusSet() {
    const k = hovered ?? selected;
    // marked nodes stand out on their own while nothing is hovered
    const marks = (marked ?? []).filter((m) => graph?.hasNode(m));
    if (!hovered && marks.length) {
      near = new Set(marks);
      if (selected && graph?.hasNode(selected)) near.add(selected);
    } else {
      near = k && graph?.hasNode(k) ? neighbours(k) : null;
    }
    renderer?.refresh({ skipIndexation: true });
  }

  function drawOverlay() {
    if (!overlay || !renderer) return;
    const dpr = window.devicePixelRatio || 1;
    const w = host.clientWidth;
    const h = host.clientHeight;
    if (overlay.width !== Math.round(w * dpr) || overlay.height !== Math.round(h * dpr)) {
      overlay.width = Math.round(w * dpr);
      overlay.height = Math.round(h * dpr);
      overlay.style.width = `${w}px`;
      overlay.style.height = `${h}px`;
    }
    const ctx = overlay.getContext('2d');
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const ring = (key: string | null, dashed: boolean) => {
      if (!key || !graph?.hasNode(key)) return;
      const d = renderer!.getNodeDisplayData(key);
      if (!d) return;
      const p = renderer!.framedGraphToViewport({ x: d.x, y: d.y });
      const r = renderer!.scaleSize(d.size) + 4;
      ctx.beginPath();
      ctx.setLineDash(dashed ? [4, 3] : []);
      ctx.lineWidth = 2.5;
      ctx.strokeStyle = colors.spark;
      ctx.arc(p.x, p.y, r, 0, Math.PI * 2);
      ctx.stroke();
    };
    drawInferred(ctx, w, h);
    ring(selected, false);
    if (focusKey !== selected) ring(focusKey, true);
    for (const m of marked ?? []) if (m !== selected) ring(m, true);
    drawBadges(ctx, w, h);
  }

  /** Inferred edges the overlay draws at most; past that they are drawn as other edges. */
  const MAX_DASHED = 4000;
  /** Badges the overlay draws at most, on screen. */
  const MAX_BADGES = 1500;

  /** The viewport position and radius of a node, or null when it is not drawn. */
  function place(key: string) {
    if (!renderer || !graph?.hasNode(key)) return null;
    const d = renderer.getNodeDisplayData(key);
    if (!d || d.hidden) return null;
    const p = renderer.framedGraphToViewport({ x: d.x, y: d.y });
    return { x: p.x, y: p.y, r: renderer.scaleSize(d.size) };
  }

  const dashOverlay = () => lens.edge && lens.inferred.size > 0 && lens.inferred.size <= MAX_DASHED;

  /** The inferred edges, dashed, with an "i" at the middle of the longer ones. */
  function drawInferred(ctx: CanvasRenderingContext2D, w: number, h: number) {
    if (!dashOverlay() || !graph) {
      overlay.dataset.inferred = overlay.dataset.arrows = '0';
      return;
    }
    // the arrowheads follow sigma's own: drawn while its edges are arrows
    const showArrows = edgeType() === 'arrow';
    let lines = 0;
    let arrows = 0;
    ctx.save();
    ctx.setLineDash([6, 4]);
    ctx.lineWidth = 1.4;
    ctx.font = `600 10px ${renderer?.getSetting('labelFont') ?? 'sans-serif'}`;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    const focus = hovered ?? selected;
    for (const key of lens.inferred) {
      if (!graph.hasEdge(key)) continue;
      const [s, t] = graph.extremities(key);
      const a = place(s);
      const b = place(t);
      if (!a || !b) continue;
      if (
        (a.x < 0 && b.x < 0) ||
        (a.y < 0 && b.y < 0) ||
        (a.x > w && b.x > w) ||
        (a.y > h && b.y > h)
      )
        continue;
      const faded = near && !(near.has(s) && near.has(t) && (s === focus || t === focus));
      ctx.strokeStyle = faded ? colors.edge : colors.text2;
      ctx.globalAlpha = faded ? 0.5 : 1;
      // the direction: an arrowhead at the edge of the target, and the two directions of a
      // reciprocal pair each set off to their own left so that they stay apart
      const len = Math.hypot(b.x - a.x, b.y - a.y);
      if (len < 1) continue;
      const ux = (b.x - a.x) / len;
      const uy = (b.y - a.y) / len;
      const back = graph.edges(t, s).some((k) => lens.inferred.has(k));
      const off = back ? 3 : 0;
      const ox = uy * off;
      const oy = -ux * off;
      const ex = b.x - ux * (b.r + 1) + ox;
      const ey = b.y - uy * (b.r + 1) + oy;
      ctx.beginPath();
      ctx.moveTo(a.x + ox, a.y + oy);
      ctx.lineTo(ex - ux * 6, ey - uy * 6);
      ctx.stroke();
      lines++;
      if (showArrows) {
        arrows++;
        ctx.fillStyle = ctx.strokeStyle;
        ctx.beginPath();
        ctx.moveTo(ex, ey);
        ctx.lineTo(ex - ux * 8 - uy * 4, ey - uy * 8 + ux * 4);
        ctx.lineTo(ex - ux * 8 + uy * 4, ey - uy * 8 - ux * 4);
        ctx.closePath();
        ctx.fill();
      }
      if (Math.hypot(b.x - a.x, b.y - a.y) > 60 && lens.inferred.size <= 600) {
        const mx = (a.x + b.x) / 2;
        const my = (a.y + b.y) / 2;
        ctx.setLineDash([]);
        ctx.fillStyle = colors.surface;
        ctx.beginPath();
        ctx.arc(mx, my, 6, 0, Math.PI * 2);
        ctx.fill();
        ctx.stroke();
        ctx.fillStyle = colors.text2;
        ctx.fillText('i', mx, my + 0.5);
        ctx.setLineDash([6, 4]);
      }
    }
    ctx.restore();
    // for the tests: the overlay has no elements to find
    overlay.dataset.inferred = String(lines);
    overlay.dataset.arrows = String(arrows);
  }

  /** The badges: a pill with "i" and the count at the upper right of each badged node. */
  function drawBadges(ctx: CanvasRenderingContext2D, w: number, h: number) {
    if (!lens.badge || !lens.nodes.size) return;
    ctx.save();
    ctx.font = `700 10px ${renderer?.getSetting('labelFont') ?? 'sans-serif'}`;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    let drawn = 0;
    for (const [key, look] of lens.nodes) {
      if (!look.badge || drawn >= MAX_BADGES) continue;
      const p = place(key);
      if (!p || p.x < -20 || p.y < -20 || p.x > w + 20 || p.y > h + 20) continue;
      const text = `i${look.badge > 99 ? '99+' : look.badge}`;
      const bw = 8 + text.length * 6;
      const x = p.x + p.r * 0.7;
      const y = p.y - p.r * 0.7 - 7;
      ctx.fillStyle = colors.spark;
      ctx.strokeStyle = colors.surface;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.roundRect(x - 2, y, bw, 14, 7);
      ctx.fill();
      ctx.stroke();
      ctx.fillStyle = colors.onSpark;
      ctx.fillText(text, x - 2 + bw / 2, y + 7.5);
      drawn++;
    }
    ctx.restore();
  }

  function edgeType() {
    return edges.length > 5000 ? 'line' : 'arrow';
  }

  async function mount() {
    const [{ default: GraphCtor }, { default: SigmaCtor }] = await Promise.all([
      import('graphology'),
      import('sigma'),
    ]);
    if (!host || destroyed) return;
    graph = new GraphCtor({ type: 'directed', multi: false, allowSelfLoops: false });
    readColors();
    try {
      renderer = new SigmaCtor(graph, host, {
        allowInvalidContainer: true,
        renderEdgeLabels: false,
        defaultEdgeType: edgeType(),
        defaultEdgeColor: colors.edge,
        labelColor: { color: colors.text },
        labelFont: cssVar('--font-ui').replace(/["']/g, '') || 'sans-serif',
        labelSize: 12,
        labelWeight: '500',
        labelDensity: 0.6,
        labelGridCellSize: 90,
        labelRenderedSizeThreshold: 7,
        hideEdgesOnMove: edges.length > 50000,
        zIndex: true,
        // room for the labels, which are drawn right of their nodes
        stagePadding: 60,
        minCameraRatio: 0.02,
        maxCameraRatio: 20,
        defaultDrawNodeHover: drawHover as never,
        nodeReducer: (key, data) => {
          const res = { ...data };
          const look = lens.nodes.get(key);
          if (lens.size) res.size = (data.size as number) * sizeFactor(look?.size);
          if (lens.colour && look?.colour != null)
            res.color = palette[look.colour] ?? palette[OTHER] ?? res.color;
          if (near && !near.has(key)) {
            res.color = colors.edge;
            res.label = '';
            res.zIndex = 0;
          } else if (near) {
            res.zIndex = 1;
          }
          if (allLabels && !(near && !near.has(key))) res.forceLabel = true;
          if (key === selected || key === hovered || key === focusKey || markSet.has(key)) {
            res.forceLabel = true;
            res.zIndex = 2;
          }
          return res;
        },
        edgeReducer: (key, data) => {
          const res = { ...data };
          // the overlay draws the inferred edges dashed
          if (dashOverlay() && lens.inferred.has(key)) {
            res.hidden = true;
            return res;
          }
          if (near && graph) {
            const [s, t] = graph.extremities(key);
            if (
              near.has(s) &&
              near.has(t) &&
              (s === (hovered ?? selected) || t === (hovered ?? selected))
            ) {
              res.color = colors.spark;
              res.zIndex = 1;
            } else {
              res.hidden = edges.length > 2000;
              res.color = colors.edge;
            }
          }
          return res;
        },
      });
    } catch (e) {
      onfail?.(`WebGL is not available in this browser (${e instanceof Error ? e.message : e})`);
      return;
    }
    watched = [...host.querySelectorAll('canvas')];
    for (const c of watched) c.addEventListener('webglcontextlost', onLost as EventListener, false);
    renderer.on('clickNode', ({ node }) => {
      focusKey = node;
      onselect?.(node);
      onopen?.(node);
    });
    renderer.on('doubleClickNode', (e) => e.preventSigmaDefault());
    renderer.on('clickStage', () => {
      focusKey = null;
      onselect?.(null);
    });
    renderer.on('enterNode', ({ node }) => {
      hovered = node;
      host.style.cursor = 'pointer';
      setFocusSet();
    });
    renderer.on('leaveNode', () => {
      hovered = null;
      host.style.cursor = '';
      setFocusSet();
    });
    renderer.on('afterRender', drawOverlay);
    ready = true;
    sync();
  }

  /** The canvases watched for a lost context. */
  let watched: HTMLCanvasElement[] = [];
  let destroyed = false;

  /**
   * Kill the renderer without counting it as a lost context: `kill` releases the WebGL
   * contexts, which fires `webglcontextlost` on its canvases.
   */
  function release() {
    for (const c of watched) c.removeEventListener('webglcontextlost', onLost as EventListener);
    watched = [];
    renderer?.kill();
    renderer = undefined;
  }

  /** Whether a lost context's renderer is being replaced. */
  let restoring = false;

  function onLost(e: Event) {
    e.preventDefault();
    // one GPU reset loses the contexts of every canvas of the renderer: it counts once,
    // and a loss counts again only on the renderer that replaced it
    if (destroyed || restoring || lost > 1) return;
    lost++;
    if (lost > 1) {
      onfail?.('WebGL is not available in this browser: the context was lost twice');
      return;
    }
    // restore once: a new renderer on the same graph
    restoring = true;
    setTimeout(() => {
      restoring = false;
      if (destroyed) return;
      release();
      ready = false;
      void mount();
    }, 50);
  }

  function stopLayout() {
    layoutId++;
    worker?.postMessage({ type: 'stop' });
    running = false;
  }

  function positions(): Map<string, { x: number; y: number }> {
    const m = new Map<string, { x: number; y: number }>();
    graph?.forEachNode((k, a) => m.set(k, { x: a.x as number, y: a.y as number }));
    return m;
  }

  function apply(p: Float32Array) {
    if (!graph) return;
    for (let i = 0; i < order.length; i++) {
      const k = order[i];
      if (graph.hasNode(k)) {
        graph.mergeNodeAttributes(k, { x: p[i * 2], y: p[i * 2 + 1] });
      }
    }
  }

  function startLayout(fit: boolean) {
    if (!graph) return;
    stopLayout();
    const n = graph.order;
    if (n === 0) return;
    if (!worker) {
      worker = new Worker(new URL('../layout.worker.ts', import.meta.url), { type: 'module' });
      worker.onmessage = (ev: MessageEvent<LayoutTick>) => {
        const m = ev.data;
        if (m.id !== layoutId) return;
        apply(m.positions);
        progress = m.progress;
        if (m.type === 'done') {
          running = false;
          progress = 1;
          onlayout?.(positions());
          if (fitAfter) fitPicture();
          fitAfter = false;
        }
      };
    }
    order = graph.nodes();
    const index = new Map(order.map((k, i) => [k, i]));
    const x = new Float32Array(n);
    const y = new Float32Array(n);
    const size = new Float32Array(n);
    const fixed = new Uint8Array(n);
    order.forEach((k, i) => {
      const a = graph!.getNodeAttributes(k);
      x[i] = a.x as number;
      y[i] = a.y as number;
      size[i] = a.size as number;
      fixed[i] = a.fixed ? 1 : 0;
    });
    const pairs: number[] = [];
    const weights: number[] = [];
    graph.forEachEdge((_e, a, s, t) => {
      pairs.push(index.get(s)!, index.get(t)!);
      weights.push((a.weight as number) ?? 1);
    });
    layoutId++;
    fitAfter = fit;
    const msg: LayoutStart = {
      type: 'start',
      id: layoutId,
      x,
      y,
      size,
      fixed,
      edges: Uint32Array.from(pairs),
      weights: Float32Array.from(weights),
      iterations: fixed.some((f) => f) ? Math.min(iterations, 200) : iterations,
      final: reduced(),
    };
    running = true;
    progress = 0;
    worker.postMessage(msg, [x.buffer, y.buffer, size.buffer, fixed.buffer]);
  }

  let fitAfter = false;
  /** Whether every node shows its label (a small picture). */
  let allLabels = false;

  /**
   * Room around the fitted picture for the labels drawn right of the nodes: the longest
   * label of a small picture, at most a quarter of the shorter side of the view.
   */
  function stagePadding(): number {
    if (nodes.length > 300) return 60;
    let chars = 0;
    for (const n of nodes) chars = Math.max(chars, n.label.length);
    const side = Math.min(host?.clientWidth ?? 800, host?.clientHeight ?? 800);
    return Math.round(Math.min(Math.max(60, 30 + chars * 7), side / 4));
  }

  /** Rebuild the graph from the props, keeping the positions of nodes already drawn. */
  function sync() {
    if (!graph || !renderer) return;
    const before = positions();
    graph.clear();
    for (const n of nodes) {
      const p = before.get(n.key);
      graph.addNode(n.key, {
        label: n.label,
        size: n.size,
        color: n.color,
        x: p?.x ?? n.x,
        y: p?.y ?? n.y,
        fixed: !!n.fixed,
      });
    }
    for (const e of edges) {
      if (e.source === e.target || !graph.hasNode(e.source) || !graph.hasNode(e.target)) continue;
      if (graph.hasEdge(e.source, e.target)) continue;
      graph.addEdgeWithKey(e.key, e.source, e.target, {
        size: e.size ?? 1,
        weight: e.size ?? 1,
        ...(e.color ? { color: e.color } : {}),
      });
    }
    renderer.setSetting('defaultEdgeType', edgeType());
    renderer.setSetting('hideEdgesOnMove', edges.length > 50000);
    renderer.setSetting('stagePadding', stagePadding());
    allLabels = nodes.length <= ALL_LABELS;
    setFocusSet();
    // a new level is fitted into view once it has settled
    startLayout(true);
  }

  /** The props' sizes, colours and labels on the drawn graph, positions kept and no layout. */
  function update() {
    if (!graph || !renderer) return;
    const g = graph;
    const want = new Set(nodes.map((n) => n.key));
    g.forEachNode((k) => {
      if (!want.has(k)) g.dropNode(k);
    });
    for (const n of nodes) {
      const a = { label: n.label, size: n.size, color: n.color };
      if (g.hasNode(n.key)) g.mergeNodeAttributes(n.key, a);
      else g.addNode(n.key, { ...a, x: n.x, y: n.y, fixed: true });
    }
    const wantE = new Set(edges.map((e) => e.key));
    g.forEachEdge((k) => {
      if (!wantE.has(k)) g.dropEdge(k);
    });
    for (const e of edges) {
      if (e.source === e.target || !g.hasNode(e.source) || !g.hasNode(e.target)) continue;
      const a = { size: e.size ?? 1, weight: e.size ?? 1, ...(e.color ? { color: e.color } : {}) };
      if (g.hasEdge(e.key)) g.mergeEdgeAttributes(e.key, a);
      else if (!g.hasEdge(e.source, e.target)) g.addEdgeWithKey(e.key, e.source, e.target, a);
    }
    setFocusSet();
    renderer.refresh();
  }

  // the props change as a whole (a new level), so one rebuild per change. The rebuild
  // reads other state, such as the selection, which must not start another one. While the
  // picture holds still, the drawn graph is updated in place instead.
  $effect(() => {
    void nodes;
    void edges;
    if (ready) untrack(() => (still && graph && graph.order > 0 ? update() : sync()));
  });

  $effect(() => {
    void selected;
    void marked;
    if (ready) setFocusSet();
  });

  // a lens's new look is drawn in place, without a layout
  $effect(() => {
    void lens;
    if (ready) untrack(() => renderer?.refresh());
  });

  /** Nodes up to which every label is drawn and the fit makes room for it. */
  const ALL_LABELS = 30;
  let measure: CanvasRenderingContext2D | null = null;

  function labelWidth(text: string): number {
    if (!text || !renderer) return 0;
    measure ??= document.createElement('canvas').getContext('2d');
    if (!measure) return text.length * 7;
    const font = renderer.getSetting('labelFont');
    measure.font = `${renderer.getSetting('labelWeight')} ${renderer.getSetting('labelSize')}px ${font}`;
    return measure.measureText(text).width;
  }

  /**
   * Fit the whole picture in the view. A small picture is fitted with its labels, which
   * are drawn right of their nodes at a fixed size: the box of the nodes and their labels
   * is measured on screen and the camera centred on it and zoomed until it fits, a few
   * times over, since the labels do not scale with the zoom.
   */
  export function fit() {
    fitPicture();
  }

  function fitPicture() {
    if (!renderer || !graph) return;
    const cam = renderer.getCamera();
    const duration = reduced() ? 0 : 300;
    const W = host?.clientWidth ?? 0;
    const H = host?.clientHeight ?? 0;
    if (graph.order === 0 || graph.order > ALL_LABELS || W <= 0 || H <= 0) {
      cam.animatedReset({ duration });
      return;
    }
    const r = renderer;
    const g = graph;
    const margin = 12;
    const before = cam.getState();
    let state = { x: 0.5, y: 0.5, angle: 0, ratio: 1 };
    cam.setState(state);
    // the mapping from graph to screen follows the positions only once they are drawn
    r.refresh();
    // the conversions use the matrix of the last frame unless given a camera state
    for (let i = 0; i < 4; i++) {
      let [left, right, top, bottom] = [Infinity, -Infinity, Infinity, -Infinity];
      g.forEachNode((key, attr) => {
        const p = r.graphToViewport({ x: attr.x, y: attr.y }, { cameraState: state });
        const size = r.getNodeDisplayData(key)?.size ?? attr.size ?? 4;
        const text = typeof attr.label === 'string' ? attr.label : '';
        left = Math.min(left, p.x - size);
        right = Math.max(right, p.x + size + (text ? labelWidth(text) + 6 : 0));
        top = Math.min(top, p.y - Math.max(size, 9));
        bottom = Math.max(bottom, p.y + Math.max(size, 9));
      });
      const scale = Math.min(
        (W - 2 * margin) / Math.max(1, right - left),
        (H - 2 * margin) / Math.max(1, bottom - top),
      );
      const c = r.viewportToFramedGraph(
        { x: (left + right) / 2, y: (top + bottom) / 2 },
        { cameraState: state },
      );
      // a ratio of 1 already frames the nodes, so a smaller one only magnifies a picture
      // whose box is a point or a line, such as a single node, past its drawn size
      state = { ...state, x: c.x, y: c.y, ratio: Math.max(1, state.ratio / scale) };
      cam.setState(state);
    }
    if (duration) {
      cam.setState(before);
      cam.animate(state, { duration });
    }
  }

  /** Centre a node and show its ring. */
  export function reveal(key: string) {
    if (!renderer || !graph?.hasNode(key)) return;
    const d = renderer.getNodeDisplayData(key);
    if (!d) return;
    renderer
      .getCamera()
      .animate(
        { x: d.x, y: d.y, ratio: Math.min(renderer.getCamera().ratio, 0.6) },
        { duration: reduced() ? 0 : 300 },
      );
    focusKey = key;
    renderer.refresh({ skipIndexation: true });
  }

  export function pause() {
    if (running) {
      stopLayout();
      onlayout?.(positions());
    } else {
      startLayout(false);
    }
  }

  /** The largest nodes on screen, largest first, for Tab. */
  function onScreen(): string[] {
    if (!renderer || !graph) return [];
    const w = host.clientWidth;
    const h = host.clientHeight;
    const out: { k: string; s: number }[] = [];
    graph.forEachNode((k, a) => {
      const d = renderer!.getNodeDisplayData(k);
      if (!d) return;
      const p = renderer!.framedGraphToViewport({ x: d.x, y: d.y });
      if (p.x >= 0 && p.y >= 0 && p.x <= w && p.y <= h) out.push({ k, s: a.size as number });
    });
    out.sort((a, b) => b.s - a.s || a.k.localeCompare(b.k));
    return out.slice(0, 200).map((o) => o.k);
  }

  function onKey(e: KeyboardEvent) {
    if (!renderer) return;
    const cam = renderer.getCamera();
    const step = 0.15;
    if (e.key === 'Tab') {
      const list = onScreen();
      if (!list.length) return;
      const i = focusKey ? list.indexOf(focusKey) : -1;
      const j = e.shiftKey ? i - 1 : i + 1;
      if (j < 0 || j >= list.length) {
        // past either end, Tab leaves the canvas
        focusKey = null;
        drawOverlay();
        return;
      }
      e.preventDefault();
      focusKey = list[j];
      onselect?.(focusKey);
      renderer.refresh({ skipIndexation: true });
      return;
    }
    if (e.key === 'Enter' && focusKey) {
      e.preventDefault();
      onopen?.(focusKey);
      return;
    }
    const pan = (dx: number, dy: number) => {
      e.preventDefault();
      const s = cam.getState();
      cam.animate({ x: s.x + dx * s.ratio, y: s.y + dy * s.ratio }, { duration: 120 });
    };
    if (e.key === 'ArrowLeft') pan(-step, 0);
    else if (e.key === 'ArrowRight') pan(step, 0);
    else if (e.key === 'ArrowUp') pan(0, step);
    else if (e.key === 'ArrowDown') pan(0, -step);
    else if (e.key === '+' || e.key === '=') {
      e.preventDefault();
      cam.animatedZoom({ duration: 150 });
    } else if (e.key === '-' || e.key === '−' || e.key === '_') {
      e.preventDefault();
      cam.animatedUnzoom({ duration: 150 });
    } else if (e.key === '0') {
      e.preventDefault();
      fit();
    }
  }

  onMount(() => {
    themeObserver = new MutationObserver(readColors);
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme', 'class'],
    });
    media = matchMedia('(prefers-color-scheme: dark)');
    media.addEventListener('change', readColors);
    void mount();
  });

  onDestroy(() => {
    themeObserver?.disconnect();
    media?.removeEventListener('change', readColors);
    stopLayout();
    worker?.terminate();
    destroyed = true;
    release();
  });
</script>

<div class="lg-wrap">
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div
    class="lg-host"
    bind:this={host}
    tabindex="0"
    role="application"
    aria-label={`${label}. Tab moves through the largest nodes, Enter opens one, arrow keys pan, plus and minus zoom.`}
    aria-roledescription="graph"
    onkeydown={onKey}
    onblur={() => {
      focusKey = null;
      drawOverlay();
    }}
  ></div>
  <canvas class="lg-overlay" bind:this={overlay} aria-hidden="true"></canvas>
  {#if running && progress < 1}
    <div class="lg-progress faint" role="status">
      Layout {Math.round(progress * 100)}%
      <button class="btn sm ghost" onclick={pause}>Pause</button>
    </div>
  {/if}
</div>

<style>
  .lg-wrap {
    position: relative;
    width: 100%;
    height: 100%;
    min-height: 0;
  }
  .lg-host {
    position: absolute;
    inset: 0;
    outline: none;
    touch-action: none;
  }
  .lg-host:focus-visible {
    box-shadow: inset var(--focus);
  }
  .lg-overlay {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .lg-progress {
    position: absolute;
    right: 8px;
    bottom: 8px;
    display: flex;
    gap: 6px;
    align-items: center;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 2px 4px 2px 8px;
    font-size: var(--fs-xs);
  }
</style>

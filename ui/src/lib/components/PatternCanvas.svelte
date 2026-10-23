<script lang="ts" module>
  import cytoscape, { type Core, type ElementDefinition, type StylesheetJson } from 'cytoscape';
  import fcose from 'cytoscape-fcose';

  cytoscape.use(fcose);

  /** One element of the drawing the keyboard can focus. */
  export type Focusable = { node: PNode } | { edge: PEdge };
</script>

<script lang="ts">
  // The Pattern canvas of spec U08 §2.1: the drawing of a query's pattern on Cytoscape,
  // in the styles of Explore's graph. Positions come from the layout map (§4.4): a node
  // whose key is in the map keeps its place, and new nodes are placed by an fcose run with
  // every known node fixed, seeded next to their first placed neighbour. The canvas is one
  // focus stop; inside it the keyboard moves through the elements in text order (§2.5).
  import { onDestroy, onMount } from 'svelte';
  import type { Drawing, PEdge, PNode } from '$lib/pattern';
  import { describe, textOrder } from '$lib/pattern';
  import type { Layout } from '$lib/pattern-layout';

  let {
    drawing,
    layout,
    dim = false,
    phone = false,
    selected = null,
    onselect,
    onlayout,
    onkey,
    label = 'Pattern canvas',
  }: {
    drawing: Drawing | null;
    /** the positions to keep, by key */
    layout: Layout;
    /** the text does not parse: the last pattern stays, dimmed */
    dim?: boolean;
    /** 44 px touch targets and short labels (§2.6) */
    phone?: boolean;
    /** the key of the element whose panel is open */
    selected?: string | null;
    onselect?: (el: Focusable | null) => void;
    /** the positions after a layout or a move, to keep */
    onlayout?: (layout: Layout) => void;
    /** a key the canvas does not handle itself, with the focused element */
    onkey?: (e: KeyboardEvent, el: Focusable | null) => void;
    label?: string;
  } = $props();

  let host: HTMLDivElement;
  let wrap: HTMLDivElement;
  let cy: Core | undefined;
  let running: cytoscape.Layouts | undefined;
  let settled = 0;
  /** The focused element, by its position in text order. */
  let focusAt = $state(-1);
  let announce = $state('');
  const order = $derived(drawing ? textOrder(drawing) : []);

  const reduced = () =>
    typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  let themeObserver: MutationObserver | undefined;
  let media: MediaQueryList | undefined;
  const restyle = () => cy?.style(styles());

  function cssVar(name: string) {
    return getComputedStyle(host).getPropertyValue(name).trim() || '#888';
  }

  function styles(): StylesheetJson {
    const text = cssVar('--text');
    const text2 = cssVar('--text-2');
    const text3 = cssVar('--text-3');
    const surface = cssVar('--surface');
    const surface2 = cssVar('--surface-2');
    const border = cssVar('--border-strong');
    const iri = cssVar('--iri');
    const lit = cssVar('--literal');
    const bnode = cssVar('--bnode');
    const spark = cssVar('--spark');
    const font = cssVar('--font-ui').replace(/["']/g, '');
    const mono = cssVar('--font-mono').replace(/["']/g, '');
    return [
      {
        selector: 'node',
        style: {
          label: 'data(label)',
          'font-family': font,
          'font-size': 11,
          color: text,
          'text-valign': 'center',
          'text-halign': 'center',
          'text-wrap': 'wrap',
          'text-max-width': '200px',
          'line-height': 1.3,
          shape: 'rectangle',
          'background-color': surface,
          'border-width': 1.5,
          'border-color': iri,
        },
      },
      // hulls take their size from their children
      { selector: 'node[w]', style: { width: 'data(w)', height: 'data(h)' } },
      {
        selector: 'node[kind = "var"]',
        style: {
          shape: 'ellipse',
          'background-color': iri,
          'background-opacity': 0.14,
          'border-color': iri,
          'font-weight': 600,
        },
      },
      {
        selector: 'node[kind = "bnode"]',
        style: {
          shape: 'ellipse',
          'background-color': bnode,
          'background-opacity': 0.12,
          'border-style': 'dotted',
          'border-color': bnode,
        },
      },
      {
        selector: 'node[kind = "literal"]',
        style: {
          shape: 'round-rectangle',
          'border-color': lit,
          color: lit,
          'font-family': mono,
          'font-size': 10,
        },
      },
      {
        selector: 'node[kind = "locked"], node[kind = "chip"]',
        style: {
          'text-halign': 'center',
          'text-justification': 'left',
          'font-family': mono,
          'font-size': 10,
          color: text2,
          'background-color': surface2,
          'border-color': text3,
          'border-style': 'dashed',
          'text-max-width': '260px',
        },
      },
      {
        selector: ':parent',
        style: {
          shape: 'round-rectangle',
          'background-color': spark,
          'background-opacity': 0.05,
          'border-style': 'dashed',
          'border-color': spark,
          'border-width': 1.5,
          label: 'data(label)',
          'text-valign': 'top',
          'text-halign': 'center',
          'font-size': 9,
          'font-weight': 600,
          color: spark,
          padding: '12px',
        },
      },
      {
        selector: 'edge',
        style: {
          width: 1.4,
          'curve-style': 'bezier',
          'line-color': border,
          'target-arrow-color': border,
          'target-arrow-shape': 'triangle',
          'arrow-scale': 0.9,
          label: 'data(label)',
          'font-family': font,
          'font-size': 10,
          color: text2,
          'text-rotation': 'autorotate',
          'text-background-color': surface,
          'text-background-opacity': 0.9,
          'text-background-padding': '2px',
        },
      },
      { selector: 'edge[style = "dashed"]', style: { 'line-style': 'dashed' } },
      {
        selector: 'edge[?optional]',
        style: { 'line-style': 'dashed', 'line-dash-pattern': [6, 3] },
      },
      {
        selector: 'edge[style = "dotted"]',
        style: {
          'line-style': 'dotted',
          'target-arrow-shape': 'none',
          width: 1,
          'line-color': text3,
        },
      },
      {
        selector: '.kfocus, :selected',
        style: {
          'border-color': spark,
          'border-width': 3,
          'line-color': spark,
          'target-arrow-color': spark,
          'underlay-color': spark,
          'underlay-opacity': 0.16,
          'underlay-padding': 4,
          'overlay-opacity': 0,
        },
      },
    ];
  }

  /** The label of a node: its name over its subtitle lines, shortened on a phone. */
  function nodeLabel(n: PNode): string {
    let main = n.label;
    if (phone && n.kind === 'iri')
      main = main.replace(/^[^:]*:/, '').replace(/^<.*[/#]([^/#>]+)>$/, '$1');
    return [main, ...n.subtitle].join('\n');
  }

  function size(label: string, kind: string) {
    const lines = label.split('\n');
    const pad = kind === 'var' || kind === 'bnode' ? 30 : 18;
    const w = Math.min(280, Math.max(phone ? 44 : 36, ...lines.map((l) => l.length * 6.4 + pad)));
    const h = Math.max(phone ? 44 : 28, lines.length * 15 + 12);
    return { w, h };
  }

  function nodeData(n: PNode) {
    const label = nodeLabel(n);
    return {
      id: n.id,
      label,
      kind: n.kind,
      title: n.title,
      ...size(label, n.kind),
      ...(n.parent ? { parent: n.parent } : {}),
    };
  }

  /** What is drawn, as text, and where, for the tests: the canvas has no DOM elements. */
  function expose() {
    if (!cy || !wrap) return;
    const d = drawing;
    wrap.dataset.graph = JSON.stringify({
      nodes: d ? d.nodes.map((n) => [n.label, ...n.subtitle]) : [],
      edges: d
        ? d.edges
            .filter((e) => e.style !== 'dotted')
            .map((e) => [e.label, e.source, e.target, e.optional])
        : [],
      hulls: d ? d.hulls.length : 0,
      locked: d ? d.nodes.filter((n) => n.locked).map((n) => n.locked!.text) : [],
    });
    const pos: Record<string, [number, number]> = {};
    cy.nodes()
      .not(':parent')
      .forEach((n) => {
        const p = n.position();
        pos[n.id()] = [Math.round(p.x * 10) / 10, Math.round(p.y * 10) / 10];
      });
    wrap.dataset.positions = JSON.stringify(pos);
  }

  /** Report the positions of every node with a key worth keeping. */
  function report() {
    if (!cy) return;
    const out: Layout = {};
    cy.nodes()
      .not(':parent')
      .forEach((n) => {
        if (n.data('kind') === 'chip') return;
        const p = n.position();
        out[n.id()] = { x: p.x, y: p.y };
      });
    onlayout?.(out);
  }

  function done() {
    settled++;
    expose();
    report();
    if (wrap) wrap.dataset.settled = String(settled);
  }

  /** Seeds next to the first neighbour with a position, spread around it. */
  function seed(n: cytoscape.NodeSingular, i: number) {
    const placed = n
      .neighborhood('node')
      .nodes()
      .filter((m) => !m.isParent() && !m.hasClass('fresh'));
    const base = placed.nonempty() ? placed[0].position() : null;
    const angle = (i * 2.4) % (2 * Math.PI);
    if (base) n.position({ x: base.x + Math.cos(angle) * 110, y: base.y + Math.sin(angle) * 110 });
  }

  function sync() {
    if (!cy) return;
    const d = drawing;
    const want = new Map<string, ElementDefinition>();
    if (d) {
      for (const h of d.hulls)
        want.set(h.id, { group: 'nodes', data: { id: h.id, label: h.label } });
      for (const n of d.nodes) want.set(n.id, { group: 'nodes', data: nodeData(n) });
      for (const e of d.edges)
        want.set(e.id, {
          group: 'edges',
          data: {
            id: e.id,
            source: e.source,
            target: e.target,
            label: e.label,
            style: e.style,
            optional: e.optional,
          },
        });
    }
    const fresh: cytoscape.NodeSingular[] = [];
    cy.batch(() => {
      cy!.elements().forEach((el) => {
        if (!want.has(el.id())) el.remove();
      });
      // a node whose hull changed is added again (Cytoscape moves no node between parents)
      for (const [id, def] of want) {
        const el = cy!.getElementById(id);
        if (
          el.nonempty() &&
          el.isNode() &&
          (el.data('parent') ?? null) !== (def.data.parent ?? null)
        )
          el.remove();
      }
      for (const [id, def] of want) {
        const el = cy!.getElementById(id);
        if (el.nonempty()) {
          el.data(def.data);
          continue;
        }
        if (def.group === 'edges') continue;
        const added = cy!.add(def);
        const at = layout[id];
        if (at) added.position({ ...at });
        else if (id.startsWith('h:')) {
          /* a hull: its children place it */
        } else {
          added.addClass('fresh');
          fresh.push(added as cytoscape.NodeSingular);
        }
      }
      for (const [id, def] of want) {
        if (def.group !== 'edges' || cy!.getElementById(id).nonempty()) continue;
        if (
          cy!.getElementById(def.data.source).empty() ||
          cy!.getElementById(def.data.target).empty()
        )
          continue;
        cy!.add(def);
      }
    });
    cy.elements().toggleClass('dimmed', dim);
    wrap.dataset.dim = dim ? 'true' : 'false';
    if (!fresh.length) {
      done();
      return;
    }
    const known = cy.nodes().not(':parent').not('.fresh');
    fresh.forEach((n, i) => seed(n, i));
    running?.stop();
    if (known.empty()) {
      // nothing to keep: a full layout
      runLayout(true);
      return;
    }
    // place only the new nodes: every known node is fixed where it is
    const fixed = known.nodes().map((k) => ({ nodeId: k.id(), position: { ...k.position() } }));
    running = cy.layout({
      name: 'fcose',
      randomize: false,
      animate: false,
      fit: false,
      quality: 'default',
      nodeRepulsion: () => 5000,
      idealEdgeLength: edgeLength,
      nodeSeparation: 50,
      fixedNodeConstraint: fixed,
      stop: () => {
        // the known nodes exactly where they were
        for (const f of fixed) cy!.getElementById(f.nodeId).position(f.position);
        cy!.nodes('.fresh').removeClass('fresh');
        done();
      },
    } as cytoscape.LayoutOptions);
    running.run();
  }

  /** A triple's edge gets room for its label; a dotted link to a locked block or a chip
   * stays short, so a block sits next to the variables it uses. */
  const edgeLength = (e: cytoscape.EdgeSingular) => (e.data('style') === 'dotted' ? 40 : 80);

  function runLayout(fit: boolean) {
    if (!cy) return;
    running?.stop();
    const n = cy.nodes().length;
    running = cy.layout({
      name: 'fcose',
      randomize: true,
      animate: false,
      fit: false,
      padding: 30,
      quality: n > 300 ? 'draft' : 'default',
      nodeRepulsion: () => 5000,
      idealEdgeLength: edgeLength,
      nodeSeparation: 50,
      packComponents: true,
      stop: () => {
        cy!.nodes('.fresh').removeClass('fresh');
        if (fit) fitView();
        done();
      },
    } as cytoscape.LayoutOptions);
    running.run();
  }

  const pixelRatio = () => Math.min(3, Math.max(2, Math.ceil(window.devicePixelRatio || 1)));

  onMount(() => {
    cy = cytoscape({
      container: host,
      style: styles(),
      minZoom: 0.1,
      maxZoom: 3,
      boxSelectionEnabled: false,
      pixelRatio: pixelRatio(),
    });
    cy.on('tap', 'node, edge', (e) => {
      const id = e.target.id();
      const i = order.findIndex((x) => ('node' in x ? x.node.id : x.edge.id) === id);
      if (i >= 0) {
        focusAt = i;
        onselect?.(order[i]);
      }
    });
    cy.on('tap', (e) => {
      if (e.target === cy) onselect?.(null);
    });
    cy.on('dragfree', 'node', () => {
      expose();
      report();
    });
    sync();
    // a full layout fits when it stops; a kept one fits now
    if (!layoutEmpty()) fitSoon();
    themeObserver = new MutationObserver(restyle);
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
    media = matchMedia('(prefers-color-scheme: dark)');
    media.addEventListener('change', restyle);
  });

  const layoutEmpty = () => Object.keys(layout).length === 0;
  function fitSoon() {
    requestAnimationFrame(() => fitView());
  }

  /** Fit the drawing in view (the Fit button). */
  export function fit() {
    fitView();
  }

  onDestroy(() => {
    themeObserver?.disconnect();
    media?.removeEventListener('change', restyle);
    running?.stop();
    cy?.destroy();
  });

  $effect(() => {
    void drawing;
    void dim;
    sync();
  });

  $effect(() => {
    void phone;
    restyle();
  });

  // the focused element is marked on the canvas
  $effect(() => {
    if (!cy) return;
    cy.elements('.kfocus').removeClass('kfocus');
    const el = order[focusAt];
    if (el) cy.getElementById('node' in el ? el.node.id : el.edge.id).addClass('kfocus');
  });

  $effect(() => {
    if (!cy) return;
    cy.elements(':selected').unselect();
    if (selected) cy.getElementById(selected).select();
  });

  /** Fit the drawing in view. */
  function fitView() {
    if (!cy || cy.elements().empty()) return;
    cy.fit(cy.elements(), 30);
    if (cy.zoom() > 1.2) {
      cy.zoom(1.2);
      cy.center();
    }
    // labels stay readable, and a phone keeps 44 px targets: a wide pattern is panned
    // from its first element instead of shrunk to fit
    const least = phone ? 1 : 0.6;
    if (cy.zoom() < least) {
      cy.zoom(least);
      const first = order[0];
      const el = first ? cy.getElementById('node' in first ? first.node.id : first.edge.id) : null;
      if (el && el.nonempty()) cy.center(el);
      else cy.center();
    }
  }

  /** A full layout that replaces the map (Tidy). */
  export function tidy() {
    runLayout(true);
  }

  export function focus() {
    wrap?.focus();
  }

  function focusEl(i: number) {
    focusAt = i;
    const el = order[i];
    if (!el || !drawing) return;
    announce = describe(el, drawing);
    const id = 'node' in el ? el.node.id : el.edge.id;
    const c = cy?.getElementById(id);
    if (c && c.nonempty() && cy) {
      const bb = c.renderedBoundingBox();
      const w = cy.width();
      const h = cy.height();
      if (bb.x1 < 0 || bb.y1 < 0 || bb.x2 > w || bb.y2 > h)
        cy.animate({ center: { eles: c }, duration: reduced() ? 0 : 200 });
    }
  }

  /** The nearest element from the focused one in an arrow's direction. */
  function nearest(dx: number, dy: number): number {
    if (!cy) return focusAt;
    const posOf = (x: Focusable) => {
      const el = cy!.getElementById('node' in x ? x.node.id : x.edge.id);
      if (el.empty()) return null;
      return el.isNode() ? el.position() : (el as cytoscape.EdgeSingular).midpoint();
    };
    const from = order[focusAt] ? posOf(order[focusAt]) : null;
    if (!from) return 0;
    let best = focusAt;
    let bestScore = Infinity;
    order.forEach((x, i) => {
      if (i === focusAt) return;
      const p = posOf(x);
      if (!p) return;
      const vx = p.x - from.x;
      const vy = p.y - from.y;
      const along = vx * dx + vy * dy;
      if (along <= 0) return;
      const across = Math.abs(vx * dy - vy * dx);
      const score = along + 2 * across;
      if (score < bestScore) {
        bestScore = score;
        best = i;
      }
    });
    return best;
  }

  function keydown(e: KeyboardEvent) {
    const el = order[focusAt] ?? null;
    const dirs: Record<string, [number, number]> = {
      ArrowLeft: [-1, 0],
      ArrowRight: [1, 0],
      ArrowUp: [0, -1],
      ArrowDown: [0, 1],
    };
    if (e.key === 'Tab' && !e.altKey && !e.ctrlKey && !e.metaKey) {
      const next = focusAt + (e.shiftKey ? -1 : 1);
      if (next >= 0 && next < order.length) {
        e.preventDefault();
        focusEl(next);
      } else focusAt = -1;
      return;
    }
    if (e.key in dirs && !e.altKey && !e.ctrlKey && !e.metaKey) {
      e.preventDefault();
      const [dx, dy] = dirs[e.key];
      if (e.shiftKey && el && 'node' in el && cy) {
        // move the focused node by 16 px
        const n = cy.getElementById(el.node.id);
        if (n.nonempty() && !n.isParent()) {
          const p = n.position();
          n.position({ x: p.x + dx * 16, y: p.y + dy * 16 });
          expose();
          report();
        }
        return;
      }
      focusEl(focusAt < 0 ? 0 : nearest(dx, dy));
      return;
    }
    if (e.key === 'Enter' && el) {
      e.preventDefault();
      onselect?.(el);
      return;
    }
    onkey?.(e, el);
  }

  // entering the canvas starts at its first element in text order
  function focusin() {
    if (focusAt < 0 && order.length) focusEl(0);
  }

  function focusout(e: FocusEvent) {
    if (!wrap.contains(e.relatedTarget as Node | null)) focusAt = -1;
  }
</script>

<!-- the canvas is one focus stop that handles its own keys (§2.5) -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  class="pcanvas"
  class:dim
  bind:this={wrap}
  tabindex="0"
  role="application"
  aria-label={label}
  aria-roledescription="pattern canvas"
  aria-describedby="pattern-keys"
  data-settled="0"
  onkeydown={keydown}
  onfocusin={focusin}
  onfocusout={focusout}
>
  <div class="cy" bind:this={host}></div>
  <p class="sr-only" id="pattern-keys">
    Tab moves through the pattern in text order, arrow keys move to the nearest element, Shift with
    an arrow moves a node, Enter opens its panel, Delete removes an edge, O toggles optional, N adds
    a triple, and Escape returns to the editor.
  </p>
  <p class="sr-only" aria-live="polite" data-testid="pattern-focus">{announce}</p>
</div>

<style>
  .pcanvas {
    position: relative;
    width: 100%;
    height: 100%;
    min-height: 160px;
    background-color: var(--surface);
    background-image: radial-gradient(
      circle,
      color-mix(in srgb, var(--text-3) 22%, transparent) 1px,
      transparent 1px
    );
    background-size: 18px 18px;
    outline: none;
  }
  .pcanvas:focus-visible {
    box-shadow: inset 0 0 0 2px var(--spark);
  }
  .pcanvas.dim .cy {
    opacity: 0.4;
  }
  .cy {
    position: absolute;
    inset: 0;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
</style>

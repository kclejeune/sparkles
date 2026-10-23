<script lang="ts" module>
  import cytoscape, { type Core, type ElementDefinition, type StylesheetJson } from 'cytoscape';
  import fcose from 'cytoscape-fcose';

  import type { Mark } from '$lib/rowdiff';

  cytoscape.use(fcose);

  export type GNodeKind = 'iri' | 'literal' | 'bnode' | 'triple';
  export type GNode = {
    id: string;
    label: string;
    kind: GNodeKind;
    title?: string;
    focus?: boolean;
    expanded?: boolean;
    /** Between two states of the time slider: added, removed or kept. */
    change?: Mark;
    /** The numbers of the found paths through the node (the path finder's overlay). */
    paths?: number[];
    /** When the path animation draws a new element, in steps of 250 ms. */
    order?: number;
    /** The node's neighbours on a path, between which a new node is placed. */
    near?: string[];
  };
  export type GEdge = {
    id: string;
    source: string;
    target: string;
    label: string;
    title?: string;
    isType?: boolean;
    change?: Mark;
    paths?: number[];
    order?: number;
  };
</script>

<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import Icon from './Icon.svelte';

  let {
    nodes,
    edges,
    selected = null,
    onselect,
    onexpand,
    emptyText = 'Nothing to draw',
    animate = false,
    highlight = null,
    onmenu,
  }: {
    nodes: GNode[];
    edges: GEdge[];
    selected?: string | null;
    onselect?: (id: string | null) => void;
    onexpand?: (id: string) => void;
    emptyText?: string;
    /**
     * Fade added elements in and removed ones out, and keep the nodes that stay where they
     * are (the time slider). Off with `prefers-reduced-motion`, then changes appear at once.
     */
    animate?: boolean;
    /** The found path to highlight, dimming the rest; null for none. */
    highlight?: number | null;
    /** A node's context menu was asked for (right click or long press), at a point of the view. */
    onmenu?: (id: string, at: { x: number; y: number }) => void;
  } = $props();

  let host: HTMLDivElement;
  let cy: Core | undefined;
  /** Animations started so far, for tests: it stays 0 under reduced motion. */
  let animations = 0;
  let wrap: HTMLDivElement | undefined;
  const started = () => {
    animations++;
    if (wrap) wrap.dataset.animations = String(animations);
  };
  const FADE_MS = 300;
  /** The path animation draws one edge in this time. */
  const STEP_MS = 250;
  const reduced = () =>
    typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  const motion = () => animate && !reduced();
  let motionOn = $state(false);
  let themeObserver: MutationObserver | undefined;
  let media: MediaQueryList | undefined;
  const restyle = () => cy?.style(styles());

  function cssVar(name: string) {
    return getComputedStyle(host).getPropertyValue(name).trim() || '#888';
  }

  function styles(): StylesheetJson {
    const text = cssVar('--text');
    const text2 = cssVar('--text-2');
    const surface = cssVar('--surface');
    const border = cssVar('--border-strong');
    const iri = cssVar('--iri');
    const lit = cssVar('--literal');
    const bnode = cssVar('--bnode');
    const spark = cssVar('--spark');
    const font = cssVar('--font-ui').replace(/["']/g, '');
    const added = cssVar('--added');
    const removed = cssVar('--removed');
    return [
      {
        selector: 'node',
        style: {
          label: 'data(label)',
          'font-family': font,
          'font-size': 11,
          color: text,
          'text-valign': 'bottom',
          'text-margin-y': 4,
          'text-wrap': 'ellipsis',
          'text-max-width': '140px',
          'min-zoomed-font-size': 7,
          'text-outline-color': surface,
          'text-outline-width': 2,
          width: 18,
          height: 18,
          'background-color': iri,
          'background-opacity': 0.9,
          'border-width': 2,
          'border-color': surface,
        },
      },
      {
        selector: 'node[kind = "literal"]',
        style: {
          shape: 'round-rectangle',
          width: 'data(w)',
          height: 18,
          'text-valign': 'center',
          'text-margin-y': 0,
          'background-color': surface,
          'background-opacity': 1,
          'border-width': 1,
          'border-color': lit,
          color: lit,
          'font-size': 10,
          'text-outline-width': 0,
          'text-max-width': '180px',
        },
      },
      {
        selector: 'node[kind = "bnode"]',
        style: { 'background-color': bnode, width: 12, height: 12 },
      },
      { selector: 'node[kind = "triple"]', style: { shape: 'diamond', 'background-color': text2 } },
      {
        selector: 'node[?expanded]',
        style: { 'border-color': iri, 'border-width': 2, 'background-opacity': 0.55 },
      },
      {
        selector: 'node[?focus]',
        style: {
          width: 26,
          height: 26,
          'border-color': spark,
          'border-width': 3,
          'font-weight': 600,
        },
      },
      // compare mode: a solid green outline and "+" for what a later state added, a dashed
      // red outline and "−" for what it removed (the label carries the sign too)
      {
        selector: 'node[change = "added"]',
        style: { 'border-color': added, 'border-width': 3, 'border-style': 'solid' },
      },
      {
        selector: 'node[change = "removed"]',
        style: {
          'border-color': removed,
          'border-width': 3,
          'border-style': 'dashed',
          'background-opacity': 0.35,
        },
      },
      {
        selector: 'node:selected',
        style: {
          'border-color': spark,
          'border-width': 3,
          'overlay-opacity': 0,
          'underlay-color': spark,
          'underlay-opacity': 0.18,
          'underlay-padding': 5,
        },
      },
      {
        selector: 'edge',
        style: {
          width: 1.2,
          'curve-style': 'bezier',
          'line-color': border,
          'target-arrow-color': border,
          'target-arrow-shape': 'triangle',
          'arrow-scale': 0.8,
          label: 'data(label)',
          'font-family': font,
          'font-size': 9,
          color: text2,
          'text-rotation': 'autorotate',
          'text-background-color': surface,
          'text-background-opacity': 0.85,
          'text-background-padding': '1px',
          'min-zoomed-font-size': 9,
        },
      },
      { selector: 'edge[?isType]', style: { 'line-style': 'dashed', 'line-dash-pattern': [4, 3] } },
      {
        selector: 'edge[change = "added"]',
        style: { 'line-color': added, 'target-arrow-color': added, width: 2.4 },
      },
      {
        selector: 'edge[change = "removed"]',
        style: {
          'line-color': removed,
          'target-arrow-color': removed,
          'line-style': 'dashed',
          'line-dash-pattern': [5, 3],
          width: 2,
        },
      },
      { selector: 'edge.quiet', style: { 'text-opacity': 0 } },
      {
        selector: 'edge:selected, edge.hl',
        style: {
          'line-color': spark,
          'target-arrow-color': spark,
          width: 2,
          color: text,
          'text-opacity': 1,
        },
      },
      { selector: '.faded', style: { opacity: 0.25 } },
      // the path finder's overlay: path elements stand out, a highlight dims the others
      {
        selector: 'edge[?onpath]',
        style: { 'line-color': spark, 'target-arrow-color': spark, width: 2.2, color: text },
      },
      { selector: 'node[?onpath]', style: { 'border-color': spark, 'border-width': 2 } },
      {
        selector: 'edge.phl',
        style: { width: 3.4, 'text-opacity': 1, 'font-weight': 600 },
      },
      { selector: 'node.phl', style: { 'border-width': 3 } },
      { selector: '.pdim', style: { opacity: 0.3 } },
    ];
  }

  /** The sign a changed element's label starts with, so that colour is not the only cue. */
  const sign = (c: Mark | undefined) => (c === 'added' ? '+ ' : c === 'removed' ? '− ' : '');

  /** An edge's label with the numbers of the paths it is on. */
  const edgeLabel = (e: GEdge) =>
    sign(e.change) + e.label + (e.paths?.length ? ` · ${e.paths.join(',')}` : '');

  const toEl = {
    node: (n: GNode): ElementDefinition => ({
      group: 'nodes',
      data: {
        id: n.id,
        label: sign(n.change) + n.label,
        change: n.change ?? '',
        kind: n.kind,
        title: n.title,
        focus: !!n.focus,
        expanded: !!n.expanded,
        w: Math.min(190, n.label.length * 6 + 14),
        paths: (n.paths ?? []).join(','),
        onpath: !!n.paths?.length,
      },
    }),
    edge: (e: GEdge): ElementDefinition => ({
      group: 'edges',
      data: {
        id: e.id,
        source: e.source,
        target: e.target,
        label: edgeLabel(e),
        change: e.change ?? '',
        title: e.title,
        isType: !!e.isType,
        paths: (e.paths ?? []).join(','),
        onpath: !!e.paths?.length,
      },
    }),
  };

  let running: cytoscape.Layouts | undefined;

  type El = cytoscape.CollectionReturnValue | cytoscape.SingularElementReturnValue;

  /** Fade an element in. */
  function fadeIn(el: El) {
    started();
    el.style('opacity', 0);
    el.animate(
      { style: { opacity: 1 } },
      { duration: FADE_MS, complete: () => el.removeStyle('opacity') },
    );
  }

  /** Show a new path element at its step of the path animation. */
  function reveal(el: El, order: number) {
    started();
    el.style('opacity', 0);
    el.delay(order * STEP_MS).animate(
      { style: { opacity: 1 } },
      { duration: STEP_MS, complete: () => el.removeStyle('opacity') },
    );
  }

  /** Fade an element out and remove it. */
  function fadeOut(el: El) {
    if (el.hasClass('leaving')) return;
    started();
    el.addClass('leaving');
    el.animate({ style: { opacity: 0 } }, { duration: FADE_MS, complete: () => el.remove() });
  }

  function layout(randomize: boolean, keep?: cytoscape.NodeCollection) {
    if (!cy || cy.nodes().length === 0) return;
    const n = cy.nodes().length;
    // A layout still animating would fight the new one (e.g. focus a node, then its
    // expansion arrives a moment later).
    running?.stop();
    // an element that is fading out takes no part in the layout
    running = cy
      .elements()
      .not('.leaving')
      .layout({
        name: 'fcose',
        quality: n > 400 ? 'draft' : 'default',
        randomize,
        animate: !randomize && n < 300 && !reduced(),
        animationDuration: 350,
        fit: randomize || n < 30,
        padding: 30,
        nodeRepulsion: () => (n > 30 ? 12000 : 6500),
        idealEdgeLength: () => (n > 30 ? 120 : 90),
        nodeSeparation: 60,
        packComponents: true,
        // the nodes that stay do not move: only the new ones are placed
        ...(keep && keep.length
          ? {
              fixedNodeConstraint: keep.map((k) => ({
                nodeId: k.id(),
                position: { ...k.position() },
              })),
            }
          : {}),
      } as cytoscape.LayoutOptions);
    running.run();
  }

  function sync() {
    if (!cy) return;
    const wasEmpty = cy.nodes().length === 0;
    const wantNodes = new Map(nodes.map((n) => [n.id, n]));
    const wantEdges = new Map(
      edges.filter((e) => wantNodes.has(e.source) && wantNodes.has(e.target)).map((e) => [e.id, e]),
    );
    let added = 0;
    const fresh = new Set<string>();
    let pathAdded = false;
    const stepping = !reduced();
    const fade = motion() && !wasEmpty;
    motionOn = motion();
    cy.batch(() => {
      cy!.elements().forEach((el) => {
        const id = el.id();
        if (el.hasClass('leaving')) return;
        if (el.isNode() ? !wantNodes.has(id) : !wantEdges.has(id)) {
          if (fade) fadeOut(el);
          else el.remove();
        }
      });
      for (const n of wantNodes.values()) {
        const existing = cy!.getElementById(n.id);
        if (existing.nonempty()) {
          if (existing.hasClass('leaving')) {
            // wanted again before it faded out
            existing.stop(true);
            existing.removeClass('leaving');
            existing.removeStyle('opacity');
          }
          existing.data({
            label: sign(n.change) + n.label,
            change: n.change ?? '',
            kind: n.kind,
            focus: !!n.focus,
            expanded: !!n.expanded,
            title: n.title,
            w: Math.min(190, n.label.length * 6 + 14),
            paths: (n.paths ?? []).join(','),
            onpath: !!n.paths?.length,
          });
        } else {
          const el = cy!.add(toEl.node(n));
          added++;
          fresh.add(n.id);
          if (n.order != null) {
            pathAdded = true;
            if (stepping) reveal(el, n.order);
          } else if (fade) fadeIn(el);
          // a new node of a path goes between its neighbours on the path
          const near = (n.near ?? [])
            .map((id) => cy!.getElementById(id))
            .filter((x) => x.nonempty() && !fresh.has(x.id()));
          if (near.length) {
            const xs = near.map((x) => x.position());
            const x = xs.reduce((a, p) => a + p.x, 0) / xs.length;
            const y = xs.reduce((a, p) => a + p.y, 0) / xs.length;
            el.position({ x: x + (Math.random() - 0.5) * 60, y: y + (Math.random() - 0.5) * 60 });
            continue;
          }
          // Seed position next to an existing neighbour so incremental layouts stay calm.
          const neighbour = edges.find((e) => e.target === n.id || e.source === n.id);
          const other = neighbour
            ? cy!.getElementById(neighbour.source === n.id ? neighbour.target : neighbour.source)
            : null;
          if (other && other.nonempty() && other.id() !== n.id) {
            const p = other.position();
            el.position({
              x: p.x + (Math.random() - 0.5) * 120,
              y: p.y + (Math.random() - 0.5) * 120,
            });
          }
        }
      }
      for (const e of wantEdges.values()) {
        const have = cy!.getElementById(e.id);
        if (have.empty()) {
          const el = cy!.add(toEl.edge(e));
          if (e.order != null) {
            pathAdded = true;
            if (stepping) reveal(el, e.order);
          } else if (fade) fadeIn(el);
        } else {
          if (have.hasClass('leaving')) {
            have.stop(true);
            have.removeClass('leaving');
            have.removeStyle('opacity');
          }
          have.data({
            change: e.change ?? '',
            label: edgeLabel(e),
            paths: (e.paths ?? []).join(','),
            onpath: !!e.paths?.length,
          });
        }
      }
    });
    // what is drawn, as text, for the tests: the canvas has no elements to find
    if (wrap && nodes.length <= 300)
      wrap.dataset.graph = JSON.stringify({
        nodes: [...wantNodes.values()].map((n) => [n.label, n.change ?? '']),
        edges: [...wantEdges.values()].map((e) => [edgeLabel(e), e.change ?? '']),
      });
    applyHighlight();
    // Too many edge labels turn into noise: show them only on hover/selection.
    cy.edges().toggleClass('quiet', cy.edges().length > 60);
    // Incremental layouts keep the user's arrangement, but when most of the graph is new
    // (a resource and then its neighbours) there is nothing to keep: lay it out afresh.
    if (added) {
      const randomize = wasEmpty || cy.nodes().length - added <= 2;
      // in the time slider the nodes that stay keep their places
      const keep =
        animate || pathAdded
          ? cy
              .nodes()
              .not('.leaving')
              .filter((k) => !fresh.has(k.id()))
          : undefined;
      layout(randomize, randomize ? undefined : keep);
    }
  }

  /**
   * Canvas resolution. Cytoscape defaults to devicePixelRatio, which on 1× and fractional
   * (1.25 / 1.5) displays leaves node edges and labels visibly jagged: elements are drawn
   * from cached textures rendered at power-of-two scales and resampled. Rendering at
   * least 2× and letting the browser downsample the canvas supersamples everything.
   */
  const pixelRatio = () => Math.min(3, Math.max(2, Math.ceil(window.devicePixelRatio || 1)));

  onMount(() => {
    cy = cytoscape({
      container: host,
      style: styles(),
      minZoom: 0.08,
      maxZoom: 4,
      boxSelectionEnabled: false,
      pixelRatio: pixelRatio(),
    });
    // Keep small graphs at a readable scale after fitting.
    cy.on('layoutstop', () => {
      if (cy && cy.zoom() > 1.25) {
        cy.zoom(1.25);
        cy.center();
      }
    });
    cy.on('tap', 'node', (e) => onselect?.(e.target.id()));
    cy.on('tap', (e) => {
      if (e.target === cy) onselect?.(null);
    });
    cy.on('dbltap', 'node', (e) => onexpand?.(e.target.id()));
    cy.on('cxttap taphold', 'node', (e) => onmenu?.(e.target.id(), e.renderedPosition));
    cy.on('mouseover', 'node', (e) => {
      const n = e.target;
      cy!.elements().not(n.closedNeighborhood()).addClass('faded');
      n.connectedEdges().addClass('hl');
    });
    cy.on('mouseout', 'node', () => {
      cy!.elements().removeClass('faded').removeClass('hl');
    });
    sync();

    themeObserver = new MutationObserver(restyle);
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
    media = matchMedia('(prefers-color-scheme: dark)');
    media.addEventListener('change', restyle);
  });

  onDestroy(() => {
    themeObserver?.disconnect();
    media?.removeEventListener('change', restyle);
    cy?.destroy();
  });

  $effect(() => {
    void nodes;
    void edges;
    sync();
  });

  $effect(() => {
    const id = selected;
    if (!cy) return;
    cy.nodes(':selected').unselect();
    if (id) {
      const n = cy.getElementById(id);
      if (n.nonempty()) n.select();
    }
  });

  /** Highlight the elements of one path and dim the rest. */
  function applyHighlight() {
    if (!cy) return;
    const h = highlight;
    cy.batch(() => {
      cy!.elements().removeClass('phl pdim');
      if (h == null) return;
      cy!.elements().forEach((el) => {
        const on = String(el.data('paths') ?? '')
          .split(',')
          .includes(String(h));
        el.addClass(on ? 'phl' : 'pdim');
      });
    });
    if (wrap) wrap.dataset.highlight = h == null ? '' : String(h);
  }

  $effect(() => {
    void highlight;
    applyHighlight();
  });

  /** Fit the view to some nodes, such as a path's. */
  export function fitTo(ids: string[]) {
    if (!cy) return;
    const els = cy.collection();
    for (const id of ids) els.merge(cy.getElementById(id));
    if (els.empty()) return;
    cy.animate({ fit: { eles: els, padding: 40 }, duration: reduced() ? 0 : 250 });
  }

  export function fit() {
    if (!cy) return;
    cy.fit(cy.elements(), 30);
    if (cy.zoom() > 1.25) {
      cy.zoom(1.25);
      cy.center();
    }
  }

  export function center(id: string) {
    const n = cy?.getElementById(id);
    if (n && n.nonempty()) cy!.animate({ center: { eles: n }, duration: reduced() ? 0 : 250 });
  }

  export function relayout() {
    layout(true);
  }
</script>

<div class="wrap" bind:this={wrap} data-motion={motionOn ? 'on' : 'off'} data-animations="0">
  <div class="cy" bind:this={host}></div>
  {#if nodes.length === 0}
    <div class="empty-overlay">{emptyText}</div>
  {/if}
  <div class="controls">
    <button class="btn sm icon" title="Fit to view" aria-label="Fit to view" onclick={fit}
      ><Icon name="fit" size={14} /></button
    >
    <button class="btn sm icon" title="Re-run layout" aria-label="Re-run layout" onclick={relayout}
      ><Icon name="refresh" size={14} /></button
    >
  </div>
</div>

<style>
  .wrap {
    position: relative;
    width: 100%;
    height: 100%;
    min-height: 200px;
    background-color: var(--surface);
    background-image: radial-gradient(
      circle,
      color-mix(in srgb, var(--text-3) 22%, transparent) 1px,
      transparent 1px
    );
    background-size: 18px 18px;
  }
  .cy {
    position: absolute;
    inset: 0;
  }
  .controls {
    position: absolute;
    right: 10px;
    top: 10px;
    display: flex;
    gap: 4px;
  }
  .empty-overlay {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    color: var(--text-3);
    pointer-events: none;
  }
</style>

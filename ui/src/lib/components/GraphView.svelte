<script lang="ts" module>
  import cytoscape, { type Core, type ElementDefinition, type StylesheetJson } from 'cytoscape';
  import fcose from 'cytoscape-fcose';

  cytoscape.use(fcose);

  export type GNodeKind = 'iri' | 'literal' | 'bnode' | 'triple';
  export type GNode = { id: string; label: string; kind: GNodeKind; title?: string; focus?: boolean; expanded?: boolean };
  export type GEdge = { id: string; source: string; target: string; label: string; title?: string; isType?: boolean };
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
  }: {
    nodes: GNode[];
    edges: GEdge[];
    selected?: string | null;
    onselect?: (id: string | null) => void;
    onexpand?: (id: string) => void;
    emptyText?: string;
  } = $props();

  let host: HTMLDivElement;
  let cy: Core | undefined;
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
      { selector: 'node[kind = "bnode"]', style: { 'background-color': bnode, width: 12, height: 12 } },
      { selector: 'node[kind = "triple"]', style: { shape: 'diamond', 'background-color': text2 } },
      { selector: 'node[?expanded]', style: { 'border-color': iri, 'border-width': 2, 'background-opacity': 0.55 } },
      { selector: 'node[?focus]', style: { width: 26, height: 26, 'border-color': spark, 'border-width': 3, 'font-weight': 600 } },
      {
        selector: 'node:selected',
        style: { 'border-color': spark, 'border-width': 3, 'overlay-opacity': 0, 'underlay-color': spark, 'underlay-opacity': 0.18, 'underlay-padding': 5 },
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
      { selector: 'edge.quiet', style: { 'text-opacity': 0 } },
      {
        selector: 'edge:selected, edge.hl',
        style: { 'line-color': spark, 'target-arrow-color': spark, width: 2, color: text, 'text-opacity': 1 },
      },
      { selector: '.faded', style: { opacity: 0.25 } },
    ];
  }

  const toEl = {
    node: (n: GNode): ElementDefinition => ({
      group: 'nodes',
      data: {
        id: n.id,
        label: n.label,
        kind: n.kind,
        title: n.title,
        focus: !!n.focus,
        expanded: !!n.expanded,
        w: Math.min(190, n.label.length * 6 + 14),
      },
    }),
    edge: (e: GEdge): ElementDefinition => ({
      group: 'edges',
      data: { id: e.id, source: e.source, target: e.target, label: e.label, title: e.title, isType: !!e.isType },
    }),
  };

  let running: cytoscape.Layouts | undefined;

  function layout(randomize: boolean) {
    if (!cy || cy.nodes().length === 0) return;
    const n = cy.nodes().length;
    // A layout still animating would fight the new one (e.g. focus a node, then its
    // expansion arrives a moment later).
    running?.stop();
    running = cy.layout({
      name: 'fcose',
      quality: n > 400 ? 'draft' : 'default',
      randomize,
      animate: !randomize && n < 300,
      animationDuration: 350,
      fit: randomize || n < 30,
      padding: 30,
      nodeRepulsion: () => (n > 30 ? 12000 : 6500),
      idealEdgeLength: () => (n > 30 ? 120 : 90),
      nodeSeparation: 60,
      packComponents: true,
    } as cytoscape.LayoutOptions);
    running.run();
  }

  function sync() {
    if (!cy) return;
    const wasEmpty = cy.nodes().length === 0;
    const wantNodes = new Map(nodes.map((n) => [n.id, n]));
    const wantEdges = new Map(edges.filter((e) => wantNodes.has(e.source) && wantNodes.has(e.target)).map((e) => [e.id, e]));
    let added = 0;
    cy.batch(() => {
      cy!.elements().forEach((el) => {
        const id = el.id();
        if (el.isNode() ? !wantNodes.has(id) : !wantEdges.has(id)) el.remove();
      });
      for (const n of wantNodes.values()) {
        const existing = cy!.getElementById(n.id);
        if (existing.nonempty()) {
          existing.data({ label: n.label, kind: n.kind, focus: !!n.focus, expanded: !!n.expanded, title: n.title, w: Math.min(190, n.label.length * 6 + 14) });
        } else {
          const el = cy!.add(toEl.node(n));
          added++;
          // Seed position next to an existing neighbour so incremental layouts stay calm.
          const neighbour = edges.find((e) => e.target === n.id || e.source === n.id);
          const other = neighbour ? cy!.getElementById(neighbour.source === n.id ? neighbour.target : neighbour.source) : null;
          if (other && other.nonempty() && other.id() !== n.id) {
            const p = other.position();
            el.position({ x: p.x + (Math.random() - 0.5) * 120, y: p.y + (Math.random() - 0.5) * 120 });
          }
        }
      }
      for (const e of wantEdges.values()) {
        if (cy!.getElementById(e.id).empty()) cy!.add(toEl.edge(e));
      }
    });
    // Too many edge labels turn into noise: show them only on hover/selection.
    cy.edges().toggleClass('quiet', cy.edges().length > 60);
    // Incremental layouts keep the user's arrangement, but when most of the graph is new
    // (a resource and then its neighbours) there is nothing to keep: lay it out afresh.
    if (added) layout(wasEmpty || cy.nodes().length - added <= 2);
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
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
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
    if (n && n.nonempty()) cy!.animate({ center: { eles: n }, duration: 250 });
  }

  export function relayout() {
    layout(true);
  }
</script>

<div class="wrap">
  <div class="cy" bind:this={host}></div>
  {#if nodes.length === 0}
    <div class="empty-overlay">{emptyText}</div>
  {/if}
  <div class="controls">
    <button class="btn sm icon" title="Fit to view" aria-label="Fit to view" onclick={fit}><Icon name="fit" size={14} /></button>
    <button class="btn sm icon" title="Re-run layout" aria-label="Re-run layout" onclick={relayout}><Icon name="refresh" size={14} /></button>
  </div>
</div>

<style>
  .wrap {
    position: relative;
    width: 100%;
    height: 100%;
    min-height: 200px;
    background-color: var(--surface);
    background-image: radial-gradient(circle, color-mix(in srgb, var(--text-3) 22%, transparent) 1px, transparent 1px);
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

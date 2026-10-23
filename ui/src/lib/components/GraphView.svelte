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
    /** A fill in place of the kind's, such as the overview's colouring when it falls back. */
    color?: string;
  };
  export type GEdge = {
    id: string;
    source: string;
    target: string;
    label: string;
    title?: string;
    isType?: boolean;
    change?: Mark;
  };
  /**
   * The validation overlay (docs/specs/U07-data-quality-and-schema-diagram.md §11): a badge
   * per node with results, the elements of the triples a result involves, and what a click
   * on a badge does.
   */
  export type GValidation = {
    badges: ReadonlyMap<
      string,
      { text: string; worst: 'violation' | 'warning' | 'info'; words: string }
    >;
    /** Show triples: the nodes and edges to mark. */
    marked?: { nodes: string[]; edges: string[] } | null;
    onbadge?: (id: string) => void;
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
    validation = null,
  }: {
    nodes: GNode[];
    edges: GEdge[];
    selected?: string | null;
    onselect?: (id: string | null) => void;
    onexpand?: (id: string) => void;
    emptyText?: string;
    /**
     * The shared selection (docs/specs/U11-linked-views.md): when it is not empty, the
     * nodes outside it and the edges that do not join two of its nodes are dimmed.
     */
    highlight?: ReadonlySet<string> | null;
    /**
     * Fade added elements in and removed ones out, and keep the nodes that stay where they
     * are (the time slider). Off with `prefers-reduced-motion`, then changes appear at once.
     */
    animate?: boolean;
    /** The validation overlay's badges and marks; none when null. */
    validation?: GValidation | null;
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
      { selector: 'node[?color]', style: { 'background-color': 'data(color)' } },
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
      {
        selector: 'node.vmark',
        style: {
          'border-color': cssVar('--danger'),
          'border-width': 3,
          'underlay-color': cssVar('--danger'),
          'underlay-opacity': 0.2,
          'underlay-padding': 6,
        },
      },
      {
        selector: 'edge.vmark',
        style: {
          'line-color': cssVar('--danger'),
          'target-arrow-color': cssVar('--danger'),
          width: 2.6,
          'text-opacity': 1,
        },
      },
      { selector: '.faded', style: { opacity: 0.25 } },
      { selector: '.muted', style: { opacity: 0.22 } },
    ];
  }

  /** The sign a changed element's label starts with, so that colour is not the only cue. */
  const sign = (c: Mark | undefined) => (c === 'added' ? '+ ' : c === 'removed' ? '− ' : '');

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
        color: n.color ?? '',
        w: Math.min(190, n.label.length * 6 + 14),
      },
    }),
    edge: (e: GEdge): ElementDefinition => ({
      group: 'edges',
      data: {
        id: e.id,
        source: e.source,
        target: e.target,
        label: sign(e.change) + e.label,
        change: e.change ?? '',
        title: e.title,
        isType: !!e.isType,
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
            color: n.color ?? '',
            w: Math.min(190, n.label.length * 6 + 14),
          });
        } else {
          const el = cy!.add(toEl.node(n));
          added++;
          fresh.add(n.id);
          if (fade) fadeIn(el);
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
          if (fade) fadeIn(el);
        } else {
          if (have.hasClass('leaving')) {
            have.stop(true);
            have.removeClass('leaving');
            have.removeStyle('opacity');
          }
          have.data({ change: e.change ?? '', label: sign(e.change) + e.label });
        }
      }
    });
    // what is drawn, as text, for the tests: the canvas has no elements to find
    if (wrap && nodes.length <= 300)
      wrap.dataset.graph = JSON.stringify({
        nodes: [...wantNodes.values()].map((n) => [n.label, n.change ?? '']),
        edges: [...wantEdges.values()].map((e) => [e.label, e.change ?? '']),
      });
    // Too many edge labels turn into noise: show them only on hover/selection.
    cy.edges().toggleClass('quiet', cy.edges().length > 60);
    // Incremental layouts keep the user's arrangement, but when most of the graph is new
    // (a resource and then its neighbours) there is nothing to keep: lay it out afresh.
    if (added) {
      const randomize = wasEmpty || cy.nodes().length - added <= 2;
      // in the time slider the nodes that stay keep their places
      const keep = animate
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

  // the shared selection dims what it does not hold
  $effect(() => {
    const keep = highlight;
    void nodes;
    void edges;
    if (!cy) return;
    cy.batch(() => {
      cy!.elements().removeClass('muted');
      if (!keep || !keep.size) return;
      cy!
        .nodes()
        .filter((n) => !keep.has(n.id()))
        .addClass('muted');
      cy!
        .edges()
        .filter((e) => !(keep.has(e.source().id()) && keep.has(e.target().id())))
        .addClass('muted');
    });
    // for the tests: the canvas has no elements to find
    if (wrap) wrap.dataset.muted = String(cy.elements('.muted').length);
  });

  // --- the validation overlay's badges: buttons over the canvas, placed at their nodes ---
  type Placed = { id: string; x: number; y: number; text: string; worst: string; label: string };
  let placed = $state<Placed[]>([]);
  let frame = 0;
  function place() {
    frame = 0;
    const b = validation?.badges;
    if (!cy || !b || !b.size) {
      if (placed.length) placed = [];
      return;
    }
    const out: Placed[] = [];
    for (const [id, badge] of b) {
      const n = cy.getElementById(id);
      if (n.empty() || !n.isNode() || n.hasClass('leaving') || !n.visible()) continue;
      const p = n.renderedPosition();
      const w = n.renderedWidth();
      const h = n.renderedHeight();
      out.push({
        id,
        x: p.x + w / 2,
        y: p.y - h / 2,
        text: badge.text,
        worst: badge.worst,
        label: `${n.data('label') || id}, ${badge.words}`,
      });
    }
    placed = out;
  }
  const schedulePlace = () => {
    if (!frame) frame = requestAnimationFrame(place);
  };
  $effect(() => {
    void validation;
    void nodes;
    if (!cy) return;
    cy.off('viewport position add remove layoutstop', schedulePlace);
    if (validation?.badges.size) cy.on('viewport position add remove layoutstop', schedulePlace);
    schedulePlace();
    // for the tests: the badges as text
    if (wrap)
      wrap.dataset.badges = JSON.stringify(
        [...(validation?.badges ?? new Map())].map(([id, x]) => [id, x.text]),
      );
  });
  // Show triples: the marked nodes and edges
  $effect(() => {
    const m = validation?.marked;
    void nodes;
    void edges;
    if (!cy) return;
    cy.batch(() => {
      cy!.elements('.vmark').removeClass('vmark');
      if (!m) return;
      for (const id of [...m.nodes, ...m.edges]) cy!.getElementById(id).addClass('vmark');
    });
    if (wrap) wrap.dataset.vmarked = String(cy.elements('.vmark').length);
  });
  onDestroy(() => {
    if (frame) cancelAnimationFrame(frame);
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
    if (n && n.nonempty()) cy!.animate({ center: { eles: n }, duration: reduced() ? 0 : 250 });
  }

  export function relayout() {
    layout(true);
  }
</script>

<div class="wrap" bind:this={wrap} data-motion={motionOn ? 'on' : 'off'} data-animations="0">
  <div class="cy" bind:this={host}></div>
  {#each placed as b (b.id)}
    <button
      class="vbadge {b.worst}"
      style:left="{b.x}px"
      style:top="{b.y}px"
      aria-label={b.label}
      title={b.label}
      data-node={b.id}
      onclick={() => validation?.onbadge?.(b.id)}>{b.text}</button
    >
  {/each}
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
  .vbadge {
    position: absolute;
    transform: translate(-30%, -70%);
    font: 600 10px/1 var(--font-ui);
    padding: 2px 4px;
    border-radius: 8px;
    border: 1px solid var(--surface);
    color: var(--surface);
    background: var(--danger);
    cursor: pointer;
    white-space: nowrap;
  }
  .vbadge.warning {
    background: var(--warn);
  }
  .vbadge.info {
    background: var(--text-2);
  }
  .vbadge:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: 1px;
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

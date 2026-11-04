<script lang="ts">
  // The graph view of an entity: its neighbourhood on the Explore canvas, with the entity,
  // its parents, its first children and the entities its axioms relate it to or that relate
  // to it, as edges named by the property. The lenses are Explore's, and double-clicking a
  // node adds its neighbourhood. In a card, `lead` is the card's own heading row and the
  // graph's tools sit under it in the card's header (ROADMAP item 633). A `compact` graph fits
  // a narrow rail with the parents and first children, and Expand opens the whole neighbourhood
  // in a dialog (item 734).
  import { onDestroy, onMount, type Snippet } from 'svelte';
  import { hierarchyOf, type GraphPiece, type Linker } from '$lib/browse';
  import { Lenses } from '$lib/lenses.svelte';
  import GraphView, { type GEdge, type GNode } from '$components/GraphView.svelte';
  import LensMenu from '$components/LensMenu.svelte';
  import LensLegend from '$components/LensLegend.svelte';
  import Icon from '$components/Icon.svelte';
  import Modal from '$components/Modal.svelte';
  import Self from './TermGraph.svelte';

  let {
    ds,
    piece,
    linker,
    exploreHref,
    onexpand,
    epoch = 0,
    lead,
    shut = false,
    compact = false,
    fitOnOpen = false,
  }: {
    /** The dataset with its branch, for the lenses' reads. */
    ds: string;
    piece: GraphPiece;
    linker: Linker;
    exploreHref: string;
    /** Add the neighbourhood of a node. */
    onexpand?: (iri: string) => void;
    epoch?: number;
    /** The card's heading row; the graph then draws as the card's body, without a frame. */
    lead?: Snippet;
    /** The card is collapsed to its heading. */
    shut?: boolean;
    /**
     * In a narrow rail: only the parents and the first children, with an Expand button that
     * opens the whole neighbourhood in a large view.
     */
    compact?: boolean;
    /** Fit the graph once the view has its size, for a graph in a dialog. */
    fitOnOpen?: boolean;
  } = $props();

  const lenses = new Lenses();
  onDestroy(() => lenses.destroy());
  let view: GraphView | undefined = $state();
  let toolbar: HTMLElement | undefined = $state();
  let selected = $state<string | null>(null);
  let big = $state(false);
  onMount(() => {
    if (!fitOnOpen) return;
    const t = setTimeout(() => view?.fit(), 400);
    return () => clearTimeout(t);
  });

  const shown = $derived(compact ? hierarchyOf(piece) : piece);
  const nodes = $derived<GNode[]>(
    shown.nodes.map((n) => ({
      id: n.id,
      label: n.label,
      kind: 'iri',
      title: n.id,
      focus: n.focus,
      term: { type: 'uri', value: n.id },
    })),
  );
  const edges = $derived<GEdge[]>(
    shown.edges.map((e) => ({ id: e.id, source: e.source, target: e.target, label: e.label })),
  );
  $effect(() => {
    lenses.setInput({
      ds,
      epoch,
      nodes: nodes.map((n) => ({ id: n.id, term: n.term })),
      edges: edges.map((e) => ({ id: e.id, source: e.source, target: e.target })),
      reasoning: false,
      label: (id) => shown.nodes.find((n) => n.id === id)?.label ?? id,
      short: (iri) => linker.short(iri) ?? linker.name(iri),
    });
  });
  const pick = $derived(selected ? shown.nodes.find((n) => n.id === selected) : null);
</script>

<div class="canvas" class:card={!!lead} class:shut class:compact>
  {#if lead}<div class="lead">{@render lead()}</div>{/if}
  <div class="bar" bind:this={toolbar}>
    <LensMenu {lenses} anchor={toolbar} short={(iri) => linker.short(iri) ?? linker.name(iri)} />
    <span class="spacer"></span>
    {#if pick && !compact && linker.href(pick.id)}
      <a class="btn sm" href={linker.href(pick.id)}>Open {pick.label}</a>
    {/if}
    <button class="btn sm" type="button" onclick={() => view?.fit()}
      ><Icon name="fit" size={14} />Fit</button
    >
    {#if compact}
      <button class="btn sm" type="button" onclick={() => (big = true)}
        ><Icon name="expand" size={14} />Expand</button
      >
    {:else}
      <a class="btn sm" href={exploreHref}><Icon name="explore" size={14} />Open in Explore</a>
    {/if}
  </div>
  <div class="host">
    <GraphView
      bind:this={view}
      {nodes}
      {edges}
      {selected}
      showControls={false}
      onselect={(id) => (selected = id)}
      onexpand={(id) => onexpand?.(id)}
      lens={lenses.encoding}
      emptyText="Nothing to draw"
    />
  </div>
  <div class="legend">
    <LensLegend results={lenses.results} {ds} />
    {#if !compact}
      <span class="faint">Double-click a node to add its neighbours.</span>
    {/if}
  </div>
</div>

{#if compact}
  <Modal
    bind:open={big}
    title="Graph of {piece.nodes.find((n) => n.focus)?.label ?? 'the entity'}"
    width={1100}
  >
    <Self {ds} {piece} {linker} {exploreHref} {onexpand} {epoch} fitOnOpen />
  </Modal>
{/if}

<style>
  .canvas {
    position: relative;
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto;
    height: 560px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    overflow: hidden;
    min-width: 0;
  }
  .bar {
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
    padding: 10px;
    border-bottom: 1px solid var(--border);
  }
  .host {
    position: relative;
    min-height: 0;
    min-width: 0;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    align-items: center;
    padding: 6px 12px;
    font-size: var(--fs-xs);
    border-top: 1px solid var(--border);
  }
  /* in a card: the card's heading, the tools, the canvas and the legend */
  .canvas.card {
    grid-template-rows: auto auto minmax(0, 1fr) auto;
    height: var(--term-graph-h, 480px);
    border: 0;
    border-radius: 0;
    background: none;
  }
  .lead {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2) 14px;
    min-height: var(--panel-head-h);
    border-bottom: 1px solid var(--border);
  }
  .card .bar {
    padding: 8px 14px;
  }
  /* in the rail the canvas is a column of the card's width and shorter */
  .canvas.card.compact {
    height: var(--term-graph-h, 400px);
  }
  @container browse (max-width: 639px) {
    .canvas.card.compact.shut {
      height: auto;
    }
  }
  .compact .bar {
    padding: 8px 12px;
    gap: 6px;
  }
  .compact .legend {
    padding: 6px 12px;
  }
  .compact .legend:empty {
    display: none;
  }
  /* a card collapsed to its heading on a phone */
  @container browse (max-width: 639px) {
    .canvas.shut {
      height: auto;
      grid-template-rows: auto;
    }
    .canvas.shut > :not(.lead) {
      display: none;
    }
    .canvas.shut .lead {
      border-bottom: 0;
    }
  }
  @container browse (max-width: 639px) {
    .canvas {
      height: 420px;
    }
  }
</style>

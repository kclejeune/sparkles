<script lang="ts">
  // The graph view of an entity: its neighbourhood on the Explore canvas, with the entity,
  // its parents, its first children and the entities its axioms relate it to or that relate
  // to it, as edges named by the property. The lenses are Explore's, and double-clicking a
  // node adds its neighbourhood.
  import { onDestroy } from 'svelte';
  import type { GraphPiece, Linker } from '$lib/browse';
  import { Lenses } from '$lib/lenses.svelte';
  import GraphView, { type GEdge, type GNode } from '$components/GraphView.svelte';
  import LensMenu from '$components/LensMenu.svelte';
  import LensLegend from '$components/LensLegend.svelte';
  import Icon from '$components/Icon.svelte';

  let {
    ds,
    piece,
    linker,
    exploreHref,
    onexpand,
    epoch = 0,
  }: {
    /** The dataset with its branch, for the lenses' reads. */
    ds: string;
    piece: GraphPiece;
    linker: Linker;
    exploreHref: string;
    /** Add the neighbourhood of a node. */
    onexpand?: (iri: string) => void;
    epoch?: number;
  } = $props();

  const lenses = new Lenses();
  onDestroy(() => lenses.destroy());
  let view: GraphView | undefined = $state();
  let toolbar: HTMLElement | undefined = $state();
  let selected = $state<string | null>(null);

  const nodes = $derived<GNode[]>(
    piece.nodes.map((n) => ({
      id: n.id,
      label: n.label,
      kind: 'iri',
      title: n.id,
      focus: n.focus,
      term: { type: 'uri', value: n.id },
    })),
  );
  const edges = $derived<GEdge[]>(
    piece.edges.map((e) => ({ id: e.id, source: e.source, target: e.target, label: e.label })),
  );
  $effect(() => {
    lenses.setInput({
      ds,
      epoch,
      nodes: nodes.map((n) => ({ id: n.id, term: n.term })),
      edges: edges.map((e) => ({ id: e.id, source: e.source, target: e.target })),
      reasoning: false,
      label: (id) => piece.nodes.find((n) => n.id === id)?.label ?? id,
      short: (iri) => linker.short(iri) ?? linker.name(iri),
    });
  });
  const pick = $derived(selected ? piece.nodes.find((n) => n.id === selected) : null);
</script>

<div class="canvas">
  <div class="bar" bind:this={toolbar}>
    <LensMenu {lenses} anchor={toolbar} short={(iri) => linker.short(iri) ?? linker.name(iri)} />
    <span class="spacer"></span>
    {#if pick && linker.href(pick.id)}
      <a class="btn sm" href={linker.href(pick.id)}>Open {pick.label}</a>
    {/if}
    <button class="btn sm" type="button" onclick={() => view?.fit()}
      ><Icon name="fit" size={14} />Fit</button
    >
    <a class="btn sm" href={exploreHref}><Icon name="explore" size={14} />Open in Explore</a>
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
    <span class="faint">Double-click a node to add its neighbours.</span>
  </div>
</div>

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
  @container browse (max-width: 639px) {
    .canvas {
      height: 420px;
    }
  }
</style>

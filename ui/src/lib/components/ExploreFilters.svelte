<script lang="ts" module>
  /** The person's choices of what to draw (ROADMAP item 725). */
  export type FilterState = {
    /** Predicate IRIs whose edges are not drawn. */
    predicates: Set<string>;
    /** Class ids (node ids) whose members are not drawn. */
    classes: Set<string>;
    minDegree: number;
    hideLeaves: boolean;
    budget: number;
    /** The groups of a colour lens the person hid, with the nodes each held when hidden. */
    hiddenGroups: Map<string, { text: string; ids: string[] }>;
  };
  export const freshFilters = (budget: number): FilterState => ({
    predicates: new Set(),
    classes: new Set(),
    minDegree: 0,
    hideLeaves: false,
    budget,
    hiddenGroups: new Map(),
  });
  /** A group of the colour lens that can be hidden, such as a community. */
  export type GroupEntry = { key: string; text: string; count: number; ids: string[] };
</script>

<script lang="ts">
  // The filters of Explore: they choose what the canvas draws and never what the page holds,
  // so a hidden edge type, class or group is one click from coming back with no new read.
  import { fmtInt } from '$lib/format';
  import { BUDGETS, NO_CLASS, type FacetEntry } from '$lib/explore-filters';
  import Icon from './Icon.svelte';

  let {
    value = $bindable(),
    predicates,
    classes,
    predicateLabel,
    classLabel,
    held,
    drawn,
    hidden,
    overBudget,
    groups,
    groupsTitle,
    canCommunities,
    oncommunities,
    maxDegree,
    webglNote = null,
    onclose,
  }: {
    value: FilterState;
    predicates: FacetEntry[];
    classes: FacetEntry[];
    predicateLabel: (iri: string) => string;
    classLabel: (id: string) => string;
    held: { nodes: number; edges: number };
    drawn: { nodes: number; edges: number };
    hidden: number;
    overBudget: number;
    /** The groups of the colour lens, when it has some. */
    groups: GroupEntry[];
    groupsTitle: string;
    /** The dataset has GraphRAG communities that the colour lens can use. */
    canCommunities: boolean;
    oncommunities: () => void;
    /** The largest degree among the held nodes, the slider's end. */
    maxDegree: number;
    webglNote?: string | null;
    onclose: () => void;
  } = $props();

  const set = <T>(s: Set<T>, v: T, on: boolean) => {
    const n = new Set(s);
    if (on) n.add(v);
    else n.delete(v);
    return n;
  };
  const active = $derived(
    value.predicates.size > 0 ||
      value.classes.size > 0 ||
      value.minDegree > 0 ||
      value.hideLeaves ||
      value.hiddenGroups.size > 0,
  );
  function reset() {
    value = {
      ...value,
      predicates: new Set(),
      classes: new Set(),
      minDegree: 0,
      hideLeaves: false,
      hiddenGroups: new Map(),
    };
  }
  function toggleGroup(g: GroupEntry, shown: boolean) {
    const m = new Map(value.hiddenGroups);
    if (shown) m.delete(g.key);
    else m.set(g.key, { text: g.text, ids: g.ids });
    value = { ...value, hiddenGroups: m };
  }
  // the hidden groups are listed with the live ones, so that each can be shown again
  const allGroups = $derived.by(() => {
    const live = new Set(groups.map((g) => g.key));
    const gone: GroupEntry[] = [...value.hiddenGroups]
      .filter(([k]) => !live.has(k))
      .map(([key, g]) => ({ key, text: g.text, count: g.ids.length, ids: g.ids }));
    return [...groups, ...gone];
  });
  const top = $derived(Math.max(1, Math.min(maxDegree, 50)));
</script>

<section class="filters" aria-label="Filters" data-testid="explore-filters">
  <div class="fhead">
    <h2>Filters</h2>
    <span class="faint small">choose what is drawn, not what is loaded</span>
    <span class="spacer"></span>
    {#if active}<button class="btn sm ghost" onclick={reset}>Reset</button>{/if}
    <button class="btn sm ghost icon" aria-label="Close the filters" onclick={onclose}
      ><Icon name="x" size={14} /></button
    >
  </div>
  <div class="fbody">
    <p class="counts" role="status" data-testid="filter-counts">
      <b>{fmtInt(drawn.nodes)}</b> of {fmtInt(held.nodes)} nodes and
      <b>{fmtInt(drawn.edges)}</b> of {fmtInt(held.edges)} edges drawn
      {#if overBudget > 0}
        <span class="warn-text"
          >· {fmtInt(overBudget)} left out by the budget, the least connected first</span
        >
      {/if}
    </p>
    {#if webglNote}<p class="faint small">{webglNote}</p>{/if}

    <div class="block">
      <h3>Draw up to</h3>
      <select
        class="select sm"
        aria-label="Nodes to draw at most"
        value={String(value.budget)}
        onchange={(e) => (value = { ...value, budget: Number(e.currentTarget.value) })}
      >
        {#each BUDGETS as b (b)}<option value={String(b)}>{fmtInt(b)} nodes</option>{/each}
      </select>
    </div>

    <div class="block">
      <h3>Connections</h3>
      <label class="slider"
        ><span>Minimum degree <b data-testid="min-degree">{value.minDegree}</b></span>
        <input
          type="range"
          min="0"
          max={top}
          step="1"
          value={value.minDegree}
          aria-label="Minimum degree"
          oninput={(e) => (value = { ...value, minDegree: Number(e.currentTarget.value) })}
        /></label
      >
      <label class="check"
        ><input
          type="checkbox"
          checked={value.hideLeaves}
          onchange={(e) => (value = { ...value, hideLeaves: e.currentTarget.checked })}
        />
        Hide leaves <span class="faint">nodes with one edge</span></label
      >
    </div>

    <div class="block">
      <h3>Edge types <span class="faint">{fmtInt(predicates.length)}</span></h3>
      {#if !predicates.length}<p class="faint small">No edges yet.</p>{/if}
      <ul class="rows" aria-label="Edge types">
        {#each predicates as p (p.key)}
          <li>
            <label class="check"
              ><input
                type="checkbox"
                checked={!value.predicates.has(p.key)}
                onchange={(e) =>
                  (value = {
                    ...value,
                    predicates: set(value.predicates, p.key, !e.currentTarget.checked),
                  })}
              />
              <span class="name mono" title={p.key}>{predicateLabel(p.key)}</span>
              <span class="n">{fmtInt(p.count)}</span></label
            >
          </li>
        {/each}
      </ul>
    </div>

    <div class="block">
      <h3>Classes <span class="faint">{fmtInt(classes.length)}</span></h3>
      {#if !classes.length}<p class="faint small">No typed nodes yet.</p>{/if}
      <ul class="rows" aria-label="Classes">
        {#each classes as c (c.key)}
          <li>
            <label class="check"
              ><input
                type="checkbox"
                checked={!value.classes.has(c.key)}
                onchange={(e) =>
                  (value = {
                    ...value,
                    classes: set(value.classes, c.key, !e.currentTarget.checked),
                  })}
              />
              <span class="name" title={c.key}
                >{c.key === NO_CLASS ? 'No class' : classLabel(c.key)}</span
              >
              <span class="n">{fmtInt(c.count)}</span></label
            >
          </li>
        {/each}
      </ul>
    </div>

    {#if canCommunities || allGroups.length}
      <div class="block">
        <h3>{groupsTitle}</h3>
        {#if canCommunities}
          <button class="btn sm" onclick={oncommunities}>Group by communities</button>
        {/if}
        <ul class="rows" aria-label={groupsTitle}>
          {#each allGroups as g (g.key)}
            <li>
              <label class="check"
                ><input
                  type="checkbox"
                  checked={!value.hiddenGroups.has(g.key)}
                  onchange={(e) => toggleGroup(g, e.currentTarget.checked)}
                />
                <span class="name" title={g.text}>{g.text}</span>
                <span class="n">{fmtInt(g.count)}</span></label
              >
            </li>
          {/each}
        </ul>
      </div>
    {/if}
  </div>
</section>

<style>
  .filters {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
    background: var(--surface);
  }
  .fhead {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px 8px;
    padding: 10px 12px 8px;
    border-bottom: 1px solid var(--border);
  }
  .fhead h2 {
    font-size: var(--fs-md);
    margin: 0;
    font-weight: 600;
  }
  .fbody {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 4px 12px 12px;
  }
  .counts {
    margin: 8px 0 4px;
    font-size: var(--fs-sm);
  }
  .block {
    padding: 8px 0 10px;
    border-bottom: 1px solid var(--border);
  }
  .block:last-child {
    border-bottom: 0;
  }
  h3 {
    margin: 0 0 6px;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 220px;
    overflow: auto;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 26px;
    font-size: var(--fs-sm);
    cursor: pointer;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .n {
    flex: none;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
    font-size: var(--fs-xs);
  }
  .slider {
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
    margin-bottom: 6px;
  }
  .slider input {
    width: 100%;
  }
  .small {
    font-size: var(--fs-xs);
  }
  @media (max-width: 760px) {
    .check {
      min-height: 36px;
    }
    .rows {
      max-height: 180px;
    }
  }
</style>

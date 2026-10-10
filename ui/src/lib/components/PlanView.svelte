<script lang="ts">
  import { tick, type Snippet } from 'svelte';
  import { planOf, type CursorPlan, type PlanNode } from '$lib/api';
  import type { Severity } from '$lib/explain';
  import { fmtCompact, fmtInt, fmtMs } from '$lib/format';
  import { shorten, type PrefixMap } from '$lib/rdf';
  import Icon from './Icon.svelte';

  let {
    plan: given,
    executed = true,
    prefixes = {},
    selected = null,
    marks = new Map(),
    lit = null,
    onhover,
    onselect,
    toolbar,
  }: {
    /** An eager `PlanNode` tree or a streamed `CursorPlan` tree. */
    plan: PlanNode | CursorPlan;
    executed?: boolean;
    prefixes?: PrefixMap;
    /** The node to select, expand the path to and scroll into view. */
    selected?: string | null;
    /** The severity of each node's notes, shown as a mark before its name. */
    marks?: Map<string, Severity>;
    /** Nodes to highlight, such as those a hovered sentence cites. */
    lit?: Set<string> | null;
    onhover?: (id: string | null) => void;
    onselect?: (id: string) => void;
    /** More controls at the end of the toolbar. */
    toolbar?: Snippet;
  } = $props();

  const plan = $derived(planOf(given));

  /**
   * Operator description for display: `<iri>` as a prefixed name where a prefix matches,
   * and the parser's generated 32-hex variable names (aggregates, DESCRIBE) abbreviated.
   * The full text stays available in the tooltip.
   */
  function desc(d: string): string {
    return d
      .replace(/<([^<>\s]+)>/g, (m, iri: string) => shorten(iri, prefixes) ?? m)
      .replace(/\?([0-9a-f]{32})\b/g, (_, h: string) => `?_${h.slice(0, 6)}`);
  }

  type Flat = {
    node: PlanNode;
    id: string;
    depth: number;
    self: number;
    hasKids: boolean;
    last: boolean[];
  };

  let collapsed = $state(new Set<string>());
  let mode = $state<'tree' | 'flame'>('tree');
  let hovered = $state<string | null>(null);

  const selfTime = (n: PlanNode) =>
    Math.max(0, n.timeMs - n.children.reduce((a, c) => a + (c.timeMs || 0), 0));

  const all = $derived.by(() => {
    const out: Flat[] = [];
    const walk = (n: PlanNode, derived: string, depth: number, last: boolean[]) => {
      const id = n.id ?? derived;
      out.push({ node: n, id, depth, self: selfTime(n), hasKids: n.children.length > 0, last });
      n.children.forEach((c, i) =>
        walk(c, `${id}.${i}`, depth + 1, [...last, i === n.children.length - 1]),
      );
    };
    walk(plan, '0', 0, []);
    return out;
  });

  const rows = $derived(all.filter((r) => ![...collapsed].some((c) => r.id.startsWith(c + '.'))));

  /** The slowest nodes by self time (top 3 with a meaningful share). */
  const hot = $derived.by(() => {
    if (!executed) return new Map<string, number>();
    const total = plan.timeMs || 1;
    const ranked = [...all]
      .sort((a, b) => b.self - a.self)
      .filter((r) => r.self / total > 0.05)
      .slice(0, 3);
    return new Map(ranked.map((r, i) => [r.id, i]));
  });

  const metric = (n: PlanNode) => (executed && plan.timeMs > 0 ? n.timeMs : n.estimatedCost);
  const rootMetric = $derived(Math.max(metric(plan), 1e-9));

  function toggle(id: string) {
    const next = new Set(collapsed);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    collapsed = next;
  }

  /**
   * Ratio of actual to estimated rows; flags big misestimates. A hidden estimate (`-1`),
   * a node that stopped early or did not run, and partial counts are not misestimates.
   */
  function misestimate(n: PlanNode): number | null {
    if (!executed || n.actualRows < 0 || n.estimatedRows < 0) return null;
    if (n.stoppedEarly || n.skipped || n.complete === false) return null;
    const a = Math.max(n.actualRows, 1);
    const e = Math.max(n.estimatedRows, 1);
    const r = a > e ? a / e : e / a;
    return r >= 10 ? r : null;
  }

  // Icicle layout: x/width in [0,1] proportional to time (or cost when not executed).
  type Box = { id: string; node: PlanNode; x: number; w: number; depth: number };
  const boxes = $derived.by(() => {
    const out: Box[] = [];
    const place = (n: PlanNode, id: string, x: number, w: number, depth: number) => {
      out.push({ id, node: n, x, w, depth });
      const total = metric(n) || 1;
      const kidsTotal = n.children.reduce((a, c) => a + metric(c), 0);
      // Children never exceed the parent; if they do (estimates), normalise.
      const scale = kidsTotal > total ? total / kidsTotal : 1;
      let cx = x;
      n.children.forEach((c, i) => {
        const cw = (w * metric(c) * scale) / total;
        place(c, `${id}.${i}`, cx, cw, depth + 1);
        cx += cw;
      });
    };
    place(plan, '0', 0, 1, 0);
    return out;
  });
  const depth = $derived(Math.max(...boxes.map((b) => b.depth)) + 1);

  /** Counts that a failure cut short. */
  const partial = (n: PlanNode) =>
    executed && n.complete === false && n.actualRows >= 0 && !n.skipped;
  const anyPartial = $derived(all.some((r) => partial(r.node)));
  const MARK: Record<Severity, string> = { high: '●', warning: '⚠', info: 'ⓘ' };

  let tree = $state<HTMLElement | undefined>();
  // selecting a node expands the path to it and scrolls it into view
  $effect(() => {
    const id = selected;
    if (!id) return;
    mode = 'tree';
    const open = [...collapsed].filter((c) => !(id === c || id.startsWith(c + '.')));
    if (open.length !== collapsed.size) collapsed = new Set(open);
    void tick().then(() => {
      const el = tree?.querySelector<HTMLElement>(`[data-node="${CSS.escape(id)}"]`);
      el?.scrollIntoView?.({ block: 'nearest' });
    });
  });
  const hoveredNode = $derived(all.find((r) => r.id === hovered)?.node ?? null);
</script>

<div class="plan">
  <div class="toolbar">
    <div class="tabs" role="tablist">
      <button
        class="tab"
        role="tab"
        aria-selected={mode === 'tree'}
        onclick={() => (mode = 'tree')}
      >
        <Icon name="tree" size={14} /> Tree
      </button>
      <button
        class="tab"
        role="tab"
        aria-selected={mode === 'flame'}
        onclick={() => (mode = 'flame')}
      >
        <Icon name="layers" size={14} /> Flame
      </button>
    </div>
    <span class="spacer"></span>
    {#if executed}
      <span class="legend"><span class="sw hot0"></span>slowest self time</span>
      <span class="legend"><span class="sw mis"></span>estimate off ≥10×</span>
      {#if anyPartial}<span class="legend" title="Counted until the query stopped"
          ><sup>p</sup> partial, counted until the query stopped</span
        >{/if}
    {:else}
      <span class="faint">Not executed: widths show estimated cost</span>
    {/if}
    {#if toolbar}{@render toolbar()}{/if}
  </div>

  {#if mode === 'tree'}
    <div class="tree scroll" bind:this={tree}>
      <div class="thead" class:est={!executed}>
        <span>Operator</span>
        <span class="num">Est. rows</span>
        {#if executed}
          <span class="num">Actual rows</span>
          <span class="num">Self</span>
          <span class="num">Total</span>
        {/if}
        <span>Share of {executed ? 'time' : 'cost'}</span>
      </div>
      {#each rows as r (r.id)}
        {@const h = hot.get(r.id)}
        {@const mis = misestimate(r.node)}
        {@const mark = marks.get(r.id)}
        <div
          class="tr"
          class:hot={h != null}
          class:est={!executed}
          class:selected={selected === r.id}
          class:lit={lit?.has(r.id)}
          data-rank={h}
          data-node={r.id}
          role="group"
          aria-label="{r.node.operator} {r.id}"
          aria-current={selected === r.id ? 'true' : undefined}
          onmouseenter={() => onhover?.(r.id)}
          onmouseleave={() => onhover?.(null)}
        >
          <div class="op" style:padding-left="{r.depth * 18 + 6}px">
            {#each r.last as isLast, d (d)}
              <span
                class="guide"
                style:left="{d * 18 + 14}px"
                class:end={isLast && d === r.last.length - 1}
              ></span>
            {/each}
            {#if r.hasKids}
              <button
                class="twisty"
                class:open={!collapsed.has(r.id)}
                onclick={() => toggle(r.id)}
                aria-label="Toggle children"
              >
                <Icon name="chevron" size={12} />
              </button>
            {:else}
              <span class="twisty-space"></span>
            {/if}
            {#if mark}<span class="mark {mark}" title="{mark} note">{MARK[mark]}</span>{/if}
            <button class="name" onclick={() => onselect?.(r.id)} title="Node {r.id}"
              >{r.node.operator}</button
            >
            <span class="desc" title={r.node.description}>{desc(r.node.description)}</span>
            {#if r.node.cached}<span class="badge ok" title="Served from the result cache"
                >cached</span
              >{/if}
            {#if r.node.skipped}<span class="badge" title={r.node.skipped}>skipped</span>{/if}
            {#if r.node.stoppedEarly}<span
                class="badge"
                title="Stopped once it had enough rows for a LIMIT, ASK or EXISTS"
                >stopped early</span
              >{/if}
            {#if r.node.runs && r.node.runs > 1}<span
                class="badge"
                title="Ran {r.node.runs} times as the executor grew its input">×{r.node.runs}</span
              >{/if}
            {#if r.node.materializes}<span
                class="badge"
                title={r.node.reason ?? 'Reads its whole input before its first row'}
                >materializes</span
              >{/if}
            {#if h != null}<span class="badge spark">#{h + 1} slowest</span>{/if}
          </div>
          {#if r.node.estimatedRows < 0}
            <span class="num faint" title="Estimates are hidden for your view">hidden</span>
          {:else}
            <span class="num faint" title={r.node.estimateGuessed ? 'A constant guess' : undefined}
              >{r.node.estimateGuessed ? '~' : ''}{fmtCompact(r.node.estimatedRows)}</span
            >
          {/if}
          {#if executed}
            <span
              class="num"
              class:mis={mis != null}
              title={mis ? `Off by ${mis.toFixed(0)}× vs estimate` : undefined}
            >
              {r.node.actualRows < 0 ? '—' : fmtInt(r.node.actualRows)}{#if partial(r.node)}<sup
                  title="Partial: counted until the query stopped">p</sup
                >{/if}
            </span>
            <span class="num">{fmtMs(r.self)}</span>
            <span class="num faint">{fmtMs(r.node.timeMs)}</span>
          {/if}
          <div
            class="bar"
            title={executed
              ? `${fmtMs(r.node.timeMs)} total, ${fmtMs(r.self)} self`
              : `cost ${fmtInt(r.node.estimatedCost)}`}
          >
            <span class="total" style:width="{(metric(r.node) / rootMetric) * 100}%"></span>
            {#if executed}<span class="self" style:width="{(r.self / rootMetric) * 100}%"
              ></span>{/if}
          </div>
        </div>
      {/each}
    </div>
  {:else}
    <div class="flame scroll">
      <div class="icicle" style:height="{depth * 26}px">
        {#each boxes as b (b.id)}
          {@const h = hot.get(b.id)}
          <button
            class="box"
            class:hot={h != null}
            class:thin={b.w < 0.04}
            style:left="{b.x * 100}%"
            style:width="calc({b.w * 100}% - 2px)"
            style:top="{b.depth * 26}px"
            onmouseenter={() => (hovered = b.id)}
            onmouseleave={() => (hovered = null)}
            onfocus={() => (hovered = b.id)}
            title="{b.node.operator} {b.node.description}"
          >
            <span>{b.node.operator}</span>
            <span class="d">{desc(b.node.description)}</span>
          </button>
        {/each}
      </div>
      <div class="detail">
        {#if hoveredNode}
          <strong>{hoveredNode.operator}</strong>
          <span class="mono">{desc(hoveredNode.description)}</span>
          <span class="muted">
            est {fmtInt(hoveredNode.estimatedRows)} rows{#if hoveredNode.actualRows >= 0}, actual {fmtInt(
                hoveredNode.actualRows,
              )}{/if}{#if executed},
              {fmtMs(hoveredNode.timeMs)} total, {fmtMs(selfTime(hoveredNode))} self{/if}
          </span>
          {#if hoveredNode.columns.length}<span class="faint mono"
              >→ {hoveredNode.columns.join(' ')}</span
            >{/if}
          {#if hoveredNode.sortedOn.length}<span class="faint"
              >sorted on <span class="mono">{hoveredNode.sortedOn.join(', ')}</span></span
            >{/if}
        {:else}
          <span class="faint"
            >Hover a box for details. Width is proportional to {executed
              ? 'wall time'
              : 'estimated cost'}.</span
          >
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .plan {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 12px;
    border-bottom: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .legend {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text-2);
  }
  .sw {
    width: 10px;
    height: 10px;
    border-radius: 2px;
  }
  .sw.hot0 {
    background: var(--spark);
  }
  .sw.mis {
    background: var(--danger);
  }
  .tree {
    flex: 1;
    min-height: 0;
    font-size: var(--fs);
  }
  .thead,
  .tr {
    display: grid;
    grid-template-columns: minmax(320px, 1fr) 80px 96px 76px 76px minmax(120px, 22%);
    align-items: center;
    column-gap: 12px;
    padding-right: 12px;
  }
  .thead.est,
  .tr.est {
    grid-template-columns: minmax(240px, 1fr) 80px minmax(100px, 26%);
  }
  .thead {
    position: sticky;
    top: 0;
    z-index: 1;
    height: 28px;
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
    color: var(--text-3);
    font-size: var(--fs-sm);
    font-weight: 500;
    padding-left: 12px;
  }
  .tr {
    position: relative;
    height: 30px;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
  }
  .tr:hover {
    background: var(--hover);
  }
  .tr.lit {
    background: color-mix(in srgb, var(--iri) 12%, transparent);
  }
  .tr.selected {
    background: color-mix(in srgb, var(--iri) 18%, transparent);
    box-shadow: inset 3px 0 0 var(--iri);
  }
  .mark {
    flex: none;
    font-size: 12px;
    line-height: 1;
  }
  .mark.high {
    color: var(--danger);
  }
  .mark.warning {
    color: var(--warn, var(--spark));
  }
  .mark.info {
    color: var(--text-3);
  }
  .tr.hot {
    background: color-mix(in srgb, var(--spark) 9%, transparent);
    box-shadow: inset 3px 0 0 var(--spark);
  }
  .tr.hot[data-rank='1'] {
    background: color-mix(in srgb, var(--spark) 6%, transparent);
  }
  .tr.hot[data-rank='2'] {
    background: color-mix(in srgb, var(--spark) 4%, transparent);
  }
  .op {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    height: 100%;
  }
  .guide {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--border);
  }
  .guide.end {
    bottom: 50%;
  }
  .twisty {
    all: unset;
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    border-radius: 3px;
    cursor: pointer;
    color: var(--text-3);
    background: var(--surface);
    z-index: 1;
  }
  .twisty:hover {
    color: var(--text);
  }
  .twisty :global(.icon) {
    transition: transform 0.12s;
  }
  .twisty.open :global(.icon) {
    transform: rotate(90deg);
  }
  .twisty-space {
    width: 16px;
    flex: none;
  }
  .name {
    all: unset;
    font-weight: 600;
    white-space: nowrap;
    cursor: pointer;
  }
  .name:hover,
  .name:focus-visible {
    text-decoration: underline;
  }
  .desc {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .num {
    font-variant-numeric: tabular-nums;
    text-align: right;
    white-space: nowrap;
  }
  .num.mis {
    color: var(--danger);
    font-weight: 600;
  }
  .bar {
    position: relative;
    height: 8px;
    border-radius: 4px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar .total {
    position: absolute;
    inset: 0 auto 0 0;
    background: color-mix(in srgb, var(--iri) 35%, transparent);
    border-radius: 4px;
  }
  .bar .self {
    position: absolute;
    inset: 0 auto 0 0;
    background: var(--spark);
    border-radius: 4px;
  }

  .flame {
    flex: 1;
    min-height: 0;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .icicle {
    position: relative;
    flex: none;
  }
  .box {
    all: unset;
    position: absolute;
    height: 24px;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 6px;
    box-sizing: border-box;
    overflow: hidden;
    white-space: nowrap;
    border-radius: 3px;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text);
    background: color-mix(in srgb, var(--iri) 20%, var(--surface));
    border: 1px solid color-mix(in srgb, var(--iri) 35%, transparent);
    cursor: default;
  }
  .box .d {
    font-family: var(--font-mono);
    font-weight: 400;
    font-size: 11px;
    color: var(--text-2);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .box.hot {
    background: color-mix(in srgb, var(--spark) 45%, var(--surface));
    border-color: var(--spark);
  }
  .box.thin > * {
    display: none;
  }
  .box:hover,
  .box:focus-visible {
    outline: 2px solid var(--text);
    outline-offset: -1px;
    z-index: 1;
  }
  .detail {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 12px;
    align-items: baseline;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
    min-height: 40px;
  }
</style>

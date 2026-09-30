<script lang="ts">
  import type { PlanNode } from '$lib/api';
  import { fmtCompact, fmtInt, fmtMs } from '$lib/format';
  import { shorten, type PrefixMap } from '$lib/rdf';
  import Icon from './Icon.svelte';

  let {
    plan,
    executed = true,
    prefixes = {},
  }: { plan: PlanNode; executed?: boolean; prefixes?: PrefixMap } = $props();

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
    const walk = (n: PlanNode, id: string, depth: number, last: boolean[]) => {
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

  /** Ratio of actual to estimated rows; flags big misestimates. */
  function misestimate(n: PlanNode): number | null {
    if (!executed || n.actualRows < 0) return null;
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
    {:else}
      <span class="faint">Not executed: widths show estimated cost</span>
    {/if}
  </div>

  {#if mode === 'tree'}
    <div class="tree scroll">
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
        <div class="tr" class:hot={h != null} class:est={!executed} data-rank={h}>
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
            <span class="name">{r.node.operator}</span>
            <span class="desc" title={r.node.description}>{desc(r.node.description)}</span>
            {#if r.node.cached}<span class="badge ok" title="Served from the result cache"
                >cached</span
              >{/if}
            {#if h != null}<span class="badge spark">#{h + 1} slowest</span>{/if}
          </div>
          <span class="num faint">{fmtCompact(r.node.estimatedRows)}</span>
          {#if executed}
            <span
              class="num"
              class:mis={mis != null}
              title={mis ? `Off by ${mis.toFixed(0)}× vs estimate` : undefined}
            >
              {r.node.actualRows < 0 ? '—' : fmtInt(r.node.actualRows)}
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
    font-weight: 600;
    white-space: nowrap;
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

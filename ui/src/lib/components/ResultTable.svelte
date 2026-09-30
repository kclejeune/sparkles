<script lang="ts">
  import type { Term } from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { displayIri, displayTerm, toSparql, type PrefixMap } from '$lib/rdf';
  import { sortedOrder, type SortSpec } from '$lib/table';
  import Icon from './Icon.svelte';
  import TermView from './TermView.svelte';

  let {
    vars,
    rows,
    prefixes,
    onopen,
  }: {
    vars: string[];
    rows: (Term | null)[][];
    prefixes: PrefixMap;
    onopen?: (iri: string) => void;
  } = $props();

  const ROW_H = 26;
  const OVERSCAN = 12;

  let scroller: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let viewportH = $state(400);
  let sort = $state<SortSpec | null>(null);
  let widths = $state<number[]>([]);

  // Reset sort/widths whenever the result set changes.
  $effect(() => {
    void rows;
    sort = null;
    scrollTop = 0;
    if (scroller) scroller.scrollTop = 0;
    widths = vars.map((_, c) => {
      let max = vars[c].length + 2;
      const n = Math.min(rows.length, 300);
      for (let i = 0; i < n; i++) {
        const t = rows[i][c];
        // literal suffix as TermView renders it: "@lang" or "^^prefix:local" (smaller font)
        const suffix =
          t?.type === 'literal'
            ? t['xml:lang']
              ? t['xml:lang'].length + 2
              : t.datatype
                ? Math.ceil((displayIri(t.datatype, prefixes).length + 3) * 0.85)
                : 0
            : 0;
        const len = displayTerm(t, prefixes).length + suffix;
        if (len > max) max = len;
      }
      return Math.round(Math.min(460, Math.max(90, max * 7.4 + 28)));
    });
  });

  const order = $derived(sortedOrder(rows, sort, prefixes));

  const start = $derived(Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN));
  const end = $derived(
    Math.min(rows.length, Math.ceil((scrollTop + viewportH) / ROW_H) + OVERSCAN),
  );
  const visible = $derived(order.slice(start, end));
  const rnWidth = $derived(Math.max(44, String(rows.length).length * 8 + 20));
  const template = $derived(`${rnWidth}px ${widths.map((w) => `${w}px`).join(' ')} minmax(0, 1fr)`);
  const totalW = $derived(rnWidth + widths.reduce((a, b) => a + b, 0));

  function toggleSort(col: number) {
    if (!sort || sort.col !== col) sort = { col, dir: 1 };
    else if (sort.dir === 1) sort = { col, dir: -1 };
    else sort = null;
  }

  async function copy(t: Term | null) {
    if (!t) return;
    const text = t.type === 'literal' ? t.value : t.type === 'uri' ? t.value : toSparql(t);
    try {
      await navigator.clipboard.writeText(text);
      toasts.push(
        'success',
        'Copied to clipboard',
        text.length > 80 ? text.slice(0, 80) + '…' : text,
        1600,
      );
    } catch (e) {
      toasts.error('Could not copy', e);
    }
  }

  // Column resizing
  function startResize(e: PointerEvent, col: number) {
    e.preventDefault();
    e.stopPropagation();
    const x0 = e.clientX;
    const w0 = widths[col];
    const move = (ev: PointerEvent) => {
      widths[col] = Math.max(60, w0 + ev.clientX - x0);
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }
</script>

<div
  class="grid"
  bind:this={scroller}
  bind:clientHeight={viewportH}
  onscroll={(e) => (scrollTop = (e.currentTarget as HTMLDivElement).scrollTop)}
  role="grid"
  aria-rowcount={rows.length + 1}
  aria-colcount={vars.length}
>
  <div class="head" style:grid-template-columns={template} style:min-width="{totalW}px" role="row">
    <div class="cell rn" role="columnheader">#</div>
    {#each vars as v, c (v + c)}
      <div
        class="cell th"
        role="columnheader"
        aria-sort={sort?.col === c ? (sort.dir === 1 ? 'ascending' : 'descending') : 'none'}
      >
        <button class="sort" onclick={() => toggleSort(c)} title="Sort by ?{v}">
          <span class="t-var">?{v}</span>
          {#if sort?.col === c}<span class="arrow">{sort.dir === 1 ? '↑' : '↓'}</span>{/if}
        </button>
        <span
          class="resize"
          role="separator"
          aria-orientation="vertical"
          onpointerdown={(e) => startResize(e, c)}
        ></span>
      </div>
    {/each}
    <div class="cell filler"></div>
  </div>
  <div class="body" style:height="{rows.length * ROW_H}px" style:min-width="{totalW}px">
    <div class="window" style:transform="translateY({start * ROW_H}px)">
      {#each visible as ri, k (ri)}
        {@const row = rows[ri]}
        <div
          class="tr"
          class:odd={(start + k) % 2 === 1}
          style:grid-template-columns={template}
          role="row"
        >
          <div class="cell rn" role="rowheader">{ri + 1}</div>
          {#each row as term, c (c)}
            <div class="cell td" role="gridcell">
              <span class="val"><TermView {term} {prefixes} {onopen} /></span>
              {#if term}
                <button
                  class="copy"
                  title="Copy value"
                  aria-label="Copy value"
                  onclick={() => copy(term)}
                >
                  <Icon name="copy" size={12} />
                </button>
              {/if}
            </div>
          {/each}
          <div class="cell filler"></div>
        </div>
      {/each}
    </div>
  </div>
</div>

<style>
  .grid {
    position: relative;
    height: 100%;
    overflow: auto;
    font-size: var(--fs);
    contain: strict;
  }
  .head {
    position: sticky;
    top: 0;
    z-index: 2;
    display: grid;
    height: 30px;
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
  }
  .body {
    position: relative;
  }
  .window {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    will-change: transform;
  }
  .tr {
    display: grid;
    height: 26px;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
  }
  .tr.odd {
    background: color-mix(in srgb, var(--surface-2) 45%, transparent);
  }
  .tr:hover {
    background: var(--hover);
  }
  .cell {
    position: relative;
    display: flex;
    align-items: center;
    min-width: 0;
    padding: 0 10px;
    border-right: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
  }
  .cell.filler {
    border-right: 0;
  }
  .rn {
    justify-content: flex-end;
    color: var(--text-3);
    font-size: var(--fs-xs);
    font-variant-numeric: tabular-nums;
    position: sticky;
    left: 0;
    background: inherit;
    z-index: 1;
  }
  .head .rn {
    background: var(--surface-2);
  }
  .tr .rn {
    background: var(--surface);
  }
  .th {
    padding: 0;
  }
  .sort {
    all: unset;
    display: flex;
    align-items: center;
    gap: 4px;
    width: 100%;
    height: 100%;
    padding: 0 10px;
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    font-weight: 600;
    cursor: pointer;
    box-sizing: border-box;
  }
  .sort:hover {
    background: var(--hover);
  }
  .sort:focus-visible {
    box-shadow: inset 0 0 0 2px var(--iri);
  }
  .arrow {
    color: var(--spark-ink);
  }
  .resize {
    position: absolute;
    right: -3px;
    top: 0;
    bottom: 0;
    width: 6px;
    cursor: col-resize;
    z-index: 1;
  }
  .resize:hover {
    background: var(--iri);
    opacity: 0.4;
  }
  .val {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    flex: 1;
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .copy {
    all: unset;
    display: none;
    place-items: center;
    width: 20px;
    height: 20px;
    margin-right: -6px;
    border-radius: 4px;
    color: var(--text-2);
    cursor: pointer;
    flex: none;
    background: var(--surface);
    box-shadow: 0 0 0 1px var(--border);
  }
  .td:hover .copy,
  .copy:focus-visible {
    display: grid;
  }
  .copy:hover {
    color: var(--text);
  }
</style>

<script module lang="ts">
  /** The drag data type of a tree node: `{id, from}` as JSON. */
  export const TREE_DRAG = 'application/x-sparkles-tree-node';

  /**
   * The indent of a row at `depth` (ROADMAP item 633): full 14-pixel steps for the first
   * four levels, then 6-pixel steps, so a row 15 levels down keeps room for its label.
   */
  export function treeIndent(depth: number): number {
    return depth <= 4 ? depth * 14 : 56 + (depth - 4) * 6;
  }

  /**
   * The guide lines of a row at `depth`: one 1-pixel line under the twisty of each open
   * ancestor of the first four levels, drawn as backgrounds of the row.
   */
  export function guideStyle(depth: number): string | undefined {
    const n = Math.min(depth, 4);
    if (n < 1) return undefined;
    const line = 'linear-gradient(var(--border), var(--border))';
    const xs = Array.from({ length: n }, (_, k) => `${treeIndent(k) + 11}px 0`);
    return (
      `background-image:${Array(n).fill(line).join(',')};` +
      `background-size:${Array(n).fill('1px 100%').join(',')};` +
      `background-position:${xs.join(',')};background-repeat:no-repeat`
    );
  }
</script>

<script lang="ts">
  // One tree of the ontology editor: the asserted hierarchy of a kind of entity or the
  // inferred class taxonomy (docs/specs/U12-ontology-workbench/SPEC.md), and the concept
  // tree of the thesaurus editor, which reads children as nodes open (`more`) and moves
  // concepts by drag and drop (`ondrop`). Each node is an id (an IRI, or `n:<node>` of the
  // taxonomy) with a name.
  //
  // The tree draws only the rows in view (ROADMAP item 725). It flattens the open nodes to
  // rows (`$lib/virtual-tree`), reads the scroll position of the nearest scrolling ancestor,
  // and draws that window with spacers above and below, so a level of 100,000 children
  // costs the rows on screen and not the level. The WAI-ARIA tree pattern's keys work on
  // the whole tree, since the rows off screen are not in the page: the tree is one tab stop
  // whose arrow keys move a cursor, and rows carry their level and place for assistive tech.
  import { onMount, tick, untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import TreeSticky from './TreeSticky.svelte';
  import {
    flattenTree,
    indexOfId,
    keyMove,
    Metrics,
    registerTree,
    scrollToRow,
    stickyCap,
    stickyPath,
    type TreeRow,
  } from '$lib/virtual-tree';

  let {
    roots,
    children,
    name,
    sub,
    badge,
    open,
    selected,
    keep = null,
    onselect,
    ontoggle,
    label,
    more,
    ondrop,
    clipped,
    onmore,
    onview,
  }: {
    roots: string[];
    children: (id: string) => string[];
    name: (id: string) => string;
    /** A second, fainter line: the prefixed name of a labelled entity. */
    sub?: (id: string) => string | null;
    badge?: (id: string) => string | null;
    open: Set<string>;
    selected: string | null;
    /** With a filter: the nodes it keeps, which open on their own. */
    keep?: Set<string> | null;
    onselect: (id: string) => void;
    ontoggle: (id: string) => void;
    /** The tree's accessible name. */
    label?: string;
    /** Whether a node has children that `children` has not read yet. */
    more?: (id: string) => boolean;
    /**
     * Drag and drop: `node` dragged from under `from` (null at the top) onto `onto`, with
     * `copy` when Ctrl or Alt was held. Nodes are draggable when this is given.
     */
    ondrop?: (node: string, from: string | null, onto: string, copy: boolean) => void;
    /** A level that holds fewer children than its node has: how many it shows of how many. */
    clipped?: (parent: string | null) => { shown: number; total: number } | null;
    /** Reads the next page of a clipped level. */
    onmore?: (parent: string | null) => void | Promise<void>;
    /** The ids of the rows now drawn, for a page that reads what it shows, such as labels. */
    onview?: (ids: string[]) => void;
  } = $props();

  const DRAG = TREE_DRAG;
  const OVERSCAN = 8;
  let over = $state<string | null>(null);
  let reading = $state<Record<string, boolean>>({});

  const rows = $derived<TreeRow[]>(
    flattenTree({
      roots,
      children,
      name,
      open: (id) => open.has(id),
      keep,
      more,
      clipped,
    }),
  );

  // --- what is in view ---------------------------------------------------------------
  let root = $state<HTMLElement>();
  let list = $state<HTMLElement>();
  let scroller: HTMLElement | null = null;
  let scrollTop = $state(0);
  let viewH = $state(600);
  /** Where the list starts inside the scrolling element. */
  let listTop = $state(0);
  let rowH = $state(26);
  /** The extra height of a selected row whose label wraps, by node id. */
  let tall = $state<Record<string, number>>({});

  const metrics = $derived.by(() => {
    const extra: { at: number; add: number }[] = [];
    if (selected && tall[selected]) {
      for (let i = 0; i < rows.length; i++) {
        const r = rows[i];
        if (r.kind === 'node' && r.id === selected) extra.push({ at: i, add: tall[selected] });
      }
    }
    return new Metrics(rows.length, rowH, extra);
  });
  const rel = $derived(scrollTop - listTop);
  const win = $derived(metrics.window(rel, viewH, OVERSCAN));
  const visible = $derived(rows.slice(win.start, win.end));
  $effect(() => {
    const ids = visible.flatMap((r) => (r.kind === 'node' ? [r.id] : []));
    if (onview && ids.length) untrack(() => onview(ids));
  });
  const padTop = $derived(metrics.top(win.start));
  const padBottom = $derived(Math.max(0, metrics.height - metrics.top(win.end)));

  // the ancestors of the first row under the sticky rows, nearest last
  const sticky = $derived.by(() => {
    if (!rows.length) return { shown: [] as number[], hidden: 0 };
    const cap = stickyCap(viewH, rowH);
    const pass = (pane: number) => stickyPath(rows, metrics.at(rel + pane), cap);
    const first = pass(0);
    const pane = (first.shown.length + (first.hidden ? 1 : 0)) * rowH;
    return rel > 0 ? pass(pane) : { shown: [], hidden: 0 };
  });
  const stickyRows = $derived(
    sticky.shown.map((i) => {
      const r = rows[i];
      return { index: i, id: r.kind === 'node' ? r.id : '', depth: r.depth };
    }),
  );

  function scrollParent(el: HTMLElement): HTMLElement | null {
    for (let p = el.parentElement; p; p = p.parentElement) {
      const o = getComputedStyle(p).overflowY;
      if (o === 'auto' || o === 'scroll') return p;
    }
    return null;
  }

  /** The element that scrolls the tree: its nearest scrolling ancestor, or else the page. */
  const viewport = () => scroller ?? (document.scrollingElement as HTMLElement | null);

  function readView() {
    const el = viewport();
    if (!el || !list) return;
    scrollTop = el.scrollTop;
    viewH = (scroller ? el.clientHeight : window.innerHeight) || viewH;
    const top = scroller ? scroller.getBoundingClientRect().top : 0;
    listTop = list.getBoundingClientRect().top - top + el.scrollTop;
  }

  onMount(() => {
    if (!root) return;
    scroller = scrollParent(root);
    let queued = 0;
    const later = () => {
      if (!queued)
        queued = requestAnimationFrame(() => {
          queued = 0;
          readView();
        });
    };
    const target: HTMLElement | Window = scroller ?? window;
    target.addEventListener('scroll', later, { passive: true });
    const ro = typeof ResizeObserver === 'function' ? new ResizeObserver(later) : null;
    if (scroller) ro?.observe(scroller);
    else window.addEventListener('resize', later);
    ro?.observe(root);
    readView();
    const off = registerTree({
      reveal(id) {
        if (indexOfId(rows, id) < 0) return false;
        // the page holds the rows it had before the ones just read, so its height is short
        // of the new rows until it is drawn: scrolling waits for that, or it lands short
        void tick().then(() => {
          const i = indexOfId(rows, id);
          if (i >= 0) ensureVisible(i);
        });
        return true;
      },
    });
    return () => {
      cancelAnimationFrame(queued);
      target.removeEventListener('scroll', later);
      window.removeEventListener('resize', later);
      ro?.disconnect();
      off();
    };
  });

  // a row's real height: the first row that does not wrap, and a selected row that does
  $effect(() => {
    void visible;
    void selected;
    queueMicrotask(() => {
      if (!list) return;
      const items = list.querySelectorAll<HTMLElement>(':scope > li[role="treeitem"]');
      let plain = 0;
      for (const li of items) {
        if (li.dataset.id !== selected) {
          plain = li.getBoundingClientRect().height;
          break;
        }
      }
      if (plain > 4 && Math.abs(plain - rowH) > 0.4) rowH = plain;
      if (selected) {
        const li = [...items].find((x) => x.dataset.id === selected);
        if (li) {
          const add = Math.round(li.getBoundingClientRect().height - rowH);
          const had = tall[selected] ?? 0;
          if (add > 2 && add !== had) tall = { ...tall, [selected]: add };
          else if (add <= 2 && had) {
            const { [selected]: _gone, ...rest } = tall;
            tall = rest;
          }
        }
      }
      readView();
    });
  });

  /** Scroll row `i` into view with the least movement, below the sticky rows. */
  function ensureVisible(i: number) {
    const el = viewport();
    if (!el) return;
    const pinned = (sticky.shown.length + (sticky.hidden ? 1 : 0)) * rowH;
    const want = scrollToRow(metrics, i, rel, viewH, pinned);
    if (want != null) {
      el.scrollTop = want + listTop;
      readView();
    }
  }

  // --- the keyboard cursor ------------------------------------------------------------
  const uid = $props.id();
  let cursorKey = $state<string | null>(null);
  let focused = $state(false);
  const cursor = $derived.by(() => {
    if (!rows.length) return -1;
    if (cursorKey) {
      const i = rows.findIndex((r) => r.key === cursorKey);
      if (i >= 0) return i;
    }
    if (selected) {
      const i = indexOfId(rows, selected);
      if (i >= 0) return i;
    }
    return 0;
  });
  const rowDomId = (i: number) => `${uid}-r${i}`;
  let typed = '';
  let typedAt = 0;

  async function doMore(of: string | null, key: string) {
    if (reading[key]) return;
    reading = { ...reading, [key]: true };
    try {
      await onmore?.(of);
    } finally {
      reading = { ...reading, [key]: false };
    }
  }

  function onkey(e: KeyboardEvent) {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const page = Math.max(1, Math.floor(viewH / rowH) - 1);
    const now = Date.now();
    // a space goes on a name being typed, and otherwise selects the row
    const typing = now - typedAt <= 1000 && typed.length > 0;
    if (e.key.length === 1 && (e.key !== ' ' || typing)) {
      // type-ahead: the next row whose name starts with what was typed
      typed = typing ? typed + e.key : e.key;
      typedAt = now;
      const t = typed.toLowerCase();
      const from = cursor + (typed.length === 1 ? 1 : 0);
      for (let k = 0; k < rows.length; k++) {
        const i = (from + k) % rows.length;
        const r = rows[i];
        if (r.kind === 'node' && name(r.id).toLowerCase().startsWith(t)) {
          cursorKey = r.key;
          ensureVisible(i);
          e.preventDefault();
          break;
        }
      }
      return;
    }
    const m = keyMove(rows, cursor, e.key, page);
    if (!m) return;
    e.preventDefault();
    // a row's button that scrolls out of the window leaves the page, so the tree keeps the focus
    if (document.activeElement !== list) list?.focus({ preventScroll: true });
    if ('to' in m) {
      cursorKey = rows[m.to].key;
      ensureVisible(m.to);
    } else if ('toggle' in m) ontoggle(m.toggle);
    else if ('select' in m) {
      cursorKey = rows[cursor].key;
      onselect(m.select);
    } else if ('more' in m) void doMore(m.more, rows[cursor].key);
  }

  function pickRow(i: number) {
    cursorKey = rows[i].key;
  }

  function goSticky(index: number) {
    const el = viewport();
    if (!el) return;
    // the ancestor's row at the top, under the sticky rows that stay above it
    const at = stickyRows.findIndex((r) => r.index === index);
    const offset = (sticky.hidden ? rowH : 0) + at * rowH;
    el.scrollTop = listTop + metrics.top(index) - offset;
    readView();
  }
</script>

<div class="vtree" bind:this={root}>
  <TreeSticky rows={stickyRows} hidden={sticky.hidden} {name} {rowH} ongo={goSticky} />
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <ul
    class="tree"
    role="tree"
    aria-label={label}
    tabindex="0"
    aria-activedescendant={focused && cursor >= win.start && cursor < win.end
      ? rowDomId(cursor)
      : undefined}
    bind:this={list}
    style:padding-top="{padTop}px"
    style:padding-bottom="{padBottom}px"
    onkeydown={onkey}
    onfocus={() => (focused = true)}
    onblur={() => (focused = false)}
  >
    {#each visible as row, v (row.key)}
      {@const i = win.start + v}
      {#if row.kind === 'more'}
        <li
          class="more-row"
          role="none"
          id={rowDomId(i)}
          style:padding-left="{treeIndent(row.depth) + 22}px"
        >
          <span class="faint"
            >Showing {row.shown.toLocaleString('en-US')} of {row.total.toLocaleString(
              'en-US',
            )}</span
          >
          <button
            class="btn ghost sm"
            class:cur={focused && cursor === i}
            tabindex="-1"
            disabled={reading[row.key]}
            onclick={() => doMore(row.of, row.key)}
            >{#if reading[row.key]}<span class="spinner"></span>{/if} Show more</button
          >
        </li>
      {:else}
        {@const id = row.id}
        {@const s = sub?.(id)}
        {@const b = badge?.(id)}
        <li
          role="treeitem"
          id={rowDomId(i)}
          aria-label={name(id)}
          aria-level={row.depth + 1}
          aria-setsize={row.size}
          aria-posinset={row.pos}
          aria-expanded={row.hasKids ? row.open : undefined}
          aria-selected={selected === id}
          data-id={id}
          style={guideStyle(row.depth)}
        >
          <div
            class="node"
            class:sel={selected === id}
            class:over={over === id}
            class:cur={focused && cursor === i}
            style:padding-left="{treeIndent(row.depth) + 2}px"
            role="presentation"
            draggable={ondrop ? 'true' : undefined}
            ondragstart={ondrop
              ? (e) => {
                  e.stopPropagation();
                  const from = row.parent >= 0 ? (rows[row.parent] as { id: string }).id : null;
                  e.dataTransfer?.setData(DRAG, JSON.stringify({ id, from }));
                  e.dataTransfer?.setData('text/plain', id);
                  if (e.dataTransfer) e.dataTransfer.effectAllowed = 'copyMove';
                }
              : undefined}
            ondragover={ondrop
              ? (e) => {
                  if (!e.dataTransfer?.types.includes(DRAG)) return;
                  e.preventDefault();
                  e.dataTransfer.dropEffect = e.ctrlKey || e.altKey ? 'copy' : 'move';
                  over = id;
                }
              : undefined}
            ondragleave={ondrop ? () => over === id && (over = null) : undefined}
            ondrop={ondrop
              ? (e) => {
                  over = null;
                  const raw = e.dataTransfer?.getData(DRAG);
                  if (!raw) return;
                  e.preventDefault();
                  e.stopPropagation();
                  const src = JSON.parse(raw) as { id: string; from: string | null };
                  if (src.id !== id) ondrop(src.id, src.from, id, e.ctrlKey || e.altKey);
                }
              : undefined}
          >
            {#if row.hasKids}
              <button
                class="twisty"
                class:open={row.open}
                aria-label={row.open ? `Collapse ${name(id)}` : `Expand ${name(id)}`}
                tabindex="-1"
                onclick={() => {
                  pickRow(i);
                  ontoggle(id);
                }}
              >
                <Icon name="chevron" size={12} />
              </button>
            {:else}
              <span class="twisty-space"></span>
            {/if}
            <button
              class="label"
              tabindex="-1"
              title={id.startsWith('n:') ? undefined : [name(id), s, id].filter(Boolean).join('\n')}
              onclick={() => {
                pickRow(i);
                onselect(id);
              }}
            >
              <span class="name">{name(id)}</span>
              {#if s}<span class="iri" aria-hidden="true">{s}</span>{/if}
            </button>
            {#if b}<span class="badge">{b}</span>{/if}
            {#if row.kids}<span class="kids faint">{row.kids}</span>{/if}
          </div>
        </li>
      {/if}
    {/each}
  </ul>
</div>

<style>
  .vtree {
    min-width: 0;
  }
  .tree {
    list-style: none;
    margin: 0;
    padding: 0;
    outline: none;
  }
  .tree:focus-visible {
    box-shadow: inset var(--focus);
  }
  /* the keyboard cursor, shown while the tree has the focus */
  .node.cur {
    outline: 2px solid var(--spark);
    outline-offset: -2px;
  }
  .more-row .cur {
    outline: 2px solid var(--spark);
  }
  .more-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: var(--row-h);
    font-size: var(--fs-sm);
  }
  .node {
    display: flex;
    align-items: center;
    gap: 2px;
    min-height: var(--row-h);
    border-radius: var(--r-sm);
    padding-right: 6px;
    min-width: 0;
  }
  .node:hover {
    background-color: var(--hover);
  }
  .node.sel {
    background-color: var(--spark-soft);
  }
  .node.over {
    outline: 2px dashed var(--spark);
    outline-offset: -2px;
  }
  .twisty,
  .twisty-space {
    width: 18px;
    height: 18px;
    flex: none;
  }
  .twisty {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 0;
    background: none;
    color: var(--text-3);
    cursor: pointer;
    border-radius: var(--r-sm);
    padding: 0;
  }
  .twisty :global(svg) {
    transition: transform 0.12s;
  }
  .twisty.open :global(svg) {
    transform: rotate(90deg);
  }
  .label {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    flex: 1;
    container-type: inline-size;
    border: 0;
    background: none;
    padding: 2px 4px;
    font: inherit;
    color: var(--text);
    text-align: left;
    cursor: pointer;
    border-radius: var(--r-sm);
  }
  .name {
    flex: 0 1 auto;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .iri {
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    color: var(--text-3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
    flex: 0 100 auto;
  }
  /* a narrow row keeps its label: the short form goes first */
  @container (max-width: 240px) {
    .iri {
      display: none;
    }
  }
  /* the selected row shows its whole label and short form */
  .node.sel .label {
    flex-wrap: wrap;
  }
  .node.sel .name {
    white-space: normal;
    overflow-wrap: anywhere;
  }
  .node.sel .iri {
    display: inline;
    white-space: normal;
    overflow-wrap: anywhere;
  }
  .badge {
    font-size: var(--fs-xs);
    color: var(--spark-ink);
    background: var(--spark-soft);
    border-radius: 999px;
    padding: 0 6px;
    flex: none;
  }
  .kids {
    font-size: var(--fs-xs);
    flex: none;
  }
</style>

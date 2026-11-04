<script lang="ts">
  // Sticky ancestor rows at the top of a tree's scroll box (ROADMAP item 633), as in the
  // sticky scroll of code editors: the path of the topmost visible row, nearest last, with
  // the nearest six shown when the path is longer. Clicking one scrolls to that ancestor.
  // It goes first inside the scrolling element, whose tree it reads.
  import { onMount } from 'svelte';
  import { treeIndent } from './OntoTree.svelte';

  const MAX = 6;

  type Row = { id: string; name: string; depth: number; node: HTMLElement };
  let el = $state<HTMLElement>();
  let pane = $state<HTMLElement>();
  let rows = $state<Row[]>([]);
  let hidden = $state(0);

  function depthOf(li: HTMLElement) {
    let d = 0;
    for (let p = li.parentElement?.closest('li[role="treeitem"]'); p;) {
      d++;
      p = p.parentElement?.closest('li[role="treeitem"]');
    }
    return d;
  }

  function update() {
    const box = el?.parentElement;
    if (!el || !box) return;
    // the rows overlay the tree without taking room in it, so a row counts as hidden
    // once it is under them
    const top = box.getBoundingClientRect().top + (pane?.offsetHeight ?? 0);
    const nodes = box.querySelectorAll<HTMLElement>('li[role="treeitem"] > .node');
    let first: HTMLElement | null = null;
    for (const n of nodes) {
      if (n.getBoundingClientRect().bottom > top + 1) {
        first = n;
        break;
      }
    }
    const path: Row[] = [];
    let li = first?.parentElement?.parentElement?.closest<HTMLElement>('li[role="treeitem"]');
    while (li) {
      const node = li.querySelector<HTMLElement>(':scope > .node');
      if (node && node.getBoundingClientRect().top < top) {
        path.unshift({
          id: li.dataset.id ?? '',
          name: li.getAttribute('aria-label') ?? '',
          depth: depthOf(li),
          node,
        });
      }
      li = li.parentElement?.closest<HTMLElement>('li[role="treeitem"]');
    }
    // at most 30% of the box, so a short tree keeps most of its rows in view
    const rowH = first?.offsetHeight || 26;
    const cap = Math.max(1, Math.min(MAX, Math.floor((box.clientHeight * 0.3) / rowH)));
    hidden = Math.max(0, path.length - cap);
    rows = path.slice(-cap);
  }

  onMount(() => {
    const box = el?.parentElement;
    if (!box) return;
    let queued = 0;
    const later = () => {
      if (!queued) queued = requestAnimationFrame(() => ((queued = 0), update()));
    };
    box.addEventListener('scroll', later, { passive: true });
    const mo = new MutationObserver(later);
    mo.observe(box, { childList: true, subtree: true });
    update();
    return () => {
      cancelAnimationFrame(queued);
      box.removeEventListener('scroll', later);
      mo.disconnect();
    };
  });

  function go(r: Row) {
    const box = el?.parentElement;
    if (!box) return;
    // the ancestor's row at the top, under the rows that stay above it
    const above = rows.findIndex((x) => x.id === r.id);
    const rowH = pane?.lastElementChild?.getBoundingClientRect().height ?? 0;
    const offset = hidden ? (pane?.firstElementChild?.getBoundingClientRect().height ?? 0) : 0;
    box.scrollTop +=
      r.node.getBoundingClientRect().top - box.getBoundingClientRect().top - offset - above * rowH;
  }
</script>

<div class="sticky" bind:this={el}>
  <div
    class="pane"
    class:on={rows.length > 0}
    bind:this={pane}
    role="group"
    aria-label="Path to the first row"
  >
    {#if hidden}<div class="row faint" style:padding-left="4px">… {hidden} more above</div>{/if}
    {#each rows as r (r.id)}
      <button
        class="row"
        style:padding-left="{treeIndent(r.depth) + 22}px"
        title={r.name}
        onclick={() => go(r)}>{r.name}</button
      >
    {/each}
  </div>
</div>

<style>
  /* a sticky line of no height, so showing rows never moves the tree under them */
  .sticky {
    position: sticky;
    top: 0;
    z-index: 2;
    height: 0;
  }
  .pane {
    position: absolute;
    inset: 0 0 auto 0;
    display: grid;
    background: var(--surface);
  }
  .pane.on {
    border-bottom: 1px solid var(--border);
    box-shadow: 0 2px 4px -2px color-mix(in srgb, var(--text) 18%, transparent);
  }
  .row {
    display: block;
    min-height: var(--row-h);
    padding: 0 6px;
    border: 0;
    background: none;
    color: var(--text-2);
    font: inherit;
    font-size: var(--fs-sm);
    line-height: var(--row-h);
    text-align: left;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    cursor: pointer;
  }
  button.row:hover {
    background: var(--hover);
    color: var(--text);
  }
  button.row:focus-visible {
    box-shadow: var(--focus);
  }
</style>

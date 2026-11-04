<script lang="ts">
  // Sticky ancestor rows at the top of a tree's scroll box (ROADMAP item 633), as in the
  // sticky scroll of code editors: the path of the topmost visible row, nearest last, with
  // the nearest six shown when the path is longer. Clicking one scrolls to that ancestor.
  // The tree works out which ancestors from its rows (ROADMAP item 725), since a tree that
  // draws only the rows in view has no rows above to read them from. It goes first inside
  // the tree's box.
  import { treeIndent } from './OntoTree.svelte';

  let {
    rows,
    hidden,
    name,
    rowH,
    ongo,
  }: {
    rows: { index: number; id: string; depth: number }[];
    hidden: number;
    name: (id: string) => string;
    rowH: number;
    ongo: (index: number) => void;
  } = $props();
</script>

<div class="sticky">
  <div
    class="pane"
    class:on={rows.length > 0}
    role="group"
    aria-label="Path to the first row"
    style:--sticky-h="{rowH}px"
  >
    {#if hidden}<div class="row faint" style:padding-left="4px">… {hidden} more above</div>{/if}
    {#each rows as r (r.index)}
      <button
        class="row"
        tabindex="-1"
        style:padding-left="{treeIndent(r.depth) + 22}px"
        title={name(r.id)}
        onclick={() => ongo(r.index)}>{name(r.id)}</button
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
    height: var(--sticky-h, var(--row-h));
    padding: 0 6px;
    border: 0;
    background: none;
    color: var(--text-2);
    font: inherit;
    font-size: var(--fs-sm);
    line-height: var(--sticky-h, var(--row-h));
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

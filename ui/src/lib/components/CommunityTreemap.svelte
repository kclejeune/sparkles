<script lang="ts">
  // The community hierarchy as a nested treemap (ROADMAP item 633). A community's area is
  // its member count, a parent holds its children under its title, and a report's status
  // tints each box and is named in it wherever it fits. Every box is a button that opens the
  // community, so the map and the tree list are two views of one hierarchy.
  import { fmtInt } from '$lib/format';
  import { statusClass, type TreeNode } from '$lib/graphrag';
  import { layout, type TreemapNode } from '$lib/treemap';

  let {
    forest,
    selected = null,
    name,
    onopen,
  }: {
    forest: TreeNode[];
    selected?: string | null;
    /** A community's title, or its shortened IRI when it has no report. */
    name: (n: TreeNode) => string;
    onopen: (iri: string) => void;
  } = $props();

  let width = $state(0);
  // a phone gets a taller map, where the width is the scarce side
  const height = $derived(
    Math.round(Math.min(560, Math.max(260, width * (width < 640 ? 1.15 : 0.5)))),
  );

  const byIri = $derived.by(() => {
    const m = new Map<string, TreeNode>();
    const walk = (l: TreeNode[]) => {
      for (const n of l) {
        m.set(n.iri, n);
        walk(n.children);
      }
    };
    walk(forest);
    return m;
  });
  const toNode = (n: TreeNode): TreemapNode => ({
    id: n.iri,
    weight: n.memberCount,
    children: n.children.map(toNode),
  });
  const boxes = $derived(
    width > 0 ? layout(forest.map(toNode), width, height, { minSide: width < 640 ? 72 : 48 }) : [],
  );
  const status = (n: TreeNode) => n.report?.status ?? 'pending';
  const describe = (n: TreeNode) =>
    `${name(n)}, level ${n.level}, ${fmtInt(n.memberCount)} member${n.memberCount === 1 ? '' : 's'}, ${status(n)}`;
</script>

<div class="treemap-wrap">
  <div
    class="treemap"
    role="group"
    aria-label="Community map"
    bind:clientWidth={width}
    style:height="{height}px"
  >
    {#each boxes as b (b.id)}
      {@const n = byIri.get(b.id)}
      {#if n}
        {@const roomy = b.leaf && b.rect.w >= 96 && b.rect.h >= 58}
        <button
          class="box {statusClass(n.report?.status)}"
          class:leaf={b.leaf}
          class:active={selected === n.iri}
          aria-current={selected === n.iri ? 'true' : undefined}
          aria-label={describe(n)}
          title={describe(n)}
          style:left="{b.rect.x}px"
          style:top="{b.rect.y}px"
          style:width="{b.rect.w}px"
          style:height="{b.rect.h}px"
          onclick={() => onopen(n.iri)}
        >
          <span class="t">{name(n)}</span>
          {#if roomy}
            <span class="m"
              >{fmtInt(n.memberCount)} member{n.memberCount === 1 ? '' : 's'} · {status(n)}</span
            >
          {:else if !b.leaf}
            <span class="m n">{fmtInt(n.memberCount)}</span>
          {/if}
        </button>
      {/if}
    {/each}
  </div>
  <p class="legend faint small">
    Each box is a community, sized by its members. A box holds the communities of the level below
    it.
  </p>
</div>

<style>
  .treemap-wrap {
    display: grid;
    gap: var(--sp-2);
    padding: var(--sp-3);
    min-width: 0;
  }
  .treemap {
    position: relative;
    min-width: 0;
    overflow: hidden;
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .box {
    --c: var(--text-3);
    position: absolute;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    padding: 3px 6px;
    margin: 0;
    border: 1px solid color-mix(in srgb, var(--c) 45%, var(--surface));
    border-radius: 4px;
    background: color-mix(in srgb, var(--c) 10%, var(--surface));
    color: var(--text);
    font: inherit;
    font-size: var(--fs-sm);
    line-height: 1.25;
    text-align: left;
    overflow: hidden;
    cursor: pointer;
    /* a one-pixel gutter between neighbours */
    outline: 1px solid var(--surface-2);
    outline-offset: -1px;
  }
  .box:not(.leaf) {
    flex-direction: row;
    align-items: baseline;
    gap: var(--sp-2);
  }
  .box:not(.leaf) .m {
    flex: none;
  }
  .box.leaf {
    background: color-mix(in srgb, var(--c) 18%, var(--surface));
  }
  .box.spark {
    --c: var(--spark);
  }
  .box.ok {
    --c: var(--ok);
  }
  .box.warn {
    --c: var(--warn);
  }
  .box.danger {
    --c: var(--danger);
  }
  .box:hover {
    border-color: var(--c);
  }
  .box.active {
    border-color: var(--c);
    box-shadow: inset 0 0 0 2px var(--c);
  }
  .box:focus-visible {
    z-index: 2;
    box-shadow: var(--focus);
  }
  .t {
    display: block;
    max-width: 100%;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .leaf .t {
    white-space: normal;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 3;
    line-clamp: 3;
  }
  .m {
    max-width: 100%;
    color: var(--text-2);
    font-size: var(--fs-xs);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .legend {
    margin: 0;
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>

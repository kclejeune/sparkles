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
</script>

<script lang="ts">
  // One tree of the ontology editor: the asserted hierarchy of a kind of entity or the
  // inferred class taxonomy (docs/specs/U12-ontology-workbench/SPEC.md), and the concept
  // tree of the thesaurus editor, which reads children as nodes open (`more`) and moves
  // concepts by drag and drop (`ondrop`). Each node is an id (an IRI, or `n:<node>` of the
  // taxonomy) with a name; the WAI-ARIA tree pattern.
  import Icon from './Icon.svelte';
  import OntoTree from './OntoTree.svelte';

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
    depth = 0,
    path = [],
    label,
    more,
    ondrop,
    clipped,
    onmore,
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
    depth?: number;
    path?: string[];
    /** The tree's accessible name (the top level only). */
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
    onmore?: (parent: string | null) => void;
  } = $props();
  const parent = $derived(path.at(-1) ?? null);
  const clip = $derived(clipped?.(parent) ?? null);
  let reading = $state(false);

  const DRAG = TREE_DRAG;
  let over = $state<string | null>(null);

  const sorted = (ids: string[]) =>
    [...ids].sort((a, b) => name(a).localeCompare(name(b), undefined, { sensitivity: 'base' }));
</script>

<ul
  class="tree"
  role={depth === 0 ? 'tree' : 'group'}
  aria-label={depth === 0 ? label : undefined}
  class:guided={depth > 0 && depth <= 4}
  style:--guide={depth > 0 && depth <= 4 ? `${treeIndent(depth - 1) + 11}px` : undefined}
>
  {#each sorted(roots.filter((r) => !keep || keep.has(r))) as id (id)}
    {#if !path.includes(id)}
      {@const kids = children(id).filter((k) => !path.includes(k) && (!keep || keep.has(k)))}
      {@const hasKids = kids.length > 0 || (!keep && !!more?.(id))}
      {@const isOpen = keep ? true : open.has(id)}
      {@const s = sub?.(id)}
      {@const b = badge?.(id)}
      <li
        role="treeitem"
        aria-label={name(id)}
        aria-expanded={hasKids ? isOpen : undefined}
        aria-selected={selected === id}
        data-id={id}
      >
        <div
          class="node"
          class:sel={selected === id}
          class:over={over === id}
          style:padding-left="{treeIndent(depth) + 2}px"
          role="presentation"
          draggable={ondrop ? 'true' : undefined}
          ondragstart={ondrop
            ? (e) => {
                e.stopPropagation();
                e.dataTransfer?.setData(DRAG, JSON.stringify({ id, from: path.at(-1) ?? null }));
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
          {#if hasKids}
            <button
              class="twisty"
              class:open={isOpen}
              aria-label={isOpen ? `Collapse ${name(id)}` : `Expand ${name(id)}`}
              tabindex="-1"
              onclick={() => ontoggle(id)}
            >
              <Icon name="chevron" size={12} />
            </button>
          {:else}
            <span class="twisty-space"></span>
          {/if}
          <button
            class="label"
            title={id.startsWith('n:') ? undefined : [name(id), s, id].filter(Boolean).join('\n')}
            onclick={() => onselect(id)}
          >
            <span class="name">{name(id)}</span>
            {#if s}<span class="iri" aria-hidden="true">{s}</span>{/if}
          </button>
          {#if b}<span class="badge">{b}</span>{/if}
          {#if kids.length}<span class="kids faint">{kids.length}</span>{/if}
        </div>
        {#if kids.length && isOpen}
          <OntoTree
            roots={kids}
            {children}
            {name}
            {sub}
            {badge}
            {open}
            {selected}
            {keep}
            {onselect}
            {ontoggle}
            depth={depth + 1}
            path={[...path, id]}
            {more}
            {ondrop}
            {clipped}
            {onmore}
          />
        {/if}
      </li>
    {/if}
  {/each}
  {#if clip && clip.shown < clip.total && !keep}
    <li class="more-row" role="none" style:padding-left="{treeIndent(depth) + 22}px">
      <span class="faint"
        >Showing {clip.shown.toLocaleString('en-US')} of {clip.total.toLocaleString('en-US')}</span
      >
      <button
        class="btn ghost sm"
        disabled={reading}
        onclick={async () => {
          reading = true;
          try {
            await onmore?.(parent);
          } finally {
            reading = false;
          }
        }}
        >{#if reading}<span class="spinner"></span>{/if} Show more</button
      >
    </li>
  {/if}
</ul>

<style>
  .tree {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  /* a guide line under each open node of the first levels, down the length of its
     children; deeper levels step only 6px, where a line per level reads as a comb */
  .tree.guided {
    background: linear-gradient(var(--border), var(--border)) no-repeat;
    background-size: 1px 100%;
    background-position: var(--guide, 0) 0;
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
    background: var(--hover);
  }
  .node.sel {
    background: var(--spark-soft);
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

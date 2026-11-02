<script lang="ts">
  // One tree of the ontology editor: the asserted hierarchy of a kind of entity or the
  // inferred class taxonomy (docs/specs/U12-ontology-workbench/SPEC.md). Each node is an id
  // (an IRI, or `n:<node>` of the taxonomy) with a name; the WAI-ARIA tree pattern.
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
  } = $props();

  const sorted = (ids: string[]) =>
    [...ids].sort((a, b) => name(a).localeCompare(name(b), undefined, { sensitivity: 'base' }));
</script>

<ul class="tree" role={depth === 0 ? 'tree' : 'group'} aria-label={depth === 0 ? label : undefined}>
  {#each sorted(roots.filter((r) => !keep || keep.has(r))) as id (id)}
    {#if !path.includes(id)}
      {@const kids = children(id).filter((k) => !path.includes(k) && (!keep || keep.has(k)))}
      {@const isOpen = keep ? true : open.has(id)}
      {@const s = sub?.(id)}
      {@const b = badge?.(id)}
      <li
        role="treeitem"
        aria-label={name(id)}
        aria-expanded={kids.length ? isOpen : undefined}
        aria-selected={selected === id}
        data-id={id}
      >
        <div class="node" class:sel={selected === id} style:padding-left="{depth * 14 + 2}px">
          {#if kids.length}
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
            title={id.startsWith('n:') ? undefined : id}
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
          />
        {/if}
      </li>
    {/if}
  {/each}
</ul>

<style>
  .tree {
    list-style: none;
    margin: 0;
    padding: 0;
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

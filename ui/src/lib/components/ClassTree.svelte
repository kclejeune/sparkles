<script lang="ts">
  import type { Schema } from '$lib/explore';
  import { fmtCompact } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import type { PrefixMap } from '$lib/rdf';
  import ClassTree from './ClassTree.svelte';
  import Icon from './Icon.svelte';

  let {
    schema,
    iris,
    prefixes,
    open,
    selected,
    onselect,
    ontoggle,
    depth = 0,
    path = [],
  }: {
    schema: Schema;
    iris: string[];
    prefixes: PrefixMap;
    open: Set<string>;
    selected: string | null;
    onselect: (iri: string) => void;
    ontoggle: (iri: string) => void;
    depth?: number;
    path?: string[];
  } = $props();
</script>

<ul class="tree" role={depth === 0 ? 'tree' : 'group'}>
  {#each iris as iri (iri)}
    {@const c = schema.classes.get(iri)}
    {#if c && !path.includes(iri)}
      {@const kids = c.subs.filter((s) => !path.includes(s))}
      {@const isOpen = open.has(iri)}
      <li
        role="treeitem"
        aria-expanded={kids.length ? isOpen : undefined}
        aria-selected={selected === iri}
      >
        <div class="node" class:sel={selected === iri} style:padding-left="{depth * 16 + 4}px">
          {#if kids.length}
            <button
              class="twisty"
              class:open={isOpen}
              aria-label={isOpen ? 'Collapse' : 'Expand'}
              onclick={() => ontoggle(iri)}
            >
              <Icon name="chevron" size={12} />
            </button>
          {:else}
            <span class="twisty-space"></span>
          {/if}
          <button class="label" title={iri} onclick={() => onselect(iri)}>
            <span class="name">{c.label ?? shortLabel(iri, prefixes)}</span>
            {#if c.label}<span class="iri">{shortLabel(iri, prefixes)}</span>{/if}
          </button>
          {#if kids.length}<span class="kids faint" title="{kids.length} subclasses"
              >{kids.length}</span
            >{/if}
          <span class="count" class:zero={!c.instances} title="{c.instances} instances"
            >{fmtCompact(c.instances)}</span
          >
        </div>
        {#if kids.length && isOpen}
          <ClassTree
            {schema}
            iris={kids}
            {prefixes}
            {open}
            {selected}
            {onselect}
            {ontoggle}
            depth={depth + 1}
            path={[...path, iri]}
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
    gap: 4px;
    height: 28px;
    padding-right: 10px;
    border-radius: 4px;
  }
  .node:hover {
    background: var(--hover);
  }
  .node.sel {
    background: color-mix(in srgb, var(--iri) 12%, transparent);
  }
  .twisty {
    all: unset;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 3px;
    color: var(--text-3);
    cursor: pointer;
    flex: none;
  }
  .twisty:hover {
    color: var(--text);
    background: var(--active);
  }
  .twisty :global(.icon) {
    transition: transform 0.12s;
  }
  .twisty.open :global(.icon) {
    transform: rotate(90deg);
  }
  .twisty-space {
    width: 18px;
    flex: none;
  }
  .label {
    all: unset;
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
    flex: 1;
    cursor: pointer;
    overflow: hidden;
    white-space: nowrap;
  }
  .label:focus-visible {
    box-shadow: var(--focus);
  }
  .name {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .iri {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-3);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .kids {
    font-size: var(--fs-xs);
  }
  .count {
    min-width: 34px;
    text-align: right;
    font-size: var(--fs-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--literal);
  }
  .count.zero {
    color: var(--text-3);
    font-weight: 400;
  }
</style>

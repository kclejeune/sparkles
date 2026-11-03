<script lang="ts">
  // One kind of link of a SKOS resource in the thesaurus editor (docs/specs/
  // U12-ontology-workbench/SPEC.md): broader, narrower, related, top concepts, schemes or a
  // mapping relation, with an add field that picks a concept. A hierarchical or related
  // link stated in one direction only is marked, and the fix adds the other direction.
  import type { PrefixMap } from '$lib/rdf';
  import type { Link } from '$lib/skos';
  import Icon from './Icon.svelte';
  import SkosPicker from './SkosPicker.svelte';

  let {
    title,
    id,
    links,
    name,
    sub,
    readonly,
    paired = false,
    picker,
    onopen,
    onadd,
    onremove,
    onrepair,
    extra,
  }: {
    title: string;
    /** The section's id, `data-section` for tests and the add field's name. */
    id: string;
    links: Link[];
    name: (iri: string) => string;
    sub: (iri: string) => string | null;
    readonly: boolean;
    /** Whether the link is kept in both directions, so a one-way link is marked. */
    paired?: boolean;
    /** The picker's settings; without them the section has no add field. */
    picker?: {
      target: string;
      dataset: string;
      graph: string | null;
      prefixes: PrefixMap;
      langs: readonly string[];
    };
    onopen: (iri: string) => void;
    onadd?: (iri: string, name?: string) => void;
    onremove?: (iri: string) => void;
    onrepair?: (link: Link) => void;
    extra?: (iri: string) => string | null;
  } = $props();

  let adding = $state(false);
</script>

<section class="links" data-section={id} aria-label={title}>
  <h3>
    {title} <span class="faint">{links.length || ''}</span>
    {#if !readonly && picker && onadd}
      <button
        class="btn ghost icon xs"
        aria-label="Add {title.toLowerCase()}"
        title="Add"
        onclick={() => (adding = !adding)}><Icon name="plus" size={12} /></button
      >
    {/if}
  </h3>
  {#if links.length}
    <ul>
      {#each links as l (l.iri)}
        {@const s = sub(l.iri)}
        {@const x = extra?.(l.iri)}
        <li>
          <button class="link" onclick={() => onopen(l.iri)}>{name(l.iri)}</button>
          {#if s}<span class="mono faint xs">{s}</span>{/if}
          {#if x}<span class="badge">{x}</span>{/if}
          {#if paired && !(l.forward && l.inverse)}
            <span class="warn-text xs" title="Only one of the two triples of this link is stated"
              >one direction</span
            >
            {#if !readonly && onrepair}
              <button class="btn ghost xs" onclick={() => onrepair(l)}>Add the other</button>
            {/if}
          {/if}
          {#if !readonly && onremove}
            <button
              class="btn ghost icon xs"
              aria-label="Remove {name(l.iri)} from {title.toLowerCase()}"
              title="Remove"
              onclick={() => onremove(l.iri)}><Icon name="x" size={11} /></button
            >
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
  {#if adding && picker && onadd}
    <SkosPicker
      {...picker}
      label={title}
      exclude={links.map((l) => l.iri)}
      onpick={(iri, n) => {
        adding = false;
        onadd(iri, n);
      }}
    />
  {/if}
</section>

<style>
  .links {
    display: grid;
    gap: 4px;
    margin-bottom: 10px;
    min-width: 0;
  }
  h3 {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    border-bottom: 1px solid var(--border);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  li {
    display: flex;
    gap: 6px;
    align-items: baseline;
    flex-wrap: wrap;
    overflow-wrap: anywhere;
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--iri);
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }
  .badge {
    font-size: var(--fs-xs);
    color: var(--spark-ink);
    background: var(--spark-soft);
    border-radius: 999px;
    padding: 0 6px;
  }
  .btn.xs {
    padding: 0 4px;
    min-height: 18px;
    font-size: var(--fs-xs);
  }
  .xs {
    font-size: var(--fs-xs);
  }
</style>

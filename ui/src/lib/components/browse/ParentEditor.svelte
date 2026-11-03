<script lang="ts">
  // The parents of an OWL entity in edit mode (ROADMAP item 630), at the top of the
  // hierarchy panel: each direct parent with Remove when the edited graph states it, and a
  // field that searches the ontology's entities of the same kind and previews the new
  // `SubClassOf`, or the subproperty axiom of a property.
  import type { Linker, TreeKind } from '$lib/browse';
  import { axiomOp, parentAxiom, type OnEdit } from '$lib/browse-edit';
  import { searchEntities, type EntitySearchKind } from '$lib/entity-search';
  import type { AxiomItem } from '$lib/ontology';
  import type { PrefixMap } from '$lib/rdf';
  import EntityPicker, { type Picked } from '$components/EntityPicker.svelte';
  import Icon from '$components/Icon.svelte';

  let {
    kind,
    iri,
    title,
    rows,
    linker,
    target,
    graph,
    prefixes,
    readonly,
    onedit,
  }: {
    kind: TreeKind;
    iri: string;
    title: string;
    rows: { iri: string; axiom: AxiomItem | null }[];
    linker: Linker;
    target: string;
    graph: string | null;
    prefixes: PrefixMap;
    readonly: boolean;
    onedit: OnEdit;
  } = $props();

  let picked = $state<Picked | null>(null);
  const noun = $derived(kind === 'class' ? 'class' : 'property');

  async function search(text: string, signal: AbortSignal) {
    const r = await searchEntities(target, text, {
      kind: [kind as EntitySearchKind],
      graph,
      limit: 10,
      signal,
    });
    return r.results
      .filter((h) => h.iri !== iri)
      .map((h) => ({
        iri: h.iri,
        label:
          (h.label ?? h.shortForm ?? h.iri) +
          (h.shortForm && h.label && h.shortForm !== h.label ? ` (${h.shortForm})` : ''),
      }));
  }

  function add() {
    const p = picked;
    if (!p || p.term.type !== 'uri') return;
    picked = null;
    const a = parentAxiom(kind, iri, p.term.value);
    if (!a) return;
    if (rows.some((r) => r.iri === p.term.value)) return;
    onedit(axiomOp(a, false), `Add ${p.label} as a parent of ${title}`);
  }
</script>

<div class="parents" aria-labelledby="parents-h">
  <h3 id="parents-h">
    Parents {#if rows.length}<span class="count-badge">{rows.length}</span>{/if}
  </h3>
  {#if rows.length}
    <ul class="prows">
      {#each rows as r (r.iri)}
        <li class="prow">
          {#if linker.href(r.iri)}<a class="l" href={linker.href(r.iri)}>{linker.name(r.iri)}</a
            >{:else}<span class="l">{linker.name(r.iri)}</span>{/if}
          {#if r.axiom && !readonly}
            <button
              type="button"
              class="btn icon sm ghost"
              aria-label="Remove the parent {linker.name(r.iri)}"
              title="Remove parent"
              onclick={() =>
                onedit(
                  axiomOp(r.axiom!.axiom, true),
                  `Remove ${linker.name(r.iri)} as a parent of ${title}`,
                )}><Icon name="x" size={14} /></button
            >
          {:else if !r.axiom}
            <span class="from" title="Another graph or an equivalence states this parent"
              >not editable here</span
            >
          {/if}
        </li>
      {/each}
    </ul>
  {:else}
    <p class="none">No parents. The {noun} is a root.</p>
  {/if}
  {#if !readonly}
    <div class="add">
      <EntityPicker
        id="browse-add-parent"
        label="Add a parent {noun}"
        hideLabel
        ds={target}
        {prefixes}
        bind:value={picked}
        {search}
        onpicked={add}
      />
    </div>
  {/if}
</div>

<style>
  .parents {
    margin: 0 0 var(--sp-4);
    padding: 0 0 var(--sp-4);
    border-bottom: 1px solid var(--border);
    min-width: 0;
  }
  h3 {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    margin: 0 0 6px;
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .prows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 1px;
  }
  .prow {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 32px;
    padding: 0 2px 0 8px;
    border-radius: var(--r);
    min-width: 0;
  }
  .prow:hover,
  .prow:focus-within {
    background: var(--hover);
  }
  .l {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text);
  }
  .from {
    font-size: var(--fs-xs);
    color: var(--text-3);
    flex: none;
  }
  .none {
    font-size: var(--fs-sm);
    color: var(--text-3);
    margin: 0 0 6px;
  }
  .add {
    margin-top: 6px;
  }
  @container browse (max-width: 639px) {
    .prow .btn {
      width: 36px;
      height: 36px;
    }
  }
</style>

<script lang="ts">
  // The fields of a nested blank-node child of a form (docs/specs/U02-shape-driven-forms/
  // SPEC.md §2.3.1), inside the parent field whose value it is. A child's own nested values
  // render the same way, and a node that is already an ancestor shows a note instead, so
  // a cycle in the data does not repeat forever. The state lives in the form (ShapeForm):
  // `entries` by field key and the registry of `nodes`.
  import type { Term } from '$lib/api';
  import {
    fieldKey,
    nestedField,
    widgetOf,
    type Entry,
    type FieldResult,
    type FormField as Field,
    type FormNode,
    type Problem,
  } from '$lib/forms';
  import type { PrefixMap } from '$lib/rdf';
  import FormField from './FormField.svelte';
  import NodeFields from './NodeFields.svelte';
  import TermView from './TermView.svelte';

  let {
    node,
    path,
    nodes,
    entries = $bindable(),
    editable,
    problems,
    results,
    ds,
    prefixes,
    lang = 'en',
    fullText = false,
    canAdd,
    addchild,
  }: {
    node: FormNode;
    /** The keys of the node and its ancestors. */
    path: string[];
    nodes: Record<string, FormNode>;
    entries: Record<string, Entry[]>;
    editable: boolean;
    problems: Record<string, Problem[]>;
    results: Record<string, FieldResult[]>;
    ds: string;
    prefixes: PrefixMap;
    lang?: string;
    fullText?: boolean;
    /** Whether a field can take a new nested child. */
    canAdd: (f: Field) => boolean;
    /** Add a new nested child to a field of the node `owner`. */
    addchild: (owner: string | null, f: Field) => void;
  } = $props();
</script>

{#snippet child(term: Term)}
  {@const k = term.type === 'bnode' ? term.value : ''}
  {#if path.includes(k)}
    <p class="faint small">The same node as above.</p>
  {:else if nodes[k]}
    <NodeFields
      node={nodes[k]}
      path={[...path, k]}
      {nodes}
      bind:entries
      {editable}
      {problems}
      {results}
      {ds}
      {prefixes}
      {lang}
      {fullText}
      {canAdd}
      {addchild}
    />
  {:else}
    <TermView {term} {prefixes} />
  {/if}
{/snippet}

<div class="node-fields">
  {#if node.isNew}<p class="faint small new">New</p>{/if}
  {#each node.fields as f (f.id)}
    {@const key = fieldKey(node.key, f.id)}
    {#if entries[key]}
      <FormField
        field={f}
        {key}
        widget={widgetOf(f)}
        bind:entries={entries[key]}
        editable={editable && f.editable}
        problems={problems[key] ?? []}
        results={results[key] ?? []}
        {ds}
        {prefixes}
        {lang}
        {fullText}
        child={nestedField(f) ? child : undefined}
        onaddchild={canAdd(f) ? () => addchild(node.key, f) : undefined}
      />
    {/if}
  {/each}
</div>

<style>
  .node-fields {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .new {
    margin: 0;
  }
</style>

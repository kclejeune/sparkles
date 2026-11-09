<script lang="ts">
  import type { PrefixMap } from '$lib/rdf';
  import {
    parseIri,
    PATH_LABELS,
    PATH_OPS,
    type PathKind,
    type ShapePath,
  } from '$lib/shape-editor';
  import { shorten } from '$lib/rdf';
  import ShapePathEditor from './ShapePath.svelte';
  let {
    value,
    prefixes,
    readonly = false,
    onchange,
    depth = 0,
  }: {
    value: ShapePath;
    prefixes: PrefixMap;
    readonly?: boolean;
    onchange: (path: ShapePath) => void;
    depth?: number;
  } = $props();
  const children = $derived(value.children ?? []);
  let error = $state<string | null>(null);
  function changeKind(kind: PathKind) {
    const child: ShapePath = { kind: 'predicate', iri: '' };
    onchange(
      kind === 'predicate'
        ? child
        : {
            kind,
            children:
              kind === 'sequence' || kind === 'alternativePath' ? [child, { ...child }] : [child],
          },
    );
  }
</script>

<div class="path-node" class:nested={depth > 0}>
  <div class="path-row">
    <select
      class="select"
      aria-label={`Path kind at depth ${depth}`}
      value={value.kind}
      onchange={(e) => changeKind(e.currentTarget.value as PathKind)}
      disabled={readonly || depth >= 64}
    >
      {#each PATH_OPS as op (op)}<option value={op}>{PATH_LABELS[op]}</option>{/each}
    </select>
    {#if value.kind === 'predicate'}
      <input
        class="input"
        aria-label={`Path predicate at depth ${depth}`}
        value={value.iri ? (shorten(value.iri, prefixes) ?? value.iri) : ''}
        placeholder="ex:property or <IRI>"
        disabled={readonly}
        oninput={(e) => {
          const text = e.currentTarget.value;
          try {
            const iri = parseIri(text, prefixes);
            error = null;
            onchange({ kind: 'predicate', iri });
          } catch {
            error = 'Enter an absolute predicate IRI or a declared prefixed name.';
            onchange({ kind: 'predicate', iri: text });
          }
        }}
      />
    {/if}
  </div>
  {#if depth < 64}
    {#each children as child, i (i)}
      <div class="child">
        <ShapePathEditor
          value={child}
          {prefixes}
          {readonly}
          depth={depth + 1}
          onchange={(p) => {
            const next = [...children];
            next[i] = p;
            onchange({ ...value, children: next });
          }}
        />
        {#if value.kind === 'sequence' || value.kind === 'alternativePath'}
          <button
            class="btn ghost"
            aria-label={`Move path member ${i + 1} up at depth ${depth}`}
            disabled={readonly || i === 0}
            onclick={() => {
              const next = [...children];
              [next[i - 1], next[i]] = [next[i], next[i - 1]];
              onchange({ ...value, children: next });
            }}>↑</button
          >
          <button
            class="btn ghost"
            aria-label={`Remove path member ${i + 1} at depth ${depth}`}
            disabled={readonly || children.length <= 2}
            onclick={() => onchange({ ...value, children: children.filter((_, j) => i !== j) })}
            >×</button
          >
        {/if}
      </div>
    {/each}
    {#if value.kind === 'sequence' || value.kind === 'alternativePath'}<button
        class="btn ghost"
        disabled={readonly}
        onclick={() =>
          onchange({ ...value, children: [...children, { kind: 'predicate', iri: '' }] })}
        >Add path member</button
      >{/if}
  {:else}<p class="warn-text">The path nesting limit is 64 levels.</p>{/if}
  {#if error}<p class="warn-text">{error}</p>{/if}
</div>

<style>
  .path-node {
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  .nested {
    border-left: 1px solid var(--border);
    padding-left: 12px;
  }
  .path-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .path-row input {
    flex: 1 1 170px;
    min-width: 0;
  }
  .child {
    display: flex;
    gap: 4px;
    align-items: start;
    min-width: 0;
  }
  .child > :global(.path-node) {
    flex: 1;
  }
  p {
    font-size: var(--fs-sm);
    margin: 0;
  }
</style>

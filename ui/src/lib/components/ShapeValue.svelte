<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import type { Term } from '$lib/api';
  import { displayTerm, shorten, type PrefixMap } from '$lib/rdf';
  import {
    id,
    inputTerm,
    listKind,
    ShapeGraph,
    valueProblem,
    type Field,
    type ValueKind,
  } from '$lib/shape-editor';

  let {
    field,
    value = null,
    graph,
    prefixes,
    readonly = false,
    onapply,
    onremove,
    onpending,
  }: {
    field: Field;
    value?: Term | null;
    graph: ShapeGraph;
    prefixes: PrefixMap;
    readonly?: boolean;
    onapply: (values: Term[]) => boolean | void;
    onremove?: () => void;
    onpending: (pending: boolean) => void;
  } = $props();
  type Entry = { type: string; text: string; qualifier: string; direction: string };
  const editable = (t: Term): Entry =>
    t.type === 'literal'
      ? {
          type: 'literal',
          text: t.value,
          qualifier: t['xml:lang']
            ? '@' + t['xml:lang']
            : t.datatype
              ? (shorten(t.datatype, prefixes) ?? `<${t.datatype}>`)
              : '',
          direction: t['its:dir'] ?? '',
        }
      : {
          type: t.type === 'bnode' ? 'bnode' : 'uri',
          text:
            t.type === 'uri'
              ? (shorten(t.value, prefixes) ?? `<${t.value}>`)
              : t.type === 'bnode'
                ? '_:' + t.value
                : '',
          qualifier: '',
          direction: '',
        };
  let malformed = $state<string | null>(null);
  const initial = untrack(() => {
    if (!value) return [];
    try {
      return (listKind(field.kind) ? graph.list(value) : [value]).map(editable);
    } catch (e) {
      malformed = String((e as Error).message);
      return [];
    }
  });
  let entries = $state<Entry[]>(structuredClone(initial));
  let replacing = $state(false);
  let saved = $state(JSON.stringify(initial));
  let error = $state<string | null>(null);
  const changed = $derived(replacing || JSON.stringify(entries) !== saved);
  $effect(() => {
    const p = changed;
    untrack(() => onpending(p));
  });
  onDestroy(() => onpending(false));
  const kind = $derived<ValueKind>(
    field.kind === 'iris'
      ? 'iri'
      : field.kind === 'shapes'
        ? 'shape'
        : field.kind === 'languages'
          ? 'text'
          : field.kind === 'terms'
            ? 'term'
            : field.kind,
  );
  const generic = $derived(kind === 'term' || kind === 'shape');
  const text = $derived(kind === 'text' || kind === 'term');
  const options = $derived(
    field.name === 'severity'
      ? ['sh:Violation', 'sh:Warning', 'sh:Info']
      : field.name === 'nodeKind'
        ? [
            'sh:IRI',
            'sh:BlankNode',
            'sh:Literal',
            'sh:BlankNodeOrIRI',
            'sh:BlankNodeOrLiteral',
            'sh:IRIOrLiteral',
          ]
        : [],
  );

  function add() {
    entries = [
      ...entries,
      {
        type: kind === 'iri' || kind === 'shape' ? 'uri' : 'literal',
        text: kind === 'boolean' ? 'true' : '',
        qualifier: '',
        direction: '',
      },
    ];
    replacing = true;
  }
  function apply() {
    try {
      if (!listKind(field.kind) && entries.length !== 1) throw new Error('Enter one value.');
      const values = entries.map((e) =>
        inputTerm(
          kind,
          e.text,
          prefixes,
          generic ? e.type : 'literal',
          text ? e.qualifier : '',
          text ? e.direction : '',
        ),
      );
      if (
        field.kind === 'languages' &&
        values.some(
          (t) =>
            t.type !== 'literal' || !/^(?:\*|[A-Za-z]{1,8}(?:-[A-Za-z0-9]{1,8})*)$/.test(t.value),
        )
      )
        throw new Error('Use basic language ranges such as en, fr-CA or *.');
      if (kind === 'shape' && values.some((t) => !graph.shapes().some((s) => id(s) === id(t))))
        throw new Error('Choose an existing shape or create that shape first.');
      if (
        ['minExclusive', 'minInclusive', 'maxExclusive', 'maxInclusive'].includes(field.name) &&
        values.some((t) => t.type !== 'literal')
      )
        throw new Error('A range bound must be a literal with its datatype.');
      for (const v of values) {
        const scalar = { ...field, kind };
        const issue = valueProblem(graph, scalar, v);
        if (issue) throw new Error(issue);
      }
      error = null;
      if (onapply(values) === false) return;
      saved = JSON.stringify(entries);
      replacing = false;
    } catch (e) {
      error = (e as Error).message;
    }
  }
</script>

<div class="value-editor">
  {#if value && valueProblem(graph, field, value)}<p class="warn-text">
      {valueProblem(graph, field, value)}
    </p>{/if}
  {#if malformed && !replacing}
    <p class="warn-text">{malformed} {value ? displayTerm(value, prefixes) : ''}</p>
    <button
      class="btn"
      disabled={readonly}
      onclick={() => {
        replacing = true;
        entries = [];
      }}>Replace malformed list</button
    >
  {:else}
    {#if value?.type === 'triple'}<p class="warn-text">
        This quoted triple is preserved until you explicitly replace it.
      </p>{/if}
    {#each entries as entry, i (i)}
      <div class="entry">
        {#if generic}
          <select
            class="select type"
            aria-label={`${field.label} value kind ${i + 1}`}
            bind:value={entry.type}
            disabled={readonly}
          >
            <option value="uri">IRI</option><option value="bnode">Blank node</option>
            {#if kind === 'term'}<option value="literal">Literal</option>{/if}
          </select>
        {/if}
        {#if kind === 'boolean'}
          <select
            class="select main"
            aria-label={`${field.label} value ${i + 1}`}
            bind:value={entry.text}
            disabled={readonly}
          >
            {#if !['true', 'false'].includes(entry.text)}<option value={entry.text}
                >{entry.text} (stored)</option
              >{/if}
            <option value="true">true</option><option value="false">false</option>
          </select>
        {:else}
          <input
            class="input main"
            aria-label={`${field.label} value ${i + 1}`}
            bind:value={entry.text}
            disabled={readonly}
            list={options.length
              ? `shape-${field.name}`
              : kind === 'shape'
                ? 'shape-references'
                : undefined}
            placeholder={kind === 'integer'
              ? '0'
              : kind === 'iri'
                ? 'ex:Term or <IRI>'
                : kind === 'shape'
                  ? 'Existing shape'
                  : 'Value'}
          />
        {/if}
        {#if text && (!generic || entry.type === 'literal')}
          <input
            class="input qualifier"
            aria-label={`${field.label} language or datatype ${i + 1}`}
            bind:value={entry.qualifier}
            placeholder="@en or xsd:string"
            disabled={readonly}
          />
          <select
            class="select"
            aria-label={`${field.label} text direction ${i + 1}`}
            bind:value={entry.direction}
            disabled={readonly}
          >
            <option value="">No direction</option><option value="ltr">Left to right</option><option
              value="rtl">Right to left</option
            >
          </select>
        {/if}
        {#if listKind(field.kind)}
          <button
            class="btn ghost"
            aria-label={`Move ${field.label} item ${i + 1} up`}
            disabled={readonly || i === 0}
            onclick={() => {
              const a = [...entries];
              [a[i - 1], a[i]] = [a[i], a[i - 1]];
              entries = a;
            }}>↑</button
          >
          <button
            class="btn ghost"
            aria-label={`Remove ${field.label} item ${i + 1}`}
            disabled={readonly}
            onclick={() => {
              entries = entries.filter((_, j) => j !== i);
              replacing = true;
            }}>×</button
          >
        {/if}
      </div>
    {/each}
    {#if options.length}<datalist id={`shape-${field.name}`}
        >{#each options as option (option)}<option value={option}></option>{/each}</datalist
      >{/if}
    <div class="actions">
      {#if listKind(field.kind) || !entries.length}<button
          class="btn"
          disabled={readonly}
          onclick={add}>{listKind(field.kind) ? 'Add list item' : 'Enter value'}</button
        >{/if}
      <button class="btn" disabled={readonly || (!changed && value != null)} onclick={apply}
        >Apply {field.label}</button
      >
      {#if changed}<button
          class="btn ghost"
          onclick={() => {
            entries = structuredClone(initial);
            replacing = false;
            error = null;
          }}>Discard field edits</button
        >{/if}
    </div>
  {/if}
  {#if onremove && value}<button class="btn ghost remove" disabled={readonly} onclick={onremove}
      >Remove {field.label}</button
    >{/if}
  {#if error}<p class="error-box" role="alert">{error}</p>{/if}
</div>

<style>
  .value-editor {
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  .entry,
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .main {
    flex: 1 1 180px;
    min-width: 0;
  }
  .qualifier {
    flex: 1 1 135px;
    min-width: 0;
  }
  .type {
    width: 104px;
  }
  .remove {
    justify-self: start;
    color: var(--text-3);
  }
  p {
    margin: 0;
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
</style>

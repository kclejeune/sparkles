<script lang="ts">
  // The annotations of an entity or of an axiom in the ontology editor: each a property
  // and a value, which is a literal with a language tag or a datatype, or an IRI. A change
  // hands the whole new list to the caller, which turns it into one previewed edit.
  import {
    COMMON_ANNOTATION_PROPERTIES,
    isLit,
    literalOf,
    LITERAL_TYPES,
    validIri,
    validLang,
    type Annotation,
    type AnnValue,
  } from '$lib/ontology';
  import type { PrefixMap } from '$lib/rdf';
  import Icon from './Icon.svelte';

  let {
    annotations,
    name,
    prefixes,
    properties = [],
    readonly = false,
    label,
    onchange,
  }: {
    annotations: Annotation[];
    /** An IRI as the editor shows it. */
    name: (iri: string) => string;
    prefixes: PrefixMap;
    /** The annotation properties the ontology declares, offered after the common ones. */
    properties?: string[];
    readonly?: boolean;
    /** What the annotations describe, for the accessible names. */
    label: string;
    onchange: (next: Annotation[], what: string) => void;
  } = $props();

  /** The row being edited (its index), or `annotations.length` for a new one. */
  let editing = $state<number | null>(null);
  let property = $state('');
  let value = $state('');
  let lang = $state('');
  let datatype = $state(LITERAL_TYPES[0]);
  let isIriValue = $state(false);
  let problem = $state<string | null>(null);

  const offered = $derived([...new Set([...COMMON_ANNOTATION_PROPERTIES, ...properties])]);
  const uid = Math.random().toString(36).slice(2, 8);

  function expand(t: string): string | null {
    const s = t.trim();
    if (s.startsWith('<') && s.endsWith('>')) return s.slice(1, -1);
    const byName = offered.find((p) => name(p) === s);
    if (byName) return byName;
    const c = s.indexOf(':');
    if (c > 0 && prefixes[s.slice(0, c)] !== undefined && !s.slice(c + 1).startsWith('//'))
      return prefixes[s.slice(0, c)] + s.slice(c + 1);
    return validIri(s) ? s : null;
  }

  function begin(i: number) {
    editing = i;
    problem = null;
    const a = annotations[i];
    if (a) {
      property = name(a.property);
      isIriValue = !isLit(a.value);
      value = isLit(a.value) ? a.value['@value'] : name(String(a.value));
      lang = isLit(a.value) ? (a.value['@language'] ?? '') : '';
      datatype = isLit(a.value) ? (a.value['@type'] ?? LITERAL_TYPES[0]) : LITERAL_TYPES[0];
    } else {
      property = name(COMMON_ANNOTATION_PROPERTIES[0]);
      value = '';
      lang = navigator.language?.split('-')[0] ?? 'en';
      datatype = LITERAL_TYPES[0];
      isIriValue = false;
    }
  }

  function save() {
    const p = expand(property);
    if (!p) return void (problem = 'The property is not a known name, a prefixed name or an IRI.');
    let v: AnnValue;
    if (isIriValue) {
      const iri = expand(value);
      if (!iri) return void (problem = 'The value is not an IRI or a prefixed name.');
      v = iri;
    } else {
      if (!validLang(lang.trim()))
        return void (problem = 'The language tag is not a BCP 47 tag, such as en or pt-BR.');
      v = literalOf(value, lang.trim(), datatype);
    }
    const next = [...annotations];
    const row: Annotation = { property: p, value: v };
    const i = editing ?? next.length;
    const what =
      i < annotations.length
        ? `Change ${name(p)} of ${label}`
        : `Annotate ${label} with ${name(p)}`;
    if (i < next.length)
      next[i] = { ...row, ...(next[i].annotations ? { annotations: next[i].annotations } : {}) };
    else next.push(row);
    editing = null;
    onchange(next, what);
  }

  function remove(i: number) {
    const a = annotations[i];
    onchange(
      annotations.filter((_, j) => j !== i),
      `Remove ${name(a.property)} of ${label}`,
    );
  }

  const shownValue = (v: AnnValue) => (isLit(v) ? v['@value'] : name(String(v)));
</script>

<div class="annotations">
  {#if annotations.length}
    <ul class="rows" aria-label="Annotations of {label}">
      {#each annotations as a, i (i)}
        <li class="row-item">
          {#if editing === i}
            {@render form()}
          {:else}
            <span class="prop">{name(a.property)}</span>
            <span class="value" class:iri={!isLit(a.value)}>{shownValue(a.value)}</span>
            {#if isLit(a.value) && a.value['@language']}<span class="lang" title="Language"
                >{a.value['@language']}</span
              >{/if}
            {#if isLit(a.value) && a.value['@type']}<span class="lang" title="Datatype"
                >{name(a.value['@type'])}</span
              >{/if}
            {#if !readonly}
              <span class="acts">
                <button
                  class="btn ghost icon sm"
                  aria-label="Edit {name(a.property)} {shownValue(a.value)}"
                  onclick={() => begin(i)}><Icon name="pencil" size={13} /></button
                >
                <button
                  class="btn ghost icon sm"
                  aria-label="Remove {name(a.property)} {shownValue(a.value)}"
                  onclick={() => remove(i)}><Icon name="trash" size={13} /></button
                >
              </span>
            {/if}
          {/if}
        </li>
      {/each}
    </ul>
  {:else}
    <p class="faint none">No annotations.</p>
  {/if}
  {#if editing === annotations.length}
    {@render form()}
  {:else if !readonly && editing === null}
    <button class="btn sm ghost add" onclick={() => begin(annotations.length)}
      ><Icon name="plus" size={13} /> Add annotation</button
    >
  {/if}
</div>

{#snippet form()}
  <form
    class="form"
    aria-label="Annotation of {label}"
    onsubmit={(e) => {
      e.preventDefault();
      save();
    }}
  >
    <label class="f prop-f">
      <span>Property</span>
      <input class="input" list="ann-props-{uid}" bind:value={property} required />
      <datalist id="ann-props-{uid}">
        {#each offered as p (p)}<option value={name(p)}></option>{/each}
      </datalist>
    </label>
    <label class="f kind-f">
      <span>Value is</span>
      <select class="select" bind:value={isIriValue}>
        <option value={false}>text</option>
        <option value={true}>an IRI</option>
      </select>
    </label>
    <label class="f value-f">
      <span>Value</span>
      {#if isIriValue}
        <input class="input mono" bind:value placeholder="prefix:name or IRI" required />
      {:else}
        <textarea class="textarea" rows="2" bind:value></textarea>
      {/if}
    </label>
    {#if !isIriValue}
      <label class="f lang-f">
        <span>Language</span>
        <input
          class="input"
          bind:value={lang}
          placeholder="en"
          size="6"
          aria-invalid={!validLang(lang.trim())}
        />
      </label>
      {#if !lang.trim()}
        <label class="f type-f">
          <span>Datatype</span>
          <select class="select" bind:value={datatype}>
            {#each LITERAL_TYPES as t (t)}<option value={t}>{name(t)}</option>{/each}
          </select>
        </label>
      {/if}
    {/if}
    {#if problem}<p class="problem" role="alert">{problem}</p>{/if}
    <div class="form-acts">
      <button class="btn sm" type="button" onclick={() => (editing = null)}>Cancel</button>
      <button class="btn sm primary" type="submit">Preview</button>
    </div>
  </form>
{/snippet}

<style>
  .annotations {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .row-item {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
    padding: 3px 4px;
    border-radius: var(--r-sm);
    min-width: 0;
  }
  .row-item:hover {
    background: var(--hover);
  }
  .prop {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    color: var(--text-2);
    flex: none;
  }
  .value {
    color: var(--literal);
    overflow-wrap: anywhere;
    min-width: 0;
    flex: 1 1 12em;
  }
  .value.iri {
    color: var(--iri);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
  }
  .lang {
    font-size: var(--fs-xs);
    color: var(--text-3);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0 6px;
  }
  .acts {
    margin-left: auto;
    display: inline-flex;
    gap: 2px;
  }
  .none {
    font-size: var(--fs-sm);
  }
  .add {
    justify-self: start;
  }
  .form {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: end;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--bg);
    width: 100%;
    min-width: 0;
  }
  .f {
    display: grid;
    gap: 2px;
    font-size: var(--fs-xs);
    color: var(--text-2);
    min-width: 0;
  }
  .prop-f {
    flex: 1 1 10em;
  }
  .value-f {
    flex: 3 1 16em;
  }
  .value-f .textarea,
  .value-f .input,
  .prop-f .input {
    width: 100%;
  }
  .problem {
    flex-basis: 100%;
    color: var(--danger);
    font-size: var(--fs-sm);
  }
  .form-acts {
    display: flex;
    gap: 6px;
    margin-left: auto;
  }
</style>

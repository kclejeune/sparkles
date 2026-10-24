<script lang="ts">
  // One field of an editing form (docs/specs/U02-shape-driven-forms.md §2.2, §2.3): its
  // label and help, an input per value in the widget its constraints choose, and the
  // problems of the browser's checks and the server's check under it.
  import type { Term } from '$lib/api';
  import {
    entryOf,
    entryTerm,
    fieldDomId,
    multiValued,
    REASONS,
    required,
    resultText,
    sameTerm,
    serverPatterns,
    termText,
    type Entry,
    type FieldResult,
    type FormField,
    type Problem,
    type Widget,
  } from '$lib/forms';
  import { displayTerm, type PrefixMap } from '$lib/rdf';
  import { numberStep, typeBounds, XSD } from '$lib/xsd';
  import EntityPicker from './EntityPicker.svelte';
  import TermView from './TermView.svelte';

  let {
    field,
    widget,
    entries = $bindable(),
    editable,
    problems = [],
    results = [],
    ds,
    prefixes,
    lang = 'en',
  }: {
    field: FormField;
    widget: Widget;
    entries: Entry[];
    /** Whether the person may change this field now. */
    editable: boolean;
    /** The browser's problems of the field and its values. */
    problems?: Problem[];
    /** The server check's results on this field. */
    results?: FieldResult[];
    ds: string;
    prefixes: PrefixMap;
    /** The language new text with a language takes. */
    lang?: string;
  } = $props();

  const base = $derived(fieldDomId(field.id));
  const c = $derived(field.constraints);
  const many = $derived(multiValued(field));
  const req = $derived(required(field));
  const options = $derived(c.in ?? []);
  /**
   * A list of allowed values takes checkboxes when the shapes allow several values
   * (`sh:maxCount` above 1) or the resource has several. Without `sh:maxCount` and with
   * at most one value it is a radio group, as the spec's table says.
   */
  const checkList = $derived(
    (c.maxCount !== undefined && c.maxCount > 1) || field.values.length > 1,
  );
  const errId = $derived(`${base}-problems`);
  const helpId = $derived(`${base}-help`);
  const hasProblems = $derived(problems.length > 0 || results.length > 0);
  const describedby = $derived(
    [field.description ? helpId : '', hasProblems ? errId : ''].filter(Boolean).join(' ') ||
      undefined,
  );
  const full = $derived(
    c.maxCount !== undefined && entries.filter((e) => e.term).length >= c.maxCount,
  );
  const noted = $derived(serverPatterns(field));
  const inputType = $derived(
    widget === 'date'
      ? 'date'
      : widget === 'datetime'
        ? 'datetime-local'
        : widget === 'number'
          ? 'number'
          : 'text',
  );
  const bounds = $derived.by(() => {
    const b = typeBounds(c.datatype);
    const lit = (t: Term | undefined) => (t?.type === 'literal' ? t.value : undefined);
    return {
      min: lit(c.minInclusive) ?? b.min,
      max: lit(c.maxInclusive) ?? b.max,
    };
  });
  const langs = $derived(c.languageIn ?? []);

  /** The id of an entry's input: the field's own id for the first. */
  const inputId = (i: number) => (i === 0 ? base : `${base}-${i}`);
  /** The accessible name of an entry's input. */
  const nameOf = (i: number) =>
    many && entries.length > 1 ? `${field.label} ${i + 1}` : field.label;

  function setText(i: number, text: string, langTag = entries[i].lang) {
    const r = entryTerm(field, widget, text, langTag, prefixes);
    entries[i] = { ...entries[i], text, lang: langTag, term: r.term, error: r.error };
  }

  function setTerm(i: number, term: Term | null, label?: string) {
    entries[i] = {
      ...entries[i],
      term,
      text: term ? termText(term) : '',
      error: undefined,
      label,
    };
  }

  function add() {
    const e = entryOf(null, widget);
    if (widget === 'text-lang') e.lang = langs[0] ?? lang;
    entries = [...entries, e];
    queueMicrotask(() => document.getElementById(inputId(entries.length - 1))?.focus());
  }

  function remove(i: number) {
    entries = entries.filter((_, j) => j !== i);
    if (!entries.length && (!many || req)) entries = [entryOf(null, widget)];
    queueMicrotask(() => document.getElementById(`${base}-add`)?.focus());
  }

  /** Whether the option `t` is among the values (radio groups and checkbox lists). */
  const chosen = (t: Term) => entries.some((e) => e.term && sameTerm(e.term, t));

  function toggle(t: Term, on: boolean) {
    if (on && !chosen(t)) entries = [...entries.filter((e) => e.term), { ...entryOf(t, widget) }];
    else if (!on) entries = entries.filter((e) => !(e.term && sameTerm(e.term, t)));
  }

  function choose(t: Term | null) {
    entries = [t ? entryOf(t, widget) : entryOf(null, widget)];
  }

  const boolTerm = (v: boolean): Term => ({
    type: 'literal',
    value: String(v),
    datatype: `${XSD}boolean`,
  });
  const optionText = (t: Term) => displayTerm(t, prefixes);
  const optionKey = (t: Term) => JSON.stringify(t);
  const selectValue = (e: Entry | undefined) => (e?.term ? optionKey(e.term) : '');
  const entryProblems = (i: number) => problems.filter((p) => p.index === i);
</script>

<div class="ff" class:invalid={hasProblems} data-field={field.id}>
  {#if !editable}
    <div class="ff-head">
      <span class="ff-label" id="{base}-label">{field.label}</span>
      {#if req}<span class="req">required</span>{/if}
      <span class="ro" title={field.reason ? REASONS[field.reason] : 'Read-only'}
        >read-only{field.reason ? `: ${REASONS[field.reason]}` : ''}</span
      >
    </div>
    {#if field.description}<p class="help" id={helpId}>{field.description}</p>{/if}
    <div class="ro-values" id={base} tabindex="-1" aria-labelledby="{base}-label">
      {#each field.values as v, i (i)}
        <div class="ro-value"><TermView term={v.term} {prefixes} /></div>
      {:else}
        <span class="faint">No value</span>
      {/each}
      {#if field.truncated}<span class="faint small">More values are not listed.</span>{/if}
    </div>
  {:else if (widget === 'radio' || widget === 'select') && options.length && checkList}
    <fieldset class="ff-set" aria-describedby={describedby}>
      <legend class="ff-label"
        >{field.label}{#if req}<span class="req"> required</span>{/if}</legend
      >
      {#if field.description}<p class="help" id={helpId}>{field.description}</p>{/if}
      <div class="choices">
        {#each options as o, i (optionKey(o))}
          <label class="choice">
            <input
              type="checkbox"
              id={i === 0 ? base : `${base}-o${i}`}
              checked={chosen(o)}
              onchange={(e) => toggle(o, e.currentTarget.checked)}
            />
            {optionText(o)}
          </label>
        {/each}
      </div>
    </fieldset>
  {:else if widget === 'radio' && options.length}
    <fieldset class="ff-set" aria-describedby={describedby}>
      <legend class="ff-label"
        >{field.label}{#if req}<span class="req"> required</span>{/if}</legend
      >
      {#if field.description}<p class="help" id={helpId}>{field.description}</p>{/if}
      <div class="choices" role="radiogroup" aria-label={field.label}>
        {#each options as o, i (optionKey(o))}
          <label class="choice">
            <input
              type="radio"
              name={base}
              id={i === 0 ? base : `${base}-o${i}`}
              checked={chosen(o)}
              onchange={() => choose(o)}
            />
            {optionText(o)}
          </label>
        {/each}
        {#if !req && entries.some((e) => e.term)}
          <button type="button" class="btn sm ghost" onclick={() => choose(null)}>Clear</button>
        {/if}
      </div>
    </fieldset>
  {:else}
    <div class="ff-head">
      {#if many}
        <span class="ff-label" id="{base}-label">{field.label}</span>
      {:else}
        <label class="ff-label" for={base} id="{base}-label">{field.label}</label>
      {/if}
      {#if req}<span class="req">required</span>{/if}
    </div>
    {#if field.description}<p class="help" id={helpId}>{field.description}</p>{/if}
    <div
      class="values"
      role={many ? 'group' : undefined}
      aria-labelledby={many ? `${base}-label` : undefined}
    >
      {#each entries as e, i (e.key)}
        <div class="value-row">
          {#if widget === 'select' && options.length}
            <select
              class="select"
              id={inputId(i)}
              aria-label={many ? nameOf(i) : undefined}
              aria-describedby={describedby}
              aria-invalid={hasProblems || undefined}
              value={selectValue(e)}
              onchange={(ev) => {
                const v = ev.currentTarget.value;
                setTerm(i, v ? (JSON.parse(v) as Term) : null);
              }}
            >
              <option value="">{req ? 'Choose a value' : 'None'}</option>
              {#each options as o (optionKey(o))}
                <option value={optionKey(o)}>{optionText(o)}</option>
              {/each}
            </select>
          {:else if widget === 'checkbox'}
            <input
              type="checkbox"
              id={inputId(i)}
              aria-describedby={describedby}
              checked={e.term?.type === 'literal' &&
                (e.term.value === 'true' || e.term.value === '1')}
              onchange={(ev) => setTerm(i, boolTerm(ev.currentTarget.checked))}
            />
          {:else if widget === 'boolean-select'}
            <select
              class="select"
              id={inputId(i)}
              aria-label={many ? nameOf(i) : undefined}
              aria-describedby={describedby}
              value={e.term?.type === 'literal'
                ? e.term.value === '1'
                  ? 'true'
                  : e.term.value === '0'
                    ? 'false'
                    : e.term.value
                : ''}
              onchange={(ev) => {
                const v = ev.currentTarget.value;
                setTerm(i, v ? boolTerm(v === 'true') : null);
              }}
            >
              <option value="">Not set</option>
              <option value="true">true</option>
              <option value="false">false</option>
            </select>
          {:else if widget === 'entity'}
            <EntityPicker
              id={inputId(i)}
              label={nameOf(i)}
              hideLabel
              {ds}
              {prefixes}
              {describedby}
              invalid={hasProblems}
              bind:value={
                () =>
                  e.term ? { term: e.term, label: e.label ?? displayTerm(e.term, prefixes) } : null,
                (v) => setTerm(i, v?.term ?? null, v?.label)
              }
            />
          {:else if widget === 'textarea'}
            <textarea
              class="textarea"
              id={inputId(i)}
              rows="3"
              aria-label={many ? nameOf(i) : undefined}
              aria-describedby={describedby}
              aria-invalid={hasProblems || undefined}
              value={e.text}
              oninput={(ev) => setText(i, ev.currentTarget.value)}></textarea>
          {:else}
            <input
              class="input"
              class:mono={widget === 'term' || widget === 'iri'}
              type={inputType}
              id={inputId(i)}
              aria-label={many ? nameOf(i) : undefined}
              aria-describedby={describedby}
              aria-invalid={hasProblems || e.error ? true : undefined}
              step={widget === 'number'
                ? numberStep(c.datatype)
                : widget === 'datetime'
                  ? '1'
                  : undefined}
              min={widget === 'number' ? bounds.min : undefined}
              max={widget === 'number' ? bounds.max : undefined}
              placeholder={widget === 'term'
                ? 'IRI, prefixed name or "literal"'
                : widget === 'iri'
                  ? 'IRI or prefixed name'
                  : undefined}
              value={e.text}
              oninput={(ev) => setText(i, ev.currentTarget.value)}
            />
            {#if widget === 'text-lang'}
              {#if langs.length}
                <select
                  class="select lang"
                  aria-label="Language of {nameOf(i)}"
                  value={e.lang}
                  onchange={(ev) => setText(i, e.text, ev.currentTarget.value)}
                >
                  {#if !langs.includes(e.lang)}<option value={e.lang}>{e.lang || 'none'}</option
                    >{/if}
                  {#each langs as l (l)}<option value={l}>{l}</option>{/each}
                </select>
              {:else}
                <input
                  class="input lang"
                  aria-label="Language of {nameOf(i)}"
                  placeholder="lang"
                  value={e.lang}
                  oninput={(ev) => setText(i, e.text, ev.currentTarget.value)}
                />
              {/if}
            {/if}
          {/if}
          {#if many || (!req && widget === 'entity' && e.term)}
            <button
              type="button"
              class="btn sm ghost icon"
              aria-label="Remove {nameOf(i)}"
              title="Remove this value"
              onclick={() => remove(i)}>×</button
            >
          {/if}
        </div>
        {#if e.error}<p class="problem small">{e.error}</p>{/if}
        {#each entryProblems(i) as p (p.message)}
          <p class="problem small">
            <span aria-hidden="true">⚠</span>
            {e.text || optionText(e.term ?? { type: 'literal', value: '' })}
            {p.message}
          </p>
        {/each}
      {/each}
      {#if many}
        <button
          type="button"
          class="btn sm add"
          id="{base}-add"
          disabled={full}
          title={full ? `At most ${c.maxCount} values` : undefined}
          onclick={add}>+ Add {field.label}</button
        >
      {/if}
    </div>
  {/if}
  {#if hasProblems}
    <div class="problems" id={errId}>
      {#each problems.filter((p) => p.index === null) as p (p.message)}
        <p class="problem small"><span aria-hidden="true">⚠</span> {field.label} {p.message}</p>
      {/each}
      {#each results as r, i (i)}
        <p class="problem small server">
          <span aria-hidden="true">⚠</span>
          {r.value ? `${displayTerm(r.value, prefixes)}: ` : ''}{resultText(r)}
          <span class="faint">(server check)</span>
        </p>
      {/each}
    </div>
  {/if}
  {#if noted.length && editable}
    <p class="faint small">
      The server checks the pattern {noted.join(', ')}, which the browser cannot compile.
    </p>
  {/if}
</div>

<style>
  .ff {
    display: grid;
    gap: 4px;
    padding: 6px 0;
    min-width: 0;
  }
  .ff-head {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 6px;
  }
  .ff-label {
    font-weight: 600;
    font-size: var(--fs-sm);
    color: var(--text);
  }
  .req {
    font-size: var(--fs-xs);
    color: var(--text-2);
    font-weight: 500;
  }
  .ro {
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
  .help {
    margin: 0;
    font-size: var(--fs-xs);
    color: var(--text-2);
  }
  .ff-set {
    border: 0;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .ff-set legend {
    padding: 0;
  }
  .choices {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
    align-items: center;
  }
  .choice {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    font-size: var(--fs-sm);
  }
  .values {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .value-row {
    display: flex;
    gap: 6px;
    align-items: center;
    min-width: 0;
  }
  .value-row .input,
  .value-row .select,
  .value-row .textarea {
    flex: 1 1 auto;
    min-width: 0;
  }
  .value-row .lang {
    flex: 0 0 76px;
  }
  .add {
    justify-self: start;
  }
  .ro-values {
    display: grid;
    gap: 2px;
    font-size: var(--fs-sm);
  }
  .ro-values:focus {
    outline: 2px solid var(--spark);
    outline-offset: 2px;
  }
  .problem {
    margin: 0;
    color: var(--danger);
  }
  .problem.server {
    color: var(--warn);
  }
  .invalid :global(.input),
  .invalid :global(.select),
  .invalid :global(.textarea) {
    border-color: color-mix(in srgb, var(--danger) 55%, var(--border));
  }
  .small {
    font-size: var(--fs-xs);
  }
</style>

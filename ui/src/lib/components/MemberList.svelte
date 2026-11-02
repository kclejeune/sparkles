<script lang="ts">
  // The ordered editor of a list field (SHACL 1.2 list constraints,
  // docs/specs/U02-shape-driven-forms/SPEC.md §6 Phase 3): the members of the field's
  // `rdf:List`, each in the widget of the member shape, with buttons that move, remove and
  // add members. A save replaces the stored list with these members in this order.
  import type { Term } from '$lib/api';
  import {
    entryOf,
    entryTerm,
    memberField,
    termText,
    type Entry,
    type FormField,
    type Problem,
  } from '$lib/forms';
  import { searchInstances } from '$lib/explore';
  import { displayTerm, type PrefixMap } from '$lib/rdf';
  import EntityPicker from './EntityPicker.svelte';
  import TermView from './TermView.svelte';

  let {
    field,
    entries = $bindable(),
    editable,
    base,
    problems = [],
    ds,
    prefixes,
    fullText = false,
    lang = 'en',
  }: {
    field: FormField;
    /** The members, in order. */
    entries: Entry[];
    editable: boolean;
    /** The DOM id of the first member's input. */
    base: string;
    /** The browser's problems; `index` is a member's position. */
    problems?: Problem[];
    ds: string;
    prefixes: PrefixMap;
    fullText?: boolean;
    /** The language a new member with a language takes. */
    lang?: string;
  } = $props();

  const member = $derived(memberField(field));
  const widget = $derived(field.constraints.list?.memberWidget ?? 'term');
  const max = $derived(field.constraints.list?.maxLength);
  const langs = $derived(member?.constraints.languageIn ?? []);
  const inputId = (i: number) => (i === 0 ? base : `${base}-m${i}`);
  const nameOf = (i: number) => `${field.label} member ${i + 1}`;
  const searchHits = (text: string, signal: AbortSignal) =>
    searchInstances(ds, text, { classes: member?.constraints.class ?? [], fullText, signal });

  function setText(i: number, text: string, langTag = entries[i].lang) {
    if (!member) return;
    // a date-time member keeps the offset it was loaded with
    const r = entryTerm(member, widget, text, langTag, prefixes, entries[i].zone);
    entries[i] = { ...entries[i], text, lang: langTag, term: r.term, error: r.error };
  }

  function setTerm(i: number, term: Term | null, label?: string) {
    entries[i] = { ...entries[i], term, text: term ? termText(term) : '', error: undefined, label };
  }

  function move(i: number, by: number) {
    const j = i + by;
    if (j < 0 || j >= entries.length) return;
    const next = [...entries];
    [next[i], next[j]] = [next[j], next[i]];
    entries = next;
    const dir = by < 0 ? 'up' : 'down';
    queueMicrotask(() => document.getElementById(`${inputId(j)}-${dir}`)?.focus());
  }

  function remove(i: number) {
    entries = entries.filter((_, j) => j !== i);
    queueMicrotask(() => document.getElementById(`${base}-add`)?.focus());
  }

  function add() {
    const e = entryOf(null, widget);
    if (widget === 'text-lang') e.lang = langs[0] ?? lang;
    entries = [...entries, e];
    queueMicrotask(() => document.getElementById(inputId(entries.length - 1))?.focus());
  }

  const memberProblems = (i: number) => problems.filter((p) => p.index === i);
</script>

<ol class="members" aria-label="{field.label} members">
  {#each entries as e, i (e.key)}
    <li class="member">
      <span class="pos faint" aria-hidden="true">{i + 1}</span>
      {#if !editable}
        {#if e.term}<TermView term={e.term} {prefixes} />{/if}
      {:else if widget === 'entity'}
        <EntityPicker
          id={inputId(i)}
          label={nameOf(i)}
          hideLabel
          {ds}
          {prefixes}
          invalid={memberProblems(i).length > 0}
          search={searchHits}
          bind:value={
            () =>
              e.term ? { term: e.term, label: e.label ?? displayTerm(e.term, prefixes) } : null,
            (v) => setTerm(i, v?.term ?? null, v?.label)
          }
        />
      {:else}
        <input
          class="input"
          class:mono={widget === 'term' || widget === 'iri'}
          id={inputId(i)}
          aria-label={nameOf(i)}
          aria-invalid={memberProblems(i).length > 0 || !!e.error || undefined}
          aria-describedby={memberProblems(i).length || e.error
            ? `${inputId(i)}-problems`
            : undefined}
          placeholder={widget === 'term' ? 'IRI, prefixed name or "literal"' : undefined}
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
              {#if !langs.includes(e.lang)}<option value={e.lang}>{e.lang || 'none'}</option>{/if}
              {#each langs as l (l)}<option value={l}>{l}</option>{/each}
            </select>
          {:else}
            <input
              class="input lang"
              aria-label="Language of {nameOf(i)}"
              placeholder="lang"
              value={e.lang}
              oninput={(ev) => setText(i, e.text, ev.currentTarget.value.trim())}
            />
          {/if}
        {/if}
      {/if}
      {#if editable}
        <span class="moves">
          <button
            type="button"
            class="btn sm ghost icon"
            id="{inputId(i)}-up"
            aria-label="Move {nameOf(i)} up"
            disabled={i === 0}
            onclick={() => move(i, -1)}>↑</button
          >
          <button
            type="button"
            class="btn sm ghost icon"
            id="{inputId(i)}-down"
            aria-label="Move {nameOf(i)} down"
            disabled={i === entries.length - 1}
            onclick={() => move(i, 1)}>↓</button
          >
          <button
            type="button"
            class="btn sm ghost icon"
            aria-label="Remove {nameOf(i)}"
            onclick={() => remove(i)}>×</button
          >
        </span>
      {/if}
      {#if e.error || memberProblems(i).length}
        <div class="member-problems" id="{inputId(i)}-problems">
          {#if e.error}<p class="problem small">{e.error}</p>{/if}
          {#each memberProblems(i) as p (p.message)}
            <p class="problem small"><span aria-hidden="true">⚠</span> {p.message}</p>
          {/each}
        </div>
      {/if}
    </li>
  {:else}
    <li class="faint small">No members</li>
  {/each}
</ol>
{#if editable}
  <button
    type="button"
    class="btn sm add"
    id="{base}-add"
    disabled={max !== undefined && entries.length >= max}
    title={max !== undefined && entries.length >= max ? `At most ${max} members` : undefined}
    onclick={add}>+ Add member</button
  >
{/if}

<style>
  .members {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
  }
  .member {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .member :global(.input) {
    flex: 1 1 12rem;
    min-width: 0;
  }
  .member .lang {
    flex: 0 0 5rem;
  }
  .pos {
    width: 1.5em;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .moves {
    display: inline-flex;
    gap: 2px;
  }
  .member-problems {
    flex-basis: 100%;
    padding-left: 2em;
  }
  .add {
    justify-self: start;
    margin-top: 4px;
  }
</style>

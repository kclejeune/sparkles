<script lang="ts">
  import { untrack } from 'svelte';
  // One definition of an entity in edit mode (ROADMAP item 630): the text in the lede's
  // type, its language and, on an OWL page, its sources. Preview change hands the edit to
  // the page, which previews and commits it as the editors do. A definition that is new
  // has no `before`, and Remove previews the removal of one that exists.
  import {
    definitionProperty,
    sourcesOf,
    statementKey,
    statementOp,
    textLit,
    valueParts,
    withPending,
    type EditMode,
    type OnEdit,
    type Stated,
  } from '$lib/browse-edit';
  import { validLang } from '$lib/ontology';
  import Icon from '$components/Icon.svelte';
  import SourceChips from './SourceChips.svelte';

  let {
    mode,
    iri,
    title,
    before,
    graph,
    lang: defaultLang = '',
    readonly,
    onedit,
    oncancel,
    autofocus = false,
  }: {
    mode: EditMode;
    iri: string;
    /** The entity's name, for the edits' labels. */
    title: string;
    before: Stated | null;
    graph: string | null;
    /** The language a new definition starts with. */
    lang?: string;
    readonly: boolean;
    onedit: OnEdit;
    /** Cancel on a new definition, which takes the field away. */
    oncancel?: () => void;
    autofocus?: boolean;
  } = $props();

  const uid = Math.random().toString(36).slice(2, 8);
  let text = $state('');
  let lang = $state('');
  let sources = $state<string[]>([]);
  let pending = $state('');
  let shownKey: string | null = null;
  /** Set when this form's change commits, so the reload that follows starts it over. */
  let committed = false;
  let area: HTMLTextAreaElement | undefined = $state();

  function reset() {
    const p = before ? valueParts(before.value) : { text: '', lang: defaultLang };
    text = p.text;
    lang = p.lang;
    sources = sourcesOf(before);
    pending = '';
    base = fields();
  }
  /** The fields as the form last showed the definition, so a typed draft is told apart. */
  let base = '';
  const fields = () => JSON.stringify([text, lang, sources, pending]);
  // the commit of this form starts it over, while another writer's change to the same
  // definition keeps a draft the caller has typed, which then replaces the new value
  $effect(() => {
    const k = before ? statementKey(before) : '';
    if (k === shownKey) return;
    const keep = shownKey !== null && !committed && untrack(fields) !== base;
    shownKey = k;
    committed = false;
    if (!keep) untrack(reset);
  });
  $effect(() => {
    if (autofocus && area) area.focus();
  });

  const original = $derived(
    before
      ? { ...valueParts(before.value), sources: sourcesOf(before) }
      : { text: '', lang: defaultLang, sources: [] as string[] },
  );
  const dirty = $derived(
    text !== original.text ||
      lang.trim().toLowerCase() !== original.lang.toLowerCase() ||
      withPending(sources, pending).join('\n') !== original.sources.join('\n'),
  );
  const problem = $derived(!validLang(lang.trim()) ? 'The language tag is not valid.' : null);

  function preview() {
    if (!dirty || problem) return;
    const op = statementOp(
      mode,
      iri,
      before,
      text.trim()
        ? {
            property: before?.property ?? definitionProperty(mode),
            value: textLit(text.trim(), lang),
            sources: withPending(sources, pending),
          }
        : null,
      graph,
    );
    if (op)
      onedit(
        op,
        !text.trim()
          ? `Remove the definition of ${title}`
          : before
            ? `Edit the definition of ${title}`
            : `Add a definition to ${title}`,
        () => (committed = true),
      );
  }
</script>

<div
  class="def"
  class:dirty
  data-edit-form
  data-dirty={dirty ? 'true' : 'false'}
  role="group"
  aria-label="Definition{lang ? ` (${lang})` : ''}"
>
  <label class="sr-only" for="def-{uid}">Definition</label>
  <textarea
    id="def-{uid}"
    class="lede-field"
    rows="3"
    bind:this={area}
    bind:value={text}
    disabled={readonly}
    placeholder="Write a definition of {title}"></textarea>
  <div class="foot">
    <label class="f"
      ><span>Language</span><input
        class="input sm lang"
        bind:value={lang}
        disabled={readonly}
        aria-invalid={!!problem}
        size="5"
      /></label
    >
    {#if mode === 'owl'}
      <span class="f"
        ><span>Sources</span><SourceChips
          bind:sources
          bind:pending
          label="the definition"
          disabled={readonly}
        /></span
      >
    {/if}
    <span class="acts">
      {#if before && !readonly}
        <button
          type="button"
          class="btn sm ghost"
          onclick={() => {
            const op = statementOp(mode, iri, before, null, graph);
            if (op) onedit(op, `Remove the definition of ${title}`);
          }}><Icon name="trash" size={14} />Remove</button
        >
      {/if}
      {#if dirty || oncancel}
        <button
          type="button"
          class="btn sm"
          onclick={() => {
            reset();
            oncancel?.();
          }}>Cancel</button
        >
      {/if}
      <button
        type="button"
        class="btn sm primary"
        data-preview
        disabled={readonly || !dirty || !!problem}
        onclick={preview}>Preview change</button
      >
    </span>
  </div>
  {#if problem}<p class="problem" role="alert">{problem}</p>{/if}
</div>

<style>
  .def {
    display: grid;
    gap: var(--sp-2);
    max-width: 68ch;
    min-width: 0;
  }
  .lede-field {
    width: 100%;
    display: block;
    font: inherit;
    font-size: 15px;
    line-height: 1.6;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--r);
    padding: 8px 10px;
    resize: vertical;
    min-height: 5.4em;
  }
  .dirty .lede-field {
    border-color: var(--spark);
  }
  .foot {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
    min-width: 0;
  }
  .f {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-xs);
    color: var(--text-3);
    min-width: 0;
    flex-wrap: wrap;
  }
  .lang {
    width: 4.5em;
  }
  .acts {
    margin-left: auto;
    display: flex;
    gap: 6px;
  }
  .problem {
    color: var(--danger);
    font-size: var(--fs-sm);
    margin: 0;
  }
  @container browse (max-width: 639px) {
    .lede-field {
      font-size: 14px;
    }
    .acts {
      width: 100%;
      margin-left: 0;
    }
    .acts .btn {
      flex: 1;
      justify-content: center;
      height: 40px;
    }
  }
</style>

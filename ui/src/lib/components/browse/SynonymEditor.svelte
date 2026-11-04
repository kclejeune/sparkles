<script lang="ts">
  import { untrack } from 'svelte';
  // The synonyms of an OWL entity in edit mode (ROADMAP item 630): each a row of its kind,
  // text, language and sources, with Edit and Remove, and an add row. Changing a kind or a
  // text is one change that removes the old annotation assertion and adds the new one with
  // the same sources. Synonyms that another graph states are listed without actions.
  import { isLiteral } from '$lib/browse';
  import {
    inGraph,
    OWL_SYNONYMS,
    sourcesOf,
    statementKey,
    statementOp,
    textLit,
    valueParts,
    withPending,
    type OnEdit,
    type Stated,
  } from '$lib/browse-edit';
  import { validLang } from '$lib/ontology';
  import Icon from '$components/Icon.svelte';
  import SourceChips from './SourceChips.svelte';

  let {
    iri,
    title,
    synonyms,
    kinds,
    graph,
    lang: defaultLang = 'en',
    readonly,
    onedit,
  }: {
    iri: string;
    title: string;
    /** The synonym statements of the page, of every graph it reads. */
    synonyms: Stated[];
    /** Each synonym property with its kind's name. */
    kinds: [string, string][];
    graph: string | null;
    lang?: string;
    readonly: boolean;
    onedit: OnEdit;
  } = $props();

  const kindName = (p: string) => kinds.find(([k]) => k === p)?.[1] ?? p;
  /** The kinds the add and edit rows offer: the OBO ones, and any other the page has. */
  const offered = $derived([
    ...OWL_SYNONYMS,
    ...kinds.filter(
      ([p]) => !OWL_SYNONYMS.some(([q]) => q === p) && synonyms.some((s) => s.property === p),
    ),
  ]);
  const rows = $derived(
    [...synonyms].sort(
      (a, b) =>
        kinds.findIndex(([p]) => p === a.property) - kinds.findIndex(([p]) => p === b.property),
    ),
  );

  // --- the row being edited ---------------------------------------------------------------
  let editing = $state<string | null>(null);
  let eKind = $state('');
  let eText = $state('');
  let eLang = $state('');
  let eSources = $state<string[]>([]);
  let ePending = $state('');
  /** The synonym as editing began, kept when another writer's commit changes or removes it. */
  let editBase = $state<Stated | null>(null);
  function begin(s: Stated) {
    editing = statementKey(s);
    editBase = s;
    ePending = '';
    const p = valueParts(s.value);
    eKind = s.property;
    eText = p.text;
    eLang = p.lang;
    eSources = sourcesOf(s);
  }
  const live = $derived(
    editing ? (synonyms.find((s) => statementKey(s) === editing) ?? null) : null,
  );
  /**
   * Whether another writer's commit changed or removed the synonym being edited. The row
   * then stays open above the list with the whole draft, and its preview adds the draft
   * with the annotations the synonym had, beside whatever the other writer left.
   */
  const gone = $derived(!!editing && !live);
  const editRow = $derived(live ?? (editing ? editBase : null));
  const eDirty = $derived.by(() => {
    if (!editRow) return false;
    if (gone) return true;
    const p = valueParts(editRow.value);
    return (
      eKind !== editRow.property ||
      eText.trim() !== p.text ||
      eLang.trim().toLowerCase() !== p.lang.toLowerCase() ||
      withPending(eSources, ePending).join('\n') !== sourcesOf(editRow).join('\n')
    );
  });
  function save() {
    if (!editRow || !eDirty || !eText.trim() || !validLang(eLang.trim())) return;
    const draft = {
      property: eKind,
      value: textLit(eText.trim(), eLang),
      sources: withPending(eSources, ePending),
    };
    const op = gone
      ? statementOp('owl', iri, null, draft, graph, editBase)
      : statementOp('owl', iri, editRow, draft, graph);
    const was = valueParts(editRow.value).text;
    if (op)
      onedit(
        op,
        gone
          ? `Add the synonym ${eText.trim()} to ${title}`
          : `Change the synonym ${was} of ${title}`,
        () => {
          editing = null;
          editBase = null;
        },
      );
  }

  // --- the add row --------------------------------------------------------------------------
  let aKind = $state(OWL_SYNONYMS[0][0]);
  let aText = $state('');
  // the page's language fills the new synonym's language until the caller edits the field,
  // so clearing it adds an untagged synonym
  let aLang = $state('');
  let aTouched = false;
  $effect(() => {
    const l = defaultLang;
    if (!untrack(() => aTouched)) aLang = l;
  });
  const aProblem = $derived(!validLang(aLang.trim()) ? 'The language tag is not valid.' : null);
  function add() {
    const text = aText.trim();
    if (!text || aProblem) return;
    const op = statementOp(
      'owl',
      iri,
      null,
      { property: aKind, value: textLit(text, aLang), sources: [] },
      graph,
    );
    if (op)
      onedit(op, `Add the ${kindName(aKind).toLowerCase()} synonym ${text} to ${title}`, () => {
        aText = '';
        aTouched = false;
        aLang = defaultLang;
      });
  }
</script>

<section class="term-sec" aria-labelledby="syn-edit-h">
  <h2 id="syn-edit-h">
    Synonyms {#if synonyms.length}<span class="count-badge">{synonyms.length}</span>{/if}
  </h2>
  {#if gone && editBase}
    <ul class="erows">
      {@render editForm(editBase)}
    </ul>
    <p class="gone-note" role="status">
      Another commit changed the synonym {valueParts(editBase.value).text}. Preview adds your
      version beside what is there now.
    </p>
  {/if}
  {#if rows.length}
    <ul class="erows">
      {#each rows as s (statementKey(s))}
        {@const own = inGraph(s, graph)}
        {#if editing === statementKey(s)}
          {@render editForm(s)}
        {:else}
          <li class="erow" class:ro={!own}>
            <span class="k">{kindName(s.property)}</span>
            <span class="v"
              >{valueParts(s.value).text}{#if isLiteral(s.value) && s.value['@language']}<span
                  class="lang-tag">{s.value['@language']}</span
                >{/if}{#if sourcesOf(s).length}<span class="src">{sourcesOf(s).join(', ')}</span
                >{/if}{#if !own}<span class="from">from {s.graph}, not editable here</span
                >{/if}</span
            >
            <span class="acts">
              {#if own && !readonly}
                <button
                  type="button"
                  class="btn icon sm ghost"
                  aria-label="Edit the synonym {valueParts(s.value).text}"
                  title="Edit"
                  onclick={() => begin(s)}><Icon name="pencil" size={14} /></button
                >
                <button
                  type="button"
                  class="btn icon sm ghost"
                  aria-label="Remove the synonym {valueParts(s.value).text}"
                  title="Remove"
                  onclick={() => {
                    const op = statementOp('owl', iri, s, null, graph);
                    if (op)
                      onedit(op, `Remove the synonym ${valueParts(s.value).text} of ${title}`);
                  }}><Icon name="trash" size={14} /></button
                >
              {/if}
            </span>
          </li>
        {/if}
      {/each}
    </ul>
  {/if}
  {#if !readonly}
    <form
      class="addrow"
      data-edit-form
      data-dirty={aText.trim() ? 'true' : 'false'}
      aria-label="Add a synonym"
      onsubmit={(e) => {
        e.preventDefault();
        add();
      }}
    >
      <select class="select sm kind" bind:value={aKind} aria-label="Kind of the new synonym">
        {#each OWL_SYNONYMS as [p, k] (p)}<option value={p}>{k}</option>{/each}
      </select>
      <input
        id="browse-add-synonym"
        class="input sm txt"
        bind:value={aText}
        placeholder="Add a synonym"
        aria-label="New synonym"
      />
      <input
        class="input sm lng"
        bind:value={aLang}
        oninput={() => (aTouched = true)}
        aria-label="Language of the new synonym"
        aria-invalid={!!aProblem}
      />
      <button class="btn sm" type="submit" data-preview disabled={!aText.trim() || !!aProblem}
        ><Icon name="plus" size={14} />Add</button
      >
    </form>
  {/if}
</section>

{#snippet editForm(s: Stated)}
  <li
    class="erow editing"
    data-edit-form
    data-dirty={eDirty ? 'true' : 'false'}
    aria-label="Edit the synonym {valueParts(s.value).text}"
  >
    <select class="select sm" bind:value={eKind} aria-label="Kind">
      {#each offered as [p, k] (p)}<option value={p}>{k}</option>{/each}
    </select>
    <input class="input sm txt" bind:value={eText} aria-label="Synonym" />
    <input
      class="input sm lng"
      bind:value={eLang}
      aria-label="Language"
      aria-invalid={!validLang(eLang.trim())}
    />
    <span class="srcs"
      ><SourceChips bind:sources={eSources} bind:pending={ePending} label="the synonym" /></span
    >
    <span class="row-acts">
      <button type="button" class="btn sm" onclick={() => (editing = null)}>Cancel</button>
      <button
        type="button"
        class="btn sm primary"
        data-preview
        disabled={!eDirty || !eText.trim() || !validLang(eLang.trim())}
        onclick={save}>Preview change</button
      >
    </span>
  </li>
{/snippet}

<style>
  .erows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .erow {
    display: grid;
    grid-template-columns: 104px minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--sp-2);
    min-height: 34px;
    padding: 2px 4px 2px 8px;
    border-radius: var(--r);
  }
  .erow:hover,
  .erow:focus-within {
    background: var(--hover);
  }
  .erow.editing {
    grid-template-columns: 104px minmax(0, 1fr) 4.5em;
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
    padding: var(--sp-2);
  }
  .erow.editing .srcs {
    grid-column: 1 / -1;
  }
  .erow.editing .row-acts {
    grid-column: 1 / -1;
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
  .gone-note {
    margin: 4px 0 var(--sp-2);
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .k {
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .v {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .ro .v {
    color: var(--text-2);
  }
  .src {
    font-size: var(--fs-sm);
    color: var(--text-3);
    margin-left: 6px;
  }
  .from {
    font-size: var(--fs-xs);
    color: var(--text-3);
    margin-left: 6px;
  }
  .acts {
    display: flex;
    gap: 2px;
  }
  .addrow {
    display: grid;
    grid-template-columns: 104px minmax(0, 1fr) 4.5em auto;
    gap: var(--sp-2);
    align-items: center;
    margin-top: var(--sp-2);
    padding: 6px 6px 6px 8px;
    border: 1px dashed var(--border-strong);
    border-radius: var(--r);
  }
  .select,
  .input {
    width: 100%;
  }
  @container browse (max-width: 639px) {
    .erow {
      grid-template-columns: minmax(0, 1fr) auto;
    }
    .erow .k {
      grid-column: 1 / -1;
      font-size: var(--fs-xs);
    }
    .erow.editing {
      grid-template-columns: minmax(0, 1fr) 4.5em;
    }
    .erow.editing .select,
    .erow.editing .txt {
      grid-column: 1 / -1;
    }
    .acts .btn {
      width: 36px;
      height: 36px;
    }
    .addrow {
      grid-template-columns: minmax(0, 1fr) 4.5em;
      grid-template-areas: 'kind lng' 'txt txt' 'btn btn';
    }
    .addrow .kind {
      grid-area: kind;
    }
    .addrow .txt {
      grid-area: txt;
    }
    .addrow .lng {
      grid-area: lng;
    }
    .addrow .btn {
      grid-area: btn;
      justify-content: center;
      height: 40px;
    }
  }
</style>

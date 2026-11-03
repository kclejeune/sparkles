<script lang="ts">
  // The labels of one SKOS resource in the thesaurus editor (docs/specs/
  // U12-ontology-workbench/SPEC.md, "SKOS and SKOS-XL thesaurus editor"): its preferred,
  // alternative and hidden labels by language, plain and SKOS-XL, with the S13 and S14
  // clashes among them. Each change is an edit the page previews: add, change and remove a
  // label, make one preferred, and convert between a plain label and a SKOS-XL label.
  import { onDestroy } from 'svelte';
  import type { Term } from '$lib/api';
  import { validLang } from '$lib/ontology';
  import {
    addProblem,
    ambiguousXl,
    fromXlEdit,
    KIND_NAME,
    LABEL_KINDS,
    labelEdit,
    labelProblems,
    makePreferredEdit,
    normLang,
    preferredResult,
    sameEntry,
    sameLabel,
    toXlEdit,
    xlFormEdit,
    xlLabelIri,
    type Label,
    type LabelEntry,
    type LabelKind,
  } from '$lib/skos';
  import Icon from './Icon.svelte';

  let {
    iri,
    name,
    entries,
    graph,
    langs,
    readonly,
    onedit,
    isUsed = async () => false,
    scope = '',
  }: {
    iri: string;
    /** The resource's name, for the edits' labels. */
    name: string;
    entries: LabelEntry[];
    graph: string | null;
    /** The language tags the thesaurus uses, offered first. */
    langs: string[];
    readonly: boolean;
    /** Previews an edit; `false` means the caller did not take it, so the form keeps its text. */
    onedit: (sparql: string, label: string) => void | boolean;
    /** Whether the graph already uses an IRI, so a minted SKOS-XL label IRI is new. */
    isUsed?: (iri: string) => Promise<boolean>;
    /** What the page edits (target and graph): an edit read for another one is dropped. */
    scope?: string;
  } = $props();

  // a conversion that waits for the graph's answer opens its edit only if the component
  // still shows the resource it started on, in the same target and graph
  let alive = true;
  onDestroy(() => (alive = false));
  const still = (at: { iri: string; scope: string }) =>
    alive && at.iri === iri && at.scope === scope;

  const problems = $derived(labelProblems(entries));
  const clashing = (e: LabelEntry) =>
    problems.some((p) => p.labels.some((x) => x.kind === e.kind && sameLabel(x.label, e.label)));
  const byLang = $derived.by(() => {
    const m = new Map<string, LabelEntry[]>();
    for (const e of entries) m.set(e.label.lang, [...(m.get(e.label.lang) ?? []), e]);
    return [...m].sort(([a], [b]) => (a === '' ? 1 : b === '' ? -1 : a.localeCompare(b)));
  });
  const shown = (l: Label) => `“${l.value}”${l.lang ? `@${l.lang}` : ''}`;
  const xlTaken = $derived(
    new Set(entries.flatMap((e) => (e.xl?.type === 'uri' ? [e.xl.value] : []))),
  );

  // --- adding ---------------------------------------------------------------------------
  let kind = $state<LabelKind>('pref');
  let text = $state('');
  let lang = $state('');
  $effect(() => {
    if (!lang && langs.length) lang = langs[0];
  });
  const draft = $derived<Label>({ value: text.trim(), lang: normLang(lang) });
  const draftProblem = $derived(
    !draft.value
      ? null
      : !validLang(draft.lang)
        ? 'The language tag is not valid.'
        : addProblem(entries, kind, draft),
  );
  /**
   * The plain preferred label the new one would replace, for the S14 case. The draft's plain
   * alternative and hidden copies go with it, and the shortcut is offered only when the
   * resulting labels keep the rules and the draft is no SKOS-XL label already.
   */
  const replaces = $derived.by(() => {
    if (kind !== 'pref' || !draft.value || !validLang(draft.lang)) return undefined;
    const old = entries.find(
      (e) =>
        e.kind === 'pref' &&
        e.xl === null &&
        e.label.lang === draft.lang &&
        !sameLabel(e.label, draft),
    );
    if (!old) return undefined;
    if (entries.some((e) => e.xl && sameLabel(e.label, draft))) return undefined;
    const after: LabelEntry[] = [
      ...entries.filter((e) => !sameEntry(e, old) && !(e.xl === null && sameLabel(e.label, draft))),
      { kind: 'pref', label: draft, xl: null },
      { kind: 'alt', label: old.label, xl: null },
    ];
    return labelProblems(after).length ? undefined : old;
  });

  function add() {
    if (!draft.value || draftProblem) return;
    const took = onedit(
      labelEdit(iri, [], [{ kind, label: draft }], graph),
      `Add the ${KIND_NAME[kind].toLowerCase()} label ${shown(draft)} to ${name}`,
    );
    if (took !== false) text = '';
  }

  function replacePreferred() {
    if (!replaces) return;
    const copies = entries.filter(
      (e) => e.xl === null && e.kind !== 'pref' && sameLabel(e.label, draft),
    );
    const took = onedit(
      labelEdit(
        iri,
        [
          { kind: 'pref', label: replaces.label },
          ...copies.map((e) => ({ kind: e.kind, label: e.label })),
        ],
        [
          { kind: 'pref', label: draft },
          { kind: 'alt', label: replaces.label },
        ],
        graph,
      ),
      `Make ${shown(draft)} the preferred label of ${name}`,
    );
    if (took !== false) text = '';
  }

  // --- changing one label -----------------------------------------------------------------
  let editing = $state<LabelEntry | null>(null);
  let editText = $state('');
  let editLang = $state('');
  function startEdit(e: LabelEntry) {
    editing = e;
    editText = e.label.value;
    editLang = e.label.lang;
  }
  const editDraft = $derived<Label>({ value: editText.trim(), lang: normLang(editLang) });
  const editProblem = $derived.by(() => {
    if (!editing || !editDraft.value) return null;
    if (!validLang(editDraft.lang)) return 'The language tag is not valid.';
    const cur = editing;
    const rest = entries.filter((e) => !sameEntry(e, cur));
    return addProblem(rest, cur.kind, editDraft);
  });
  function saveEdit() {
    const e = editing;
    if (!e || !editDraft.value || editProblem || sameLabel(e.label, editDraft)) {
      editing = null;
      return;
    }
    const what = `Change the ${KIND_NAME[e.kind].toLowerCase()} label ${shown(e.label)} of ${name} to ${shown(editDraft)}`;
    const took = onedit(
      e.xl
        ? xlFormEdit(iri, e.kind, e.xl, e.label, editDraft, graph)
        : labelEdit(
            iri,
            [{ kind: e.kind, label: e.label }],
            [{ kind: e.kind, label: editDraft }],
            graph,
          ),
      what,
    );
    if (took !== false) editing = null;
  }

  function remove(e: LabelEntry) {
    const what = `Remove the ${KIND_NAME[e.kind].toLowerCase()} label ${shown(e.label)} of ${name}`;
    onedit(
      e.xl
        ? fromXlEdit(iri, e.kind, e.xl, e.label, null, graph)
        : labelEdit(iri, [{ kind: e.kind, label: e.label }], [], graph),
      what,
    );
  }

  function makePreferred(e: LabelEntry) {
    onedit(
      makePreferredEdit(iri, e, entries, graph),
      `Make ${shown(e.label)} the preferred label of ${name}`,
    );
  }

  /** A SKOS-XL label IRI that neither this resource nor the rest of the graph uses. */
  async function mint(e: LabelEntry, taken: Set<string>): Promise<string> {
    for (;;) {
      const node = xlLabelIri(iri, e.kind, e.label, taken);
      if (!(await isUsed(node))) return node;
      taken.add(node);
    }
  }

  async function toXl(e: LabelEntry) {
    const at = { iri, scope };
    const node = await mint(e, new Set(xlTaken));
    if (!still(at)) return;
    onedit(
      toXlEdit(iri, e.kind, e.label, node, graph),
      `Make ${shown(e.label)} a SKOS-XL label of ${name}`,
    );
  }

  function toPlain(e: LabelEntry & { xl: Term }) {
    onedit(
      fromXlEdit(iri, e.kind, e.xl, e.label, e.label, graph),
      `Make the SKOS-XL label ${shown(e.label)} of ${name} a plain label`,
    );
  }

  async function allToXl() {
    const plain = entries.filter((e) => !e.xl);
    if (!plain.length) return;
    const at = { iri, scope };
    const taken = new Set(xlTaken);
    const parts: string[] = [];
    for (const e of plain) {
      const node = await mint(e, taken);
      taken.add(node);
      parts.push(toXlEdit(iri, e.kind, e.label, node, graph));
    }
    if (!still(at)) return;
    onedit(parts.join(' ;\n'), `Make the ${plain.length} plain labels of ${name} SKOS-XL labels`);
  }
</script>

<section class="labels" aria-label="Labels">
  {#if problems.length}
    <ul class="problems" aria-label="Label problems">
      {#each problems as p, i (i)}
        <li class="problem" role="alert"><strong>{p.rule}</strong> {p.message}</li>
      {/each}
    </ul>
  {/if}

  {#if !entries.length}
    <p class="empty">No labels.</p>
  {:else}
    <table class="table">
      <thead>
        <tr>
          <th>Language</th>
          {#each LABEL_KINDS as k (k)}<th>{KIND_NAME[k]}</th>{/each}
        </tr>
      </thead>
      <tbody>
        {#each byLang as [l, es] (l)}
          <tr data-lang={l || 'none'}>
            <th class="mono">{l || 'none'}</th>
            {#each LABEL_KINDS as k (k)}
              <td>
                {#each es.filter((e) => e.kind === k) as e, i (`${e.label.value}|${i}|${e.xl ? 'x' : 'p'}`)}
                  {#if editing === e}
                    <form
                      class="edit"
                      aria-label="Change label"
                      data-edit-form
                      data-dirty={sameLabel(e.label, editDraft) ? 'false' : 'true'}
                      onsubmit={(ev) => {
                        ev.preventDefault();
                        saveEdit();
                      }}
                    >
                      <!-- svelte-ignore a11y_autofocus -->
                      <input class="input sm" bind:value={editText} aria-label="Text" autofocus />
                      <input class="input sm lang" bind:value={editLang} aria-label="Language" />
                      <button class="btn sm primary" data-preview disabled={!!editProblem}
                        >Preview</button
                      >
                      <button class="btn sm ghost" type="button" onclick={() => (editing = null)}
                        >Cancel</button
                      >
                      {#if editProblem}<span class="problem small">{editProblem}</span>{/if}
                    </form>
                  {:else}
                    <div class="chip" class:clash={clashing(e)} data-kind={e.kind}>
                      <span class="text">{e.label.value}</span>
                      {#if e.xl}<span
                          class="badge"
                          title={e.xl.type === 'uri'
                            ? `SKOS-XL label ${e.xl.value}`
                            : 'SKOS-XL label (blank node)'}>XL</span
                        >{/if}
                      {#if !readonly && ambiguousXl(entries, e)}
                        <span
                          class="faint xs"
                          title="Another SKOS-XL label of this kind is a blank node with the same literal form, and an edit cannot tell the two apart. Edit them in the query editor."
                          >twin</span
                        >
                      {:else if !readonly}
                        <span class="acts">
                          {#if k !== 'pref'}
                            <button
                              class="btn ghost icon xs"
                              title="Make preferred"
                              aria-label="Make {e.label.value} preferred"
                              onclick={() => makePreferred(e)}
                              disabled={labelProblems(preferredResult(e, entries)).length >
                                problems.length}><Icon name="check" size={11} /></button
                            >
                          {/if}
                          {#if e.xl}
                            <button
                              class="btn ghost xs"
                              title="Make it a plain label"
                              aria-label="Make {e.label.value} a plain label"
                              onclick={() => toPlain(e as LabelEntry & { xl: Term })}>plain</button
                            >
                          {:else}
                            <button
                              class="btn ghost xs"
                              title="Make it a SKOS-XL label"
                              aria-label="Make {e.label.value} a SKOS-XL label"
                              onclick={() => toXl(e)}>XL</button
                            >
                          {/if}
                          <button
                            class="btn ghost icon xs"
                            title="Change"
                            aria-label="Change {e.label.value}"
                            onclick={() => startEdit(e)}><Icon name="pencil" size={11} /></button
                          >
                          <button
                            class="btn ghost icon xs"
                            title="Remove"
                            aria-label="Remove {e.label.value}"
                            onclick={() => remove(e)}><Icon name="trash" size={11} /></button
                          >
                        </span>
                      {/if}
                    </div>
                  {/if}
                {/each}
              </td>
            {/each}
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  {#if !readonly}
    <form
      class="add"
      aria-label="Add a label"
      data-edit-form
      data-dirty={text.trim() ? 'true' : 'false'}
      onsubmit={(e) => {
        e.preventDefault();
        add();
      }}
    >
      <select class="select sm" bind:value={kind} aria-label="Kind">
        {#each LABEL_KINDS as k (k)}<option value={k}>{KIND_NAME[k]}</option>{/each}
      </select>
      <input class="input sm grow" bind:value={text} placeholder="Label" aria-label="Label" />
      <input
        class="input sm lang"
        bind:value={lang}
        list="skos-langs"
        placeholder="en"
        aria-label="Language"
        aria-invalid={!validLang(normLang(lang))}
      />
      <datalist id="skos-langs">
        {#each langs as l (l)}<option value={l}></option>{/each}
      </datalist>
      <button class="btn sm primary" data-preview disabled={!draft.value || !!draftProblem}
        >Add</button
      >
      {#if entries.some((e) => !e.xl)}
        <button class="btn sm" type="button" onclick={allToXl}>Make all SKOS-XL</button>
      {/if}
    </form>
    {#if draftProblem}
      <p class="problem small" role="alert">
        {draftProblem}
        {#if replaces && draftProblem.startsWith('S14')}
          <button class="btn sm" type="button" onclick={replacePreferred}
            >Make it preferred and keep {shown(replaces.label)} as alternative</button
          >
        {/if}
      </p>
    {/if}
  {/if}
</section>

<style>
  .labels {
    display: grid;
    gap: 10px;
    min-width: 0;
  }
  .table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--fs-sm);
  }
  .table th,
  .table td {
    text-align: left;
    vertical-align: top;
    padding: 4px 6px;
    border-bottom: 1px solid var(--border);
  }
  .table thead th {
    color: var(--text-2);
    font-weight: 600;
  }
  .chip {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-wrap: wrap;
    padding: 1px 0;
    overflow-wrap: anywhere;
  }
  .chip.clash .text {
    color: var(--danger);
    text-decoration: underline wavy;
  }
  .acts {
    display: inline-flex;
    gap: 1px;
    opacity: 0.55;
  }
  .chip:hover .acts,
  .chip:focus-within .acts {
    opacity: 1;
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
  .add,
  .edit {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: center;
  }
  .grow {
    flex: 1 1 10em;
    min-width: 0;
  }
  .lang {
    width: 6em;
  }
  .problems {
    list-style: none;
    margin: 0;
    padding: 6px 10px;
    border-radius: var(--r);
    background: var(--removed-soft);
    display: grid;
    gap: 2px;
    font-size: var(--fs-sm);
  }
  .problem {
    color: var(--danger);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .empty {
    color: var(--text-3);
  }
</style>

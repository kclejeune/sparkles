<script lang="ts">
  // The frame of one entity in the ontology editor (docs/specs/U12-ontology-workbench/
  // SPEC.md): its annotations, its characteristics and its axioms by section, as Protégé's
  // and WebProtégé's frames show them, with class expressions in Manchester syntax. The add
  // fields take an entity name, an IRI, or a class expression or data range in Manchester or
  // functional-style syntax, and the last field takes a whole axiom in either syntax. The
  // server reads Manchester text, and a problem it reports is underlined. Every change is
  // handed to the page as an operation, which previews it and commits it.
  import {
    annotationAssertion,
    buildFrame,
    characteristicsOf,
    diffAxioms,
    KIND_SINGULAR,
    literalOf,
    manchester,
    OBJECT_CHARACTERISTICS,
    propertyAssertion,
    isFunctional,
    resolveInput,
    resolveName,
    sectionAxiom,
    sectionTarget,
    validLang,
    withAnnotations,
    type Annotation,
    type AxiomItem,
    type AxiomJson,
    type EntityKind,
    type Op,
    type ManchesterProblem,
    type ParseAs,
    type Parsed,
  } from '$lib/ontology';
  import type { NameOption } from '$lib/manchester-complete';
  import type { PrefixMap } from '$lib/rdf';
  import AnnotationRows from './AnnotationRows.svelte';
  import Icon from './Icon.svelte';
  import ManchesterEditor from './ManchesterEditor.svelte';
  import { onDestroy, untrack } from 'svelte';

  let {
    kind,
    iri,
    items,
    name,
    prefixes,
    options,
    annotationProperties,
    readonly: parentReadonly = false,
    source,
    commit,
    names,
    search,
    parse,
    onedit,
    onopen,
  }: {
    kind: EntityKind;
    iri: string;
    items: AxiomItem[];
    name: (iri: string) => string;
    prefixes: PrefixMap;
    /** The entities of a kind, for the pickers of the add forms. */
    options: (kind: EntityKind) => { iri: string; name: string }[];
    annotationProperties: string[];
    readonly?: boolean;
    /** Exact target/selection/entity/commit identity of these asserted items. */
    source: string;
    commit: number | null;
    /** The entity names that Manchester fields complete. */
    names: () => NameOption[];
    /** Names found by the server as they are typed, for an ontology too large to list. */
    search?: (word: string, signal: AbortSignal) => Promise<NameOption[]>;
    /** Read Manchester text on the server. */
    parse: (
      text: string,
      as: ParseAs,
    ) => Promise<{ ok: true; parsed: Parsed } | { ok: false; problem: ManchesterProblem }>;
    onedit: (op: Op, label: string, source: string) => void;
    onopen: (iri: string) => void;
  } = $props();

  let acceptedSource = $state(untrack(() => source));
  let acceptedCommit = $state(untrack(() => commit));
  let acceptedItems = $state(untrack(() => items));
  let annotationDrafts = $state(new Set<string>());
  const frame = $derived(buildFrame(kind, iri, acceptedItems));
  const readonly = $derived(parentReadonly || source !== acceptedSource);
  const label = $derived(name(iri));
  const chars = $derived(characteristicsOf(iri, acceptedItems));
  /** The axiom rows whose annotations are open, by section and row. */
  let openAnn = $state<Set<string>>(new Set());
  /** The add form of each section: its text and its problem. */
  let adding = $state<Record<string, string>>({});
  /** The problem of each add form, with the span of Manchester text the server reports. */
  let problems = $state<Record<string, Problem | null>>({});
  type Problem = { message: string; start?: number; end?: number; candidates?: string[] };
  let busy = $state<string | null>(null);
  // a property assertion of an individual
  let assertProp = $state('');
  let assertValue = $state('');
  let assertLang = $state('');
  const uid = Math.random().toString(36).slice(2, 8);
  let alive = true;
  let parsing = 0;
  const drafting = $derived(
    annotationDrafts.size > 0 ||
      Object.values(adding).some((s) => !!s.trim()) ||
      !!assertProp.trim() ||
      !!assertValue.trim() ||
      busy !== null,
  );
  $effect(() => {
    if (source === acceptedSource || !drafting) {
      acceptedItems = items;
      acceptedSource = source;
      acceptedCommit = commit;
    }
  });
  onDestroy(() => {
    alive = false;
    parsing++;
  });
  function annotationDraft(id: string, active: boolean) {
    if (annotationDrafts.has(id) === active) return;
    const next = new Set(annotationDrafts);
    if (active) next.add(id);
    else next.delete(id);
    annotationDrafts = next;
  }
  function discardDraft() {
    parsing++;
    adding = {};
    problems = {};
    assertProp = assertValue = assertLang = '';
    annotationDrafts = new Set();
    openAnn = new Set();
    busy = null;
    acceptedItems = items;
    acceptedSource = source;
    acceptedCommit = commit;
  }
  function dispatch(op: Op, title: string, origin = acceptedSource) {
    if (!alive || readonly || origin !== source || origin !== acceptedSource) return;
    onedit(op, title, origin);
  }

  const show = (x: unknown) => manchester(x, name, prefixes);

  function toggleAnn(id: string) {
    const s = new Set(openAnn);
    if (s.has(id)) s.delete(id);
    else s.add(id);
    openAnn = s;
  }

  function entityAnnotations(next: Annotation[], what: string) {
    const before = frame.annotations.map((a) => a.axiom);
    const after = next.map((n) => ({
      ...annotationAssertion(n.property, iri, n.value),
      ...(n.annotations?.length ? { annotations: n.annotations } : {}),
    }));
    const d = diffAxioms(before, after);
    if (d.add.length || d.remove.length)
      dispatch({ kind: 'axioms', add: d.add, remove: d.remove }, what);
  }

  function axiomAnnotations(a: AxiomJson, next: Annotation[], what: string) {
    dispatch({ kind: 'axioms', remove: [a], add: [withAnnotations(a, next)] }, what);
  }

  function removeAxiom(it: AxiomItem, what: string) {
    dispatch({ kind: 'axioms', add: [], remove: [it.axiom] }, what);
  }

  function toggleCharacteristic(type: string, on: boolean) {
    const ax = { type, property: iri };
    const word = type.replace(/(Object|Data)Property$/, '').toLowerCase();
    if (on) dispatch({ kind: 'axioms', add: [ax], remove: [] }, `Make ${label} ${word}`);
    else {
      const it = chars.get(type);
      if (it)
        dispatch({ kind: 'axioms', add: [], remove: [it.axiom] }, `Make ${label} not ${word}`);
    }
  }

  const problem = (section: string, p: Problem | string | null) =>
    void (problems = { ...problems, [section]: typeof p === 'string' ? { message: p } : p });

  /** A Manchester problem as the form shows it, with the candidates of an ambiguous name. */
  const fromServer = (p: ManchesterProblem): Problem => ({
    message: p.error,
    start: p.start,
    end: p.end,
    candidates: p.candidates.map((c) => (c.label ? `${c.label} <${c.iri}>` : `<${c.iri}>`)),
  });

  /** What the server reads a section's Manchester text as, or null for a named entity only. */
  const parseAs = (target: EntityKind | null): ParseAs | null =>
    target === 'Class' ? 'classExpression' : target === 'Datatype' ? 'dataRange' : null;

  async function addTo(section: string, title: string) {
    if (readonly || !alive) return;
    const origin = acceptedSource;
    const generation = ++parsing;
    // the server reads the text as it is in the field, so a problem's span fits it
    const raw = adding[section] ?? '';
    const text = raw.trim();
    const target = sectionTarget(kind, section);
    if (!text) return;
    // a name or an IRI, then functional-style syntax, then Manchester syntax
    let value: unknown =
      resolveName(text, target ? options(target) : [], prefixes) ??
      (isFunctional(text) ? text : null);
    const as = parseAs(target);
    if (value == null) {
      if (!as) return problem(section, 'Name an entity or an IRI.');
      busy = section;
      try {
        const r = await parse(raw, as);
        if (!alive || generation !== parsing) return;
        if (readonly || source !== origin || acceptedSource !== origin)
          return problem(
            section,
            'The frame source changed. The draft was preserved; discard it to load the current frame.',
          );
        if (!r.ok) return problem(section, fromServer(r.problem));
        value = r.parsed.value;
      } catch (e) {
        if (!alive || generation !== parsing) return;
        return problem(section, e instanceof Error ? e.message : String(e));
      } finally {
        if (generation === parsing) busy = null;
      }
    }
    if (
      !alive ||
      generation !== parsing ||
      readonly ||
      source !== origin ||
      acceptedSource !== origin
    )
      return problem(
        section,
        'The frame source changed. The draft was preserved; discard it to load the current frame.',
      );
    const ax = sectionAxiom(kind, section, iri, value as string);
    if (!ax) return problem(section, 'This section takes a named entity, not an expression.');
    problem(section, null);
    adding = { ...adding, [section]: '' };
    dispatch({ kind: 'axioms', add: [ax], remove: [] }, `${title}: add ${text} to ${label}`);
  }

  /** An axiom in Manchester or functional-style syntax, added as it is. */
  async function addAxiom() {
    if (readonly || !alive) return;
    const origin = acceptedSource;
    const generation = ++parsing;
    const raw = adding.axiom ?? '';
    const text = raw.trim();
    if (!text) return;
    let ax: unknown = isFunctional(text) ? text : null;
    if (ax == null) {
      busy = 'axiom';
      try {
        const r = await parse(raw, 'axiom');
        if (!alive || generation !== parsing) return;
        if (readonly || source !== origin || acceptedSource !== origin)
          return problem(
            'axiom',
            'The frame source changed. The draft was preserved; discard it to load the current frame.',
          );
        if (!r.ok) return problem('axiom', fromServer(r.problem));
        ax = r.parsed.value;
      } catch (e) {
        if (!alive || generation !== parsing) return;
        return problem('axiom', e instanceof Error ? e.message : String(e));
      } finally {
        if (generation === parsing) busy = null;
      }
    }
    if (
      !alive ||
      generation !== parsing ||
      readonly ||
      source !== origin ||
      acceptedSource !== origin
    )
      return problem(
        'axiom',
        'The frame source changed. The draft was preserved; discard it to load the current frame.',
      );
    problem('axiom', null);
    adding = { ...adding, axiom: '' };
    dispatch({ kind: 'axioms', add: [ax as AxiomJson | string], remove: [] }, `Add ${text}`);
  }

  const objectProps = $derived(options('ObjectProperty'));
  const dataProps = $derived(options('DataProperty'));

  function addAssertion() {
    if (readonly || !alive) return;
    const p = assertProp.trim();
    const op = objectProps.find((o) => o.name === p || o.iri === p);
    const dp = dataProps.find((o) => o.name === p || o.iri === p);
    if (!op && !dp) return problem('assert', 'Choose an object or data property.');
    let ax: AxiomJson;
    if (op) {
      const v = resolveInput(assertValue, options('NamedIndividual'), prefixes);
      if (typeof v !== 'string') return problem('assert', 'Name an individual.');
      ax = propertyAssertion('ObjectProperty', op.iri, iri, v);
    } else {
      if (!validLang(assertLang.trim())) return problem('assert', 'The language tag is not valid.');
      ax = propertyAssertion(
        'DataProperty',
        dp!.iri,
        iri,
        literalOf(assertValue, assertLang.trim(), ''),
      );
    }
    problem('assert', null);
    assertValue = '';
    dispatch({ kind: 'axioms', add: [ax], remove: [] }, `Assert ${p} of ${label}`);
  }

  /** The kinds of the entity a frame row's single shown value is, for its link. */
  const linkable = (x: unknown) => typeof x === 'string' && !x.startsWith('_:');
  const characteristicList = $derived(
    kind === 'ObjectProperty'
      ? OBJECT_CHARACTERISTICS
      : kind === 'DataProperty'
        ? ([['FunctionalDataProperty', 'Functional']] as [string, string][])
        : [],
  );
</script>

<div class="frame" data-iri={iri}>
  {#if source !== acceptedSource}
    <p class="faint" role="status">
      Draft based on commit {acceptedCommit}; current frame commit {commit}. Draft text is preserved
      until you discard it.
    </p>
    <button class="btn sm" disabled={parentReadonly} onclick={discardDraft}
      >Discard draft and load current frame</button
    >
  {/if}
  {#key acceptedSource}
    <section class="block" aria-labelledby="ann-{uid}">
      <h3 id="ann-{uid}">Annotations</h3>
      <AnnotationRows
        annotations={frame.annotations.map((a) => ({
          property: String(a.axiom.property),
          value: a.axiom.value as Annotation['value'],
          ...(a.axiom.annotations ? { annotations: a.axiom.annotations } : {}),
        }))}
        {name}
        {prefixes}
        properties={annotationProperties}
        {readonly}
        {label}
        onchange={entityAnnotations}
        ondraft={(active) => annotationDraft('entity', active)}
      />
    </section>

    {#if characteristicList.length}
      <section class="block" aria-labelledby="chars-{uid}">
        <h3 id="chars-{uid}">Characteristics</h3>
        <div class="chars">
          {#each characteristicList as [type, word] (type)}
            <label class="char">
              <input
                type="checkbox"
                checked={chars.has(type)}
                disabled={readonly}
                onchange={(e) => toggleCharacteristic(type, e.currentTarget.checked)}
              />
              {word}
            </label>
          {/each}
        </div>
      </section>
    {/if}

    {#each frame.sections as s (s.id)}
      {@const target = sectionTarget(kind, s.id)}
      {@const addable =
        (!readonly || !!adding[s.id]?.trim()) &&
        ![
          'superOf',
          'subProperties',
          'disjointUnion',
          'keys',
          'chains',
          'negative',
          'objectAssertions',
          'dataAssertions',
        ].includes(s.id)}
      {#if s.rows.length || addable || (kind === 'NamedIndividual' && s.id === 'objectAssertions' && !readonly)}
        <section class="block" aria-labelledby="sec-{s.id}-{uid}" data-section={s.id}>
          <h3 id="sec-{s.id}-{uid}">
            {s.title} <span class="faint count">{s.rows.length || ''}</span>
          </h3>
          {#if s.rows.length}
            <ul class="rows">
              {#each s.rows as r, i (r.item.ofn + i)}
                {@const id = `${s.id}:${i}`}
                {@const anns = r.item.axiom.annotations ?? []}
                <li class="axiom">
                  <div class="line">
                    <span class="expr" title={r.item.ofn}>
                      {#if r.shown.length === 1 && linkable(r.shown[0])}
                        <button class="link" onclick={() => onopen(String(r.shown[0]))}
                          >{name(String(r.shown[0]))}</button
                        >
                      {:else if s.id === 'objectAssertions' || s.id === 'dataAssertions' || s.id === 'negative'}
                        <button class="link" onclick={() => onopen(String(r.shown[0]))}
                          >{name(String(r.shown[0]))}</button
                        >
                        {#if linkable(r.shown[1])}
                          <button class="link" onclick={() => onopen(String(r.shown[1]))}
                            >{name(String(r.shown[1]))}</button
                          >
                        {:else}<span class="lit">{show(r.shown[1])}</span>{/if}
                      {:else}
                        {r.shown.map(show).join(', ')}
                      {/if}
                    </span>
                    <span class="acts">
                      <button
                        class="btn ghost sm ann-toggle"
                        class:has={anns.length}
                        aria-expanded={openAnn.has(id)}
                        aria-label="Annotations of {r.item.ofn}"
                        onclick={() => toggleAnn(id)}
                        ><Icon name="info" size={12} />{anns.length || ''}</button
                      >
                      {#if !readonly}
                        <button
                          class="btn ghost icon sm"
                          aria-label="Remove {r.item.ofn}"
                          onclick={() =>
                            removeAxiom(
                              r.item,
                              `${s.title}: remove ${r.shown.map(show).join(', ')} from ${label}`,
                            )}><Icon name="trash" size={13} /></button
                        >
                      {/if}
                    </span>
                  </div>
                  {#if openAnn.has(id)}
                    <div class="axiom-ann">
                      <AnnotationRows
                        annotations={anns}
                        {name}
                        {prefixes}
                        properties={annotationProperties}
                        {readonly}
                        label={r.item.ofn}
                        onchange={(next, what) => axiomAnnotations(r.item.axiom, next, what)}
                        ondraft={(active) => annotationDraft(`${id}:${r.item.ofn}`, active)}
                      />
                    </div>
                  {/if}
                </li>
              {/each}
            </ul>
          {/if}
          {#if addable}
            {@const as = parseAs(target)}
            {@const p = problems[s.id]}
            <form
              class="add"
              onsubmit={(e) => {
                e.preventDefault();
                void addTo(s.id, s.title);
              }}
            >
              {#if as}
                <ManchesterEditor
                  bind:value={() => adding[s.id] ?? '', (v) => (adding = { ...adding, [s.id]: v })}
                  {names}
                  {search}
                  label="Add to {s.title}"
                  placeholder={as === 'classExpression'
                    ? 'Class name, IRI or class expression'
                    : 'Datatype, IRI or data range'}
                  problem={p?.start != null
                    ? { start: p.start, end: p.end ?? p.start, message: p.message }
                    : null}
                  onsubmit={() => void addTo(s.id, s.title)}
                />
              {:else}
                <input
                  class="input"
                  list={target ? `opts-${target}-${uid}` : undefined}
                  placeholder={target ? `${KIND_SINGULAR[target]} name or IRI` : 'IRI'}
                  aria-label="Add to {s.title}"
                  value={adding[s.id] ?? ''}
                  oninput={(e) => (adding = { ...adding, [s.id]: e.currentTarget.value })}
                />
              {/if}
              <button
                class="btn sm"
                type="submit"
                disabled={readonly || !(adding[s.id] ?? '').trim() || busy === s.id}>Add</button
              >
              {#if p}{@render problemText(p)}{/if}
            </form>
          {/if}
          {#if kind === 'NamedIndividual' && s.id === 'objectAssertions' && (!readonly || !!assertProp.trim() || !!assertValue.trim())}
            <form
              class="add"
              aria-label="Add a property assertion"
              onsubmit={(e) => {
                e.preventDefault();
                addAssertion();
              }}
            >
              <input
                class="input"
                list="opts-props-{uid}"
                placeholder="Property"
                aria-label="Property"
                bind:value={assertProp}
              />
              <input
                class="input"
                list={objectProps.some((o) => o.name === assertProp)
                  ? `opts-NamedIndividual-${uid}`
                  : undefined}
                placeholder="Individual or value"
                aria-label="Value"
                bind:value={assertValue}
              />
              {#if dataProps.some((o) => o.name === assertProp)}
                <input
                  class="input lang"
                  placeholder="lang"
                  aria-label="Language"
                  bind:value={assertLang}
                  size="5"
                />
              {/if}
              <button
                class="btn sm"
                type="submit"
                disabled={readonly || !assertProp.trim() || !assertValue.trim()}>Add</button
              >
              {#if problems.assert}{@render problemText(problems.assert)}{/if}
            </form>
          {/if}
        </section>
      {/if}
    {/each}

    {#if frame.other.length}
      <section class="block" aria-labelledby="other-{uid}">
        <h3 id="other-{uid}">Other axioms <span class="faint count">{frame.other.length}</span></h3>
        <ul class="rows">
          {#each frame.other as it, i (it.ofn + i)}
            <li class="axiom">
              <div class="line">
                <span class="expr mono small">{it.ofn}</span>
                {#if !readonly && it.type !== 'Declaration'}
                  <span class="acts">
                    <button
                      class="btn ghost icon sm"
                      aria-label="Remove {it.ofn}"
                      onclick={() => removeAxiom(it, `Remove ${it.type} of ${label}`)}
                      ><Icon name="trash" size={13} /></button
                    >
                  </span>
                {/if}
              </div>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if !readonly || !!adding.axiom?.trim()}
      <section class="block" aria-labelledby="axiom-{uid}">
        <h3 id="axiom-{uid}">Add an axiom</h3>
        <form
          class="add"
          onsubmit={(e) => {
            e.preventDefault();
            void addAxiom();
          }}
        >
          <ManchesterEditor
            bind:value={() => adding.axiom ?? '', (v) => (adding = { ...adding, axiom: v })}
            {names}
            {search}
            label="Add an axiom"
            placeholder="An axiom, such as {label.includes(' ')
              ? `'${label}'`
              : label} SubClassOf …"
            problem={problems.axiom?.start != null
              ? {
                  start: problems.axiom.start,
                  end: problems.axiom.end ?? problems.axiom.start,
                  message: problems.axiom.message,
                }
              : null}
            onsubmit={() => void addAxiom()}
          />
          <button
            class="btn sm"
            type="submit"
            disabled={readonly || !(adding.axiom ?? '').trim() || busy === 'axiom'}>Add</button
          >
          {#if problems.axiom}{@render problemText(problems.axiom)}{/if}
        </form>
      </section>
    {/if}
  {/key}
</div>

{#snippet problemText(p: Problem)}
  <div class="problem" role="alert">
    {p.message}
    {#if p.candidates?.length}
      <ul class="candidates">
        {#each p.candidates as c (c)}<li class="mono">{c}</li>{/each}
      </ul>
    {/if}
  </div>
{/snippet}

{#each ['Class', 'ObjectProperty', 'DataProperty', 'AnnotationProperty', 'NamedIndividual', 'Datatype'] as k (k)}
  <datalist id="opts-{k}-{uid}">
    {#each options(k as EntityKind) as o (o.iri)}<option value={o.name}></option>{/each}
  </datalist>
{/each}
<datalist id="opts-props-{uid}">
  {#each [...objectProps, ...dataProps] as o (o.iri)}<option value={o.name}></option>{/each}
</datalist>

<style>
  .frame {
    display: grid;
    gap: 14px;
    min-width: 0;
  }
  .block {
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  h3 {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    text-transform: none;
    border-bottom: 1px solid var(--border);
    padding-bottom: 3px;
  }
  .count {
    font-weight: 500;
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .axiom {
    min-width: 0;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 4px;
    border-radius: var(--r-sm);
    min-width: 0;
  }
  .line:hover {
    background: var(--hover);
  }
  .expr {
    min-width: 0;
    flex: 1;
    overflow-wrap: anywhere;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--iri);
    cursor: pointer;
    text-align: left;
  }
  .link:hover {
    text-decoration: underline;
  }
  .lit {
    color: var(--literal);
  }
  .acts {
    display: inline-flex;
    gap: 2px;
    flex: none;
  }
  .ann-toggle {
    color: var(--text-3);
    gap: 2px;
  }
  .ann-toggle.has {
    color: var(--spark-ink);
  }
  .axiom-ann {
    margin: 2px 0 6px 18px;
    padding-left: 8px;
    border-left: 2px solid var(--border);
  }
  .chars {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
  }
  .char {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
  }
  .add {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    min-width: 0;
  }
  .add .input {
    flex: 1 1 14em;
    min-width: 0;
  }
  .add .input.lang {
    flex: 0 0 5em;
  }
  .problem {
    flex-basis: 100%;
    color: var(--danger);
    font-size: var(--fs-sm);
  }
  .candidates {
    margin: 2px 0 0 16px;
    padding: 0;
    color: var(--text-2);
    font-size: var(--fs-xs);
    overflow-wrap: anywhere;
  }
</style>

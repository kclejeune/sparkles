<script lang="ts">
  // The frame of one entity in the ontology editor (docs/specs/U12-ontology-workbench/
  // SPEC.md): its annotations, its characteristics and its axioms by section, as Protégé's
  // and WebProtégé's frames show them, with class expressions in Manchester syntax. Every
  // change is handed to the page as an operation, which previews it and commits it.
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
    resolveInput,
    sectionAxiom,
    sectionTarget,
    validLang,
    withAnnotations,
    type Annotation,
    type AxiomItem,
    type AxiomJson,
    type EntityKind,
    type Op,
  } from '$lib/ontology';
  import type { PrefixMap } from '$lib/rdf';
  import AnnotationRows from './AnnotationRows.svelte';
  import Icon from './Icon.svelte';

  let {
    kind,
    iri,
    items,
    name,
    prefixes,
    options,
    annotationProperties,
    readonly = false,
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
    onedit: (op: Op, label: string) => void;
    onopen: (iri: string) => void;
  } = $props();

  const frame = $derived(buildFrame(kind, iri, items));
  const label = $derived(name(iri));
  const chars = $derived(characteristicsOf(iri, items));
  /** The axiom rows whose annotations are open, by section and row. */
  let openAnn = $state<Set<string>>(new Set());
  /** The add form of each section: its text and its problem. */
  let adding = $state<Record<string, string>>({});
  let problems = $state<Record<string, string | null>>({});
  // a property assertion of an individual
  let assertProp = $state('');
  let assertValue = $state('');
  let assertLang = $state('');
  const uid = Math.random().toString(36).slice(2, 8);

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
      onedit({ kind: 'axioms', add: d.add, remove: d.remove }, what);
  }

  function axiomAnnotations(a: AxiomJson, next: Annotation[], what: string) {
    onedit({ kind: 'axioms', remove: [a], add: [withAnnotations(a, next)] }, what);
  }

  function removeAxiom(it: AxiomItem, what: string) {
    onedit({ kind: 'axioms', add: [], remove: [it.axiom] }, what);
  }

  function toggleCharacteristic(type: string, on: boolean) {
    const ax = { type, property: iri };
    const word = type.replace(/(Object|Data)Property$/, '').toLowerCase();
    if (on) onedit({ kind: 'axioms', add: [ax], remove: [] }, `Make ${label} ${word}`);
    else {
      const it = chars.get(type);
      if (it) onedit({ kind: 'axioms', add: [], remove: [it.axiom] }, `Make ${label} not ${word}`);
    }
  }

  function addTo(section: string, title: string) {
    const text = adding[section] ?? '';
    const target = sectionTarget(kind, section);
    const r = resolveInput(text, target ? options(target) : [], prefixes);
    if (!r)
      return void (problems = {
        ...problems,
        [section]: 'Name an entity, an IRI or a functional-style expression.',
      });
    const ax = sectionAxiom(kind, section, iri, typeof r === 'string' ? r : r.ofn);
    if (!ax)
      return void (problems = {
        ...problems,
        [section]: 'This section takes a named entity, not an expression.',
      });
    problems = { ...problems, [section]: null };
    adding = { ...adding, [section]: '' };
    onedit({ kind: 'axioms', add: [ax], remove: [] }, `${title}: add ${text.trim()} to ${label}`);
  }

  const objectProps = $derived(options('ObjectProperty'));
  const dataProps = $derived(options('DataProperty'));

  function addAssertion() {
    const p = assertProp.trim();
    const op = objectProps.find((o) => o.name === p || o.iri === p);
    const dp = dataProps.find((o) => o.name === p || o.iri === p);
    if (!op && !dp)
      return void (problems = { ...problems, assert: 'Choose an object or data property.' });
    let ax: AxiomJson;
    if (op) {
      const v = resolveInput(assertValue, options('NamedIndividual'), prefixes);
      if (typeof v !== 'string')
        return void (problems = { ...problems, assert: 'Name an individual.' });
      ax = propertyAssertion('ObjectProperty', op.iri, iri, v);
    } else {
      if (!validLang(assertLang.trim()))
        return void (problems = { ...problems, assert: 'The language tag is not valid.' });
      ax = propertyAssertion(
        'DataProperty',
        dp!.iri,
        iri,
        literalOf(assertValue, assertLang.trim(), ''),
      );
    }
    problems = { ...problems, assert: null };
    assertValue = '';
    onedit({ kind: 'axioms', add: [ax], remove: [] }, `Assert ${p} of ${label}`);
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
      !readonly &&
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
                        >{show(r.shown[0])}</button
                      >
                    {:else if s.id === 'objectAssertions' || s.id === 'dataAssertions' || s.id === 'negative'}
                      <button class="link" onclick={() => onopen(String(r.shown[0]))}
                        >{show(r.shown[0])}</button
                      >
                      {#if linkable(r.shown[1])}
                        <button class="link" onclick={() => onopen(String(r.shown[1]))}
                          >{show(r.shown[1])}</button
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
                    />
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
        {#if addable}
          <form
            class="add"
            onsubmit={(e) => {
              e.preventDefault();
              addTo(s.id, s.title);
            }}
          >
            <input
              class="input"
              list={target ? `opts-${target}-${uid}` : undefined}
              placeholder={target === 'Class'
                ? 'Class name, IRI or functional-style expression'
                : target
                  ? `${KIND_SINGULAR[target]} name or IRI`
                  : 'IRI'}
              aria-label="Add to {s.title}"
              value={adding[s.id] ?? ''}
              oninput={(e) => (adding = { ...adding, [s.id]: e.currentTarget.value })}
            />
            <button class="btn sm" type="submit" disabled={!(adding[s.id] ?? '').trim()}>Add</button
            >
            {#if problems[s.id]}<p class="problem" role="alert">{problems[s.id]}</p>{/if}
          </form>
        {/if}
        {#if kind === 'NamedIndividual' && s.id === 'objectAssertions' && !readonly}
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
              disabled={!assertProp.trim() || !assertValue.trim()}>Add</button
            >
            {#if problems.assert}<p class="problem" role="alert">{problems.assert}</p>{/if}
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
</div>

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
</style>

<script lang="ts">
  // The logical definition of an OWL entity in edit mode (ROADMAP item 630): its axioms in
  // the frame's sections as the read-only page shows them, each with Remove when the edited
  // graph states it, and an add row that takes a section and a class expression or name in
  // Manchester syntax, read by the server's parser with its problem underlined, then built
  // into the section's axiom as the ontology editor's frame does. "Any axiom" takes a
  // whole axiom in Manchester or functional-style syntax.
  import type { AxiomGroup, Linker } from '$lib/browse';
  import { onDestroy } from 'svelte';
  import { axiomOp, inGraph, type OnEdit } from '$lib/browse-edit';
  import type { NameOption } from '$lib/manchester-complete';
  import {
    isFunctional,
    KIND_SINGULAR,
    resolveName,
    SECTIONS,
    sectionAxiom,
    sectionTarget,
    type AxiomJson,
    type EntityKind,
    type ManchesterProblem,
    type ParseAs,
    type Parsed,
  } from '$lib/ontology';
  import type { PrefixMap } from '$lib/rdf';
  import Icon from '$components/Icon.svelte';
  import ManchesterEditor from '$components/ManchesterEditor.svelte';
  import AxiomProse from './AxiomProse.svelte';

  let {
    groups,
    kind,
    iri,
    title,
    linker,
    prefixes,
    graph,
    readonly,
    names,
    parse,
    onedit,
    bare = false,
  }: {
    groups: AxiomGroup[];
    kind: EntityKind;
    iri: string;
    title: string;
    linker: Linker;
    prefixes: PrefixMap;
    graph: string | null;
    readonly: boolean;
    /** The entity names the Manchester field completes and resolves. */
    names: () => NameOption[];
    parse: (
      text: string,
      as: ParseAs,
    ) => Promise<{ ok: true; parsed: Parsed } | { ok: false; problem: ManchesterProblem }>;
    onedit: OnEdit;
    /** Leave out the heading, for a card whose own head names the frame. */
    bare?: boolean;
  } = $props();

  const PAIRS = new Set(['objectAssertions', 'dataAssertions', 'negative']);
  /** The sections an add row builds from one name or expression, as the frame's forms do. */
  const NOT_ADDABLE = new Set([
    'superOf',
    'subProperties',
    'disjointUnion',
    'keys',
    'chains',
    'negative',
    'objectAssertions',
    'dataAssertions',
    'instances',
    'definitions',
  ]);
  const sections = $derived([
    ...SECTIONS[kind]
      .filter(([id]) => !NOT_ADDABLE.has(id))
      .map(([id, t]) => [id, t.charAt(0) + t.slice(1).toLowerCase()] as [string, string]),
    ['axiom', 'Any axiom'] as [string, string],
  ]);

  let section = $state('');
  $effect(() => {
    if (!sections.some(([id]) => id === section)) section = sections[0][0];
  });
  let text = $state('');
  let busy = $state(false);
  let problem = $state<{ message: string; start?: number; end?: number } | null>(null);

  const fromServer = (p: ManchesterProblem) => ({ message: p.error, start: p.start, end: p.end });
  const parseAs = (t: EntityKind | null): ParseAs | null =>
    t === 'Class' ? 'classExpression' : t === 'Datatype' ? 'dataRange' : null;
  const sectionTitle = $derived(sections.find(([id]) => id === section)?.[1] ?? '');

  let alive = true;
  onDestroy(() => (alive = false));
  async function add() {
    const raw = text;
    const typed = raw.trim();
    if (!typed || busy) return;
    // the section, entity and graph as submitted; a parse that answers after any of them
    // changed, or after the editor went away, adds nothing
    const sec = section;
    const secTitle = sectionTitle;
    const at = JSON.stringify([iri, graph, kind]);
    const still = () => alive && at === JSON.stringify([iri, graph, kind]);
    problem = null;
    let ax: unknown = null;
    if (sec === 'axiom') {
      ax = isFunctional(typed) ? typed : null;
      if (ax == null) {
        busy = true;
        try {
          const r = await parse(raw, 'axiom');
          if (!still()) return;
          if (!r.ok) return void (problem = fromServer(r.problem));
          ax = r.parsed.value;
        } catch (e) {
          return void (problem = { message: e instanceof Error ? e.message : String(e) });
        } finally {
          busy = false;
        }
      }
    } else {
      const t = sectionTarget(kind, sec);
      const options = names()
        .filter((n) => !t || n.kind === KIND_SINGULAR[t])
        .map((n) => ({ iri: n.iri, name: n.name }));
      let value: unknown =
        resolveName(typed, options, prefixes) ?? (isFunctional(typed) ? typed : null);
      const as = parseAs(t);
      if (value == null) {
        if (!as) return void (problem = { message: 'Name an entity or an IRI.' });
        busy = true;
        try {
          const r = await parse(raw, as);
          if (!still()) return;
          if (!r.ok) return void (problem = fromServer(r.problem));
          value = r.parsed.value;
        } catch (e) {
          return void (problem = { message: e instanceof Error ? e.message : String(e) });
        } finally {
          busy = false;
        }
      }
      ax = sectionAxiom(kind, sec, iri, value as string);
      if (!ax)
        return void (problem = {
          message: 'This section takes a named entity, not an expression.',
        });
    }
    if (!still()) return;
    onedit(
      axiomOp(ax as AxiomJson | string, false),
      sec === 'axiom' ? `Add ${typed}` : `${secTitle}: add ${typed} to ${title}`,
      () => {
        text = '';
      },
    );
  }
</script>

<svelte:element
  this={bare ? 'div' : 'section'}
  class="term-sec"
  aria-labelledby={bare ? undefined : 'logic-edit-h'}
>
  {#if !bare}<h2 id="logic-edit-h">Logical definition</h2>{/if}
  {#each groups as g (g.id)}
    <h3>{g.title}</h3>
    <ul class="erows" data-section={g.id}>
      {#each g.items as it, i (i)}
        {@const own = inGraph(it.axiom, graph)}
        <li class="erow" class:ro={!own}>
          <span class="v">
            {#if it.word}
              {it.word}
            {:else if g.id === 'other'}
              <code class="ofn">{it.axiom.ofn}</code>
            {:else if PAIRS.has(g.id)}
              {#if g.id === 'negative'}<span class="kw">not</span>{' '}{/if}<AxiomProse
                value={it.values[0]}
                {linker}
                {prefixes}
              />{' '}<AxiomProse value={it.values[1]} {linker} {prefixes} />
            {:else}
              {#each it.values as v, j (j)}{#if j > 0},{' '}{/if}<AxiomProse
                  value={v}
                  {linker}
                  {prefixes}
                />{/each}
            {/if}
            {#if !own}<span class="from">from {it.axiom.graph}, not editable here</span>{/if}
          </span>
          {#if own && !readonly}
            <button
              type="button"
              class="btn icon sm ghost"
              aria-label="Remove the axiom {it.axiom.ofn}"
              title="Remove axiom"
              onclick={() =>
                onedit(axiomOp(it.axiom.axiom, true), `Remove ${it.axiom.ofn} from ${title}`)}
              ><Icon name="trash" size={14} /></button
            >
          {/if}
        </li>
      {/each}
    </ul>
  {/each}
  {#if !groups.length}
    <p class="none">No axioms about {title} yet.</p>
  {/if}
  {#if !readonly}
    <form
      class="addrow"
      data-edit-form
      data-dirty={text.trim() ? 'true' : 'false'}
      aria-label="Add an axiom"
      onsubmit={(e) => {
        e.preventDefault();
        void add();
      }}
    >
      <select class="select sm" bind:value={section} aria-label="Section of the new axiom">
        {#each sections as [id, t] (id)}<option value={id}>{t}</option>{/each}
      </select>
      <div class="field" id="browse-add-axiom">
        <ManchesterEditor
          bind:value={text}
          {names}
          label="New axiom of {title}"
          placeholder={section === 'axiom'
            ? 'An axiom, like Class: A SubClassOf: B'
            : 'Class name or expression, like part_of some cytoplasm'}
          problem={problem && problem.start != null && problem.end != null
            ? { start: problem.start, end: problem.end, message: problem.message }
            : null}
          onsubmit={() => void add()}
        />
      </div>
      <button class="btn sm" type="submit" data-preview disabled={!text.trim() || busy}
        >{#if busy}<span class="spinner"></span>{:else}<Icon
            name="plus"
            size={14}
          />{/if}Add</button
      >
      {#if problem}<p class="problem" role="alert">{problem.message}</p>{/if}
    </form>
  {/if}
</svelte:element>

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
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--sp-2);
    min-height: 34px;
    padding: 2px 4px 2px 8px;
    border-radius: var(--r);
    font-size: var(--fs-md);
    line-height: 1.5;
  }
  .erow:hover,
  .erow:focus-within {
    background: var(--hover);
  }
  .v {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .ro .v {
    color: var(--text-2);
  }
  .from {
    font-size: var(--fs-xs);
    color: var(--text-3);
    margin-left: 6px;
  }
  .kw {
    color: var(--kw);
    font-weight: 600;
    font-style: italic;
  }
  .ofn {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
  .none {
    color: var(--text-3);
    font-size: var(--fs-sm);
    margin: 0;
  }
  .addrow {
    display: grid;
    grid-template-columns: 150px minmax(0, 1fr) auto;
    gap: var(--sp-2);
    align-items: center;
    margin-top: var(--sp-3);
    padding: 6px 6px 6px 8px;
    border: 1px dashed var(--border-strong);
    border-radius: var(--r);
  }
  .field {
    min-width: 0;
  }
  .select {
    width: 100%;
  }
  .problem {
    grid-column: 1 / -1;
    color: var(--danger);
    font-size: var(--fs-sm);
    margin: 0;
  }
  @container browse (max-width: 639px) {
    .addrow {
      grid-template-columns: minmax(0, 1fr);
    }
    .addrow .btn {
      justify-content: center;
      height: 40px;
    }
    .erow .btn {
      width: 36px;
      height: 36px;
    }
  }
</style>

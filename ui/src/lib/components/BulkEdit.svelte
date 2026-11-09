<script lang="ts">
  // The ontology editor's Bulk edits tab (ROADMAP item 626): find and replace across labels
  // and annotations, set or remove an annotation on the entities of a list, and move the
  // classes of the list under a new parent. Each reads the graph's own axioms at the commit
  // the editor shows, lists what the edit would change, and hands one `axioms` operation to
  // the editor's preview, so it is one previewed commit on the session's undo stack. An edit
  // above BULK_LIMIT changes is refused, as is a graph with more than BULK_SCAN_LIMIT axioms
  // of the type an edit reads (lib/bulk-edit.ts).
  import { onDestroy, untrack } from 'svelte';
  import * as api from '$lib/api';
  import {
    BULK_ENTITY_LIMIT,
    BULK_LIMIT,
    BULK_SCAN_LIMIT,
    changesOp,
    compileFind,
    descendants,
    findMatches,
    namedParents,
    overLimit,
    parseEntityList,
    planMove,
    removeAnnotation,
    setAnnotation,
    valueOf,
    kindsOfTypes,
    typesQuery,
    type Change,
    type LangFilter,
    type PropertyScope,
    type RemoveFilter,
  } from '$lib/bulk-edit';
  import { fmtCount, fmtInt } from '$lib/format';
  import {
    COMMON_ANNOTATION_PROPERTIES,
    isLit,
    KIND_SINGULAR,
    LITERAL_TYPES,
    ontologyAxioms,
    type EntityKind,
    resolveInput,
    type AnnValue,
    type AxiomItem,
    type Op,
  } from '$lib/ontology';
  import { shorten, type PrefixMap } from '$lib/rdf';
  import { searchEntities } from '$lib/entity-search';
  import EntityPicker, { type Picked } from './EntityPicker.svelte';
  import Icon from './Icon.svelte';

  let {
    target,
    graph,
    commit,
    readonly,
    prefixes,
    annotationProperties = [],
    graphs = [],
    name = (i: string) => i,
    onedit,
  }: {
    /** The dataset on its branch (`ds@branch`). */
    target: string;
    /** The edited graph, null for the default graph. */
    graph: string | null;
    /** The commit the editor shows, which every read names and the edit expects. */
    commit: number | null;
    /** No edit is offered, though finds still list their matches. */
    readonly: boolean;
    prefixes: PrefixMap;
    /** The annotation properties the ontology declares, offered beside the common ones. */
    annotationProperties?: readonly string[];
    /** The graphs the ontology reads with its imports, `default` for the default graph. */
    graphs?: readonly string[];
    name?: (iri: string) => string;
    /** Preview `op` as one edit labelled `label`. */
    onedit: (op: Op, label: string) => void;
  } = $props();

  type Mode = 'replace' | 'set' | 'remove' | 'move';
  let mode = $state<Mode>('replace');

  // --- reads of the graph's own axioms at the shown commit --------------------------------
  const cache = new Map<string, Promise<AxiomItem[]>>();
  let cacheKey = '';
  const ctl = new AbortController();
  onDestroy(() => ctl.abort());

  /** Every axiom of `type` in the edited graph without its imports, or an error to show. */
  function read(type: 'AnnotationAssertion' | 'SubClassOf'): Promise<AxiomItem[]> {
    const key = JSON.stringify([target, graph, commit]);
    if (key !== cacheKey) {
      cache.clear();
      cacheKey = key;
    }
    let p = cache.get(type);
    if (!p) {
      p = ontologyAxioms(
        target,
        { graph, imports: false },
        {
          types: [type],
          cap: BULK_SCAN_LIMIT,
          at: commit != null ? `commit:${commit}` : undefined,
        },
        ctl.signal,
      ).then((r) => {
        if (r.truncated)
          throw new Error(
            `The graph holds ${fmtInt(r.total)} ${type} axioms, more than the ${fmtInt(BULK_SCAN_LIMIT)} a bulk edit reads, so it cannot be edited in bulk here.`,
          );
        return r.axioms;
      });
      p.catch(() => cache.delete(type));
      cache.set(type, p);
    }
    return p;
  }

  /**
   * The kinds that the graphs the ontology reads give each of `iris` through `rdf:type`, at
   * the shown commit: one SPARQL query for each 200 IRIs, stopped by `signal`.
   */
  async function kindsOfAll(
    iris: readonly string[],
    signal: AbortSignal,
  ): Promise<Map<string, EntityKind[]>> {
    const out = new Map<string, EntityKind[]>();
    for (let i = 0; i < iris.length; i += 200) {
      const rows = await api.select(target, typesQuery(iris.slice(i, i + 200), graphs), {
        at: commit != null ? `commit:${commit}` : undefined,
        nocache: true,
        signal,
      });
      const typed = rows.flatMap((r) =>
        r.s?.type === 'uri' && r.t?.type === 'uri' ? [{ s: r.s.value, t: r.t.value }] : [],
      );
      for (const [s, ks] of kindsOfTypes(typed)) out.set(s, ks);
    }
    return out;
  }
  const OWL_THING = 'http://www.w3.org/2002/07/owl#Thing';
  /** Kinds as a phrase: `a data property and an individual`. */
  const kindWords = (kinds: EntityKind[]) =>
    kinds
      .map((k) => KIND_SINGULAR[k] ?? k)
      .map((w) => `${/^[aeiou]/i.test(w) ? 'an' : 'a'} ${w}`)
      .join(' and ');

  // --- the entity list -----------------------------------------------------------------------
  let entities = $state<string[]>([]);
  let paste = $state('');
  let pasteBad = $state<string[]>([]);
  let listError = $state<string | null>(null);
  let picked = $state<Picked | null>(null);
  let subtreeOf = $state<Picked | null>(null);
  let listBusy = $state(false);

  function addEntities(iris: string[]) {
    listError = null;
    const next = [...new Set([...entities, ...iris])];
    if (next.length > BULK_ENTITY_LIMIT) {
      listError = `The list holds at most ${fmtInt(BULK_ENTITY_LIMIT)} entities, and adding these would make ${fmtInt(next.length)}.`;
      return false;
    }
    entities = next;
    invalidate();
    return true;
  }
  function addPasted() {
    const r = parseEntityList(paste, prefixes);
    pasteBad = r.bad;
    if (addEntities(r.iris)) paste = r.bad.join('\n');
  }
  async function searchKind(text: string, signal: AbortSignal) {
    const r = await searchEntities(target, text, { graph, limit: 10, signal });
    return r.results.map((h) => ({ iri: h.iri, label: h.label ?? h.shortForm ?? h.iri }));
  }
  async function searchClasses(text: string, signal: AbortSignal) {
    const r = await searchEntities(target, text, { kind: ['class'], graph, limit: 10, signal });
    return r.results.map((h) => ({ iri: h.iri, label: h.label ?? h.shortForm ?? h.iri }));
  }
  /** Bumped when a subtree read in flight no longer answers the list's question. */
  let subtreeRun = 0;
  async function addSubtree() {
    const root = subtreeOf;
    if (!root || root.term.type !== 'uri') return;
    const my = ++subtreeRun;
    listBusy = true;
    listError = null;
    try {
      const below = descendants(namedParents(await read('SubClassOf')), [root.term.value]);
      // the commit, the graph, the root or the list changed while the subclasses were read
      if (my !== subtreeRun) return;
      if (addEntities([root.term.value, ...below])) subtreeOf = null;
    } catch (e) {
      if (my === subtreeRun) listError = api.errorMessage(e);
    } finally {
      if (my === subtreeRun) listBusy = false;
    }
  }
  function dropSubtreeRead() {
    subtreeRun++;
    listBusy = false;
  }

  // --- the forms -------------------------------------------------------------------------------
  let find = $state('');
  let replace = $state('');
  let regex = $state(false);
  let caseSensitive = $state(false);
  let propKind = $state<'labels' | 'all' | 'one'>('labels');
  let propText = $state('');
  let langKind = $state<'any' | 'none' | 'tag'>('any');
  let langTag = $state('');
  let scope = $state<'graph' | 'list'>('graph');

  let property = $state('rdfs:comment');
  let valueText = $state('');
  let valueLang = $state('en');
  let valueType = $state('');
  let valueIri = $state(false);
  let setMode = $state<'add' | 'replace'>('add');
  let removeKind = $state<'any' | 'lang' | 'value'>('any');

  let parent = $state<Picked | null>(null);
  let toTop = $state(false);
  let keepParents = $state(false);

  const propertyOptions = $derived(
    [...new Set([...COMMON_ANNOTATION_PROPERTIES, ...annotationProperties])].map(
      (iri) => shorten(iri, prefixes) ?? iri,
    ),
  );
  const iriOf = (s: string) => {
    const r = resolveInput(s, [], prefixes);
    return typeof r === 'string' ? r : null;
  };
  const langFilter = (): LangFilter =>
    langKind === 'tag' ? { kind: 'tag', tag: langTag } : { kind: langKind };

  // --- the plan: what the edit would change, at the commit it read ----------------------------
  type Row = { subject: string; property: string; before: string | null; after: string | null };
  let plan = $state<{
    commit: number | null;
    mode: Mode;
    op: Op | null;
    label: string;
    rows: Row[];
    count: number;
    notes: string[];
    refusal: string | null;
  } | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let run = 0;

  /** Drop the plan and any calculation still reading, whose answer belongs to old inputs. */
  /** Aborts the reads of the calculation in flight. */
  let runCtl: AbortController | null = null;
  function invalidate() {
    run++;
    runCtl?.abort();
    runCtl = null;
    busy = false;
    plan = null;
    error = null;
  }
  // a new commit, graph or branch makes the plan and every read in flight stale
  $effect(() => {
    void target;
    void graph;
    void commit;
    untrack(() => {
      invalidate();
      dropSubtreeRead();
    });
  });
  $effect(() => {
    void subtreeOf;
    untrack(dropSubtreeRead);
  });
  // so does any input the plan is built from, which a calculation in flight read before
  $effect(() => {
    void [mode, find, replace, regex, caseSensitive, propKind, propText, langKind, langTag];
    void [scope, property, valueText, valueLang, valueType, valueIri, setMode, removeKind];
    void [parent, toTop, keepParents, prefixes, entities];
    untrack(invalidate);
  });

  const shown = (v: unknown): string =>
    isLit(v)
      ? `"${v['@value']}"${v['@language'] ? '@' + v['@language'] : ''}`
      : typeof v === 'string'
        ? name(v)
        : JSON.stringify(v);
  const rowsOf = (changes: readonly Change[]): Row[] =>
    changes.map((c) => ({
      subject: c.subject,
      property: c.property,
      before: c.before ? shown(c.before.value) : null,
      after: c.after ? shown(c.after.value) : null,
    }));

  async function compute() {
    const my = ++run;
    runCtl?.abort();
    const c = (runCtl = new AbortController());
    const at = commit;
    const m = mode;
    busy = true;
    error = null;
    plan = null;
    try {
      const p = await build(m, c.signal);
      if (my !== run || m !== mode || at !== commit) return;
      plan = { ...p, commit: at, mode: m };
    } catch (e) {
      if (my === run) error = api.errorMessage(e);
    } finally {
      if (my === run) busy = false;
    }
  }

  type Built = Omit<NonNullable<typeof plan>, 'commit' | 'mode'>;
  const refuse = (refusal: string): Built => ({
    op: null,
    label: '',
    rows: [],
    count: 0,
    notes: [],
    refusal,
  });
  const ofChanges = (
    changes: Change[],
    label: string,
    notes: string[],
    one: string,
    many: string,
  ): Built =>
    changes.length > BULK_LIMIT
      ? { ...refuse(overLimit(changes.length, one, many)), count: changes.length, notes }
      : {
          op: changes.length ? changesOp(changes) : null,
          label,
          rows: rowsOf(changes),
          count: changes.length,
          notes,
          refusal: changes.length ? null : 'Nothing matches, so the edit would change nothing.',
        };

  /**
   * The plan of operation `m`. Every input is read before the first await, so a field changed
   * while the graph is read cannot mix into the plan.
   */
  async function build(m: Mode, signal: AbortSignal): Promise<Built> {
    const ents = [...entities];
    if (m === 'replace') {
      const props: PropertyScope =
        propKind === 'one' ? { kind: 'one', iri: iriOf(propText) ?? '' } : { kind: propKind };
      if (props.kind === 'one' && !props.iri)
        return refuse('Name the annotation property to search.');
      if (langKind === 'tag' && !langTag.trim()) return refuse('Type the language tag to keep to.');
      if (scope === 'list' && !ents.length) return refuse('Add entities to the list first.');
      const spec = {
        find,
        replace,
        regex,
        caseSensitive,
        properties: props,
        lang: langFilter(),
        entities: scope === 'list' ? new Set(ents) : null,
      };
      const re = compileFind(spec);
      if (typeof re === 'string') return refuse(re);
      const label = (n: number) =>
        `Replace "${spec.find}" with "${spec.replace}" in ${fmtCount(n, 'annotation')}`;
      const r = findMatches(await read('AnnotationAssertion'), spec, re);
      if (r.zeroWidth != null)
        return refuse(
          `The pattern matches empty text inside "${r.zeroWidth}", where the replacement would be inserted between characters. Make it match at least one character.`,
        );
      const notes = r.emptied
        ? [
            `${fmtCount(r.emptied, 'value')} would become empty and ${r.emptied === 1 ? 'is' : 'are'} left out. Remove ${r.emptied === 1 ? 'it' : 'them'} with Remove annotation instead.`,
          ]
        : [];
      return ofChanges(r.changes, label(r.changes.length), notes, 'annotation', 'annotations');
    }
    if (m === 'set' || m === 'remove') {
      if (!ents.length) return refuse('Add entities to the list first.');
      const p = iriOf(property);
      if (!p) return refuse('Name the annotation property as a prefixed name or an IRI.');
      if (m === 'set') {
        const v: AnnValue | null = valueOf(valueText, valueLang, valueType, valueIri, prefixes);
        if (v == null)
          return refuse(
            valueIri ? 'Name the value as a prefixed name or an IRI.' : 'Type the value to set.',
          );
        const how = setMode;
        const kinds = (await kindsOfAll([p], signal)).get(p) ?? [];
        if (kinds.length && !kinds.includes('AnnotationProperty'))
          return refuse(
            `${name(p)} is ${kindWords(kinds)} in this ontology, not an annotation property, so its values cannot be set as annotations.`,
          );
        const r = setAnnotation(ents, p, v, how, await read('AnnotationAssertion'));
        const notes = r.unchanged
          ? [
              `${fmtCount(r.unchanged, 'entity', 'entities')} already ${r.unchanged === 1 ? 'holds' : 'hold'} the value.`,
            ]
          : [];
        return ofChanges(
          r.changes,
          `Set ${shorten(p, prefixes) ?? p} on ${fmtCount(ents.length - r.unchanged, 'entity', 'entities')}`,
          notes,
          'annotation',
          'annotations',
        );
      }
      let filter: RemoveFilter;
      if (removeKind === 'lang') {
        if (langKind === 'tag' && !langTag.trim())
          return refuse('Type the language tag to keep to.');
        filter = { kind: 'lang', lang: langFilter() };
      } else if (removeKind === 'value') {
        const text = valueIri ? iriOf(valueText) : valueText;
        if (!text) return refuse('Type the value to remove.');
        filter = { kind: 'value', text, iri: valueIri };
      } else filter = { kind: 'any' };
      const changes = removeAnnotation(ents, p, filter, await read('AnnotationAssertion'));
      return ofChanges(
        changes,
        `Remove ${shorten(p, prefixes) ?? p} from ${fmtCount(new Set(changes.map((c) => c.subject)).size, 'entity', 'entities')}`,
        [],
        'annotation',
        'annotations',
      );
    }
    // move
    if (!ents.length) return refuse('Add the classes to move to the entity list.');
    const to = toTop ? null : parent?.term.type === 'uri' ? parent.term.value : undefined;
    if (to === undefined) return refuse('Pick the new parent class, or move to the top.');
    const keep = keepParents && !toTop;
    if (to !== null && to !== OWL_THING) {
      const kinds = (await kindsOfAll([to], signal)).get(to) ?? [];
      if (!kinds.includes('Class'))
        return refuse(
          kinds.length
            ? `${name(to)} is ${kindWords(kinds)} in this ontology, not a class.`
            : `${name(to)} is not a class of this ontology or its imports.`,
        );
    }
    // a known entity that is not a class would become one through its SubClassOf
    const known = await kindsOfAll(ents, signal);
    const notClasses = ents.flatMap((i): [string, EntityKind[]][] => {
      const k = known.get(i);
      return k && !k.includes('Class') ? [[i, k]] : [];
    });
    if (notClasses.length)
      return refuse(
        `${notClasses
          .slice(0, 5)
          .map(([i, k]) => `${name(i)} is ${kindWords(k)}`)
          .join(
            ', ',
          )}${notClasses.length > 5 ? ` and ${notClasses.length - 5} more are not classes` : ''}. Only classes, or IRIs the ontology does not know, can be moved.`,
      );
    const mv = planMove(ents, to, await read('SubClassOf'), keep, name);
    if (!mv.ok) return { ...refuse(mv.reason), count: ents.length };
    const op = mv.op.kind === 'axioms' ? mv.op : null;
    const rows: Row[] = mv.moved.map((c) => ({
      subject: c,
      property: 'SubClassOf',
      before:
        op?.remove
          .filter((a) => typeof a !== 'string' && a.subClass === c)
          .map((a) => name(String((a as { superClass?: unknown }).superClass)))
          .join(', ') || null,
      after: to ? name(to) : null,
    }));
    return {
      op: mv.op,
      label: `Move ${fmtCount(mv.moved.length, 'class', 'classes')} ${to ? `under ${name(to)}` : 'to the top'}`,
      rows,
      count: mv.moved.length,
      notes: mv.unchanged.length
        ? [
            `${fmtCount(mv.unchanged.length, 'class', 'classes')} already ${mv.unchanged.length === 1 ? 'is' : 'are'} in place.`,
          ]
        : [],
      refusal: null,
    };
  }

  const ROWS_SHOWN = 200;
  const current = $derived(!!plan && plan.commit === commit);
</script>

<div class="bulk">
  <div class="segmented sm ops" role="group" aria-label="Bulk edit">
    <button aria-pressed={mode === 'replace'} onclick={() => (mode = 'replace')}
      >Find and replace</button
    >
    <button aria-pressed={mode === 'set'} onclick={() => (mode = 'set')}>Set annotation</button>
    <button aria-pressed={mode === 'remove'} onclick={() => (mode = 'remove')}
      >Remove annotation</button
    >
    <button aria-pressed={mode === 'move'} onclick={() => (mode = 'move')}>Move classes</button>
  </div>
  <p class="faint small">
    A bulk edit reads this graph's own axioms, without its imports, at commit {commit ?? '—'}, and
    makes one previewed commit of at most {fmtInt(BULK_LIMIT)} changes, which undo takes back whole.
  </p>
  {#if readonly}
    <p class="notice" role="status">
      Bulk edits are read-only here. Finds still list what an edit would change.
    </p>
  {/if}

  <div class="cols">
    <div class="main">
      <form
        class="form"
        aria-label={mode === 'replace'
          ? 'Find and replace'
          : mode === 'set'
            ? 'Set an annotation'
            : mode === 'remove'
              ? 'Remove an annotation'
              : 'Move classes'}
        onsubmit={(e) => {
          e.preventDefault();
          void compute();
        }}
      >
        {#if mode === 'replace'}
          <div class="two">
            <label class="f grow"
              ><span>Find</span><input class="input" bind:value={find} required /></label
            >
            <label class="f grow"
              ><span>Replace with</span><input class="input" bind:value={replace} /></label
            >
          </div>
          <div class="row wrap">
            <label class="check"
              ><input type="checkbox" bind:checked={regex} /> Regular expression</label
            >
            <label class="check"
              ><input type="checkbox" bind:checked={caseSensitive} /> Match case</label
            >
          </div>
          <div class="row wrap">
            <label class="f">
              <span>In</span>
              <select class="input" bind:value={propKind}>
                <option value="labels">Labels</option>
                <option value="all">All annotations</option>
                <option value="one">One property</option>
              </select>
            </label>
            {#if propKind === 'one'}
              <label class="f grow">
                <span>Property</span>
                <input class="input mono" list="bulk-props" bind:value={propText} />
              </label>
            {/if}
            <label class="f">
              <span>Scope</span>
              <select class="input" bind:value={scope}>
                <option value="graph">The whole graph</option>
                <option value="list">The entity list</option>
              </select>
            </label>
          </div>
        {:else if mode === 'set' || mode === 'remove'}
          <label class="f">
            <span>Annotation property</span>
            <input class="input mono" list="bulk-props" bind:value={property} required />
          </label>
          {#if mode === 'set'}
            <div class="row wrap">
              <label class="f grow"
                ><span>Value</span><input class="input" bind:value={valueText} /></label
              >
              {#if !valueIri}
                <label class="f lang"
                  ><span>Language</span><input class="input" bind:value={valueLang} /></label
                >
                {#if !valueLang.trim()}
                  <label class="f">
                    <span>Datatype</span>
                    <select class="input" bind:value={valueType}>
                      <option value="">string</option>
                      {#each LITERAL_TYPES.slice(1) as t (t)}<option value={t}
                          >{shorten(t, prefixes) ?? t}</option
                        >{/each}
                    </select>
                  </label>
                {/if}
              {/if}
            </div>
            <div class="row wrap">
              <label class="check"
                ><input type="checkbox" bind:checked={valueIri} /> The value is an IRI</label
              >
              <label class="check"
                ><input
                  type="checkbox"
                  checked={setMode === 'replace'}
                  onchange={(e) => (setMode = e.currentTarget.checked ? 'replace' : 'add')}
                />
                Replace the values the entities hold{valueIri ? '' : ' in this language'}</label
              >
            </div>
          {:else}
            <div class="row wrap">
              <label class="f">
                <span>Remove</span>
                <select class="input" bind:value={removeKind}>
                  <option value="any">Every value</option>
                  <option value="lang">Values in a language</option>
                  <option value="value">One value</option>
                </select>
              </label>
              {#if removeKind === 'value'}
                <label class="f grow"
                  ><span>Value</span><input class="input" bind:value={valueText} /></label
                >
                <label class="check"><input type="checkbox" bind:checked={valueIri} /> An IRI</label
                >
              {/if}
            </div>
          {/if}
        {:else}
          <div class="row wrap">
            <div class="adder grow">
              <EntityPicker
                id="bulk-parent"
                label="New parent class"
                ds={target}
                {prefixes}
                bind:value={parent}
                search={searchClasses}
                disabled={toTop}
              />
            </div>
          </div>
          <div class="row wrap">
            <label class="check"
              ><input type="checkbox" bind:checked={toTop} /> Move to the top</label
            >
            <label class="check"
              ><input type="checkbox" bind:checked={keepParents} disabled={toTop} /> Keep the old parents</label
            >
          </div>
        {/if}
        {#if mode === 'replace' || (mode === 'remove' && removeKind === 'lang')}
          <div class="row wrap">
            <label class="f">
              <span>Language</span>
              <select class="input" bind:value={langKind}>
                <option value="any">Any language</option>
                <option value="none">No language tag</option>
                <option value="tag">One language</option>
              </select>
            </label>
            {#if langKind === 'tag'}
              <label class="f lang"
                ><span>Tag</span><input class="input" bind:value={langTag} /></label
              >
            {/if}
          </div>
        {/if}
        <div class="row">
          <button class="btn" type="submit" disabled={busy || commit == null}>
            {busy ? 'Reading…' : mode === 'replace' ? 'Find' : 'List the changes'}
          </button>
        </div>
      </form>
      <datalist id="bulk-props">
        {#each propertyOptions as p (p)}<option value={p}></option>{/each}
      </datalist>

      {#if error}<p class="error-box" role="alert">{error}</p>{/if}
      {#if plan}
        <section class="plan" aria-label="Changes">
          {#if plan.refusal}
            <p class="error-box" role="alert">{plan.refusal}</p>
          {:else}
            <p>
              <strong>{plan.label}.</strong>
              {#if plan.rows.length > ROWS_SHOWN}The first {fmtInt(ROWS_SHOWN)} are listed.{/if}
            </p>
          {/if}
          {#each plan.notes as n, i (i)}<p class="faint small">{n}</p>{/each}
          {#if plan.rows.length}
            <div class="table-wrap">
              <table class="tbl">
                <thead>
                  <tr>
                    <th>Entity</th>
                    <th>{plan.mode === 'move' ? 'Parents' : 'Property'}</th>
                    <th>Before</th>
                    <th>After</th>
                  </tr>
                </thead>
                <tbody>
                  {#each plan.rows.slice(0, ROWS_SHOWN) as r, i (i)}
                    <tr>
                      <td title={r.subject}>{name(r.subject)}</td>
                      <td>{plan.mode === 'move' ? '' : name(r.property)}</td>
                      <td class="del">{r.before ?? ''}</td>
                      <td class="ins">{r.after ?? (plan.mode === 'move' ? 'the top' : '')}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {/if}
          {#if !current}
            <p class="warn-text">
              The ontology changed since these changes were listed. List them again.
            </p>
          {:else if plan.op && !readonly}
            <button
              class="btn primary"
              onclick={() => plan?.op && plan.mode === mode && onedit(plan.op, plan.label)}
              >Preview the edit</button
            >
          {/if}
        </section>
      {/if}
    </div>
    <section class="list" aria-labelledby="bulk-list-h">
      <h3 id="bulk-list-h">
        Entity list {#if entities.length}<span class="count-badge">{fmtInt(entities.length)}</span
          >{/if}
      </h3>
      <p class="faint small">
        Set, remove and move act on these entities, and a find can keep to them. The list holds at
        most {fmtInt(BULK_ENTITY_LIMIT)}.
      </p>
      <div class="adders">
        <div class="adder">
          <EntityPicker
            id="bulk-add-entity"
            label="Add an entity"
            ds={target}
            {prefixes}
            bind:value={picked}
            search={searchKind}
            onpicked={() => {
              if (picked?.term.type === 'uri' && addEntities([picked.term.value])) picked = null;
            }}
          />
        </div>
        <div class="adder row">
          <EntityPicker
            id="bulk-subtree"
            label="Add a class and its subclasses"
            ds={target}
            {prefixes}
            bind:value={subtreeOf}
            search={searchClasses}
          />
          <button class="btn sm" disabled={!subtreeOf || listBusy} onclick={addSubtree}
            >{listBusy ? 'Reading…' : 'Add subtree'}</button
          >
        </div>
        <label class="f">
          <span>Paste IRIs or prefixed names</span>
          <textarea class="input mono" rows="2" bind:value={paste}></textarea>
        </label>
        <div class="row">
          <button class="btn sm" disabled={!paste.trim()} onclick={addPasted}>Add pasted</button>
          {#if entities.length}
            <button
              class="btn ghost sm"
              onclick={() => {
                entities = [];
                invalidate();
                dropSubtreeRead();
              }}>Clear the list</button
            >
          {/if}
        </div>
      </div>
      {#if pasteBad.length}
        <p class="warn-text small">
          Not an IRI or a prefixed name: {pasteBad.slice(0, 5).join(', ')}{pasteBad.length > 5
            ? ` and ${pasteBad.length - 5} more`
            : ''}.
        </p>
      {/if}
      {#if listError}<p class="error-box" role="alert">{listError}</p>{/if}
      {#if entities.length}
        <ul class="chips" aria-label="Entities in the list">
          {#each entities.slice(0, 100) as e (e)}
            <li>
              <span title={e}>{name(e)}</span>
              <button
                class="btn icon ghost sm"
                aria-label="Remove {name(e)} from the list"
                onclick={() => {
                  entities = entities.filter((x) => x !== e);
                  invalidate();
                }}><Icon name="x" size={12} /></button
              >
            </li>
          {/each}
          {#if entities.length > 100}<li class="faint">
              and {fmtInt(entities.length - 100)} more
            </li>{/if}
        </ul>
      {/if}
    </section>
  </div>
</div>

<style>
  .bulk {
    display: grid;
    gap: 12px;
    min-width: 0;
  }
  /* four operations do not fit a phone's width in one row */
  @media (max-width: 560px) {
    .ops {
      display: grid;
      grid-template-columns: 1fr 1fr;
    }
  }
  .cols {
    display: grid;
    gap: 16px;
    min-width: 0;
  }
  @media (min-width: 1100px) {
    .cols {
      grid-template-columns: minmax(0, 1fr) minmax(260px, 340px);
      align-items: start;
    }
  }
  .main,
  .list,
  .form,
  .plan {
    display: grid;
    gap: 8px;
    min-width: 0;
  }
  h3 {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    margin: 0;
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .adders {
    display: grid;
    gap: 8px;
  }
  .row,
  .two {
    display: flex;
    gap: 8px;
    align-items: end;
    min-width: 0;
  }
  .wrap,
  .two {
    flex-wrap: wrap;
  }
  .grow {
    flex: 1 1 200px;
    min-width: 0;
  }
  .adder {
    min-width: 0;
  }
  .f {
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
    color: var(--text-2);
    min-width: 0;
  }
  .lang {
    width: 90px;
  }
  .check {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    font-size: var(--fs-sm);
  }
  .chips {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .chips li {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    padding: 0 0 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    font-size: var(--fs-sm);
    max-width: 100%;
    overflow-wrap: anywhere;
  }
  .plan > .btn {
    justify-self: start;
  }
  .table-wrap {
    overflow: auto;
    max-height: 360px;
  }
  table {
    width: 100%;
  }
  th {
    text-align: left;
  }
  td {
    overflow-wrap: anywhere;
    vertical-align: top;
  }
  td.del {
    background: var(--removed-soft);
  }
  td.ins {
    background: var(--added-soft);
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>

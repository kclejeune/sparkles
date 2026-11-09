<script lang="ts">
  import { iriForm } from '$lib/iri-form.svelte';
  // The ontology editor (docs/specs/U12-ontology-workbench/SPEC.md, "Ontology editor UI"):
  // the class, property and individual trees of a graph's ontology, an entity's frame and
  // its usage, the header, and the inferred class tree beside the asserted one. Every edit
  // is a dry run first and then one commit on the branch the page works on, with If-Match
  // naming the commit the page shows. The session's undo stack undoes the editor's commits
  // by reverting them, which restores exactly the triples each commit changed, and the
  // change feed refreshes the views when a commit lands.
  import PageActions from '$components/PageActions.svelte';
  import { useAskContext, useAskTarget } from '$lib/ask-thread.svelte';
  import { shortLabel } from '$lib/graph';
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onMount, untrack } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import * as api from '$lib/api';
  import { preferredLabel } from '$lib/rendering';
  import ClassificationFrame from '$components/ClassificationFrame.svelte';
  import InferredTreeView from '$components/InferredTree.svelte';
  import MaterializedFrame from '$components/MaterializedFrame.svelte';
  import { InferredPins } from '$lib/inferred-read';
  import type { InferredKind } from '$lib/inferred-tree.svelte';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { graphRestricted } from '$lib/auth';
  import { branchParam, MAIN, onBranch } from '$lib/branches';
  import { EditSession, startSessionBranch } from '$lib/edit-session.svelte';
  import { fmtCount, fmtInt } from '$lib/format';
  import { fetchHierarchy, fetchIndividuals, type Name, type TreeKind } from '$lib/browse';
  import { searchEntities } from '$lib/entity-search';
  import { IndividualTree, isGroup, groupIri } from '$lib/individual-tree.svelte';
  import { filteredTree, LEVEL_LIMIT, LevelTree, scrollToRow } from '$lib/level-tree.svelte';
  import { LabelBatch } from '$lib/label-batch';
  import { useCommands, usePageSearch, useProvider } from '$lib/launcher.svelte';
  import { branchProvider } from '$lib/palette.svelte';
  import {
    ancestorsOf,
    describeAxiom,
    annotationAssertion,
    buildTree,
    classifyOntology,
    dlQuery,
    DL_ANSWERS,
    declaration,
    entityName,
    filterTree,
    iriName,
    KIND_LABEL,
    KIND_SINGULAR,
    labelsOf,
    loadPolicy,
    manchester,
    mintIri,
    ONTOLOGY_GRAPHS,
    ontologyAxioms,
    ontologyEntities,
    ontologyKindLabels,
    ontologyLabelsFor,
    ontologyEntity,
    ontologySummary,
    RDFS_LABEL,
    renameUpdate,
    savePolicy,
    taxonomyTree,
    TREE_KINDS,
    underParent,
    usedQuery,
    validIri,
    validLang,
    opAxioms,
    parseManchester,
    type AxiomInput,
    type AxiomJson,
    type AxiomItem,
    type Classification,
    type DlAnswer,
    type DlQueryResult,
    type DlReasoning,
    type EntityKind,
    type ManchesterProblem,
    type ParseAs,
    type IriPolicy,
    type Lit,
    type Op,
    type OntologySummary,
    type Selection,
  } from '$lib/ontology';
  import Icon from '$components/Icon.svelte';
  import FollowState from '$components/FollowState.svelte';
  import ManchesterEditor from '$components/ManchesterEditor.svelte';
  import type { NameOption } from '$lib/manchester-complete';
  import Modal from '$components/Modal.svelte';
  import OntoFrame from '$components/OntoFrame.svelte';
  import OntoHeader from '$components/OntoHeader.svelte';
  import OntoPreview from '$components/OntoPreview.svelte';
  import OntoTree from '$components/OntoTree.svelte';
  import EditorHistory from '$components/EditorHistory.svelte';
  import EntityHistory from '$components/EntityHistory.svelte';
  import Discussions from '$components/Discussions.svelte';
  import BulkEdit from '$components/BulkEdit.svelte';
  import OntoImports from '$components/OntoImports.svelte';
  import { isDiscussionGraph } from '$lib/discussions';
  import TreeCrumbs from '$components/TreeCrumbs.svelte';

  import { ONTOLOGY_TABS, routes, type OntologyTab } from '$lib/routes';

  const TAB_LABEL: [OntologyTab, string][] = [
    ['tree', 'Tree'],
    ['dlquery', 'DL Query'],
    ['header', 'Header'],
    ['history', 'History'],
    ['discussions', 'Discussions'],
    ['bulk', 'Bulk edits'],
    ['imports', 'Imports'],
  ];

  type Tab = OntologyTab;

  // --- the URL's state ---------------------------------------------------------------
  const q = $derived(page.url.searchParams);
  const name = $derived(q.get('ds') ?? app.current ?? '');
  const branch = $derived(branchParam(q.get('branch')));
  const target = $derived(onBranch(name, branch));
  const graph = $derived(q.get('graph') || null);
  // `imports=0` excludes the imported axioms from every view (spec U12, import resolution)
  const withImports = $derived(q.get('imports') !== '0');
  const sel: Selection = $derived({ graph, imports: withImports });
  // the page's one tab row: the tree, DL Query, the header and the history (ROADMAP item 633)
  const tab = $derived<Tab>(
    (ONTOLOGY_TABS as readonly string[]).includes(q.get('tab') ?? '')
      ? (q.get('tab') as Tab)
      : 'tree',
  );
  /** The tree's entity kind, a control at the top of the tree rather than a second tab row. */
  const kind = $derived<EntityKind>(
    (TREE_KINDS as string[]).includes(q.get('kind') ?? '')
      ? (q.get('kind') as EntityKind)
      : 'Class',
  );
  /** The entity kind of the tree, when the page shows it. */
  const kindTab = $derived<EntityKind | null>(tab === 'tree' ? kind : null);
  const selected = $derived(q.get('iri'));
  const view = $derived(
    (['usage', 'history', 'discussion'] as const).find((v) => v === q.get('view')) ?? 'frame',
  );
  const inferredOn = $derived(q.get('inferred') === '1');
  const inferredKind = $derived<InferredKind | null>(
    kindTab && ['Class', 'ObjectProperty', 'DataProperty', 'NamedIndividual'].includes(kindTab)
      ? (kindTab as InferredKind)
      : null,
  );
  const prefixes = $derived(app.prefixes(name));
  // the entity on screen is the context of a question asked from the Ask drawer
  useAskContext(() =>
    selected
      ? {
          kind: 'entity',
          iri: selected,
          label: shortLabel(selected, prefixes),
          type: 'resource',
        }
      : null,
  );
  useAskTarget(() => ({ branch }));

  function withParams(over: Record<string, string | null>) {
    const p = new URLSearchParams(page.url.searchParams);
    if (!p.get('ds') && name) p.set('ds', name);
    for (const [k, v] of Object.entries(over)) {
      if (v == null || v === '') p.delete(k);
      else p.set(k, v);
    }
    return routes.ontology(p.get('ds') ?? name, Object.fromEntries(p));
  }
  const nav = (over: Record<string, string | null>, replace = false) =>
    goto(withParams(over), { replaceState: replace, keepFocus: true, noScroll: true });

  $effect(() => {
    if (name && name !== app.current && app.datasets.some((d) => d.name === name))
      app.setDataset(name);
    if (name) void app.loadPrefixes(name);
  });

  // --- what the page read -------------------------------------------------------------
  let summary = $state<OntologySummary | null>(null);
  const inferredPins = $derived(
    new InferredPins(JSON.stringify([target, graph, sel.imports, summary?.commit])),
  );
  let etag = $state<string | null>(null);
  let loadError = $state<api.ApiError | Error | null>(null);
  let loading = $state(false);
  // What the page reads is what its views show (ROADMAP item 725): the summary when it
  // opens, the levels of the tree on screen, the labels of the names it draws, and the
  // lists of entities only when a picker or completion needs them.
  /** The entities of the kinds listed so far, for the pickers. */
  let entities = $state<Map<EntityKind, string[]>>(new Map());
  /** The kinds whose list was read, or skipped for being longer than `LIST_MAX`. */
  let listed = $state<Set<EntityKind>>(new Set());
  /** The kinds too long to list: their names are found by the server's search. */
  let bigKinds = $state<Set<EntityKind>>(new Set());
  /** The labels the views know. A name read since the page opened is added in place. */
  const labels = new SvelteMap<string, Lit[]>();
  /** All languages in the selected graph; hierarchy reads also include imported graphs. */
  const graphLabels = new Map<string, Lit[]>();
  /** The current language's authoritative hierarchy names, including imported labels. */
  const hierarchyNames = new Set<string>();
  /** A failed read of the lists is not tried again until the person asks for a list anew. */
  let listFailed = $state(false);
  /** The most entities of a kind that the pickers list. A longer kind is searched instead. */
  const LIST_MAX = 5_000;
  /** The kinds the pickers and completion list. */
  const PICKER_KINDS: EntityKind[] = [
    'Class',
    'ObjectProperty',
    'DataProperty',
    'AnnotationProperty',
    'NamedIndividual',
    'Datatype',
  ];
  let graphs = $state<string[]>([]);
  let branchList = $state<api.Branch[]>([]);
  /** The commit the views show, which a change after it makes stale. */
  let shown = 0;
  /** The target whose first read the views show, which the change feed waits for. */
  let loadedTarget: string | null = null;
  /**
   * The latest request of each loader. An answer to an older one, such as a read of the
   * graph or dataset the page showed before, is dropped.
   */
  let loadSeq = 0;
  let frameSeq = 0;
  let classifySeq = 0;

  const langs = $derived(iriForm.languages);
  /** The name of `iri` as a tree draws it: the tree's own levels carry the labels. */
  const treeName = (iri: string) => entityName(iri, labels, prefixes, langs);
  /** A local batch never replaces the hierarchy's choice over its wider import scope. */
  function addLabels(more: Map<string, Lit[]>) {
    for (const [s, l] of more) {
      graphLabels.set(s, l);
      if (!hierarchyNames.has(s)) labels.set(s, l);
    }
  }
  const batch = new LabelBatch(
    (iris) => ontologyLabelsFor(target, sel, iris),
    (m) => addLabels(m),
  );
  /** The name of `iri`, reading its label when the views name it for the first time. */
  const nameOf = (iri: string) => {
    if (iriForm.mode === 'label' && !labels.get(iri)?.length) batch.want(iri);
    return treeName(iri);
  };
  /** The short forms the hierarchy route gives, such as GO:0008150, as the browse pages show them. */
  let shortForms = $state<Map<string, string>>(new Map());
  const subOf = (iri: string) =>
    iriForm.mode === 'label' && labels.get(iri)?.length
      ? (shortForms.get(iri) ?? iriName(iri, prefixes))
      : null;

  async function loadAll() {
    const t = target;
    const s = sel;
    if (!name) return;
    const my = ++loadSeq;
    loading = true;
    try {
      // the summary alone: its counts head the page, and the tree reads its own levels
      const r = await ontologySummary(t, s);
      if (my !== loadSeq) return;
      // what was read for the commit before is read again when a view needs it, and what the
      // first summary of a scope arrives beside, such as the labels of the tree, is kept
      if (loadedTarget === t && shown !== r.summary.commit) resetLazy();
      summary = r.summary;
      etag = r.etag;
      shown = r.summary.commit;
      loadedTarget = t;
      loadError = null;
    } catch (e) {
      if (my === loadSeq) loadError = e as Error;
    } finally {
      if (my === loadSeq) loading = false;
    }
  }

  /** Forget the lists and labels read, which belong to a commit or graph the page left. */
  function resetLazy() {
    batch.reset();
    labels.clear();
    graphLabels.clear();
    hierarchyNames.clear();
    entities = new Map();
    listed = new Set();
    bigKinds = new Set();
    listFailed = false;
    listSeq++;
  }
  let listSeq = 0;

  /**
   * Read the entities of `kinds` that the pickers and completion list, once for each kind.
   * A kind with more than `LIST_MAX` entities is not listed: the server's search finds its
   * names as they are typed. The labels of the kinds read come in one query.
   */
  async function ensureKinds(kinds: EntityKind[]) {
    if (!summary) return;
    const want = kinds.filter((k) => !listed.has(k));
    if (!want.length) return;
    const my = listSeq;
    const t = target;
    const s = sel;
    const counts = summary.entities as Partial<Record<EntityKind, number>>;
    const big = want.filter((k) => (counts[k] ?? 0) > LIST_MAX);
    const read = want.filter((k) => !big.includes(k));
    listed = new Set([...listed, ...want]);
    if (big.length) bigKinds = new Set([...bigKinds, ...big]);
    if (!read.length) return;
    try {
      const [ents, more] = await Promise.all([
        ontologyEntities(t, s, read),
        ontologyKindLabels(t, s, read).catch(() => new Map<string, Lit[]>()),
      ]);
      if (my !== listSeq) return;
      const next = new Map(entities);
      const byKind = new Map<EntityKind, string[]>();
      for (const e of ents.entities) byKind.set(e.kind, [...(byKind.get(e.kind) ?? []), e.iri]);
      for (const k of read)
        next.set(k, [...new Set([...(next.get(k) ?? []), ...(byKind.get(k) ?? [])])]);
      entities = next;
      addLabels(more);
      batch.know(more.keys());
    } catch {
      // the pickers stay empty, and the next need the person creates reads the lists again
      if (my === listSeq) {
        listed = new Set([...listed].filter((k) => !read.includes(k)));
        listFailed = true;
      }
    }
  }

  async function loadSide() {
    const ds = name;
    if (!ds) return;
    try {
      const rows = await api.select(onBranch(ds, branch), ONTOLOGY_GRAPHS, { nocache: true });
      // a discussion graph is not an ontology even if a comment's text were to declare one
      graphs = rows
        .map((r) => (r.g?.type === 'uri' ? r.g.value : ''))
        .filter((g) => g && !isDiscussionGraph(g));
    } catch {
      graphs = [];
    }
    try {
      branchList = (await api.branches(ds)).branches;
    } catch {
      branchList = [];
    }
  }

  $effect(() => {
    void target;
    void graph;
    untrack(() => {
      // a preview belongs to the target and graph it ran on
      summary = null;
      shown = 0;
      loadedTarget = null;
      // the names, lists and levels of the scope before are another scope's, and a read of a
      // tree not on screen must not bring them back
      resetLazy();
      for (const lt of levelTrees.values()) lt.drop();
      individualLevel.drop();
      classification = null;
      session.reset(`sparkles.onto.undo.${target}|${graph ?? ''}`);
      void loadAll();
      void loadSide();
    });
  });

  // including or excluding the imported axioms reads every view again, and keeps the undo stack
  let importsShown: boolean | null = null;
  $effect(() => {
    const w = withImports;
    untrack(() => {
      if (importsShown !== null && importsShown !== w) {
        summary = null;
        shown = 0;
        loadedTarget = null;
        resetLazy();
        for (const lt of levelTrees.values()) lt.drop();
        individualLevel.drop();
        classification = null;
        void loadAll();
      }
      importsShown = w;
    });
  });

  // --- trees ---------------------------------------------------------------------------
  let filter = $state('');
  let opened = $state<Record<string, Set<string>>>({});
  const openSet = (key: string) => opened[key] ?? new Set<string>();
  function toggle(key: string, id: string) {
    const s = new Set(openSet(key));
    if (s.has(id)) s.delete(id);
    else s.add(id);
    opened = { ...opened, [key]: s };
  }
  // --- the class and property trees, one level at a time (item 607) ----------------------
  // The hierarchy route reads a level from the store's indexes, so these trees are whole
  // however large the ontology, where the capped listings above stop at 50,000 entries.
  const LEVEL_KIND: Partial<Record<EntityKind, TreeKind>> = {
    Class: 'class',
    ObjectProperty: 'objectProperty',
    DataProperty: 'dataProperty',
    AnnotationProperty: 'annotationProperty',
  };
  /** The labels the hierarchy answers carry, merged into the page's labels. */
  function takeNames(nodes: Name[]) {
    for (const n of nodes) {
      if (n.label?.trim()) {
        hierarchyNames.add(n.iri);
        labels.set(n.iri, [
          { '@value': n.label, ...(n.labelLang ? { '@language': n.labelLang } : {}) },
        ]);
      } else {
        hierarchyNames.delete(n.iri);
        labels.set(n.iri, graphLabels.get(n.iri) ?? []);
        // A blank literal can rank ahead of usable literals in the server's choice.
        if (n.label != null && !graphLabels.has(n.iri)) batch.want(n.iri);
      }
    }
    // A missing label is known absent; a blank literal may have usable alternatives.
    batch.know(nodes.filter((n) => n.label == null).map((n) => n.iri));
    const forms = nodes.filter((n) => n.shortForm && !shortForms.has(n.iri));
    if (forms.length)
      shortForms = new Map([...shortForms, ...forms.map((n) => [n.iri, n.shortForm!] as const)]);
  }
  const levelTrees = new Map<EntityKind, LevelTree>(
    Object.entries(LEVEL_KIND).map(([k, kind]) => [
      k as EntityKind,
      new LevelTree(
        (q) =>
          fetchHierarchy(target, sel, {
            kind,
            iri: q.iri,
            ancestors: q.ancestors,
            limit: q.ancestors ? 1 : LEVEL_LIMIT,
            offset: q.offset,
            lang: langs,
          }),
        takeNames,
      ),
    ]),
  );
  const individualLevel = new IndividualTree(
    (q, signal) =>
      fetchIndividuals(
        target,
        sel,
        {
          ...q,
          at: q.at ?? (summary ? `commit:${summary.commit}` : undefined),
          lang: langs,
        },
        signal,
      ),
    takeNames,
  );
  function individualName(id: string): string {
    return id === '@untyped' ? 'Untyped' : treeName(groupIri(id) ?? id);
  }
  function individualSub(id: string): string | null {
    return id === '@untyped' ? null : subOf(groupIri(id) ?? id);
  }
  function individualSelect(id: string) {
    if (isGroup(id)) {
      toggle('NamedIndividual', id);
      if (openSet('NamedIndividual').has(id)) void individualLevel.load(id);
    } else nav({ iri: id });
  }
  /** The level tree of the tab, when its kind has a hierarchy. */
  const level = $derived(kindTab ? (levelTrees.get(kindTab) ?? null) : null);
  // a new target, graph or commit reads the open levels again, of the tree on screen now and
  // of each other tree when its tab opens
  let levelEpoch = $state(0);
  const levelRead = new Map<EntityKind, number>();
  // The tree reads at once, without waiting for the summary, which on a large ontology takes
  // longer than the first level. A later commit reads the levels again.
  let levelScope = '';
  let levelContext = '';
  let levelCommit: number | null = null;
  $effect(() => {
    const context = `${target}\u0000${graph ?? ''}`;
    const scope = `${context}\u0000${langs.join(',')}`;
    const commit = summary?.commit ?? null;
    untrack(() => {
      if (scope !== levelScope) {
        batch.reset();
        hierarchyNames.clear();
        labels.clear();
        if (context !== levelContext) {
          graphLabels.clear();
        } else {
          for (const [s, l] of graphLabels) labels.set(s, l);
          batch.know(graphLabels.keys());
        }
        levelContext = context;
        for (const lt of levelTrees.values()) lt.drop();
        individualLevel.drop();
        levelScope = scope;
        levelCommit = commit;
        levelEpoch++;
      } else if (commit != null && commit !== levelCommit) {
        const first = levelCommit == null;
        levelCommit = commit;
        if (!first) {
          // a read of a tree not on screen, begun for the commit before, must not land
          for (const lt of levelTrees.values()) lt.drop();
          individualLevel.drop();
          levelEpoch++;
        }
      }
    });
  });
  $effect(() => {
    const epoch = levelEpoch;
    const k = kindTab;
    const lt = k === 'NamedIndividual' ? individualLevel : level;
    // Individuals keep the summary-first opening and anchor every page to its commit.
    if (k === 'NamedIndividual' && summary?.commit == null) return;
    if (!k || !lt || !epoch || levelRead.get(k) === epoch) return;
    levelRead.set(k, epoch);
    untrack(() => lt.reset(openSet(k)));
  });
  function toggleLevel(k: EntityKind, id: string) {
    toggle(k, id);
    if (openSet(k).has(id)) void levelTrees.get(k)?.load(id);
  }
  // a filter reads the search's hits of the tab's kind and their ancestors
  let filtered = $state<{
    text: string;
    tree: ReturnType<typeof filteredTree>;
    pending?: boolean;
    error?: string;
    absent?: boolean;
  } | null>(null);
  // the dataset, branch and graph the filtered tree was read in
  let filteredIn = '';
  $effect(() => {
    const text = filter.trim();
    const k = kindTab;
    const kind = k === 'NamedIndividual' ? 'individual' : k ? LEVEL_KIND[k] : undefined;
    // the scope is read here, where the effect tracks it, not in the timer below
    const scopeTarget = target;
    const scopeGraph = graph;
    const individualCommit = k === 'NamedIndividual' ? (summary?.commit ?? null) : null;
    const scope = `${scopeTarget}\u0000${scopeGraph ?? ''}\u0000${langs.join(',')}\u0000${individualCommit ?? ''}`;
    if (scope !== filteredIn) {
      // another dataset, branch or graph must not keep showing the last scope's hits
      filteredIn = scope;
      filtered = null;
    }
    if (!kind || text.length < 2 || (kind === 'individual' && individualCommit == null)) {
      filtered = null;
      return;
    }
    filtered = { text, tree: filteredTree([]), pending: true };
    const ctl = new AbortController();
    const t = setTimeout(async () => {
      try {
        if (ctl.signal.aborted) return;
        const r = await searchEntities(scopeTarget, text, {
          kind: [kind],
          limit: 20,
          graph: scopeGraph,
          signal: ctl.signal,
        });
        if (kind === 'individual') {
          const results = await Promise.allSettled(
            r.results.map((h) =>
              fetchIndividuals(
                scopeTarget,
                { graph: scopeGraph, imports: true },
                {
                  iri: h.iri,
                  at: `commit:${individualCommit}`,
                  lang: langs,
                },
                ctl.signal,
              ),
            ),
          );
          if (ctl.signal.aborted) return;
          const pages: Awaited<ReturnType<typeof fetchIndividuals>>[] = [];
          const errors: string[] = [];
          let absent = false;
          for (const result of results) {
            if (result.status === 'rejected') {
              if (result.reason instanceof api.ApiError && result.reason.status === 404)
                absent = true;
              else errors.push(api.errorMessage(result.reason));
            } else if (result.value.commit !== individualCommit) {
              errors.push('A matching entity was read from another revision.');
            } else pages.push(result.value);
          }
          const ids = new Set<string>();
          const edges: [string, string][] = [];
          for (const p of pages) {
            takeNames([
              ...p.groups.flatMap((g) => (g.type ? [g.type] : [])),
              ...p.individuals.flatMap((i) => [i, ...i.types]),
            ]);
            for (const i of p.individuals) {
              ids.add(i.iri);
              for (const g of p.groups) {
                const key = g.type ? '@type:' + g.type.iri : '@untyped';
                ids.add(key);
                edges.push([i.iri, key]);
              }
            }
          }
          filtered = {
            text,
            tree: buildTree(ids, edges),
            absent,
            ...(errors.length
              ? { error: `Some matching entities could not be read. ${errors[0]}` }
              : {}),
          };
          return;
        }
        const answers = await Promise.all(
          r.results.map((h) =>
            fetchHierarchy(
              scopeTarget,
              { graph: scopeGraph, imports: true },
              { kind, iri: h.iri, ancestors: true, limit: 1 },
              ctl.signal,
            ),
          ),
        );
        if (ctl.signal.aborted) return;
        for (const a of answers)
          takeNames([...(a.node ? [a.node] : []), ...(a.ancestors?.nodes ?? [])]);
        filtered = { text, tree: filteredTree(answers) };
      } catch (e) {
        if (!ctl.signal.aborted)
          filtered = { text, tree: filteredTree([]), error: api.errorMessage(e) };
      }
    }, 200);
    return () => {
      clearTimeout(t);
      ctl.abort();
    };
  });

  // open the path to the selection: through the route's ancestors in a level tree
  $effect(() => {
    const iri = selected;
    const k = kindTab;
    const lt = k === 'NamedIndividual' ? individualLevel : level;
    if (!iri || !lt || !k) return;
    if (k === 'NamedIndividual' && summary?.commit == null) return;
    void summary?.commit;
    // after the levels were read again, which this reads the path of
    void levelEpoch;
    let active = true;
    untrack(() => {
      void lt.reveal(iri).then((anc) => {
        if (!active) return;
        const cur = openSet(k);
        if (!anc.every((a) => cur.has(a))) opened = { ...opened, [k]: new Set([...cur, ...anc]) };
        scrollToRow(iri);
      });
    });
    return () => {
      active = false;
    };
  });

  // the lists that the pickers and completion read: the page reads them once an entity, the
  // header, the DL query or the create dialog needs them, and not when it opens
  let needKey = '';
  $effect(() => {
    const need =
      !!selected || tab === 'header' || tab === 'dlquery' || tab === 'bulk' || createOpen;
    const key = `${selected ?? ''}\u0000${tab}\u0000${createOpen}`;
    const missing = PICKER_KINDS.some((k) => !listed.has(k));
    const failed = listFailed;
    const ready = !!summary;
    untrack(() => {
      // another need is a new try
      if (key !== needKey) {
        needKey = key;
        if (failed) {
          listFailed = false;
          return;
        }
      }
      if (need && ready && missing && !failed) void ensureKinds(PICKER_KINDS);
    });
  });
  // --- the inferred tree ---------------------------------------------------------------
  let classification = $state<Classification | null>(null);
  let classifyError = $state<string | null>(null);
  let classifying = $state(false);
  let classifyAbort: AbortController | null = null;
  let inferredScope: { target: string; selection: Selection; commit: number } | null = null;
  const inferredLabels = new SvelteMap<string, Lit[]>();
  const inferredBatch = new LabelBatch(
    (iris) => {
      const scope = inferredScope;
      return scope
        ? ontologyLabelsFor(
            scope.target,
            scope.selection,
            iris,
            undefined,
            `commit:${scope.commit}`,
          )
        : Promise.resolve(new Map());
    },
    (more) => {
      for (const [iri, labels] of more) inferredLabels.set(iri, labels);
    },
  );
  function clearClassification() {
    classifySeq++;
    classifyAbort?.abort();
    classifyAbort = null;
    classification = null;
    inferredScope = null;
    inferredBatch.reset();
    inferredLabels.clear();
    classifying = false;
    classifyError = null;
  }
  async function classify() {
    if (!summary || classifying) return;
    const my = ++classifySeq;
    const input = { target, selection: { ...sel }, commit: summary.commit };
    classifyAbort?.abort();
    const abort = new AbortController();
    classifyAbort = abort;
    classifying = true;
    classifyError = null;
    try {
      const c = await classifyOntology(
        input.target,
        input.selection,
        abort.signal,
        `commit:${input.commit}`,
      );
      if (my !== classifySeq) return;
      inferredScope = { ...input, commit: c.commit };
      inferredBatch.reset();
      inferredLabels.clear();
      classification = c;
    } catch (e) {
      if (my === classifySeq) classifyError = api.errorMessage(e);
    } finally {
      if (my === classifySeq) {
        classifying = false;
        classifyAbort = null;
      }
    }
  }
  // Reading/toggling the tree, including inferred=1, never starts classification.
  $effect(() => {
    void target;
    void graph;
    void summary?.commit;
    untrack(clearClassification);
    return () => classifyAbort?.abort();
  });
  const inferred = $derived(classification ? taxonomyTree(classification) : null);
  const inferredIriName = (iri: string) => {
    if (iriForm.mode !== 'label') return iri;
    inferredBatch.want(iri);
    return preferredLabel(inferredLabels.get(iri), langs) ?? iri;
  };
  const inferredName = (id: string) => {
    const cs = inferred?.classesOf.get(id) ?? [];
    return cs.length ? cs.map(inferredIriName).join(' ≡ ') : id;
  };
  const inferredKeep = $derived(
    inferred && filter.trim() ? filterTree(inferred.tree, inferredName, filter) : null,
  );
  const inferredParents = (iri: string) => {
    const n = inferred?.nodeOf.get(iri);
    if (!n || !inferred) return [];
    return (inferred.tree.parents.get(n) ?? []).flatMap((p) => inferred.classesOf.get(p) ?? []);
  };
  $effect(() => {
    const iri = selected;
    const inf = inferred;
    if (!iri || !inf) return;
    const n = inf.nodeOf.get(iri);
    if (!n) return;
    const anc = ancestorsOf(inf.tree, n);
    untrack(() => {
      const cur = openSet('inferred');
      if (anc.every((a) => cur.has(a))) return;
      opened = { ...opened, inferred: new Set([...cur, ...anc]) };
    });
  });

  // --- the selected entity ---------------------------------------------------------------
  let frameItems = $state<AxiomItem[]>([]);
  let frameCommit = $state<number | null>(null);
  let frameKinds = $state<EntityKind[]>([]);
  let frameError = $state<string | null>(null);
  let frameFor = $state<string | null>(null);
  let frameScope = '';
  let framePending: { key: string; promise: Promise<void> } | null = null;
  let frameAbort: AbortController | null = null;
  async function loadFrame() {
    const iri = selected;
    const t = target;
    const commit = summary?.commit;
    const selection = { ...sel };
    const scope = JSON.stringify([t, selection.graph, selection.imports]);
    if (!iri || commit == null) {
      frameSeq++;
      frameAbort?.abort();
      framePending = null;
      frameItems = [];
      frameKinds = [];
      frameFor = null;
      frameCommit = null;
      return;
    }
    const key = JSON.stringify([scope, iri, commit]);
    if (frameScope === scope && frameFor === iri && frameCommit === commit) return;
    if (framePending?.key === key) return framePending.promise;
    const my = ++frameSeq;
    frameAbort?.abort();
    const abort = new AbortController();
    frameAbort = abort;
    if (frameScope !== scope || frameFor !== iri) {
      frameFor = null;
      frameCommit = null;
    }
    frameError = null;
    const pending = { key, promise: Promise.resolve() };
    framePending = pending;
    pending.promise = (async () => {
      try {
        const [report, ax] = await Promise.all([
          ontologyEntity(t, selection, iri, abort.signal, `commit:${commit}`),
          ontologyAxioms(
            t,
            selection,
            { about: iri, assertions: true, cap: 5_000, at: `commit:${commit}` },
            abort.signal,
          ).catch((e) => {
            if (e instanceof api.ApiError && e.status === 404)
              return { axioms: [], total: 0, truncated: false };
            throw e;
          }),
        ]);
        if (my !== frameSeq) return;
        frameKinds = report?.kinds ?? [];
        frameItems = ax.axioms;
        frameCommit = commit;
        frameScope = scope;
        frameFor = iri;
        frameError = report ? null : 'The ontology neither declares nor uses this IRI.';
      } catch (e) {
        if (my === frameSeq) frameError = api.errorMessage(e);
      } finally {
        if (framePending === pending) framePending = null;
        if (frameAbort === abort) frameAbort = null;
      }
    })();
    return pending.promise;
  }
  $effect(() => () => {
    frameSeq++;
    frameAbort?.abort();
  });
  $effect(() => {
    void selected;
    void target;
    void graph;
    void summary?.commit;
    untrack(() => void loadFrame());
  });
  /** The kind the frame shows: the tab's when the entity has it. */
  const frameKind = $derived<EntityKind>(
    kindTab && frameKinds.includes(kindTab)
      ? kindTab
      : (frameKinds.find((k) => k !== 'Datatype') ?? frameKinds[0] ?? 'Class'),
  );
  const options = (k: EntityKind) =>
    (k === 'Datatype'
      ? [...(entities.get('Datatype') ?? []), ...XSD_TYPES]
      : (entities.get(k) ?? [])
    ).map((iri) => ({
      iri,
      name: nameOf(iri),
    }));
  const XSD_TYPES = [
    'string',
    'boolean',
    'integer',
    'decimal',
    'double',
    'float',
    'date',
    'dateTime',
    'anyURI',
    'gYear',
  ].map((t) => `http://www.w3.org/2001/XMLSchema#${t}`);
  const annotationProperties = $derived(entities.get('AnnotationProperty') ?? []);
  /** The names that Manchester fields complete: labels and prefixed names of every kind. */
  const completionNames = $derived.by(() => {
    const out: NameOption[] = [];
    for (const k of [...TREE_KINDS, 'Datatype'] as EntityKind[])
      for (const iri of k === 'Datatype'
        ? [...(entities.get('Datatype') ?? []), ...XSD_TYPES]
        : (entities.get(k) ?? [])) {
        const kind = KIND_SINGULAR[k];
        const shown = nameOf(iri);
        out.push({ name: shown, iri, kind });
        const short = iriName(iri, prefixes);
        if (short !== shown) out.push({ name: short, iri, kind });
      }
    return out;
  });
  const names = () => completionNames;
  /** Names of the kinds too long to list, found by the server's search as they are typed. */
  async function searchNames(word: string, signal: AbortSignal): Promise<NameOption[]> {
    const r = await searchEntities(target, word, { limit: 20, graph, signal });
    return r.results.map((h) => ({
      name: h.label ?? h.shortForm ?? iriName(h.iri, prefixes),
      iri: h.iri,
      kind: h.kinds[0] ?? 'entity',
    }));
  }
  const parse = (text: string, as: ParseAs) => parseManchester(target, sel, text, as);

  // --- the DL query --------------------------------------------------------------------
  let dlText = $state('');
  let dlAnswers = $state<DlAnswer[]>(DL_ANSWERS.map(([k]) => k));
  let dlReasoning = $state<DlReasoning>('auto');
  let dlResult = $state<DlQueryResult | null>(null);
  let dlProblem = $state<ManchesterProblem | null>(null);
  let dlError = $state<string | null>(null);
  let dlBusy = $state(false);
  /**
   * The query text the answers or the problem shown are for, with the commit the page
   * showed and the dataset, branch and graph it read.
   */
  let dlRan = $state<{ text: string; commit: number | null; scope: string } | null>(null);
  let dlSeq = 0;
  /** The run in flight: its scope and the query the address held when it started. */
  let dlPending: { scope: string; url: string | null } | null = null;
  const dlScope = () => `${target}\u0000${JSON.stringify(sel)}`;
  /** Forget the run in flight, whose answer no longer fits the page. */
  function dropDl() {
    dlSeq++;
    dlPending = null;
    dlBusy = false;
  }
  async function runDl(text = dlText) {
    // the text is sent as it is, so that a problem's span fits the editor's text
    if (!text.trim()) return;
    const my = ++dlSeq;
    const scope = dlScope();
    const commit = summary?.commit ?? null;
    dlPending = { scope, url: q.get('dl') };
    dlBusy = true;
    dlError = null;
    try {
      const r = await dlQuery(target, sel, {
        expression: text,
        answers: dlAnswers,
        reasoning: dlReasoning,
      });
      if (my !== dlSeq || dlScope() !== scope) return;
      dlPending = null;
      if (r.ok) {
        dlResult = r.result;
        dlProblem = null;
      } else {
        dlProblem = r.problem;
        dlResult = null;
      }
      dlRan = { text, commit, scope };
      // each query run is a step of the history, so that Back shows the one before
      if (q.get('dl') !== text) void nav({ dl: text });
    } catch (e) {
      if (my === dlSeq) dlError = api.errorMessage(e);
    } finally {
      if (my === dlSeq) dlBusy = false;
    }
  }
  // a link to the tab with a query runs it and shows it in the editor, and a commit the
  // views show, or another dataset, branch or graph, runs it again
  $effect(() => {
    const text = q.get('dl');
    const commit = summary?.commit;
    const scope = dlScope();
    untrack(() => {
      // Back, another query in the address or another scope makes the run in flight moot
      if (dlPending && (dlPending.scope !== scope || dlPending.url !== text)) dropDl();
      if (dlRan && dlRan.scope !== scope) {
        dlResult = null;
        dlProblem = null;
      }
    });
    if (tab !== 'dlquery' || !text || commit == null) return;
    untrack(() => {
      if (dlRan && dlRan.text === text && dlRan.commit === commit && dlRan.scope === scope) return;
      if (dlRan && dlRan.scope !== scope) {
        dlResult = null;
        dlProblem = null;
      }
      if (!dlRan || dlRan.text !== text) dlText = text;
      void runDl(text);
    });
  });
  /** A reason or note of the server, which is a clause, as a sentence. */
  const sentence = (t: string) =>
    t ? t[0].toUpperCase() + t.slice(1) + (/[.!?]$/.test(t) ? '' : '.') : t;
  function toggleAnswer(k: DlAnswer, on: boolean) {
    dlAnswers = DL_ANSWERS.map(([a]) => a).filter((a) => (a === k ? on : dlAnswers.includes(a)));
  }
  const usageGroups = $derived.by(() => {
    const g = new Map<string, AxiomItem[]>();
    for (const it of frameItems) g.set(it.type, [...(g.get(it.type) ?? []), it]);
    return [...g].sort((a, b) => a[0].localeCompare(b[0]));
  });
  const mentionedBy = (it: AxiomItem) => {
    const out = new Set<string>();
    const walk = (v: unknown) => {
      if (typeof v === 'string') {
        if (v !== selected && validIri(v) && v.includes('/')) out.add(v);
      } else if (Array.isArray(v)) v.forEach(walk);
      else if (v && typeof v === 'object')
        for (const [k, x] of Object.entries(v)) if (k !== 'type' && k !== 'kind') walk(x);
    };
    walk(it.axiom);
    return [...out];
  };

  // --- access and the session -------------------------------------------------------------
  const currentBranch = $derived(branchList.find((b) => b.name === (branch ?? MAIN)));
  const canWrite = $derived(auth.can(name, 'write'));
  const protectedBranch = $derived(!!currentBranch?.protected);
  const readonly = $derived(!canWrite || protectedBranch);
  /** The name a comment records as its creator. */
  const commenter = $derived(auth.who?.principal?.name ?? auth.who?.principal?.kind ?? 'anonymous');
  let sessionError = $state<string | null>(null);
  async function startSession() {
    sessionError = null;
    try {
      const n = await startSessionBranch(name, {
        from: branch,
        branches: branchList,
        user: auth.who?.principal?.name,
        prefix: 'onto',
        note: 'ontology editing session',
      });
      await nav({ branch: n });
    } catch (e) {
      sessionError = api.errorMessage(e);
    }
  }

  // --- edits, previews and the undo stack ---------------------------------------------------
  let previewPin = $state.raw<{
    op: Op;
    target: string;
    sel: Selection;
    etag: string;
    head: number | null;
    frameContext: string | null;
  } | null>(null);
  const frameContext = $derived(
    JSON.stringify([target, sel.graph, sel.imports, selected, frameKind]),
  );
  const frameSource = $derived(
    JSON.stringify([target, sel.graph, sel.imports, selected, frameKind, frameCommit]),
  );
  const session = new EditSession({
    reload: async () => {
      await loadAll();
      await loadFrame();
    },
    staleText: 'Another commit landed since the editor read the ontology.',
  });
  /**
   * A comment, resolve or reopen committed `seq`: an ordinary commit of the branch, which
   * moves the head the next edit names, so the views read it again. Undo leaves it alone.
   */
  const discussed = (seq: number | null) => {
    if (seq != null) session.own.add(seq);
    void loadAll();
  };
  /**
   * Preview `op`. `selection` is the graphs the edit reads and writes, the page's own unless
   * the edit was planned from another reading, as a bulk edit is from the graph alone.
   */
  const edit = (
    op: Op,
    label: string,
    after?: () => void,
    fromFrame: string | null = null,
    selection: Selection = sel,
  ) => {
    // an edit names the commit the summary read, which a large ontology's summary may still be reading
    if (readonly || !etag)
      return void toasts.push('info', 'The ontology is still being read', 'Try again in a moment.');
    const origin = { target, sel: { ...selection }, etag, head: summary?.commit ?? null };
    session.edit(op, label, after);
    previewPin = { ...origin, op: session.previewOp!, frameContext: fromFrame };
  };
  const editFrame = (op: Op, label: string, origin: string) => {
    if (
      readonly ||
      origin !== frameSource ||
      frameCommit !== summary?.commit ||
      frameFor !== selected ||
      frameError
    )
      return void toasts.push(
        'info',
        'The frame source changed',
        'The draft must be reviewed on the current frame.',
      );
    return edit(op, label, undefined, frameContext);
  };
  $effect(() => {
    const pin = previewPin;
    if (
      pin?.op === session.previewOp &&
      pin?.frameContext != null &&
      pin.frameContext !== frameContext
    )
      session.previewOpen = false;
  });

  /** The labels the editor knows with those the previewed edit adds, for its axioms. */
  const previewLabels = $derived.by(() => {
    const added = session.previewOp ? opAxioms(session.previewOp).add : [];
    const extra = labelsOf(added.filter((a): a is AxiomJson => typeof a !== 'string'));
    if (!extra.size) return labels;
    const m = new Map(labels);
    for (const [k, v] of extra) if (!m.has(k)) m.set(k, v);
    return m;
  });

  const describe = (a: AxiomInput) =>
    describeAxiom(
      a,
      (iri) => {
        if (!previewLabels.get(iri)?.length) batch.want(iri);
        return entityName(iri, previewLabels, prefixes, langs);
      },
      prefixes,
    );

  // --- create, rename, delete --------------------------------------------------------------
  let policy = $state<IriPolicy>(loadPolicy('', null));
  $effect(() => {
    const iri = summary?.header.iri ?? null;
    untrack(() => (policy = loadPolicy(name, iri)));
  });
  const taken = $derived(new Set([...entities.values()].flat()));

  let createOpen = $state(false);
  let createKind = $state<EntityKind>('Class');
  let createParent = $state<string | null>(null);
  let createLabel = $state('');
  let createLang = $state('en');
  let createIri = $state('');
  let createIriTouched = $state(false);
  let createProblem = $state<string | null>(null);
  $effect(() => {
    if (!createOpen || createIriTouched) return;
    createIri = mintIri(policy, createLabel, createKind, taken);
  });
  function openCreate(kind: EntityKind, parent: string | null) {
    createKind = kind;
    createParent = parent;
    createLabel = '';
    createLang = langs[0]?.split('-')[0] ?? 'en';
    createIriTouched = false;
    createProblem = null;
    createIri = mintIri(policy, '', kind, taken);
    createOpen = true;
  }
  async function submitCreate() {
    const iri = createIri.trim();
    if (!validIri(iri)) return void (createProblem = 'The IRI is not an absolute IRI.');
    if (!validLang(createLang.trim()))
      return void (createProblem = 'The language tag is not valid.');
    if (taken.has(iri))
      return void (createProblem = 'The ontology already has an entity with this IRI.');
    try {
      const used = await api.query(target, usedQuery(iri, sel.graph), { nocache: true });
      if (used.boolean) return void (createProblem = 'The graph already uses this IRI.');
    } catch {
      /* the dry run still shows what the edit does */
    }
    const add: AxiomInput[] = [declaration(createKind, iri)];
    if (createLabel.trim())
      add.push(
        annotationAssertion(
          RDFS_LABEL,
          iri,
          createLang.trim()
            ? { '@value': createLabel.trim(), '@language': createLang.trim() }
            : { '@value': createLabel.trim() },
        ),
      );
    const p = createParent ? underParent(createKind, iri, createParent) : null;
    if (p) add.push(p);
    createOpen = false;
    const what = `Create ${KIND_SINGULAR[createKind]} ${createLabel.trim() || iriName(iri, prefixes)}`;
    edit({ kind: 'axioms', add, remove: [] }, what, () =>
      nav({ tab: null, kind: createKind, iri, view: null }),
    );
  }

  let renameOpen = $state(false);
  let renameFrom = $state('');
  let renameTo = $state('');
  let renameProblem = $state<string | null>(null);
  function openRename(iri: string) {
    renameFrom = iri;
    renameTo = iri;
    renameProblem = null;
    renameOpen = true;
  }
  async function submitRename() {
    const to = renameTo.trim();
    if (!validIri(to)) return void (renameProblem = 'The new IRI is not an absolute IRI.');
    if (to === renameFrom) return void (renameProblem = 'The new IRI is the same as the old one.');
    try {
      const used = await api.query(target, usedQuery(to, sel.graph), { nocache: true });
      if (used.boolean) return void (renameProblem = 'The graph already uses the new IRI.');
    } catch (e) {
      return void (renameProblem = api.errorMessage(e));
    }
    renameOpen = false;
    const from = renameFrom;
    edit(
      {
        kind: 'update',
        sparql: renameUpdate(from, to, sel.graph),
      },
      `Rename ${iriName(from, prefixes)} to ${iriName(to, prefixes)}`,
      () => {
        if (selected === from) void nav({ iri: to });
      },
    );
  }

  function remove(iri: string) {
    const uses = frameFor === iri ? frameItems.length : null;
    edit(
      { kind: 'removeEntity', iri },
      `Delete ${nameOf(iri)}${uses != null ? ` and the ${fmtCount(uses, 'axiom')} that mention${uses === 1 ? 's' : ''} it` : ''}`,
      () => {
        if (selected === iri) void nav({ iri: null, view: null });
      },
    );
  }

  let policyOpen = $state(false);
  let policyDraft = $state<IriPolicy>(loadPolicy('', null));
  function savePolicyDraft() {
    if (!validIri(policyDraft.namespace)) return;
    policy = { ...policyDraft };
    savePolicy(name, policy);
    policyOpen = false;
  }

  // --- live refresh ---------------------------------------------------------------------------
  /** Bumped to follow the change feed again after it stopped. */
  let followRun = $state(0);
  $effect(() => {
    const t = target;
    void followRun;
    if (!name) return;
    return session.follow(
      t,
      () => shown,
      () => loadedTarget === t,
    );
  });

  onMount(() => session.keys(() => readonly));
  onMount(() => () => individualLevel.drop());

  // the command palette (ROADMAP item 631): `/` filters the tree, and this page's commands
  let filterInput: HTMLInputElement | undefined = $state();
  usePageSearch(() => {
    if (!filterInput) return false;
    filterInput.focus();
    filterInput.select();
  });
  /** The parent a new entity of `kind` gets from the selection, as the tree's button does. */
  const createParentFor = (kind: EntityKind) =>
    selected &&
    frameFor === selected &&
    frameKinds.includes(kind === 'NamedIndividual' ? 'Class' : kind)
      ? selected
      : null;
  useCommands(() => [
    {
      id: 'workbench.page',
      label: 'Open page in Browse',
      icon: 'book',
      keywords: ['browse', 'page', 'read', 'open page'],
      active: () => !!name && !!selected,
      run: () => void goto(routes.browse(name, { iri: selected, graph, branch })),
    },
    {
      id: 'ontology.session',
      label: 'Start session',
      icon: 'branch',
      keywords: ['edit', 'branch'],
      subtitle: 'Edit on a branch of your own, then merge it',
      active: () => !!name && canWrite && !branch,
      run: startSession,
    },
    ...TREE_KINDS.map((k) => ({
      id: `ontology.new.${k}`,
      label: `New ${KIND_SINGULAR[k]}…`,
      icon: 'plus',
      keywords: ['create', 'add', 'entity'],
      active: () => !!name && !readonly,
      run: () => openCreate(k, createParentFor(k)),
    })),
    {
      id: 'ontology.undo',
      label: 'Undo',
      icon: 'stepBack',
      active: () => !readonly && session.done.length > 0,
      run: () => void session.undoLast(),
    },
    {
      id: 'ontology.redo',
      label: 'Redo',
      icon: 'stepForward',
      active: () => !readonly && session.undone.length > 0,
      run: () => void session.redoLast(),
    },
    {
      id: 'ontology.dlquery',
      label: 'Open DL query',
      icon: 'query',
      keywords: ['description logic', 'manchester'],
      active: () => !!name && tab !== 'dlquery',
      run: () => void nav({ tab: 'dlquery' }),
    },
  ]);
  useProvider(
    branchProvider({
      list: () => branchList.map((b) => b.name),
      current: () => branch ?? MAIN,
      choose: (n) => void nav({ branch: n === MAIN ? null : n }),
    }),
  );

  const counts = $derived(
    Object.fromEntries(
      TREE_KINDS.map((k) => [k, summary?.entities[k] ?? entities.get(k)?.length ?? 0]),
    ),
  );
  const headerTitle = $derived(
    summary?.header.iri ? nameOf(summary.header.iri) : 'Anonymous ontology',
  );
  // the ontology's own label is the one name the head asks for, once the summary is in
  $effect(() => {
    const iri = summary?.header.iri;
    if (iri) batch.want(iri);
  });
</script>

<svelte:head><title>Ontology{name ? ` · ${name}` : ''} | Sparkles</title></svelte:head>

<div class="page page-container fill onto">
  <header class="page-head">
    <div class="title">
      <h1>Ontology</h1>
      <p class="meta">
        {#if summary}
          {headerTitle} · commit {summary.commit}
          {#if summary.header.versionIri}· version {iriName(
              summary.header.versionIri,
              prefixes,
            )}{/if}
        {:else if !name}
          Choose a dataset in the top bar.
        {:else}
          <span class="spinner"></span> Reading the ontology of {name}…
        {/if}
      </p>
    </div>
    <PageActions>
      <FollowState live={session.live} onretry={() => followRun++} />
      {#if canWrite}
        <button
          class="btn ghost"
          disabled={!session.done.length || readonly}
          onclick={() => session.undoLast()}
          title={session.done.at(-1) ? `Undo: ${session.done.at(-1)?.label}` : 'Nothing to undo'}
          >Undo{session.done.length ? ` (${session.done.length})` : ''}</button
        >
        <button
          class="btn ghost"
          disabled={!session.undone.length || readonly}
          onclick={() => session.redoLast()}
          title={session.undone.at(-1)
            ? `Redo: ${session.undone.at(-1)?.label}`
            : 'Nothing to redo'}>Redo</button
        >
      {/if}
      {#if canWrite && !branch}
        <button
          class="btn"
          data-primary
          onclick={startSession}
          title="Edit on a branch of your own, then merge it"
          ><Icon name="branch" size={14} /> Start session</button
        >
      {/if}
      {#if branch}
        <a
          class="btn"
          data-primary
          href="{resolve('/datasets/[name]/merge', { name })}?source={encodeURIComponent(
            branch,
          )}&target={encodeURIComponent(currentBranch?.upstream ?? MAIN)}"
          ><Icon name="merge" size={14} /> Merge</a
        >
      {/if}
    </PageActions>
  </header>
  {#if name}
    <div class="page-context">
      <label class="form-row">
        Graph
        <select
          class="select"
          value={graph ?? ''}
          onchange={(e) => nav({ graph: e.currentTarget.value || null, iri: null })}
        >
          <option value="">default graph</option>
          {#each graphs as g (g)}<option value={g}>{iriName(g, prefixes)}</option>{/each}
          {#if graph && !graphs.includes(graph)}<option value={graph}>{graph}</option>{/if}
        </select>
      </label>
      <label class="form-row check" title="Include or exclude the axioms of the imported graphs">
        <input
          type="checkbox"
          checked={withImports}
          onchange={(e) => nav({ imports: e.currentTarget.checked ? null : '0' })}
        />
        Imported axioms
      </label>
      {#if branchList.length}
        <label class="form-row">
          Branch
          <select
            class="select"
            value={branch ?? MAIN}
            onchange={(e) =>
              nav({ branch: e.currentTarget.value === MAIN ? null : e.currentTarget.value })}
          >
            {#each branchList as b (b.name)}<option value={b.name}
                >{b.name}{b.protected ? ' (protected)' : ''}</option
              >{/each}
          </select>
        </label>
      {/if}
    </div>
  {/if}

  {#if sessionError}<p class="error-box" role="alert">{sessionError}</p>{/if}
  {#if session.notice}
    <p class="notice" role="status">
      {session.notice}. The views show the new state.
      <button class="btn ghost sm" onclick={() => (session.notice = null)} aria-label="Dismiss"
        ><Icon name="x" size={12} /></button
      >
    </p>
  {/if}
  {#if protectedBranch}
    <p class="notice" role="status">
      {branch ?? MAIN} is protected and takes changes through merges only. Start a session branch to edit.
    </p>
  {:else if !canWrite && name}
    <p class="notice" role="status">
      <span
        ><strong>Read-only.</strong> You can read {name} but not edit it, so the editing controls are
        hidden. The tree, DL Query, the header and the history still read.</span
      >
    </p>
  {/if}
  {#if loadError}
    <div class="error-box" role="alert">
      <strong>The ontology could not be read.</strong>
      {api.errorMessage(loadError)}
    </div>
  {/if}
  {#if name && (summary || (tab === 'tree' && !loadError))}
    <div class="tabs onto-tabs" role="tablist" aria-label="Ontology views">
      {#each TAB_LABEL as [t, l] (t)}
        <button
          class="tab"
          role="tab"
          aria-selected={tab === t}
          onclick={() => nav({ tab: t === 'tree' ? null : t })}>{l}</button
        >
      {/each}
    </div>
    {#if tab === 'header' && summary}
      <section class="panel" aria-label="Ontology header">
        <div class="panel-head"><h2>Ontology header</h2></div>
        <div class="panel-body">
          <OntoHeader
            {target}
            {sel}
            {summary}
            name={nameOf}
            {prefixes}
            {annotationProperties}
            {readonly}
            onedit={(op, label) => edit(op, label)}
            onrename={openRename}
          />
        </div>
      </section>
    {:else if tab === 'bulk' && summary}
      <section class="panel" aria-label="Bulk edits">
        <div class="panel-body">
          {#key `${target}|${graph}`}<BulkEdit
              {target}
              {graph}
              commit={summary.commit ?? null}
              {readonly}
              {prefixes}
              {annotationProperties}
              graphs={summary.graphs}
              name={nameOf}
              onedit={(op, label) =>
                // the plan was read without the imports, and its edit reads and writes the same
                edit(op, label, undefined, null, { graph, imports: false })}
            />{/key}
        </div>
      </section>
    {:else if tab === 'imports' && summary}
      <section class="panel" aria-label="Imports">
        <div class="panel-body">
          {#key target}<OntoImports
              {target}
              root={graph ?? 'default'}
              closure={summary.closure ?? []}
              {etag}
              {readonly}
              imports={withImports}
              name={(i) => (i === 'default' ? 'default graph' : iriName(i, prefixes))}
              oncommitted={discussed}
              onchanged={() => void loadAll()}
            />{/key}
        </div>
      </section>
    {:else if tab === 'discussions' && summary}
      <section class="panel" aria-label="Discussions">
        <div class="panel-body">
          {#key `${target}|${graph}`}<Discussions
              {target}
              {sel}
              {readonly}
              user={commenter}
              name={nameOf}
              refreshKey={summary.commit ?? 0}
              oncommitted={discussed}
              onopen={(iri) => nav({ tab: null, iri, view: 'discussion' })}
            />{/key}
        </div>
      </section>
    {:else if tab === 'history' && summary}
      <section class="panel" aria-label="History">
        <div class="panel-body">
          <EditorHistory
            {name}
            {target}
            {branch}
            {graph}
            {langs}
            {prefixes}
            onopen={(iri) => nav({ tab: null, iri, view: null })}
            refreshKey={summary?.commit ?? 0}
          />
        </div>
      </section>
    {:else}
      <div class="panes split-view">
        <section class="panel trees" aria-label="Entities">
          {#if kindTab}
            <div class="tree-bar">
              <select
                class="select sm kind-select"
                aria-label="Entity kind"
                value={kind}
                onchange={(e) =>
                  nav({
                    kind: e.currentTarget.value === 'Class' ? null : e.currentTarget.value,
                    iri: null,
                    view: null,
                  })}
              >
                {#each TREE_KINDS as k (k)}<option value={k}
                    >{KIND_LABEL[k]}{counts[k]
                      ? ` · ${fmtInt(counts[k])}`
                      : !summary
                        ? ' · …'
                        : ''}</option
                  >{/each}
              </select>
              <input
                class="input"
                type="search"
                placeholder="Filter {KIND_LABEL[kindTab].toLowerCase()}"
                aria-label="Filter"
                bind:this={filterInput}
                bind:value={filter}
              />
              {#if !readonly}
                <button
                  class="btn sm"
                  onclick={() => openCreate(kindTab, createParentFor(kindTab))}
                  title={selected
                    ? `New ${KIND_SINGULAR[kindTab]} under the selection`
                    : `New ${KIND_SINGULAR[kindTab]}`}><Icon name="plus" size={13} /> New</button
                >
              {/if}
              {#if inferredKind}
                <label class="inferred-toggle"
                  ><input
                    type="checkbox"
                    checked={inferredOn}
                    onchange={(e) => nav({ inferred: e.currentTarget.checked ? '1' : null })}
                  />
                  Inferred</label
                >
              {/if}
              <button
                class="btn ghost icon sm"
                aria-label="IRI policy"
                title="How new entities get their IRIs"
                onclick={() => ((policyDraft = { ...policy }), (policyOpen = true))}
                ><Icon name="key" size={13} /></button
              >
            </div>
            <div class="tree-cols" class:dual={!!inferredKind && inferredOn}>
              <div class="tree-col">
                {#if inferredKind && inferredOn}<h2 class="col-title">Asserted</h2>{/if}
                <div class="tree-scroll">
                  {#if (level || kindTab === 'NamedIndividual') && filtered}
                    {#if kindTab === 'NamedIndividual'}<p class="faint small">
                        Names are searched at the current branch head. Matching individuals stay at
                        the displayed revision.
                      </p>{/if}
                    {#if filtered.pending}
                      <p class="empty">Searching…</p>
                    {:else}
                      {#if filtered.error}<p class="error-box" role="alert">
                          {filtered.error}
                        </p>{/if}
                      {#if filtered.absent}<p class="faint small">
                          Some search matches are absent at the displayed revision.
                        </p>{/if}
                      {#if filtered.tree.roots.length}
                        <OntoTree
                          label="{KIND_LABEL[kindTab]} (asserted)"
                          roots={filtered.tree.roots}
                          children={(id) => filtered?.tree.children.get(id) ?? []}
                          name={kindTab === 'NamedIndividual' ? individualName : treeName}
                          sub={kindTab === 'NamedIndividual' ? individualSub : subOf}
                          open={filtered.tree.nodes}
                          {selected}
                          keep={filtered.tree.nodes}
                          onselect={(id) =>
                            kindTab === 'NamedIndividual' ? individualSelect(id) : nav({ iri: id })}
                          ontoggle={() => {}}
                        />
                      {:else if !filtered.error}
                        <p class="empty">
                          {kindTab === 'NamedIndividual'
                            ? 'No current search matches are present at the displayed revision.'
                            : `Nothing matches “${filtered.text}”.`}
                        </p>
                      {/if}
                    {/if}
                  {:else if level}
                    {#if level.roots().length}
                      <OntoTree
                        label="{KIND_LABEL[kindTab]} (asserted)"
                        roots={level.roots()}
                        children={(id) => level?.children(id) ?? []}
                        more={(id) => level?.more(id) ?? false}
                        clipped={(p) => level?.clippedOf(p) ?? null}
                        onmore={(p) => level?.loadMore(p)}
                        name={treeName}
                        sub={subOf}
                        open={openSet(kindTab)}
                        {selected}
                        onselect={(id) => nav({ iri: id })}
                        ontoggle={(id) => toggleLevel(kindTab!, id)}
                      />
                    {:else if level.loading}
                      <p class="empty">Reading the {KIND_LABEL[kindTab].toLowerCase()}…</p>
                    {:else if level.error}
                      <p class="empty">{level.error}</p>
                    {:else}
                      <p class="empty">No {KIND_LABEL[kindTab].toLowerCase()}.</p>
                    {/if}
                  {:else if kindTab === 'NamedIndividual'}
                    {#if individualLevel.error}<p class="error-box" role="alert">
                        {individualLevel.error}
                      </p>{/if}
                    {#if individualLevel.roots().length}
                      <OntoTree
                        label="Individuals (asserted)"
                        roots={individualLevel.roots()}
                        children={(id) => individualLevel.children(id)}
                        more={(id) => individualLevel.more(id)}
                        clipped={(p) => individualLevel.clippedOf(p)}
                        onmore={(p) => individualLevel.loadMore(p)}
                        name={individualName}
                        sub={individualSub}
                        count={(id) => (isGroup(id) ? individualLevel.counts[id] : 0)}
                        badge={(id) =>
                          isGroup(id)
                            ? null
                            : (individualLevel.types[id] ?? []).map(nameOf).join(', ') || null}
                        open={openSet(kindTab)}
                        {selected}
                        onselect={individualSelect}
                        ontoggle={(id) => individualSelect(id)}
                      />
                    {:else if individualLevel.loading || !summary}
                      <p class="empty">Reading the individuals…</p>
                    {:else}
                      <p class="empty">No individuals.</p>
                    {/if}
                  {/if}
                </div>
              </div>
              {#if inferredKind && inferredOn}
                <div class="tree-col">
                  <div class="tree-scroll">
                    <InferredTreeView
                      {target}
                      {graph}
                      imports={sel.imports}
                      commit={summary?.commit ?? null}
                      kind={inferredKind}
                      {selected}
                      pins={inferredPins}
                      onopen={(iri) => nav({ iri })}
                    />
                  </div>
                  {#if kindTab === 'Class'}
                    <h2 class="col-title">
                      Inferred
                      {#if classification}<span class="faint">
                          {classification.complete
                            ? `${classification.tier?.toUpperCase() ?? 'Classification'} · commit ${classification.commit}`
                            : `${fmtInt(classification.excludedCount)} omitted or partly used axioms · commit ${classification.commit}`}</span
                        >{/if}
                    </h2>
                    <button
                      class="btn sm ghost"
                      disabled={classifying || !summary}
                      onclick={() => void classify()}>Classify tree</button
                    >
                    <p class="faint">This explicit action reads the full taxonomy in POST pages.</p>
                    {#if classification?.nodesTruncated}<p class="note">
                        The browser's taxonomy display limit was reached.
                      </p>{/if}
                    <div class="tree-scroll">
                      {#if classifying && !inferred}<p class="faint">
                          <span class="spinner"></span> Classifying…
                        </p>{/if}
                      {#if classifyError}<p class="error-box" role="alert">{classifyError}</p>{/if}
                      {#if inferred}
                        {#if classification && !classification.consistent}
                          <p class="error-box" role="alert">
                            The ontology is inconsistent, so every class is unsatisfiable.
                          </p>
                        {/if}
                        <OntoTree
                          label="Classes (inferred)"
                          roots={inferred.tree.roots}
                          children={(id) => inferred?.tree.children.get(id) ?? []}
                          name={inferredName}
                          onview={(ids) => {
                            for (const id of ids)
                              for (const c of inferred?.classesOf.get(id) ?? [])
                                inferredBatch.want(c);
                          }}
                          badge={(id) => (id === 'n:1' ? 'unsatisfiable' : null)}
                          open={openSet('inferred')}
                          selected={selected ? (inferred.nodeOf.get(selected) ?? null) : null}
                          keep={inferredKeep}
                          onselect={(id) => {
                            const c = inferred?.classesOf.get(id)?.[0];
                            if (c) void nav({ iri: c });
                          }}
                          ontoggle={(id) => toggle('inferred', id)}
                        />
                      {/if}
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/if}
          {#if tab === 'dlquery'}
            {@render dlPanel()}
          {/if}
        </section>

        <section class="panel detail" aria-label="Details">
          {#if kindTab && selected}
            <TreeCrumbs
              roots={kindTab === 'NamedIndividual'
                ? individualLevel.roots()
                : (level?.roots() ?? [])}
              children={(id) =>
                kindTab === 'NamedIndividual'
                  ? individualLevel.children(id)
                  : (level?.children(id) ?? [])}
              target={selected}
              name={kindTab === 'NamedIndividual' ? individualName : nameOf}
              onselect={(id) =>
                kindTab === 'NamedIndividual' ? individualSelect(id) : nav({ iri: id })}
            />
          {/if}
          {#if !selected}
            <div class="panel-head"><h2>Ontology header</h2></div>
            <div class="panel-body">
              {#if summary}
                <OntoHeader
                  {target}
                  {sel}
                  {summary}
                  name={nameOf}
                  {prefixes}
                  {annotationProperties}
                  {readonly}
                  onedit={(op, label) => edit(op, label)}
                  onrename={openRename}
                />
              {:else}
                <p class="faint"><span class="spinner"></span> Reading the ontology…</p>
              {/if}
            </div>
          {:else}
            <div class="panel-head entity-head">
              <div class="who">
                <h2>{nameOf(selected)}</h2>
                <span class="mono faint iri">{selected}</span>
                <span class="kinds">
                  {#each frameKinds as k (k)}<span class="badge">{KIND_SINGULAR[k]}</span>{/each}
                </span>
              </div>
              <span class="spacer"></span>
              <a
                class="btn sm ghost"
                href={routes.browse(name, { iri: selected, graph, branch })}
                title="Open this entity's page in Browse"
                ><Icon name="book" size={13} /> Open page</a
              >
              {#if !readonly && frameKinds.length}
                <button class="btn sm" onclick={() => openRename(selected!)}
                  ><Icon name="pencil" size={13} /> Rename</button
                >
                <button class="btn sm danger" onclick={() => remove(selected!)}
                  ><Icon name="trash" size={13} /> Delete</button
                >
              {/if}
            </div>
            <div class="view-bar">
              <div class="segmented" role="group" aria-label="Entity views">
                <button aria-pressed={view === 'frame'} onclick={() => nav({ view: null })}
                  >Frame</button
                >
                <button aria-pressed={view === 'usage'} onclick={() => nav({ view: 'usage' })}
                  >Usage{frameFor === selected && frameItems.length
                    ? ` ${fmtInt(frameItems.length)}`
                    : ''}</button
                >
                <button aria-pressed={view === 'history'} onclick={() => nav({ view: 'history' })}
                  >History</button
                >
                <button
                  aria-pressed={view === 'discussion'}
                  onclick={() => nav({ view: 'discussion' })}>Discussion</button
                >
              </div>
            </div>
            <div class="panel-body">
              {#if frameError}<p class="error-box" role="alert">{frameError}</p>{/if}
              {#if inferredOn && frameKind === 'Class' && inferred && selected}
                {@const sup = inferredParents(selected)}
                {#if sup.length}
                  <p class="inferred-sup">
                    <span class="faint">Inferred direct superclasses:</span>
                    {#each sup as c, i (c)}{#if i},
                      {/if}<button class="link" onclick={() => nav({ iri: c })}
                        >{inferredIriName(c)}</button
                      >{/each}
                  </p>
                {/if}
              {/if}
              {#if frameFor === selected && view === 'frame'}
                {#key `${target}|${graph}|${selected}|${frameKind}`}
                  {#if frameCommit !== summary?.commit}<p class="faint">
                      Displaying frame commit {frameCommit}; reading commit {summary?.commit}.
                    </p>{/if}
                  <OntoFrame
                    kind={frameKind}
                    iri={selected}
                    items={frameItems}
                    source={frameSource}
                    commit={frameCommit}
                    name={nameOf}
                    {prefixes}
                    {options}
                    {annotationProperties}
                    readonly={readonly || frameCommit !== summary?.commit}
                    {names}
                    search={bigKinds.size ? searchNames : undefined}
                    {parse}
                    onedit={editFrame}
                    onopen={(iri) => nav({ iri, view: null })}
                  />
                  {#if frameKind === 'Class'}
                    <ClassificationFrame
                      {target}
                      {graph}
                      imports={sel.imports}
                      iri={selected}
                      commit={frameCommit}
                      items={frameItems}
                      pins={frameCommit === summary?.commit ? inferredPins : undefined}
                      onopen={(iri) => nav({ iri, view: null })}
                    />
                  {:else if ['ObjectProperty', 'DataProperty', 'NamedIndividual'].includes(frameKind)}
                    <MaterializedFrame
                      {target}
                      {graph}
                      imports={sel.imports}
                      iri={selected}
                      commit={frameCommit}
                      items={frameItems}
                      pins={frameCommit === summary?.commit ? inferredPins : undefined}
                    />
                  {/if}
                {/key}
              {:else if view === 'discussion' && summary && selected}
                {#key `${target}|${graph}|${selected}`}<Discussions
                    {target}
                    {sel}
                    iri={selected}
                    {readonly}
                    user={commenter}
                    axioms={frameFor === selected ? frameItems.map((it) => it.ofn) : []}
                    name={nameOf}
                    refreshKey={summary.commit ?? 0}
                    oncommitted={discussed}
                  />{/key}
              {:else if view === 'history' && summary}
                <EntityHistory
                  {name}
                  {target}
                  {branch}
                  {sel}
                  iri={selected}
                  readonly={readonly || graphRestricted(auth.who, name)}
                  refreshKey={summary.commit}
                  onrevert={(seq, label) => session.edit({ kind: 'revert', commit: seq }, label)}
                />
              {:else if frameFor === selected && view === 'usage'}
                {#if !usageGroups.length}<p class="empty">No axiom mentions this entity.</p>{/if}
                {#each usageGroups as [type, items] (type)}
                  <section class="usage" aria-label={type}>
                    <h3>{type} <span class="faint">{items.length}</span></h3>
                    <ul>
                      {#each items as it, i (it.ofn + i)}
                        <li>
                          <span class="mono small">{it.ofn}</span>
                          <span class="refs">
                            {#each mentionedBy(it).slice(0, 4) as m (m)}
                              <button class="link" onclick={() => nav({ iri: m, view: null })}
                                >{nameOf(m)}</button
                              >
                            {/each}
                          </span>
                        </li>
                      {/each}
                    </ul>
                  </section>
                {/each}
              {:else}
                <p class="faint"><span class="spinner"></span> Reading the entity…</p>
              {/if}
            </div>
          {/if}
        </section>
      </div>
    {/if}
  {:else if loading}
    <p class="faint"><span class="spinner"></span> Reading the ontology…</p>
  {:else if !name}
    <p class="empty">Choose a dataset to edit its ontology.</p>
  {/if}
</div>

<OntoPreview
  bind:open={session.previewOpen}
  title={session.previewTitle}
  {target}
  {sel}
  op={session.previewOp}
  {etag}
  head={summary?.commit ?? null}
  pin={previewPin}
  {describe}
  oncommitted={(op, a) => void session.committed(op, a)}
  onstale={() => void session.stale()}
/>

<Modal bind:open={createOpen} title="New {KIND_SINGULAR[createKind]}" width={520}>
  <form
    id="create-form"
    class="dialog-form"
    onsubmit={(e) => {
      e.preventDefault();
      void submitCreate();
    }}
  >
    {#if createParent}
      <p class="muted">
        {createKind === 'NamedIndividual' ? 'An instance of' : 'Under'}
        <strong>{nameOf(createParent)}</strong>
        <button class="btn ghost sm" type="button" onclick={() => (createParent = null)}
          >At the top</button
        >
      </p>
    {/if}
    <div class="two">
      <label class="f grow">
        <span>Label</span>
        <!-- svelte-ignore a11y_autofocus -->
        <input class="input" bind:value={createLabel} autofocus required />
      </label>
      <label class="f">
        <span>Language</span>
        <input
          class="input"
          bind:value={createLang}
          size="6"
          aria-invalid={!validLang(createLang.trim())}
        />
      </label>
    </div>
    <label class="f">
      <span
        >IRI <span class="faint"
          >({policy.mode === 'label'
            ? 'from the label'
            : policy.mode === 'uuid'
              ? 'a UUID'
              : 'the next number'} under {policy.namespace})</span
        ></span
      >
      <input
        class="input mono"
        value={createIri}
        oninput={(e) => {
          createIri = e.currentTarget.value;
          createIriTouched = true;
        }}
      />
    </label>
    {#if createProblem}<p class="problem" role="alert">{createProblem}</p>{/if}
  </form>
  {#snippet actions()}
    <button class="btn" onclick={() => (createOpen = false)}>Cancel</button>
    <button class="btn primary" type="submit" form="create-form">Preview</button>
  {/snippet}
</Modal>

<Modal bind:open={renameOpen} title="Rename {nameOf(renameFrom)}" width={560}>
  <form
    id="rename-form"
    class="dialog-form"
    onsubmit={(e) => {
      e.preventDefault();
      void submitRename();
    }}
  >
    <p class="muted">
      The rename replaces the IRI in every triple of the graph, as subject, predicate or object, in
      one commit. It renames nothing when the graph already uses the new IRI.
    </p>
    <label class="f">
      <span>New IRI</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="input mono" bind:value={renameTo} autofocus />
    </label>
    {#if renameProblem}<p class="problem" role="alert">{renameProblem}</p>{/if}
  </form>
  {#snippet actions()}
    <button class="btn" onclick={() => (renameOpen = false)}>Cancel</button>
    <button class="btn primary" type="submit" form="rename-form">Preview</button>
  {/snippet}
</Modal>

<Modal bind:open={policyOpen} title="IRIs of new entities" width={520}>
  <form
    id="policy-form"
    class="dialog-form"
    onsubmit={(e) => {
      e.preventDefault();
      savePolicyDraft();
    }}
  >
    <label class="f">
      <span>Namespace</span>
      <input
        class="input mono"
        bind:value={policyDraft.namespace}
        aria-invalid={!validIri(policyDraft.namespace)}
      />
    </label>
    <fieldset class="modes">
      <legend>Local name</legend>
      <label
        ><input type="radio" bind:group={policyDraft.mode} value="label" /> From the label, such as GraduateStudent</label
      >
      <label><input type="radio" bind:group={policyDraft.mode} value="uuid" /> A random UUID</label>
      <label
        ><input type="radio" bind:group={policyDraft.mode} value="sequence" /> A prefix and the next number</label
      >
    </fieldset>
    {#if policyDraft.mode === 'sequence'}
      <div class="two">
        <label class="f grow"
          ><span>Prefix</span><input class="input mono" bind:value={policyDraft.prefix} /></label
        >
        <label class="f"
          ><span>Digits</span><input
            class="input"
            type="number"
            min="1"
            max="12"
            bind:value={policyDraft.digits}
          /></label
        >
      </div>
    {/if}
    <p class="faint">
      The policy is kept in this browser for {name}. The editor checks every new IRI against the
      graph.
    </p>
  </form>
  {#snippet actions()}
    <button class="btn" onclick={() => (policyOpen = false)}>Cancel</button>
    <button
      class="btn primary"
      type="submit"
      form="policy-form"
      disabled={!validIri(policyDraft.namespace)}>Save</button
    >
  {/snippet}
</Modal>

{#snippet dlList(key: DlAnswer, title: string)}
  {@const list = dlResult?.[key]}
  {#if list}
    {@const count = dlResult?.counts[key] ?? list.length}
    <section class="dl-answer" aria-label={title}>
      <h3>
        {title} <span class="faint">{count}</span>
      </h3>
      {#if list.length}
        <ul>
          {#each list as iri (iri)}
            <li>
              <button
                class="link"
                class:on={selected === iri}
                title={iri}
                onclick={() => nav({ iri, view: null })}>{nameOf(iri)}</button
              >
            </li>
          {/each}
        </ul>
        {#if count > list.length}
          <p class="faint small">The first {list.length} of {count} are shown.</p>
        {/if}
      {:else}
        <p class="faint small">None.</p>
      {/if}
    </section>
  {/if}
{/snippet}

{#snippet dlPanel()}
  <form
    class="dl-form"
    aria-label="DL query"
    onsubmit={(e) => {
      e.preventDefault();
      void runDl();
    }}
  >
    <span class="f">Class expression in Manchester syntax</span>
    <ManchesterEditor
      bind:value={dlText}
      {names}
      search={bigKinds.size ? searchNames : undefined}
      label="Class expression"
      placeholder="Person and (teaches some Course)"
      multiline
      problem={dlProblem
        ? { start: dlProblem.start, end: dlProblem.end, message: dlProblem.error }
        : null}
      onsubmit={() => void runDl()}
    />
    {#if dlProblem}
      <div class="problem" role="alert">
        {dlProblem.error}
        {#if dlProblem.candidates?.length}
          <span>It could be</span>
          {#each dlProblem.candidates as c, i (c.iri)}
            <code title={c.iri}>{c.label ?? iriName(c.iri, prefixes)}</code>{i <
            dlProblem.candidates.length - 1
              ? ', '
              : '.'}
          {/each}
        {/if}
      </div>
    {/if}
    {#if dlError}<p class="problem" role="alert">{dlError}</p>{/if}
    <fieldset class="dl-answers">
      <legend class="faint">Answers</legend>
      {#each DL_ANSWERS as [k, label] (k)}
        <label class="check"
          ><input
            type="checkbox"
            checked={dlAnswers.includes(k)}
            onchange={(e) => toggleAnswer(k, e.currentTarget.checked)}
          />
          {label}</label
        >
      {/each}
    </fieldset>
    <div class="dl-run">
      <label class="pick">
        <span>Reasoning</span>
        <select class="input" bind:value={dlReasoning} aria-label="Reasoning">
          <option value="auto">Automatic</option>
          <option value="el">OWL 2 EL classifier</option>
          <option value="rl">OWL 2 RL rules</option>
        </select>
      </label>
      <button class="btn primary" type="submit" disabled={dlBusy || !dlText.trim()}
        >{dlBusy ? 'Running…' : 'Execute'}</button
      >
    </div>
  </form>
  {#if dlResult}
    {@const r = dlResult.reasoning}
    <section class="dl-result" aria-label="DL query answers">
      <div class="dl-label" role="status">
        <span class="badge spark">{r.title}</span>
        {#if r.complete && r.instancesComplete !== false}
          <span class="badge ok">Complete</span>
        {:else if r.complete}
          <span class="badge ok">Classes complete</span>
          <span class="badge danger">Instances possibly incomplete</span>
        {:else if r.instancesComplete === true}
          <span class="badge danger">Classes possibly incomplete</span>
          <span class="badge ok">Instances complete</span>
        {:else}
          <span class="badge danger">Possibly incomplete</span>
        {/if}
        {#if dlResult.incremental}<span class="badge">Incremental</span>{/if}
        <span class="faint small">{dlResult.elapsedMs.toFixed(1)} ms</span>
      </div>
      <p class="small muted">{sentence(r.why)}</p>
      {#each r.notes as n (n)}<p class="small muted">{sentence(n)}</p>{/each}
      {#if r.gaps.length}
        <details class="dl-gaps">
          <summary class="small"
            >{r.gapCount} {r.gapCount === 1 ? 'axiom' : 'axioms'} outside the profile</summary
          >
          <ul>
            {#each r.gaps as g, i (i)}
              <li class="small">
                <code>{g.manchester ?? g.ofn ?? ''}</code>
                <span class="faint">{g.message}</span>
              </li>
            {/each}
          </ul>
          {#if r.gapCount > r.gaps.length}
            <p class="faint small">The first {r.gaps.length} are shown.</p>
          {/if}
        </details>
      {/if}
      {#if dlResult.consistent === false}
        <p class="problem" role="alert">
          The ontology is inconsistent, so every class is a subclass of every other.
        </p>
      {:else if dlResult.satisfiable === false}
        <p class="problem" role="alert">
          The expression is unsatisfiable. It is equivalent to owl:Nothing and has no instances.
        </p>
      {/if}
      {#each DL_ANSWERS as [k, label] (k)}
        {@render dlList(k, label)}
      {/each}
    </section>
  {:else if !dlProblem && !dlBusy}
    <p class="faint pad">
      Write a class expression and execute it to list its equivalent classes, superclasses,
      subclasses and instances.
    </p>
  {/if}
{/snippet}

<style>
  .notice {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border-radius: var(--r);
    background: var(--spark-soft);
    font-size: var(--fs-sm);
  }
  /* the trees take two fifths: the inferred tree sits beside the asserted one */
  .panes {
    grid-template-columns: minmax(260px, 2fr) minmax(0, 3fr);
  }
  .trees {
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .detail {
    overflow: auto;
  }
  .detail > :global(.panel-head) {
    position: sticky;
    top: 0;
    z-index: 3;
    background: var(--surface);
    border-radius: var(--r-lg) var(--r-lg) 0 0;
  }
  .onto-tabs {
    border-bottom: 1px solid var(--border);
  }
  .dl-form {
    display: grid;
    gap: 8px;
    padding: 10px;
  }
  .dl-answers {
    border: 0;
    padding: 0;
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    font-size: var(--fs-sm);
  }
  .dl-answers legend {
    font-size: var(--fs-xs);
    padding: 0;
    margin-right: 4px;
    float: left;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .dl-run {
    display: flex;
    gap: 8px;
    align-items: end;
    flex-wrap: wrap;
    justify-content: space-between;
  }
  .dl-result {
    display: grid;
    gap: 6px;
    padding: 0 10px 12px;
    min-width: 0;
  }
  .dl-label {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .dl-gaps ul {
    margin: 4px 0;
    padding-left: 18px;
    display: grid;
    gap: 3px;
  }
  .dl-gaps code {
    overflow-wrap: anywhere;
  }
  .dl-answer h3 {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    border-bottom: 1px solid var(--border);
    margin: 6px 0 4px;
  }
  .dl-answer ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
    max-height: 240px;
    overflow: auto;
  }
  .dl-answer .link {
    text-align: left;
    overflow-wrap: anywhere;
  }
  .dl-answer .link.on {
    font-weight: 600;
  }
  .tree-bar {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 8px;
    flex-wrap: wrap;
  }
  .tree-bar .input {
    flex: 1 1 10em;
    min-width: 0;
  }
  .inferred-toggle {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .tree-cols {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    min-width: 0;
    flex: 1;
    min-height: 0;
  }
  .tree-cols.dual {
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  }
  .tree-col {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .tree-cols.dual .tree-col + .tree-col {
    border-left: 1px solid var(--border);
  }
  .col-title {
    font-size: var(--fs-sm);
    font-weight: 600;
    padding: 4px 10px;
    color: var(--text-2);
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: baseline;
  }
  .col-title .faint {
    font-weight: 400;
    font-size: var(--fs-xs);
  }
  .tree-scroll {
    flex: 1;
    min-height: 200px;
    overflow: auto;
    padding: 0 6px 10px;
  }
  .pad {
    padding: 10px 14px;
  }
  .entity-head .who {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .entity-head h2 {
    font-size: var(--fs-lg);
  }
  .entity-head .iri {
    font-size: var(--fs-xs);
    overflow-wrap: anywhere;
  }
  .kinds {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
  }
  .view-bar {
    display: flex;
    padding: 8px 10px;
    border-bottom: 1px solid var(--border);
  }
  .inferred-sup {
    font-size: var(--fs-sm);
    margin-bottom: 10px;
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--iri);
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }
  .usage {
    display: grid;
    gap: 4px;
    margin-bottom: 12px;
  }
  .usage h3 {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    border-bottom: 1px solid var(--border);
  }
  .usage ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 3px;
  }
  .usage li {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
    overflow-wrap: anywhere;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .refs {
    display: inline-flex;
    gap: 8px;
    flex-wrap: wrap;
    font-size: var(--fs-sm);
  }
  .dialog-form {
    display: grid;
    gap: 10px;
  }
  .f {
    display: grid;
    gap: 3px;
    font-size: var(--fs-sm);
    color: var(--text-2);
    min-width: 0;
  }
  .two {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .grow {
    flex: 1 1 12em;
  }
  .modes {
    border: 0;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .modes legend {
    color: var(--text-2);
    margin-bottom: 4px;
  }
  .problem {
    color: var(--danger);
    font-size: var(--fs-sm);
  }
  @media (max-width: 760px) {
    .panes {
      grid-template-columns: minmax(0, 1fr);
    }
    .tree-scroll {
      max-height: 50vh;
    }
    .tree-cols.dual {
      grid-template-columns: minmax(0, 1fr);
    }
    .tree-cols.dual .tree-col + .tree-col {
      border-left: 0;
      border-top: 1px solid var(--border);
    }
  }
</style>

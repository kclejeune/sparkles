<script lang="ts">
  // The ontology editor (docs/specs/U12-ontology-workbench/SPEC.md, "Ontology editor UI"):
  // the class, property and individual trees of a graph's ontology, an entity's frame and
  // its usage, the header, and the inferred class tree beside the asserted one. Every edit
  // is a dry run first and then one commit on the branch the page works on, with If-Match
  // naming the commit the page shows. The session's undo stack undoes the editor's commits
  // by reverting them, which restores exactly the triples each commit changed, and the
  // change feed refreshes the views when a commit lands.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { branchParam, MAIN, onBranch, validBranchName } from '$lib/branches';
  import { followCommits } from '$lib/changes-feed';
  import { fmtInt } from '$lib/format';
  import {
    ancestorsOf,
    annotationAssertion,
    buildTree,
    changedAnything,
    classifyOntology,
    committedSeq,
    dlQuery,
    DL_ANSWERS,
    declaration,
    entityName,
    filterTree,
    hierarchyEdges,
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
    ontologyEntity,
    ontologySummary,
    RDFS_LABEL,
    renameUpdate,
    savePolicy,
    sessionBranchName,
    taxonomyTree,
    TREE_KINDS,
    underParent,
    UndoStack,
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
    type OpAnswer,
    type OntologySummary,
    type Selection,
    type Tree,
  } from '$lib/ontology';
  import Icon from '$components/Icon.svelte';
  import ManchesterEditor from '$components/ManchesterEditor.svelte';
  import type { NameOption } from '$lib/manchester-complete';
  import Modal from '$components/Modal.svelte';
  import OntoFrame from '$components/OntoFrame.svelte';
  import OntoHeader from '$components/OntoHeader.svelte';
  import OntoPreview from '$components/OntoPreview.svelte';
  import OntoTree from '$components/OntoTree.svelte';

  type Tab = EntityKind | 'header' | 'dlquery';

  // --- the URL's state ---------------------------------------------------------------
  const q = $derived(page.url.searchParams);
  const name = $derived(q.get('ds') ?? app.current ?? '');
  const branch = $derived(branchParam(q.get('branch')));
  const target = $derived(onBranch(name, branch));
  const graph = $derived(q.get('graph') || null);
  const sel: Selection = $derived({ graph, imports: true });
  const tab = $derived<Tab>(
    (TREE_KINDS as string[]).includes(q.get('tab') ?? '') ||
      q.get('tab') === 'header' ||
      q.get('tab') === 'dlquery'
      ? (q.get('tab') as Tab)
      : 'Class',
  );
  /** The entity kind of the tab, when it shows a tree. */
  const kindTab = $derived<EntityKind | null>(tab === 'header' || tab === 'dlquery' ? null : tab);
  const selected = $derived(q.get('iri'));
  const view = $derived(q.get('view') === 'usage' ? 'usage' : 'frame');
  const inferredOn = $derived(q.get('inferred') === '1');
  const prefixes = $derived(app.prefixes(name));

  function withParams(over: Record<string, string | null>) {
    const p = new URLSearchParams(page.url.searchParams);
    if (!p.get('ds') && name) p.set('ds', name);
    for (const [k, v] of Object.entries(over)) {
      if (v == null || v === '') p.delete(k);
      else p.set(k, v);
    }
    return `${resolve('/ontology')}?${p}`;
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
  let etag = $state<string | null>(null);
  let loadError = $state<api.ApiError | Error | null>(null);
  let loading = $state(false);
  let entities = $state<Map<EntityKind, string[]>>(new Map());
  let labels = $state<Map<string, Lit[]>>(new Map());
  let hierarchy = $state<AxiomItem[]>([]);
  let typeAxioms = $state<AxiomItem[]>([]);
  let truncated = $state(false);
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

  const langs = typeof navigator === 'undefined' ? ['en'] : [...(navigator.languages ?? []), 'en'];
  const nameOf = (iri: string) => entityName(iri, labels, prefixes, langs);
  const subOf = (iri: string) => (labels.get(iri)?.length ? iriName(iri, prefixes) : null);

  async function loadAll() {
    const t = target;
    const s = sel;
    if (!name) return;
    const my = ++loadSeq;
    loading = true;
    try {
      const r = await ontologySummary(t, s);
      const [ents, ax, types] = await Promise.all([
        ontologyEntities(t, s, [...TREE_KINDS, 'Datatype']),
        ontologyAxioms(t, s, {
          types: [
            'SubClassOf',
            'SubObjectPropertyOf',
            'SubDataPropertyOf',
            'SubAnnotationPropertyOf',
            'AnnotationAssertion',
          ],
        }),
        ontologyAxioms(t, s, { types: ['ClassAssertion'], assertions: true, cap: 10_000 }),
      ]);
      if (my !== loadSeq) return;
      const byKind = new Map<EntityKind, string[]>();
      for (const e of ents.entities) {
        const l = byKind.get(e.kind) ?? [];
        l.push(e.iri);
        byKind.set(e.kind, l);
      }
      // individuals that only their class assertions name
      const inds = new Set(byKind.get('NamedIndividual') ?? []);
      for (const a of types.axioms)
        if (typeof a.axiom.individual === 'string' && !a.axiom.individual.startsWith('_:'))
          inds.add(a.axiom.individual);
      byKind.set('NamedIndividual', [...inds]);
      summary = r.summary;
      etag = r.etag;
      shown = r.summary.commit;
      loadedTarget = t;
      entities = byKind;
      hierarchy = ax.axioms;
      typeAxioms = types.axioms;
      labels = labelsOf(ax.axioms);
      truncated = ents.truncated || ax.truncated || types.truncated;
      loadError = null;
    } catch (e) {
      if (my === loadSeq) loadError = e as Error;
    } finally {
      if (my === loadSeq) loading = false;
    }
  }

  async function loadSide() {
    const ds = name;
    if (!ds) return;
    try {
      const rows = await api.select(onBranch(ds, branch), ONTOLOGY_GRAPHS, { nocache: true });
      graphs = rows.map((r) => (r.g?.type === 'uri' ? r.g.value : '')).filter(Boolean);
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
      previewOpen = false;
      summary = null;
      shown = 0;
      loadedTarget = null;
      classification = null;
      undo = new UndoStack(`sparkles.onto.undo.${target}|${graph ?? ''}`);
      void loadAll();
      void loadSide();
    });
  });

  // --- trees ---------------------------------------------------------------------------
  const trees = $derived.by(() => {
    const out = new Map<EntityKind, Tree>();
    for (const k of TREE_KINDS) {
      const edges = hierarchyEdges(k, k === 'NamedIndividual' ? [] : hierarchy);
      out.set(k, buildTree(entities.get(k) ?? [], edges));
    }
    return out;
  });
  /** The individuals by their named types. */
  const typesOf = $derived.by(() => {
    const m = new Map<string, string[]>();
    for (const [i, c] of hierarchyEdges('NamedIndividual', typeAxioms))
      m.set(i, [...(m.get(i) ?? []), c]);
    return m;
  });
  let filter = $state('');
  let opened = $state<Record<string, Set<string>>>({});
  const openSet = (key: string) => opened[key] ?? new Set<string>();
  function toggle(key: string, id: string) {
    const s = new Set(openSet(key));
    if (s.has(id)) s.delete(id);
    else s.add(id);
    opened = { ...opened, [key]: s };
  }
  const tree = $derived(kindTab ? trees.get(kindTab) : null);
  const keep = $derived(tree ? filterTree(tree, nameOf, filter) : null);

  // open the path to the selection
  $effect(() => {
    const iri = selected;
    const t = tree;
    const k = kindTab;
    if (!iri || !t || !k) return;
    const anc = ancestorsOf(t, iri);
    untrack(() => {
      const cur = openSet(k);
      if (anc.every((a) => cur.has(a))) return;
      opened = { ...opened, [k]: new Set([...cur, ...anc]) };
    });
  });

  // --- the inferred tree ---------------------------------------------------------------
  let classification = $state<Classification | null>(null);
  let classifyError = $state<string | null>(null);
  let classifying = $state(false);
  async function classify() {
    const my = ++classifySeq;
    classifying = true;
    classifyError = null;
    try {
      const c = await classifyOntology(target, sel);
      if (my === classifySeq) classification = c;
    } catch (e) {
      if (my === classifySeq) classifyError = api.errorMessage(e);
    } finally {
      if (my === classifySeq) classifying = false;
    }
  }
  $effect(() => {
    if (
      inferredOn &&
      tab === 'Class' &&
      summary &&
      (!classification || classification.commit !== summary.commit)
    )
      untrack(() => void classify());
  });
  const inferred = $derived(classification ? taxonomyTree(classification) : null);
  const inferredName = (id: string) => {
    const cs = inferred?.classesOf.get(id) ?? [];
    return cs.length ? cs.map(nameOf).join(' ≡ ') : id;
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
  let frameKinds = $state<EntityKind[]>([]);
  let frameError = $state<string | null>(null);
  let frameFor = $state<string | null>(null);
  async function loadFrame() {
    const iri = selected;
    const t = target;
    const my = ++frameSeq;
    if (!iri) {
      frameItems = [];
      frameKinds = [];
      frameFor = null;
      return;
    }
    try {
      const [report, ax] = await Promise.all([
        ontologyEntity(t, sel, iri),
        ontologyAxioms(t, sel, { about: iri, assertions: true, cap: 5_000 }).catch((e) => {
          if (e instanceof api.ApiError && e.status === 404)
            return { axioms: [], total: 0, truncated: false };
          throw e;
        }),
      ]);
      if (my !== frameSeq) return;
      frameKinds = report?.kinds ?? [];
      frameItems = ax.axioms;
      frameFor = iri;
      frameError = report ? null : 'The ontology neither declares nor uses this IRI.';
    } catch (e) {
      if (my === frameSeq) frameError = api.errorMessage(e);
    }
  }
  $effect(() => {
    void selected;
    void target;
    void graph;
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
  let sessionError = $state<string | null>(null);
  async function startSession() {
    const n = sessionBranchName(auth.who?.principal?.name);
    sessionError = null;
    if (!validBranchName(n)) return void (sessionError = `Cannot name a branch ${n}.`);
    try {
      if (!branchList.some((b) => b.name === n))
        await api.createBranch(name, {
          name: n,
          from: branch ?? MAIN,
          note: 'ontology editing session',
        });
      toasts.push('success', `Editing on branch ${n}`);
      await nav({ branch: n });
    } catch (e) {
      sessionError = api.errorMessage(e);
    }
  }

  // --- edits, previews and the undo stack ---------------------------------------------------
  let undo = $state(new UndoStack());
  let stackTick = $state(0);
  const done = $derived((void stackTick, undo.done));
  const undone = $derived((void stackTick, undo.undone));
  let previewOpen = $state(false);
  let previewOp = $state<Op | null>(null);
  let previewTitle = $state('');
  let pending: { mode: 'edit' | 'undo' | 'redo'; label: string; after?: () => void } = {
    mode: 'edit',
    label: '',
  };
  /** The commits this page made, which live refresh does not announce. */
  const ownCommits = new Set<number>();

  function edit(op: Op, label: string, after?: () => void) {
    pending = { mode: 'edit', label, after };
    previewOp = op;
    previewTitle = label;
    previewOpen = true;
  }

  function undoLast() {
    const top = undo.done.at(-1);
    if (!top) return;
    pending = { mode: 'undo', label: top.label };
    previewOp = { kind: 'revert', commit: top.commit };
    previewTitle = `Undo: ${top.label}`;
    previewOpen = true;
  }

  function redoLast() {
    const top = undo.undone.at(-1);
    if (!top) return;
    pending = { mode: 'redo', label: top.label };
    previewOp = { kind: 'revert', commit: top.commit };
    previewTitle = `Redo: ${top.label}`;
    previewOpen = true;
  }

  async function committed(op: Op, a: OpAnswer) {
    const seq = committedSeq(a);
    if (seq != null) ownCommits.add(seq);
    const p = pending;
    if (p.mode === 'undo') undo.undid(seq);
    else if (p.mode === 'redo') undo.redid(seq);
    else if (seq != null) undo.push({ label: p.label, commit: seq });
    stackTick++;
    if (changedAnything(a)) {
      toasts.push(
        'success',
        p.mode === 'edit' ? p.label : `${p.mode === 'undo' ? 'Undid' : 'Redid'}: ${p.label}`,
        seq != null ? `Commit ${seq}` : undefined,
      );
    } else toasts.push('info', 'The edit changed nothing.');
    p.after?.();
    await loadAll();
    await loadFrame();
  }

  async function stale() {
    toasts.push(
      'info',
      'Another commit landed since the editor read the ontology.',
      'The views now show the new state.',
    );
    await loadAll();
    await loadFrame();
  }

  /** The labels the editor knows with those the previewed edit adds, for its axioms. */
  const previewLabels = $derived.by(() => {
    const added = previewOp ? opAxioms(previewOp).add : [];
    const extra = labelsOf(added.filter((a): a is AxiomJson => typeof a !== 'string'));
    if (!extra.size) return labels;
    const m = new Map(labels);
    for (const [k, v] of extra) if (!m.has(k)) m.set(k, v);
    return m;
  });

  const describe = (a: AxiomInput) => {
    if (typeof a === 'string') return a;
    const named = (iri: string) => entityName(iri, previewLabels, prefixes, langs);
    const sh = (x: unknown) => manchester(x, named, prefixes);
    const anns = a.annotations?.length
      ? ` [${a.annotations.length} annotation${a.annotations.length > 1 ? 's' : ''}]`
      : '';
    switch (a.type) {
      case 'Declaration':
        return `Declaration: ${KIND_SINGULAR[a.kind as EntityKind] ?? a.kind} ${sh(a.iri)}`;
      case 'SubClassOf':
        return `${sh(a.subClass)} SubClassOf ${sh(a.superClass)}${anns}`;
      case 'AnnotationAssertion':
        return `${sh(a.subject)} ${sh(a.property)} ${sh(a.value)}${anns}`;
      case 'ClassAssertion':
        return `${sh(a.individual)} Type ${sh(a.class)}${anns}`;
      default: {
        const parts = Object.entries(a)
          .filter(([k]) => k !== 'type' && k !== 'annotations')
          .map(([, v]) => (Array.isArray(v) ? v.map(sh).join(', ') : sh(v)));
        return `${a.type}(${parts.join(' ')})${anns}`;
      }
    }
  };

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
      nav({ tab: createKind, iri, view: null }),
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
      `Delete ${nameOf(iri)}${uses != null ? ` and the ${fmtInt(uses)} axioms that mention it` : ''}`,
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
  let live = $state<'off' | 'on' | 'stopped'>('off');
  let notice = $state<string | null>(null);
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const t = target;
    if (!name) return;
    const ctl = new AbortController();
    const pause = (ms: number) => new Promise((r) => setTimeout(r, ms));
    const run = async () => {
      // wait for this target's first read, so the stream starts at the commit it shows
      while (!ctl.signal.aborted && loadedTarget !== t) await pause(200);
      let after = shown;
      let first = true;
      while (!ctl.signal.aborted) {
        // a stream that ended, or that the server refused, is opened again after a pause
        if (!first) await pause(1000);
        first = false;
        if (ctl.signal.aborted) return;
        live = 'on';
        try {
          after = await followCommits(
            t,
            after,
            (c) => {
              if (c.seq <= shown) return;
              if (!ownCommits.has(c.seq))
                notice = `Commit ${c.seq}${c.message ? ` (${c.message})` : ''} by another writer`;
              clearTimeout(refreshTimer);
              refreshTimer = setTimeout(() => {
                void loadAll().then(loadFrame);
              }, 300);
            },
            ctl.signal,
          );
        } catch {
          if (!ctl.signal.aborted) live = 'stopped';
          return;
        }
      }
    };
    void run();
    return () => {
      ctl.abort();
      clearTimeout(refreshTimer);
      live = 'off';
    };
  });

  onMount(() => {
    const key = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable))
        return;
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z' && !readonly) {
        e.preventDefault();
        if (e.shiftKey) redoLast();
        else undoLast();
      }
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  });

  const counts = $derived(
    Object.fromEntries(TREE_KINDS.map((k) => [k, entities.get(k)?.length ?? 0])),
  );
  const headerTitle = $derived(
    summary?.header.iri
      ? labels.get(summary.header.iri)
        ? nameOf(summary.header.iri)
        : summary.header.iri
      : 'Anonymous ontology',
  );
</script>

<svelte:head><title>Ontology{name ? ` · ${name}` : ''} | Sparkles</title></svelte:head>

<div class="page page-container onto">
  <header class="page-head">
    <div class="title">
      <h1>Ontology</h1>
      <p class="muted">
        {#if summary}
          <span class="mono">{headerTitle}</span>
          · commit {summary.commit}
          {#if summary.header.versionIri}· version <span class="mono"
              >{iriName(summary.header.versionIri, prefixes)}</span
            >{/if}
        {:else if !name}
          Choose a dataset.
        {:else}
          Reading the ontology of {name}…
        {/if}
      </p>
    </div>
    <span class="spacer"></span>
    <div class="bar">
      <label class="pick">
        <span class="faint">Dataset</span>
        <select
          class="select"
          value={name}
          onchange={(e) =>
            goto(`${resolve('/ontology')}?ds=${encodeURIComponent(e.currentTarget.value)}`)}
        >
          {#if !name}<option value="">Choose…</option>{/if}
          {#each app.datasets as d (d.name)}<option value={d.name}>{d.name}</option>{/each}
        </select>
      </label>
      <label class="pick">
        <span class="faint">Graph</span>
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
      {#if branchList.length}
        <label class="pick">
          <span class="faint">Branch</span>
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
      {#if canWrite && !branch}
        <button
          class="btn sm"
          onclick={startSession}
          title="Edit on a branch of your own, then merge it"
          ><Icon name="branch" size={13} /> Start session</button
        >
      {/if}
      {#if branch}
        <a
          class="btn sm"
          href="{resolve('/datasets/[name]/merge', { name })}?source={encodeURIComponent(
            branch,
          )}&target={encodeURIComponent(currentBranch?.upstream ?? MAIN)}"
          ><Icon name="merge" size={13} /> Merge</a
        >
      {/if}
      <button
        class="btn sm"
        disabled={!done.length || readonly}
        onclick={undoLast}
        title={done.at(-1) ? `Undo: ${done.at(-1)?.label}` : 'Nothing to undo'}
        >Undo{done.length ? ` (${done.length})` : ''}</button
      >
      <button
        class="btn sm"
        disabled={!undone.length || readonly}
        onclick={redoLast}
        title={undone.at(-1) ? `Redo: ${undone.at(-1)?.label}` : 'Nothing to redo'}>Redo</button
      >
      <span
        class="live"
        class:on={live === 'on'}
        title={live === 'on' ? 'Following the change feed' : 'Not following the change feed'}
        ><span class="dot"></span>{live === 'on'
          ? 'Live'
          : live === 'stopped'
            ? 'Live updates stopped'
            : ''}</span
      >
    </div>
  </header>

  {#if sessionError}<p class="error-box" role="alert">{sessionError}</p>{/if}
  {#if notice}
    <p class="notice" role="status">
      {notice}. The views show the new state.
      <button class="btn ghost sm" onclick={() => (notice = null)} aria-label="Dismiss"
        ><Icon name="x" size={12} /></button
      >
    </p>
  {/if}
  {#if protectedBranch}
    <p class="notice" role="status">
      {branch ?? MAIN} is protected and takes changes through merges only. Start a session branch to edit.
    </p>
  {:else if !canWrite && name}
    <p class="notice" role="status">You can read this dataset but not edit it.</p>
  {/if}
  {#if loadError}
    <div class="error-box" role="alert">
      <strong>The ontology could not be read.</strong>
      {api.errorMessage(loadError)}
    </div>
  {/if}
  {#if truncated}
    <p class="notice" role="status">
      The ontology is larger than the editor reads at once. The trees show the first 50,000 entries.
    </p>
  {/if}

  {#if name && summary}
    <!-- one strip for the entity kinds and the ontology's views, which wraps rather than
         hiding tabs past the edge -->
    <div class="tabs onto-tabs" role="tablist" aria-label="Ontology views">
      {#each TREE_KINDS as k (k)}
        <button class="tab" role="tab" aria-selected={tab === k} onclick={() => nav({ tab: k })}
          >{KIND_LABEL[k]} <span class="count">{counts[k] || ''}</span></button
        >
      {/each}
      <span class="tab-sep" aria-hidden="true"></span>
      <button
        class="tab"
        role="tab"
        aria-selected={tab === 'dlquery'}
        onclick={() => nav({ tab: 'dlquery' })}>DL Query</button
      >
      <button
        class="tab"
        role="tab"
        aria-selected={tab === 'header'}
        onclick={() => nav({ tab: 'header', iri: null })}>Header</button
      >
    </div>
    <div class="panes">
      <section class="panel trees" aria-label="Entities">
        {#if kindTab && tree}
          <div class="tree-bar">
            <input
              class="input"
              type="search"
              placeholder="Filter {KIND_LABEL[kindTab].toLowerCase()}"
              aria-label="Filter"
              bind:value={filter}
            />
            {#if !readonly}
              <button
                class="btn sm"
                onclick={() =>
                  openCreate(
                    kindTab,
                    selected && kindTab !== 'NamedIndividual' && tree?.nodes.has(selected)
                      ? selected
                      : kindTab === 'NamedIndividual' &&
                          selected &&
                          (trees.get('Class')?.nodes.has(selected) ?? false)
                        ? selected
                        : null,
                  )}
                title={selected
                  ? `New ${KIND_SINGULAR[kindTab]} under the selection`
                  : `New ${KIND_SINGULAR[kindTab]}`}><Icon name="plus" size={13} /> New</button
              >
            {/if}
            {#if tab === 'Class'}
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
          <div class="tree-cols" class:two={tab === 'Class' && inferredOn}>
            <div class="tree-col">
              {#if tab === 'Class' && inferredOn}<h2 class="col-title">Asserted</h2>{/if}
              <div class="tree-scroll">
                {#if tree.roots.length}
                  <OntoTree
                    label="{KIND_LABEL[kindTab]} (asserted)"
                    roots={tree.roots}
                    children={(id) => tree?.children.get(id) ?? []}
                    name={nameOf}
                    sub={subOf}
                    badge={kindTab === 'NamedIndividual'
                      ? (id) => (typesOf.get(id) ?? []).map(nameOf).join(', ') || null
                      : undefined}
                    open={openSet(kindTab)}
                    {selected}
                    {keep}
                    onselect={(id) => nav({ iri: id })}
                    ontoggle={(id) => toggle(kindTab, id)}
                  />
                {:else}
                  <p class="empty">No {KIND_LABEL[kindTab].toLowerCase()}.</p>
                {/if}
              </div>
            </div>
            {#if tab === 'Class' && inferredOn}
              <div class="tree-col">
                <h2 class="col-title">
                  Inferred
                  {#if classification}<span class="faint">
                      {classification.complete
                        ? 'OWL 2 EL'
                        : `${fmtInt(classification.excludedCount)} axioms outside OWL 2 EL`}</span
                    >{/if}
                </h2>
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
              </div>
            {/if}
          </div>
        {/if}
        {#if tab === 'header'}
          <p class="faint pad">The header describes the ontology of the graph as a whole.</p>
        {/if}
        {#if tab === 'dlquery'}
          {@render dlPanel()}
        {/if}
      </section>

      <section class="panel detail" aria-label="Details">
        {#if tab === 'header' || !selected}
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
            {#if !readonly && frameKinds.length}
              <button class="btn sm" onclick={() => openRename(selected!)}
                ><Icon name="pencil" size={13} /> Rename</button
              >
              <button class="btn sm danger" onclick={() => remove(selected!)}
                ><Icon name="trash" size={13} /> Delete</button
              >
            {/if}
          </div>
          <div class="tabs sub" role="tablist" aria-label="Entity views">
            <button
              class="tab"
              role="tab"
              aria-selected={view === 'frame'}
              onclick={() => nav({ view: null })}>Frame</button
            >
            <button
              class="tab"
              role="tab"
              aria-selected={view === 'usage'}
              onclick={() => nav({ view: 'usage' })}
              >Usage <span class="count">{frameFor === selected ? frameItems.length : ''}</span
              ></button
            >
          </div>
          <div class="panel-body">
            {#if frameError}<p class="error-box" role="alert">{frameError}</p>{/if}
            {#if inferredOn && frameKind === 'Class' && inferred && selected}
              {@const sup = inferredParents(selected)}
              {#if sup.length}
                <p class="inferred-sup">
                  <span class="faint">Inferred direct superclasses:</span>
                  {#each sup as c, i (c)}{#if i},
                    {/if}<button class="link" onclick={() => nav({ iri: c })}>{nameOf(c)}</button
                    >{/each}
                </p>
              {/if}
            {/if}
            {#if frameFor === selected && view === 'frame'}
              {#key `${selected}|${frameKind}`}
                <OntoFrame
                  kind={frameKind}
                  iri={selected}
                  items={frameItems}
                  name={nameOf}
                  {prefixes}
                  {options}
                  {annotationProperties}
                  {readonly}
                  {names}
                  {parse}
                  onedit={(op, label) => edit(op, label)}
                  onopen={(iri) => nav({ iri, view: null })}
                />
              {/key}
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
  {:else if loading}
    <p class="faint"><span class="spinner"></span> Reading the ontology…</p>
  {:else if !name}
    <p class="empty">Choose a dataset to edit its ontology.</p>
  {/if}
</div>

<OntoPreview
  bind:open={previewOpen}
  title={previewTitle}
  {target}
  {sel}
  op={previewOp}
  {etag}
  head={summary?.commit ?? null}
  {describe}
  oncommitted={(op, a) => void committed(op, a)}
  onstale={() => void stale()}
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
  .onto {
    gap: 12px;
  }
  .title h1 {
    font-size: var(--fs-xl);
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: end;
  }
  .pick {
    display: grid;
    gap: 2px;
    font-size: var(--fs-xs);
  }
  .live {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-xs);
    color: var(--text-3);
    min-height: 26px;
  }
  .live .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--border-strong);
  }
  .live.on .dot {
    background: var(--ok);
  }
  .notice {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border-radius: var(--r);
    background: var(--spark-soft);
    font-size: var(--fs-sm);
  }
  .panes {
    display: grid;
    grid-template-columns: minmax(260px, 2fr) minmax(0, 3fr);
    gap: 12px;
    align-items: start;
    min-width: 0;
  }
  .trees,
  .detail {
    min-width: 0;
    display: grid;
    align-content: start;
  }
  .onto-tabs {
    flex-wrap: wrap;
    row-gap: 0;
    border-bottom: 1px solid var(--border);
  }
  .tab-sep {
    width: 1px;
    align-self: center;
    height: 18px;
    margin: 0 6px;
    background: var(--border);
    flex: none;
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
  }
  .tree-cols.two {
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  }
  .tree-col {
    min-width: 0;
  }
  .tree-cols.two .tree-col + .tree-col {
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
    max-height: calc(100vh - 240px);
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
  .tabs.sub {
    padding: 0 10px;
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
  @media (max-width: 900px) {
    .panes {
      grid-template-columns: minmax(0, 1fr);
    }
    .tree-scroll {
      max-height: 50vh;
    }
  }
  @media (max-width: 600px) {
    .tree-cols.two {
      grid-template-columns: minmax(0, 1fr);
    }
    .tree-cols.two .tree-col + .tree-col {
      border-left: 0;
      border-top: 1px solid var(--border);
    }
  }
</style>

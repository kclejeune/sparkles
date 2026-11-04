<script lang="ts">
  // The thesaurus editor (docs/specs/U12-ontology-workbench/SPEC.md, "SKOS and SKOS-XL
  // thesaurus editor"): the concept schemes, concepts and collections of one graph, with the
  // concept tree of a scheme read one level at a time, label search in any language, the
  // labels, links and notes of a resource, and the SKOS integrity checks of the built-in
  // shapes. Every edit is a dry run first and then one commit on the branch the page works
  // on, with If-Match naming the commit the page shows, through the ontology editor's
  // session: undo and redo by revert, session branches and live refresh.
  import { useAskContext } from '$lib/ask-thread.svelte';
  import { shortLabel } from '$lib/graph';
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { graphRestricted } from '$lib/auth';
  import { branchParam, MAIN, onBranch } from '$lib/branches';
  import { EditSession, startSessionBranch } from '$lib/edit-session.svelte';
  import { getForm, type FormSaveAnswer } from '$lib/forms-api';
  import type { FormModel } from '$lib/forms';
  import { fmtInt } from '$lib/format';
  import { useCommands, usePageSearch, useProvider } from '$lib/launcher.svelte';
  import { branchProvider } from '$lib/palette.svelte';
  import {
    iriName,
    loadPolicy,
    mintIri,
    renameUpdate,
    usedQuery,
    validIri,
    validLang,
    type AxiomInput,
    type Selection,
  } from '$lib/ontology';
  import {
    aboveQuery,
    ancestorsQuery,
    CHILD_CAP,
    childrenQuery,
    COLLECTION,
    collectionsQuery,
    CONCEPT,
    ConceptTree,
    createConceptEdit,
    createTypedEdit,
    deleteEdit,
    detailsQuery,
    displayLabel,
    isCollection,
    isConcept,
    isScheme,
    labelMap,
    labelsQuery,
    languagesQuery,
    linkEdit,
    linkTriples,
    MAPPING_NAME,
    MAPPINGS,
    moveEdit,
    normLang,
    ORDERED,
    otherGraphsQuery,
    parseDetails,
    readHead,
    rootsQuery,
    SCHEME,
    schemesQuery,
    searchHits,
    searchQuery,
    SESSION_PREFIX,
    SKOS_GRAPHS,
    skosShapes,
    triplesEdit,
    type Details,
    type Label,
    type Link,
    type Place,
    type SearchHit,
  } from '$lib/skos';
  import Icon from '$components/Icon.svelte';
  import Modal from '$components/Modal.svelte';
  import OntoPreview from '$components/OntoPreview.svelte';
  import OntoTree, { TREE_DRAG } from '$components/OntoTree.svelte';
  import ShapeForm from '$components/ShapeForm.svelte';
  import SkosLabels from '$components/SkosLabels.svelte';
  import SkosLinks from '$components/SkosLinks.svelte';

  type Tab = 'concepts' | 'collections' | 'checks';
  type View = 'labels' | 'relations' | 'notes' | 'members';
  const GROUP = 'urn:x-sparkles:skos:';

  // --- the URL's state ---------------------------------------------------------------
  const q = $derived(page.url.searchParams);
  const name = $derived(q.get('ds') ?? app.current ?? '');
  const branch = $derived(branchParam(q.get('branch')));
  const target = $derived(onBranch(name, branch));
  const graph = $derived(q.get('graph') || null);
  const sel: Selection = $derived({ graph, imports: false });
  const tab = $derived<Tab>(
    q.get('tab') === 'collections' || q.get('tab') === 'checks'
      ? (q.get('tab') as Tab)
      : 'concepts',
  );
  const selected = $derived(q.get('iri'));
  const viewParam = $derived(q.get('view') as View | null);
  const displayLang = $derived(q.get('lang') ?? '');
  const prefixes = $derived(app.prefixes(name));
  // the entity on screen is the context of a question asked from the Ask drawer
  useAskContext(() =>
    selected
      ? {
          kind: 'entity',
          iri: selected,
          label: shortLabel(selected, prefixes),
          type: 'concept',
          branch,
        }
      : null,
  );
  const browserLangs =
    typeof navigator === 'undefined' ? ['en'] : [...(navigator.languages ?? []), 'en'];
  const langs = $derived(displayLang ? [displayLang, ...browserLangs] : browserLangs);

  function withParams(over: Record<string, string | null>) {
    const p = new URLSearchParams(page.url.searchParams);
    if (!p.get('ds') && name) p.set('ds', name);
    for (const [k, v] of Object.entries(over)) {
      if (v == null || v === '') p.delete(k);
      else p.set(k, v);
    }
    return `${resolve('/thesaurus')}?${p}`;
  }
  const nav = (over: Record<string, string | null>, replace = false) =>
    goto(withParams(over), { replaceState: replace, keepFocus: true, noScroll: true });

  $effect(() => {
    if (name && name !== app.current && app.datasets.some((d) => d.name === name))
      app.setDataset(name);
    if (name) void app.loadPrefixes(name);
  });

  // --- what the page read -------------------------------------------------------------
  let head = $state<{ etag: string | null; commit: number } | null>(null);
  let loadError = $state<string | null>(null);
  let loading = $state(false);
  let schemes = $state<string[]>([]);
  let collections = $state<{ iri: string; ordered: boolean }[]>([]);
  let languages = $state<string[]>([]);
  let labels = $state<Map<string, Label[]>>(new Map());
  let tree = $state(new ConceptTree());
  let treeTick = $state(0);
  let graphs = $state<string[]>([]);
  let branchList = $state<api.Branch[]>([]);
  /** The commit the views show, and the target whose first read they show. */
  let shown = 0;
  let loadedTarget = $state<string | null>(null);
  let loadSeq = 0;
  let detailSeq = 0;
  /** The tree nodes that are open. */
  let opened = $state(new Set<string>());

  /** The scheme the tree shows: the URL's, else the first; `*` is every concept. */
  const schemeParam = $derived(q.get('scheme'));
  const scheme = $derived(
    schemeParam === '*' ? null : (schemeParam ?? (schemes.length ? schemes[0] : null)),
  );

  const nameOf = (iri: string) => displayLabel(labels.get(iri), langs) ?? iriName(iri, prefixes);
  const subOf = (iri: string) => (labels.get(iri)?.length ? iriName(iri, prefixes) : null);

  const select = (sparql: string) => api.select(target, sparql, { nocache: true });

  /** Read the labels of `iris` the page does not know yet, or all of them with `again`. */
  async function readLabels(iris: string[], again = false) {
    const want = [...new Set(iris)].filter((i) => again || !labels.has(i));
    if (!want.length) return;
    const m = new Map(labels);
    for (let i = 0; i < want.length; i += 400) {
      const part = want.slice(i, i + 400);
      const got = labelMap(await select(labelsQuery(part, graph)));
      for (const iri of part) m.set(iri, got.get(iri) ?? []);
    }
    labels = m;
  }

  async function loadAll() {
    const t = target;
    if (!name) return;
    const my = ++loadSeq;
    loading = true;
    try {
      // without the tag no edit could name the commit it was made against: read-only
      const h = await readHead(t, graph).catch(() => ({ etag: null, commit: 0 }));
      const [sRows, cRows, lRows] = await Promise.all([
        select(schemesQuery(graph)),
        select(collectionsQuery(graph)),
        select(languagesQuery(graph)).catch(() => []),
      ]);
      if (my !== loadSeq) return;
      schemes = sRows.flatMap((r) => (r.s?.type === 'uri' ? [r.s.value] : []));
      collections = cRows.flatMap((r) =>
        r.c?.type === 'uri' ? [{ iri: r.c.value, ordered: Number(r.ordered?.value ?? 0) > 0 }] : [],
      );
      languages = lRows.flatMap((r) =>
        r.lang?.type === 'literal' && r.lang.value ? [r.lang.value] : [],
      );
      await loadTree(my);
      if (my !== loadSeq) return;
      await readLabels([...schemes, ...collections.map((c) => c.iri)], true);
      head = h;
      shown = h.commit;
      loadedTarget = t;
      loadError = null;
    } catch (e) {
      if (my === loadSeq) loadError = api.errorMessage(e);
    } finally {
      if (my === loadSeq) loading = false;
    }
  }

  /** The roots of the scheme and the children of every node the tree has read. */
  async function loadTree(my = loadSeq) {
    const next = new ConceptTree();
    const sc = schemeParam === '*' ? null : (schemeParam ?? schemes[0] ?? null);
    next.setRoots(await select(rootsQuery(sc, graph)));
    // the nodes read before and the ones an edit opened
    const want = [...new Set([...tree.children.keys(), ...opened])];
    for (let i = 0; i < want.length; i += 200) {
      const part = want.slice(i, i + 200);
      next.addChildren(part, await select(childrenQuery(part, graph)));
    }
    if (my !== loadSeq) return;
    tree = next;
    treeTick++;
    await readLabels([...next.roots, ...[...next.children.values()].flat()], true);
  }

  async function loadChildren(parents: string[]) {
    const want = parents.filter((p) => !tree.loaded(p));
    if (!want.length) return;
    for (let i = 0; i < want.length; i += 200) {
      const part = want.slice(i, i + 200);
      tree.addChildren(part, await select(childrenQuery(part, graph)));
    }
    treeTick++;
    await readLabels(want.flatMap((p) => tree.children.get(p) ?? []));
  }

  async function loadSide() {
    const ds = name;
    if (!ds) return;
    try {
      const rows = await api.select(onBranch(ds, branch), SKOS_GRAPHS, { nocache: true });
      graphs = rows.flatMap((r) => (r.g?.type === 'uri' ? [r.g.value] : []));
    } catch {
      graphs = [];
    }
    try {
      branchList = (await api.branches(ds)).branches;
    } catch {
      branchList = [];
    }
  }

  const session = new EditSession({
    reload: async () => {
      await loadAll();
      await loadDetails();
    },
    staleText: 'Another commit landed since the editor read the thesaurus.',
  });

  $effect(() => {
    void target;
    void graph;
    untrack(() => {
      head = null;
      shown = 0;
      loadedTarget = null;
      tree = new ConceptTree();
      labels = new Map();
      session.reset(`sparkles.skos.undo.${target}|${graph ?? ''}`);
      void loadAll();
      void loadSide();
    });
  });

  // a new scheme: its own roots
  $effect(() => {
    void schemeParam;
    untrack(() => {
      if (!loadedTarget) return;
      tree = new ConceptTree();
      void loadTree();
    });
  });

  // --- the tree -------------------------------------------------------------------------
  function toggle(id: string) {
    const s = new Set(opened);
    if (s.has(id)) s.delete(id);
    else {
      s.add(id);
      void loadChildren([id]);
    }
    opened = s;
  }
  const kidsOf = (id: string) => (void treeTick, tree.children.get(id) ?? []);
  const moreOf = (id: string) => (void treeTick, !tree.loaded(id) && tree.count(id) > 0);
  const badgeOf = (id: string) => {
    void treeTick;
    const ps = tree.ups.get(id) ?? 0;
    const parts: string[] = [];
    if (scheme && tree.roots.includes(id) && !tree.top.has(id)) parts.push('not a top concept');
    if (ps > 1) parts.push(`${ps} broader`);
    if (tree.capped.has(id)) parts.push(`first ${fmtInt(CHILD_CAP)}`);
    return parts.join(' · ') || null;
  };

  /** Open the paths from the roots to `iri`. */
  async function reveal(iri: string) {
    try {
      const anc = (await select(ancestorsQuery(iri, graph))).flatMap((r) =>
        r.a?.type === 'uri' ? [r.a.value] : [],
      );
      await loadChildren(anc);
      opened = new Set([...opened, ...anc]);
    } catch {
      /* the tree stays as it is */
    }
  }

  // --- search ---------------------------------------------------------------------------
  let search = $state('');
  let searchLang = $state('');
  let hits = $state<SearchHit[]>([]);
  let searching = $state(false);
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  let searchSeq = 0;
  $effect(() => {
    const text = search.trim();
    const l = searchLang;
    const sc = scheme;
    void target;
    clearTimeout(searchTimer);
    if (!text) {
      hits = [];
      return;
    }
    const my = ++searchSeq;
    searchTimer = setTimeout(async () => {
      searching = true;
      try {
        const rows = await select(searchQuery(text, { lang: l || null, scheme: sc, graph }));
        const hs = searchHits(rows, text);
        await readLabels(hs.map((h) => h.iri));
        if (my === searchSeq) hits = hs;
      } catch (e) {
        if (my === searchSeq) toasts.push('error', 'The search failed.', api.errorMessage(e));
      } finally {
        if (my === searchSeq) searching = false;
      }
    }, 250);
  });
  function pickHit(h: SearchHit) {
    search = '';
    void nav({ iri: h.iri, tab: null });
    void reveal(h.iri);
  }

  // --- the selection ------------------------------------------------------------------------
  let details = $state<Details | null>(null);
  let detailsError = $state<string | null>(null);
  /** The shapes the notes form reads: the dataset's when they cover the resource, else SKOS. */
  let formShapes = $state<'skos' | undefined>('skos');
  async function loadDetails() {
    const iri = selected;
    const my = ++detailSeq;
    if (!iri || !name) {
      details = null;
      return;
    }
    try {
      const d = parseDetails(iri, await select(detailsQuery(iri, graph)));
      await readLabels(
        [iri, ...d.broader, ...d.narrower, ...d.related, ...d.topConceptOf, ...d.topConcepts]
          .map((l) => (typeof l === 'string' ? l : l.iri))
          .concat(
            d.inScheme,
            d.members,
            d.memberOf,
            d.mappings.map((m) => m.target),
          ),
        // an edit may have changed any of them
        true,
      );
      if (my !== detailSeq) return;
      details = d;
      detailsError = null;
    } catch (e) {
      if (my === detailSeq) detailsError = api.errorMessage(e);
    }
  }
  $effect(() => {
    void selected;
    void target;
    void graph;
    untrack(() => void loadDetails());
  });
  // the dataset's own shapes when they lay out a form for the resource
  $effect(() => {
    const iri = selected;
    const ds = name;
    const b = branch;
    if (!iri || !ds) return;
    const ctl = new AbortController();
    void getForm(ds, iri, { branch: b, signal: ctl.signal })
      .then((m) => (formShapes = m.fields.length ? undefined : 'skos'))
      .catch(() => (formShapes = 'skos'));
    return () => ctl.abort();
  });
  // open the path to a selection made from a link
  $effect(() => {
    const iri = selected;
    if (iri && loadedTarget && tab === 'concepts') untrack(() => void reveal(iri));
  });

  const kind = $derived<'concept' | 'scheme' | 'collection' | 'other'>(
    !details
      ? 'other'
      : isCollection(details)
        ? 'collection'
        : isScheme(details) && !isConcept(details)
          ? 'scheme'
          : isConcept(details) || details.labels.length
            ? 'concept'
            : 'other',
  );
  const view = $derived<View>(
    viewParam &&
      (kind === 'collection'
        ? ['labels', 'members', 'notes']
        : kind === 'scheme'
          ? ['labels', 'relations', 'notes']
          : ['labels', 'relations', 'notes']
      ).includes(viewParam)
      ? viewParam
      : 'labels',
  );

  // --- access and the session -----------------------------------------------------------
  const currentBranch = $derived(branchList.find((b) => b.name === (branch ?? MAIN)));
  const canWrite = $derived(auth.can(name, 'write'));
  const canAdmin = $derived(auth.can(name, 'admin'));
  const protectedBranch = $derived(!!currentBranch?.protected);
  /** The head could not be read, so edits could not carry `If-Match`. */
  const noHead = $derived(!!head && !head.etag);
  // until the head and its tag are read, no edit, undo or redo can name its commit
  const readonly = $derived(!canWrite || protectedBranch || !head?.etag);
  let sessionError = $state<string | null>(null);
  async function startSession() {
    sessionError = null;
    try {
      const n = await startSessionBranch(name, {
        from: branch,
        branches: branchList,
        user: auth.who?.principal?.name,
        prefix: SESSION_PREFIX,
        note: 'thesaurus editing session',
      });
      await nav({ branch: n });
    } catch (e) {
      sessionError = api.errorMessage(e);
    }
  }

  $effect(() => {
    const t = target;
    if (!name) return;
    return session.follow(
      t,
      () => shown,
      () => loadedTarget === t,
    );
  });
  onMount(() => session.keys(() => readonly));

  // the command palette (ROADMAP item 631): `/` searches the labels, and this page's commands
  let searchInput: HTMLInputElement | undefined = $state();
  usePageSearch(() => {
    if (!searchInput) return false;
    searchInput.focus();
    searchInput.select();
  });
  useCommands(() => [
    {
      id: 'thesaurus.session',
      label: 'Start session',
      icon: 'branch',
      keywords: ['edit', 'branch'],
      subtitle: 'Edit on a branch of your own, then merge it',
      active: () => !!name && canWrite && !branch,
      run: startSession,
    },
    {
      id: 'thesaurus.new.concept',
      label: 'New concept…',
      icon: 'plus',
      keywords: ['create', 'add', 'entity'],
      active: () => !!name && !readonly,
      run: () => openCreate('concept'),
    },
    {
      id: 'thesaurus.new.scheme',
      label: 'New concept scheme…',
      icon: 'plus',
      keywords: ['create', 'add'],
      active: () => !!name && !readonly,
      run: () => openCreate('scheme'),
    },
    {
      id: 'thesaurus.new.collection',
      label: 'New collection…',
      icon: 'plus',
      keywords: ['create', 'add'],
      active: () => !!name && !readonly,
      run: () => openCreate('collection'),
    },
    {
      id: 'thesaurus.checks',
      label: 'Run checks',
      icon: 'check',
      keywords: ['validate', 'quality'],
      active: () => !!name,
      run: () => (tab === 'checks' ? runChecks() : void nav({ tab: 'checks' })),
    },
    {
      id: 'thesaurus.undo',
      label: 'Undo',
      icon: 'stepBack',
      active: () => !readonly && session.done.length > 0,
      run: () => void session.undoLast(),
    },
    {
      id: 'thesaurus.redo',
      label: 'Redo',
      icon: 'stepForward',
      active: () => !readonly && session.undone.length > 0,
      run: () => void session.redoLast(),
    },
  ]);
  useProvider(
    branchProvider({
      list: () => branchList.map((b) => b.name),
      current: () => branch ?? MAIN,
      choose: (n) => void nav({ branch: n === MAIN ? null : n }),
    }),
  );

  const edit = (sparql: string, label: string, after?: () => void) =>
    session.edit({ kind: 'update', sparql }, label, after);
  const describe = (a: AxiomInput) => (typeof a === 'string' ? a : a.type);

  function formSaved(a: FormSaveAnswer, what: string) {
    const seq = a.committed ? a.commit.seq : null;
    session.record(seq, what);
    if (seq != null) toasts.push('success', what, `Commit ${seq}`);
    if (a.branchCreated || (a.branch && a.branch !== (branch ?? MAIN)))
      void nav({ branch: a.branch === MAIN ? null : a.branch });
    else void loadAll().then(loadDetails);
  }

  // --- moves ---------------------------------------------------------------------------
  /** Where a node of the tree hangs: under `from`, or at the top of the scheme. */
  const placeOf = (node: string, from: string | null): Place =>
    from ? { parent: from } : scheme && tree.top.has(node) ? { top: scheme } : null;

  async function move(node: string, from: string | null, to: Place, copy: boolean) {
    if (readonly) return;
    if (to && 'parent' in to) {
      try {
        const r = await api.query(target, aboveQuery(node, to.parent, graph), { nocache: true });
        if (r.boolean)
          return void toasts.push(
            'error',
            `${nameOf(to.parent)} is ${nameOf(node)} or one of its narrower concepts.`,
          );
      } catch {
        /* the dry run still shows the edit */
      }
    }
    const fromPlace = placeOf(node, from);
    if (!copy && JSON.stringify(fromPlace) === JSON.stringify(to)) return;
    const where = !to
      ? 'the top'
      : 'parent' in to
        ? nameOf(to.parent)
        : `the top of ${nameOf(to.top)}`;
    edit(
      moveEdit(node, fromPlace, to, copy, graph),
      copy
        ? `Add ${where} as a broader place of ${nameOf(node)}`
        : `Move ${nameOf(node)} under ${where}`,
      () => {
        if (to && 'parent' in to) opened = new Set([...opened, to.parent]);
      },
    );
  }
  let topOver = $state(false);

  // --- links -----------------------------------------------------------------------------
  const linkName = (p: string) =>
    ({
      broader: 'broader',
      narrower: 'narrower',
      related: 'related',
      inScheme: 'in scheme',
      topConceptOf: 'top concept of',
      hasTopConcept: 'top concept',
    })[p] ??
    MAPPING_NAME[p]?.toLowerCase() ??
    p;
  async function addLink(property: string, other: string, picked?: string) {
    const r = selected;
    if (!r) return;
    await readLabels([other]).catch(() => {});
    const n = labels.get(other)?.length ? nameOf(other) : (picked ?? nameOf(other));
    edit(
      linkEdit(r, property, other, false, graph),
      `Add ${n} as ${linkName(property)} of ${nameOf(r)}`,
    );
  }
  function removeLink(property: string, other: string) {
    if (!selected) return;
    edit(
      linkEdit(selected, property, other, true, graph),
      `Remove ${nameOf(other)} as ${linkName(property)} of ${nameOf(selected)}`,
    );
  }
  function repair(property: string, l: Link) {
    if (!selected) return;
    const ts = linkTriples(selected, property, l.iri);
    edit(
      triplesEdit([], ts, graph),
      `State both directions of ${nameOf(selected)} ${linkName(property)} ${nameOf(l.iri)}`,
    );
  }
  const strLinks = (xs: string[]): Link[] =>
    xs.map((iri) => ({ iri, forward: true, inverse: true }));
  const picker = $derived({ target, dataset: name, graph, prefixes, langs });

  // --- create, rename, delete --------------------------------------------------------------
  let createOpen = $state(false);
  let createKind = $state<'concept' | 'scheme' | 'collection'>('concept');
  let createAt = $state<Place>(null);
  let createLabel = $state('');
  let createLang = $state('en');
  let createIri = $state('');
  let createIriTouched = $state(false);
  let createOrdered = $state(false);
  let createProblem = $state<string | null>(null);
  const policy = $derived(loadPolicy(name, scheme));
  const taken = $derived(
    (void treeTick,
    new Set([
      ...schemes,
      ...collections.map((c) => c.iri),
      ...tree.roots,
      ...[...tree.children.values()].flat(),
    ])),
  );
  $effect(() => {
    if (!createOpen || createIriTouched) return;
    createIri = mintIri(policy, createLabel, 'NamedIndividual', taken);
  });
  function openCreate(k: typeof createKind) {
    createKind = k;
    createAt =
      k !== 'concept'
        ? null
        : selected && kind === 'concept'
          ? { parent: selected }
          : scheme
            ? { top: scheme }
            : null;
    createLabel = '';
    createLang = normLang(displayLang || browserLangs[0]?.split('-')[0] || 'en');
    createIriTouched = false;
    createOrdered = false;
    createProblem = null;
    createIri = mintIri(policy, '', 'NamedIndividual', taken);
    createOpen = true;
  }
  async function submitCreate() {
    const iri = createIri.trim();
    const label: Label = { value: createLabel.trim(), lang: normLang(createLang) };
    if (!label.value) return void (createProblem = 'Give the new resource a label.');
    if (!validIri(iri)) return void (createProblem = 'The IRI is not an absolute IRI.');
    if (!validLang(label.lang)) return void (createProblem = 'The language tag is not valid.');
    try {
      const used = await api.query(target, usedQuery(iri, graph), { nocache: true });
      if (used.boolean) return void (createProblem = 'The graph already uses this IRI.');
    } catch {
      /* the dry run still shows what the edit does */
    }
    createOpen = false;
    const sparql =
      createKind === 'concept'
        ? createConceptEdit(iri, label, { scheme, at: createAt }, graph)
        : createTypedEdit(
            iri,
            createKind === 'scheme' ? SCHEME : createOrdered ? ORDERED : COLLECTION,
            label,
            graph,
          );
    const what = `Create the ${createKind === 'collection' && createOrdered ? 'ordered collection' : createKind} ${label.value}`;
    edit(sparql, what, () => {
      if (createAt && 'parent' in createAt) opened = new Set([...opened, createAt.parent]);
      void nav({
        iri,
        view: null,
        ...(createKind === 'scheme' ? { scheme: iri, tab: null } : {}),
        ...(createKind === 'collection' ? { tab: 'collections' } : {}),
      });
    });
  }

  let renameOpen = $state(false);
  let renameTo = $state('');
  let renameProblem = $state<string | null>(null);
  async function submitRename() {
    const from = selected;
    const to = renameTo.trim();
    if (!from) return;
    if (!validIri(to)) return void (renameProblem = 'The new IRI is not an absolute IRI.');
    if (to === from) return void (renameProblem = 'The new IRI is the same as the old one.');
    try {
      const used = await api.query(target, usedQuery(to, graph), { nocache: true });
      if (used.boolean) return void (renameProblem = 'The graph already uses the new IRI.');
    } catch (e) {
      return void (renameProblem = api.errorMessage(e));
    }
    renameOpen = false;
    edit(
      renameUpdate(from, to, graph),
      `Rename ${iriName(from, prefixes)} to ${iriName(to, prefixes)}`,
      () => void nav({ iri: to, ...(schemeParam === from ? { scheme: to } : {}) }),
    );
  }

  function remove() {
    const iri = selected;
    if (!iri) return;
    const n = details ? details.narrower.length : 0;
    edit(
      deleteEdit(iri, graph),
      `Delete ${nameOf(iri)}${n ? `, leaving its ${fmtInt(n)} narrower concepts without it` : ''}`,
      () => void nav({ iri: null, view: null }),
    );
  }

  // --- checks ---------------------------------------------------------------------------
  let report = $state<api.ShaclReport | null>(null);
  let checking = $state(false);
  let checkError = $state<string | null>(null);
  let showInfo = $state(false);
  let guard = $state<api.WriteValidation | null | undefined>(undefined);
  async function runChecks() {
    checking = true;
    checkError = null;
    try {
      const shapes = await skosShapes();
      const r = await api.shacl(target, shapes, { graph: graph ?? 'default' });
      report = r;
      await readLabels(
        r.results.flatMap((x) => (x.focusNode.type === 'uri' ? [x.focusNode.value] : [])),
      );
    } catch (e) {
      checkError = api.errorMessage(e);
    } finally {
      checking = false;
    }
    if (canAdmin) guard = await api.writeValidation(name).catch(() => undefined);
  }
  $effect(() => {
    if (tab === 'checks' && loadedTarget) untrack(() => void runChecks());
  });
  const severityOf = (r: api.ShaclResult) =>
    r.severity.type === 'uri' ? r.severity.value.replace(/.*#/, '') : '';
  const results = $derived(
    (report?.results ?? []).filter((r) => showInfo || severityOf(r) !== 'Info'),
  );
  async function enforce() {
    try {
      const shapes = await skosShapes();
      await api.setWriteValidation(name, {
        language: 'shacl',
        mode: 'reject',
        baseline: 'grandfather',
        shapes: { inline: shapes },
      });
      toasts.push('success', 'Writes to this dataset now have to keep the SKOS rules.');
      guard = await api.writeValidation(name).catch(() => undefined);
    } catch (e) {
      toasts.push('error', 'The write guard was not installed.', api.errorMessage(e));
    }
  }

  // The notes and members forms read every graph, write where the dataset's forms setting
  // says, and delete a removed value's nested nodes and list cells wherever they are. The
  // page lets them edit only a dataset that keeps no graph but the one it shows, where no
  // write can reach another graph, and only when their target is that graph.
  let formModel = $state<{ key: string; m: FormModel } | null>(null);
  /** For a target and graph, whether the dataset holds triples elsewhere (absent: unread). */
  let otherGraphs = $state<{ key: string; others: boolean } | null>(null);
  const graphsKey = $derived(`${target}|${graph ?? ''}|${head?.commit}`);
  $effect(() => {
    if (view !== 'notes' && view !== 'members') return;
    const key = graphsKey;
    const t = target;
    const g = graph;
    if (otherGraphs?.key === key) return;
    void api
      .query(t, otherGraphsQuery(g), { nocache: true })
      .then((r) => !!r.boolean)
      .catch(() => true)
      .then((others) => {
        // an answer for a page that has moved on is dropped
        if (key === graphsKey) otherGraphs = { key, others };
      });
  });
  const formKey = $derived(`${selected}|${branch}|${formShapes}|${view}|${head?.commit}`);
  const formLock = $derived.by(() => {
    if (readonly) return 'The page is read-only, so the form is too.';
    // the graphs a caller cannot read are hidden from the check below
    if (graphRestricted(auth.who, name, branch))
      return branch && branch !== MAIN
        ? `The page cannot read which graphs your access covers on ${branch}, so it cannot tell whether a form’s save stays in the graph it edits, and the form is read-only.`
        : 'Your access covers only some graphs of this dataset, so the page cannot tell whether a form’s save stays in the graph it edits, and the form is read-only.';
    const fm = formModel?.key === formKey ? formModel.m : null;
    if (!fm) return null;
    const here = graph ?? null;
    const graphName = (g: string | null) => (g ? iriName(g, prefixes) : 'the default graph');
    if ((fm.graph ?? null) !== here)
      return `The form would write to ${graphName(fm.graph ?? null)}, not to ${graphName(here)} that the page edits, so it is read-only here.`;
    const others = otherGraphs?.key === graphsKey ? otherGraphs.others : null;
    if (others === null) return 'Checking the graphs of the dataset…';
    if (others)
      return `The dataset keeps triples outside ${graphName(here)}, and a form's save could change them, so the form is read-only here.`;
    return null;
  });

  const title = $derived(selected ? nameOf(selected) : '');
  const typeBadges = $derived(
    (details?.types ?? []).map((t) => iriName(t, prefixes)).filter(Boolean),
  );
</script>

<svelte:head><title>Thesaurus{name ? ` · ${name}` : ''} | Sparkles</title></svelte:head>

<div class="page page-container fill thes">
  <header class="page-head">
    <div class="title">
      <h1>Thesaurus</h1>
      <p class="muted">
        {#if head}
          {fmtInt(schemes.length)} schemes · {fmtInt(collections.length)} collections · commit {head.commit}
        {:else if !name}
          Choose a dataset in the top bar.
        {:else}
          Reading the thesaurus of {name}…
        {/if}
      </p>
    </div>
    <div class="page-actions">
      <span
        class="live"
        class:on={session.live === 'on'}
        title={session.live === 'on'
          ? 'Following the change feed'
          : 'Not following the change feed'}
        ><span class="dot"></span>{session.live === 'on'
          ? 'Live'
          : session.live === 'stopped'
            ? 'Live updates stopped'
            : ''}</span
      >
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
        title={session.undone.at(-1) ? `Redo: ${session.undone.at(-1)?.label}` : 'Nothing to redo'}
        >Redo</button
      >
      {#if canWrite && !branch}
        <button
          class="btn"
          onclick={startSession}
          title="Edit on a branch of your own, then merge it"
          ><Icon name="branch" size={14} /> Start session</button
        >
      {/if}
      {#if branch}
        <a
          class="btn"
          href="{resolve('/datasets/[name]/merge', { name })}?source={encodeURIComponent(
            branch,
          )}&target={encodeURIComponent(currentBranch?.upstream ?? MAIN)}"
          ><Icon name="merge" size={14} /> Merge</a
        >
      {/if}
    </div>
  </header>
  {#if name}
    <div class="page-context">
      <label class="form-row">
        Graph
        <select
          class="select"
          value={graph ?? ''}
          onchange={(e) => nav({ graph: e.currentTarget.value || null, iri: null, scheme: null })}
        >
          <option value="">default graph</option>
          {#each graphs as g (g)}<option value={g}>{iriName(g, prefixes)}</option>{/each}
          {#if graph && !graphs.includes(graph)}<option value={graph}>{graph}</option>{/if}
        </select>
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
      <label class="form-row">
        Labels in
        <select
          class="select"
          value={displayLang}
          onchange={(e) => nav({ lang: e.currentTarget.value || null })}
          aria-label="Label language"
        >
          <option value="">the browser's language</option>
          {#each languages as l (l)}<option value={l}>{l}</option>{/each}
        </select>
      </label>
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
    <p class="notice" role="status">You can read this dataset but not edit it.</p>
  {:else if noHead && name}
    <p class="notice" role="status">
      The page could not read the commit this graph is at, so it cannot guard its edits against
      other writers and shows the thesaurus read-only. Reload to try again.
    </p>
  {/if}
  {#if loadError}
    <div class="error-box" role="alert">
      <strong>The thesaurus could not be read.</strong>
      {loadError}
    </div>
  {/if}

  {#if name && head}
    <div class="panes split-view">
      <section class="panel side" aria-label="Thesaurus">
        <div class="scheme-bar">
          <label class="pick grow">
            <span class="faint">Concept scheme</span>
            <select
              class="select"
              value={scheme ?? '*'}
              onchange={(e) => nav({ scheme: e.currentTarget.value, iri: null })}
              aria-label="Concept scheme"
            >
              {#each schemes as s (s)}<option value={s}>{nameOf(s)}</option>{/each}
              <option value="*">All concepts</option>
            </select>
          </label>
          {#if scheme}
            <button
              class="btn sm"
              onclick={() => nav({ iri: scheme, view: null, tab: null })}
              title="The scheme's labels, top concepts and notes">Scheme</button
            >
          {/if}
          {#if !readonly}
            <button class="btn sm" onclick={() => openCreate('scheme')}
              ><Icon name="plus" size={13} /> Scheme</button
            >
          {/if}
        </div>
        <div class="search-bar">
          <input
            class="input"
            type="search"
            placeholder="Search labels in any language"
            aria-label="Search labels"
            bind:this={searchInput}
            bind:value={search}
          />
          <select class="select sm" bind:value={searchLang} aria-label="Search language">
            <option value="">any language</option>
            {#each languages as l (l)}<option value={l}>{l}</option>{/each}
          </select>
        </div>
        {#if search.trim()}
          <div class="results" role="region" aria-label="Search results">
            {#if searching && !hits.length}<p class="faint pad">
                <span class="spinner"></span> Searching…
              </p>{/if}
            {#if !searching && !hits.length}<p class="empty pad">No label matches.</p>{/if}
            <ul>
              {#each hits as h, i (`${h.iri}|${h.kind}|${h.label.value}|${i}`)}
                <li>
                  <button class="hit" onclick={() => pickHit(h)}>
                    <span class="name">{nameOf(h.iri)}</span>
                    <span class="faint small"
                      >{h.kind} “{h.label.value}”{h.label.lang ? `@${h.label.lang}` : ''}</span
                    >
                  </button>
                </li>
              {/each}
            </ul>
          </div>
        {:else}
          <div class="tabs" role="tablist" aria-label="Views">
            <button
              class="tab"
              role="tab"
              aria-selected={tab === 'concepts'}
              onclick={() => nav({ tab: null })}>Concepts</button
            >
            <button
              class="tab"
              role="tab"
              aria-selected={tab === 'collections'}
              onclick={() => nav({ tab: 'collections' })}
              >Collections <span class="count">{collections.length || ''}</span></button
            >
            <button
              class="tab"
              role="tab"
              aria-selected={tab === 'checks'}
              onclick={() => nav({ tab: 'checks' })}>Checks</button
            >
          </div>
          {#if tab === 'concepts'}
            <div class="tree-bar">
              {#if !readonly}
                <button
                  class="btn sm"
                  onclick={() => openCreate('concept')}
                  title={selected && kind === 'concept'
                    ? 'New concept under the selection'
                    : 'New top concept'}><Icon name="plus" size={13} /> Concept</button
                >
              {/if}
              <span class="faint small">
                {fmtInt(tree.roots.length)}
                {scheme ? 'top concepts' : 'concepts without a broader one'}
              </span>
            </div>
            {#if scheme && !readonly}
              <div
                class="top-drop"
                class:over={topOver}
                role="region"
                aria-label="Top of the scheme"
                ondragover={(e) => {
                  if (!e.dataTransfer?.types.includes(TREE_DRAG)) return;
                  e.preventDefault();
                  topOver = true;
                }}
                ondragleave={() => (topOver = false)}
                ondrop={(e) => {
                  topOver = false;
                  const raw = e.dataTransfer?.getData(TREE_DRAG);
                  if (!raw || !scheme) return;
                  e.preventDefault();
                  const src = JSON.parse(raw) as { id: string; from: string | null };
                  void move(src.id, src.from, { top: scheme }, e.ctrlKey || e.altKey);
                }}
              >
                Drop a concept here to make it a top concept of {nameOf(scheme)}
              </div>
            {/if}
            <div class="tree-scroll">
              {#if tree.roots.length}
                <OntoTree
                  label="Concepts"
                  roots={tree.roots}
                  children={kidsOf}
                  more={moreOf}
                  name={nameOf}
                  sub={subOf}
                  badge={badgeOf}
                  open={opened}
                  {selected}
                  onselect={(id) => nav({ iri: id })}
                  ontoggle={toggle}
                  ondrop={readonly
                    ? undefined
                    : (node, from, onto, copy) => void move(node, from, { parent: onto }, copy)}
                />
              {:else}
                <p class="empty">
                  {scheme ? 'The scheme has no concepts.' : 'The graph has no concepts.'}
                </p>
              {/if}
            </div>
            {#if !readonly && tree.roots.length}
              <p class="faint small pad">
                Drag a concept onto another to move it there. Hold Ctrl or Alt to add a broader
                concept and keep the old one.
              </p>
            {/if}
          {:else if tab === 'collections'}
            <div class="tree-bar">
              {#if !readonly}
                <button class="btn sm" onclick={() => openCreate('collection')}
                  ><Icon name="plus" size={13} /> Collection</button
                >
              {/if}
            </div>
            <ul class="plain-list" aria-label="Collections">
              {#each collections as c (c.iri)}
                <li>
                  <button
                    class="hit"
                    class:sel={selected === c.iri}
                    onclick={() => nav({ iri: c.iri, view: null })}
                  >
                    <span class="name">{nameOf(c.iri)}</span>
                    {#if c.ordered}<span class="badge">ordered</span>{/if}
                  </button>
                </li>
              {:else}
                <li class="empty">The graph has no collections.</li>
              {/each}
            </ul>
          {:else}
            <div class="checks">
              <div class="tree-bar">
                <button class="btn sm" onclick={runChecks} disabled={checking}
                  ><Icon name="refresh" size={13} /> Run the checks</button
                >
                <label class="small"
                  ><input type="checkbox" bind:checked={showInfo} /> Information too</label
                >
              </div>
              {#if checking && !report}<p class="faint pad">
                  <span class="spinner"></span> Checking…
                </p>{/if}
              {#if checkError}<p class="error-box" role="alert">{checkError}</p>{/if}
              {#if report}
                <p class="pad small" role="status" data-conforms={report.conforms}>
                  {#if !results.length}
                    The thesaurus keeps the SKOS integrity conditions.
                  {:else}
                    {fmtInt(results.length)}
                    {results.length === 1 ? 'result' : 'results'} of the built-in SKOS shapes.
                  {/if}
                </p>
                <ul class="plain-list" aria-label="Check results">
                  {#each results as r, i (i)}
                    {@const f = r.focusNode.type === 'uri' ? r.focusNode.value : null}
                    <li class="result" data-severity={severityOf(r)}>
                      {#if f}<button class="link" onclick={() => nav({ iri: f, tab: null })}
                          >{nameOf(f)}</button
                        >{:else}<span class="mono">{r.focusNode.value}</span>{/if}
                      <span class="small">{r.messages[0] ?? 'violates a shape'}</span>
                      {#if r.value}<span class="mono faint xs">{r.value.value}</span>{/if}
                    </li>
                  {/each}
                </ul>
              {/if}
              {#if canAdmin && guard === null}
                <p class="pad small">
                  The dataset has no write-time validation.
                  <button class="btn sm" onclick={enforce}>Enforce the SKOS rules on writes</button>
                </p>
              {:else if guard}
                <p class="pad faint small">
                  The dataset validates writes with its own {guard.config.mode} guard.
                </p>
              {/if}
            </div>
          {/if}
        {/if}
      </section>

      <section class="panel detail" aria-label="Details">
        {#if !selected}
          <div class="panel-body">
            <p class="empty">
              Choose a concept in the tree, a collection or a search result to edit it.
            </p>
          </div>
        {:else}
          <div class="panel-head entity-head">
            <div class="who">
              <h2>{title}</h2>
              <span class="mono faint iri">{selected}</span>
              <span class="kinds">
                {#each typeBadges as t (t)}<span class="badge">{t}</span>{/each}
              </span>
            </div>
            <span class="spacer"></span>
            {#if !readonly && details}
              <button
                class="btn sm"
                onclick={() => {
                  renameTo = selected ?? '';
                  renameProblem = null;
                  renameOpen = true;
                }}><Icon name="pencil" size={13} /> Rename</button
              >
              <button class="btn sm danger" onclick={remove}
                ><Icon name="trash" size={13} /> Delete</button
              >
            {/if}
          </div>
          <div class="tabs sub" role="tablist" aria-label="Resource views">
            <button
              class="tab"
              role="tab"
              aria-selected={view === 'labels'}
              onclick={() => nav({ view: null })}
              >Labels <span class="count">{details?.labels.length || ''}</span></button
            >
            {#if kind === 'collection'}
              <button
                class="tab"
                role="tab"
                aria-selected={view === 'members'}
                onclick={() => nav({ view: 'members' })}
                >Members <span class="count">{details?.members.length || ''}</span></button
              >
            {:else}
              <button
                class="tab"
                role="tab"
                aria-selected={view === 'relations'}
                onclick={() => nav({ view: 'relations' })}
                >{kind === 'scheme' ? 'Top concepts' : 'Relations'}</button
              >
            {/if}
            <button
              class="tab"
              role="tab"
              aria-selected={view === 'notes'}
              onclick={() => nav({ view: 'notes' })}>Notes</button
            >
          </div>
          <div class="panel-body">
            {#if detailsError}<p class="error-box" role="alert">{detailsError}</p>{/if}
            {#if details && details.iri === selected}
              {#if view === 'labels'}
                <SkosLabels
                  iri={selected}
                  name={title}
                  entries={details.labels}
                  {graph}
                  langs={languages}
                  {readonly}
                  onedit={edit}
                  scope={`${target}|${graph ?? ''}`}
                  isUsed={async (i) => {
                    const r = await api.query(target, usedQuery(i, graph), { nocache: true });
                    return !!r.boolean;
                  }}
                />
                {#if details.notations.length}
                  <p class="small">
                    <span class="faint">Notation:</span>
                    <span class="mono">{details.notations.join(', ')}</span>
                  </p>
                {/if}
              {:else if view === 'relations' && kind === 'scheme'}
                <SkosLinks
                  title="Top concepts"
                  id="hasTopConcept"
                  links={details.topConcepts}
                  name={nameOf}
                  sub={subOf}
                  {readonly}
                  paired
                  {picker}
                  onopen={(iri) => nav({ iri, view: null })}
                  onadd={(iri, n) => void addLink('hasTopConcept', iri, n)}
                  onremove={(iri) => removeLink('hasTopConcept', iri)}
                  onrepair={(l) => repair('hasTopConcept', l)}
                />
              {:else if view === 'relations'}
                <div class="relations">
                  <SkosLinks
                    title="Broader"
                    id="broader"
                    links={details.broader}
                    name={nameOf}
                    sub={subOf}
                    {readonly}
                    paired
                    {picker}
                    onopen={(iri) => nav({ iri, view: null })}
                    onadd={(iri, n) => void addLink('broader', iri, n)}
                    onremove={(iri) => removeLink('broader', iri)}
                    onrepair={(l) => repair('broader', l)}
                  />
                  <SkosLinks
                    title="Narrower"
                    id="narrower"
                    links={details.narrower}
                    name={nameOf}
                    sub={subOf}
                    {readonly}
                    paired
                    {picker}
                    onopen={(iri) => nav({ iri, view: null })}
                    onadd={(iri, n) => void addLink('narrower', iri, n)}
                    onremove={(iri) => removeLink('narrower', iri)}
                    onrepair={(l) => repair('narrower', l)}
                  />
                  <SkosLinks
                    title="Related"
                    id="related"
                    links={details.related}
                    name={nameOf}
                    sub={subOf}
                    {readonly}
                    paired
                    {picker}
                    onopen={(iri) => nav({ iri, view: null })}
                    onadd={(iri, n) => void addLink('related', iri, n)}
                    onremove={(iri) => removeLink('related', iri)}
                    onrepair={(l) => repair('related', l)}
                  />
                  {#each MAPPINGS as m (m)}
                    <SkosLinks
                      title={MAPPING_NAME[m]}
                      id={m}
                      links={strLinks(
                        details.mappings.filter((x) => x.property === m).map((x) => x.target),
                      )}
                      name={nameOf}
                      sub={(iri) => iriName(iri, prefixes)}
                      {readonly}
                      {picker}
                      onopen={(iri) => nav({ iri, view: null })}
                      onadd={(iri, n) => void addLink(m, iri, n)}
                      onremove={(iri) => removeLink(m, iri)}
                    />
                  {/each}
                  <SkosLinks
                    title="In scheme"
                    id="inScheme"
                    links={strLinks(details.inScheme)}
                    name={nameOf}
                    sub={subOf}
                    {readonly}
                    picker={{ ...picker }}
                    onopen={(iri) => nav({ scheme: iri, iri, view: null })}
                    onadd={(iri, n) => void addLink('inScheme', iri, n)}
                    onremove={(iri) => removeLink('inScheme', iri)}
                  />
                  <SkosLinks
                    title="Top concept of"
                    id="topConceptOf"
                    links={details.topConceptOf}
                    name={nameOf}
                    sub={subOf}
                    {readonly}
                    paired
                    {picker}
                    onopen={(iri) => nav({ scheme: iri, iri, view: null })}
                    onadd={(iri, n) => void addLink('topConceptOf', iri, n)}
                    onremove={(iri) => removeLink('topConceptOf', iri)}
                    onrepair={(l) => repair('topConceptOf', l)}
                  />
                  {#if details.memberOf.length}
                    <SkosLinks
                      title="Member of"
                      id="memberOf"
                      links={strLinks(details.memberOf)}
                      name={nameOf}
                      sub={subOf}
                      readonly
                      onopen={(iri) => nav({ iri, view: null, tab: 'collections' })}
                    />
                  {/if}
                </div>
              {:else if view === 'members' || view === 'notes'}
                {#if formLock}<p class="notice small" role="status">{formLock}</p>{/if}
                {#key formKey}
                  <fieldset class="form-lock" disabled={!!formLock}>
                    <ShapeForm
                      ds={name}
                      iri={selected}
                      {branch}
                      {prefixes}
                      {title}
                      shapes={formShapes}
                      groups={formShapes === 'skos'
                        ? view === 'members'
                          ? [`${GROUP}Members`]
                          : [`${GROUP}Notes`, `${GROUP}Documentation`]
                        : undefined}
                      onsaved={(a) =>
                        formSaved(
                          a,
                          view === 'members'
                            ? `Edit the members of ${title}`
                            : `Edit the notes of ${title}`,
                        )}
                      onmodel={(m) => (formModel = { key: formKey, m })}
                    />
                  </fieldset>
                {/key}
              {/if}
            {:else if !detailsError}
              <p class="faint"><span class="spinner"></span> Reading the resource…</p>
            {/if}
          </div>
        {/if}
      </section>
    </div>
  {:else if loading}
    <p class="faint"><span class="spinner"></span> Reading the thesaurus…</p>
  {:else if !name}
    <p class="empty">Choose a dataset to edit its thesaurus.</p>
  {/if}
</div>

<OntoPreview
  bind:open={session.previewOpen}
  title={session.previewTitle}
  {target}
  {sel}
  op={session.previewOp}
  etag={head?.etag ?? null}
  head={head?.commit ?? null}
  {describe}
  oncommitted={(op, a) => void session.committed(op, a)}
  onstale={() => void session.stale()}
/>

<Modal
  bind:open={createOpen}
  title="New {createKind === 'scheme' ? 'concept scheme' : createKind}"
  width={520}
>
  <form
    id="skos-create-form"
    class="dialog-form"
    onsubmit={(e) => {
      e.preventDefault();
      void submitCreate();
    }}
  >
    {#if createKind === 'concept'}
      <p class="muted">
        {#if createAt && 'parent' in createAt}
          Under <strong>{nameOf(createAt.parent)}</strong>
        {:else if createAt}
          A top concept of <strong>{nameOf(createAt.top)}</strong>
        {:else}
          Without a scheme or a broader concept
        {/if}
        {#if createAt && 'parent' in createAt && scheme}
          <button class="btn ghost sm" type="button" onclick={() => (createAt = { top: scheme! })}
            >At the top</button
          >
        {/if}
      </p>
    {/if}
    <div class="two">
      <label class="f grow">
        <span>Preferred label</span>
        <!-- svelte-ignore a11y_autofocus -->
        <input class="input" bind:value={createLabel} autofocus required />
      </label>
      <label class="f">
        <span>Language</span>
        <input
          class="input"
          bind:value={createLang}
          size="6"
          aria-invalid={!validLang(normLang(createLang))}
        />
      </label>
    </div>
    {#if createKind === 'collection'}
      <label class="small"><input type="checkbox" bind:checked={createOrdered} /> Ordered</label>
    {/if}
    <label class="f">
      <span>IRI <span class="faint">(under {policy.namespace})</span></span>
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
    <button class="btn primary" type="submit" form="skos-create-form">Preview</button>
  {/snippet}
</Modal>

<Modal bind:open={renameOpen} title="Rename {title}" width={560}>
  <form
    id="skos-rename-form"
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
    <button class="btn primary" type="submit" form="skos-rename-form">Preview</button>
  {/snippet}
</Modal>

<style>
  .pick {
    display: grid;
    gap: 2px;
    font-size: var(--fs-xs);
    min-width: 0;
  }
  .live {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-xs);
    color: var(--text-3);
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
    grid-template-columns: minmax(260px, 2fr) minmax(0, 3fr);
  }
  .side {
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .side > :global(*) {
    flex: none;
  }
  .side > .tree-scroll,
  .side > .results,
  .side > :global(.plain-list),
  .side > :global(.checks) {
    flex: 1 1 auto;
    min-height: 120px;
    overflow: auto;
  }
  .detail {
    overflow: auto;
  }
  .scheme-bar,
  .search-bar,
  .tree-bar {
    display: flex;
    gap: 6px;
    align-items: end;
    padding: 8px;
    flex-wrap: wrap;
  }
  .search-bar {
    padding-top: 0;
  }
  .search-bar .input {
    flex: 1 1 10em;
    min-width: 0;
  }
  .grow {
    flex: 1 1 12em;
  }
  .side > .tabs {
    padding: 0 8px;
    border-bottom: 1px solid var(--border);
  }
  .tree-scroll,
  .results {
    overflow: auto;
    padding: 0 6px 10px;
  }
  .top-drop {
    margin: 0 8px 6px;
    padding: 4px 8px;
    border: 1px dashed var(--border-strong);
    border-radius: var(--r);
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
  .top-drop.over {
    border-color: var(--spark);
    background: var(--spark-soft);
  }
  .results ul,
  .plain-list {
    list-style: none;
    margin: 0;
    padding: 0 6px;
  }
  .hit {
    display: flex;
    gap: 8px;
    align-items: baseline;
    flex-wrap: wrap;
    width: 100%;
    border: 0;
    background: none;
    padding: 3px 6px;
    font: inherit;
    text-align: left;
    color: var(--text);
    cursor: pointer;
    border-radius: var(--r-sm);
  }
  .hit:hover {
    background: var(--hover);
  }
  .hit.sel {
    background: var(--spark-soft);
  }
  .result {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: baseline;
    padding: 3px 6px;
    border-bottom: 1px solid var(--border);
    overflow-wrap: anywhere;
  }
  .result[data-severity='Violation'] .small {
    color: var(--danger);
  }
  .badge {
    font-size: var(--fs-xs);
    color: var(--spark-ink);
    background: var(--spark-soft);
    border-radius: 999px;
    padding: 0 6px;
  }
  .pad {
    padding: 6px 10px;
  }
  .entity-head .who {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .form-lock {
    border: 0;
    margin: 0;
    padding: 0;
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
  .relations {
    display: grid;
    gap: 2px;
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
  .small {
    font-size: var(--fs-sm);
  }
  .xs {
    font-size: var(--fs-xs);
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
  .problem {
    color: var(--danger);
    font-size: var(--fs-sm);
  }
  @media (max-width: 760px) {
    .panes {
      grid-template-columns: minmax(0, 1fr);
    }
    .tree-scroll,
    .results {
      max-height: 50vh;
    }
  }
</style>

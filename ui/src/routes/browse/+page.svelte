<script lang="ts">
  // The ontology browser (docs/specs/U12-ontology-workbench/SPEC.md, "Ontology browser"):
  // a read-only page for each entity of a dataset's ontologies and thesauri, and a landing
  // page for the ontology of a graph. An entity's page reads the term route and one level
  // of the hierarchy route with its ancestors, which read the store's indexes around the
  // entity and never the whole ontology. The landing page reads the header and counts by
  // SPARQL and the roots from the hierarchy route. The components come from
  // lib/components/browse, and this page only lays them out.
  import { askThread, useAskContext, useAskTarget } from '$lib/ask-thread.svelte';
  import { chipTypeOf } from '$lib/ask-thread';
  import { beforeNavigate, goto } from '$app/navigation';
  import { base } from '$app/paths';
  import { page } from '$app/state';
  import { onMount, tick, untrack } from 'svelte';
  import { balance } from '$lib/balance';
  import { fmtInt } from '$lib/format';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { branchParam, branchPath, MAIN, onBranch } from '$lib/branches';
  import {
    annotationsOp,
    inGraph,
    isAnnotation,
    parentRows,
    parentsEditable,
    toAnnotation,
    statementKey,
    withAssertions,
    type DefinitionDraft,
    type EditMode,
    type OnEdit,
  } from '$lib/browse-edit';
  import { EditSession, startSessionBranch } from '$lib/edit-session.svelte';
  import { launcher, useCommands } from '$lib/launcher.svelte';
  import type { NameOption } from '$lib/manchester-complete';
  import {
    describeAxiom,
    iriName,
    KIND_SINGULAR,
    parseManchester,
    usedQuery,
    type AxiomInput,
    type ParseAs,
  } from '$lib/ontology';
  import {
    detailsQuery,
    linkEdit,
    MAPPING_NAME,
    MAPPINGS,
    parseDetails,
    SESSION_PREFIX,
    type Details,
  } from '$lib/skos';
  import AnnotationRows from '$components/AnnotationRows.svelte';
  import OntoPreview from '$components/OntoPreview.svelte';
  import SkosLabels from '$components/SkosLabels.svelte';
  import SkosLinks from '$components/SkosLinks.svelte';
  import AxiomEditor from '$components/browse/AxiomEditor.svelte';
  import DefinitionField from '$components/browse/DefinitionField.svelte';
  import EditBar from '$components/browse/EditBar.svelte';
  import ParentEditor from '$components/browse/ParentEditor.svelte';
  import SynonymEditor from '$components/browse/SynonymEditor.svelte';
  import {
    axiomGroups,
    browseHref,
    builtIn,
    countsQuery,
    definitionsQuery,
    namesQuery,
    DEFINITION_PROPS,
    displayName,
    downloadHref,
    editHref,
    editorKindOf,
    fetchHierarchy,
    fetchTerm,
    headGraphsQuery,
    incomingQuery,
    usageQuery,
    kindWord,
    mentionGroups,
    mergePieces,
    neighbourhood,
    oboIri,
    ontologyHead,
    parseCounts,
    parseDefinitions,
    parseReuse,
    reuseQuery,
    SYNONYM_KINDS,
    termSections,
    treeKindOf,
    treeWords,
    isLiteral,
    valueText,
    type BrowseSelection,
    type GraphPiece,
    type Hierarchy,
    type InfoRow,
    type Linker,
    type OntologyHead,
    type Reuse,
    type TermPage,
    type TreeKind,
    type TreeNode,
  } from '$lib/browse';
  import type { EntitySearchKind } from '$lib/entity-search';
  import { WELL_KNOWN } from '$lib/rdf';
  import Icon from '$components/Icon.svelte';
  import DownloadButtons from '$components/browse/DownloadButtons.svelte';
  import EntitySearchList from '$components/browse/EntitySearchList.svelte';
  import LineageTree from '$components/browse/LineageTree.svelte';
  import TermGraph from '$components/browse/TermGraph.svelte';
  import TermHeader from '$components/browse/TermHeader.svelte';
  import TermInfobox from '$components/browse/TermInfobox.svelte';
  import TermLabels from '$components/browse/TermLabels.svelte';
  import AxiomProse from '$components/browse/AxiomProse.svelte';
  import TermAxioms from '$components/browse/TermAxioms.svelte';
  import TermNotes from '$components/browse/TermNotes.svelte';
  import TermReuse from '$components/browse/TermReuse.svelte';
  import TermSynonyms from '$components/browse/TermSynonyms.svelte';
  import TermUsage from '$components/browse/TermUsage.svelte';
  import TermXrefs from '$components/browse/TermXrefs.svelte';
  import TopTerms from '$components/browse/TopTerms.svelte';
  import SimilarTerms from '$components/browse/SimilarTerms.svelte';
  import { editorOf } from '$lib/routes';

  const RDFS = WELL_KNOWN.rdfs;
  const SKOS = WELL_KNOWN.skos;
  const OWL = WELL_KNOWN.owl;
  const OBO = 'http://purl.obolibrary.org/obo/';
  const OIO = 'http://www.geneontology.org/formats/oboInOwl#';

  // --- the URL's state -----------------------------------------------------------------
  const q = $derived(page.url.searchParams);
  const name = $derived(q.get('ds') ?? app.current ?? '');
  const branch = $derived(branchParam(q.get('branch')));
  const target = $derived(onBranch(name, branch));
  const graph = $derived(q.get('graph') || null);
  const at = $derived(q.get('at') || null);
  const iri = $derived(q.get('iri') || null);
  /** The address asks for edit mode, which holds only for a caller who may edit (below). */
  const wantEdit = $derived(q.get('edit') === '1');
  const view = $derived(q.get('view') === 'graph' && !wantEdit ? 'graph' : 'tree');
  const sel = $derived<BrowseSelection>({ graph, at });
  const prefixes = $derived(app.prefixes(name));
  const langs: readonly string[] = typeof navigator === 'undefined' ? [] : [...navigator.languages];

  $effect(() => {
    if (name && name !== app.current && app.datasets.some((d) => d.name === name))
      app.setDataset(name);
    if (name) void app.loadPrefixes(name);
  });

  const keep = $derived.by(() => ({ graph, branch, at, edit: editing ? '1' : null }));
  const hrefOf = (i: string | null, extra: Record<string, string | null> = {}) =>
    browseHref(base, name, i, { ...keep, ...extra });

  // --- an entity's page ----------------------------------------------------------------
  let term = $state<TermPage | null>(null);
  let termError = $state<{ status: number | null; text: string } | null>(null);
  let tree = $state<Hierarchy | null>(null);
  let treeKind = $state<TreeKind | null>(null);
  let piece = $state<GraphPiece | null>(null);
  /** The header of the graph's ontology or scheme, for the breadcrumbs and the infobox. */
  let termHead = $state<{ head: OntologyHead; scheme: boolean } | null>(null);
  let graphEpoch = $state(0);
  let reuse = $state<{ checked: string[]; found: Reuse[]; loading: boolean }>({
    checked: [],
    found: [],
    loading: false,
  });

  // the page reads again when the target, graph, state or entity change, and not when
  // only the view or edit mode does
  $effect(() => {
    const t = target;
    const g = graph;
    const a = at;
    const i = iri;
    if (!t || !i) return;
    const ctl = new AbortController();
    untrack(() => void loadTerm(t, { graph: g, at: a }, i, ctl.signal));
    return () => ctl.abort();
  });

  async function loadTerm(t: string, s: BrowseSelection, i: string, signal: AbortSignal) {
    term = null;
    termError = null;
    tree = null;
    piece = null;
    reuse = { checked: [], found: [], loading: false };
    let got: TermPage;
    try {
      got = await fetchTerm(t, s, i, {}, signal);
    } catch (e) {
      if (signal.aborted) return;
      termError = {
        status: e instanceof api.ApiError ? e.status : null,
        text: api.errorMessage(e),
      };
      return;
    }
    if (signal.aborted) return;
    term = got;
    termTarget = t;
    piece = neighbourhood(got, prefixes);
    graphEpoch++;
    const kind = treeKindOf(got.kinds);
    treeKind = kind;
    if (kind)
      fetchHierarchy(t, s, { kind, iri: i, ancestors: true, limit: 50 }, signal).then(
        (h) => {
          if (signal.aborted) return;
          tree = h;
          if (piece) piece = mergePieces(piece, lineagePiece(h));
        },
        () => {},
      );
    ontologyHead(t, s, langs, signal).then(
      (h) => {
        if (!signal.aborted) termHead = h;
      },
      () => {},
    );
    void loadReuse(i, signal);
  }

  /** The entity's direct parents for the graph view, from the hierarchy's ancestors. */
  function lineagePiece(h: Hierarchy): GraphPiece {
    const nodes = new Map<string, { id: string; label: string }>();
    const edges: GraphPiece['edges'] = [];
    const word =
      h.kind === 'concept' ? 'broader' : h.kind === 'class' ? 'subclass of' : 'subproperty of';
    for (const [c, p] of h.ancestors?.edges ?? []) {
      if (c !== iri) continue;
      const n = h.ancestors?.nodes.find((x) => x.iri === p);
      nodes.set(p, { id: p, label: n?.label ?? n?.shortForm ?? iriName(p, prefixes) });
      edges.push({ id: `${c} ${word} ${p}`, source: c, target: p, label: word });
    }
    return { nodes: [...nodes.values()], edges };
  }

  async function loadReuse(i: string, signal: AbortSignal) {
    const others = app.datasets
      .map((d) => d.name)
      .filter((d) => d !== name)
      .slice(0, 20);
    reuse = { checked: others, found: [], loading: others.length > 0 };
    if (!others.length) return;
    const answers = await Promise.allSettled(
      others.map(async (d) =>
        parseReuse(d, (await api.select(d, reuseQuery(i), { signal })) as never),
      ),
    );
    if (signal.aborted) return;
    reuse = {
      checked: others,
      found: answers.flatMap((a) => (a.status === 'fulfilled' && a.value ? [a.value] : [])),
      loading: false,
    };
  }

  async function expand(id: string) {
    if (!piece || builtIn(id)) return;
    try {
      const t = await fetchTerm(target, sel, id, { mentions: 50, incoming: 10 });
      if (piece) piece = mergePieces(piece, neighbourhood(t, prefixes, 10));
    } catch {
      // a node the graphs do not describe has no neighbourhood to add
    }
  }

  async function moreIncoming(
    kind: 'incoming' | 'usage',
    property: string,
    offset: number,
  ): Promise<string[]> {
    if (!term) return [];
    const q =
      kind === 'usage'
        ? usageQuery(term.iri, term.graphs, offset)
        : incomingQuery(term.iri, property, term.graphs, offset);
    const rows = await api.select(target, q, {
      at: at ?? undefined,
    });
    return rows.flatMap((r) => (r.s && r.s.type === 'uri' ? [r.s.value] : []));
  }

  const linker = $derived<Linker>({
    href: (i) => (builtIn(i) ? null : hrefOf(i)),
    name: (i) => (term && i === term.iri ? title : displayName(i, term?.names, prefixes)),
    short: (i) => term?.names[i]?.shortForm ?? (term?.iri === i ? term.shortForm : null),
  });

  /** CURIEs such as GO:0032047 link when they are in the entity's own OBO namespace. */
  const curieIri = $derived.by(() => {
    const sf = term?.shortForm ?? '';
    const own = /^([A-Za-z][A-Za-z0-9_]*):\d+$/.exec(sf)?.[1];
    return (c: string) => (own && c.startsWith(own + ':') ? oboIri(c) : null);
  });

  const sections = $derived(term ? termSections(term, langs) : null);
  const groups = $derived(term ? axiomGroups(term.iri, term.kinds, term.axioms) : []);
  /** A class's EquivalentTo axioms, the OBO genus and differentia, which the Definition card
   * also shows as its logical definition; the Axioms card holds every axiom. */
  const logicalDefinition = $derived(
    term && editorKindOf(term.kinds) === 'Class'
      ? groups
          .filter((g) => g.id === 'equivalent')
          .flatMap((g) => g.items.flatMap((it) => it.values))
      : [],
  );
  const mentions = $derived(term ? mentionGroups(term.iri, term.mentions.axioms) : []);
  const title = $derived(term ? (term.label ?? term.shortForm ?? iriName(term.iri, prefixes)) : '');
  const kind = $derived(term ? kindWord(term.kinds) : '');
  // the term on screen is the context of a question asked from the Ask drawer
  useAskContext(() =>
    term
      ? {
          kind: 'entity',
          iri: term.iri,
          label: title,
          type: chipTypeOf(term.kinds),
        }
      : null,
  );
  useAskTarget(() => ({ branch, at }));
  const isSkos = $derived(
    !!term?.kinds.some((k) => k === 'concept' || k === 'conceptScheme' || k === 'collection'),
  );
  const values = (p: string) =>
    term?.statements.filter((s) => s.property === p).map((s) => s.value) ?? [];
  const iris = (p: string) => values(p).flatMap((v) => (isLiteral(v) ? [] : [v as string]));
  const ontologyIri = $derived(term?.ontologies.find((o) => o.ontology)?.ontology ?? null);
  const ontologyName = $derived(
    termHead?.head.title ?? (ontologyIri ? displayName(ontologyIri, term?.names, prefixes) : name),
  );
  const plural: Record<string, string> = {
    class: 'Classes',
    'object property': 'Object properties',
    'data property': 'Data properties',
    'annotation property': 'Annotation properties',
    individual: 'Individuals',
    concept: 'Concepts',
    'concept scheme': 'Concept schemes',
    collection: 'Collections',
    datatype: 'Datatypes',
  };
  const skipIncoming = $derived(
    treeKind === 'class'
      ? [RDFS + 'subClassOf']
      : treeKind === 'concept'
        ? [SKOS + 'broader', SKOS + 'narrower']
        : treeKind
          ? [RDFS + 'subPropertyOf']
          : [],
  );

  /** The counts under a term's header, each a link to its section (ROADMAP item 633). */
  const termCounts = $derived.by(() => {
    if (!term || !sections) return [];
    const out: {
      key: string;
      label: string;
      one: string;
      many: string;
      n: number;
      href: string;
      loading?: boolean;
    }[] = [];
    if (treeKind && tree?.node) {
      const w = treeWords(treeKind);
      const concept = treeKind === 'concept';
      out.push({
        key: 'parents',
        label: concept ? 'Broader concepts' : 'Parents',
        one: concept ? 'broader' : 'parent',
        many: concept ? 'broader' : 'parents',
        n: tree.node.parents,
        href: '#tree-h',
      });
      out.push({
        key: 'children',
        label: w.children[0].toUpperCase() + w.children.slice(1),
        one: concept ? 'narrower' : w.child,
        many: concept ? 'narrower' : w.children,
        n: tree.node.children,
        href: '#tree-h',
      });
    }
    out.push({
      key: 'incoming',
      label: 'Incoming links',
      one: 'incoming',
      many: 'incoming',
      // the count What links here shows: mentions, statements and uses as a property
      n:
        term.mentions.total +
        term.incoming
          .filter((i) => !skipIncoming.includes(i.property))
          .reduce((n, i) => n + i.count, 0) +
        (term.usage?.count ?? 0),
      href: '#usage-h',
    });
    out.push({
      key: 'synonyms',
      label: 'Synonyms',
      one: 'synonym',
      many: 'synonyms',
      n: sections.synonyms.reduce((n, g) => n + g.values.length, 0),
      href: '#syn-h',
    });
    out.push({
      key: 'reuse',
      label: 'Other datasets',
      one: 'dataset',
      many: 'datasets',
      n: reuse.found.length,
      href: '#reuse-h',
      loading: reuse.loading,
    });
    return out;
  });
  const infoRows = $derived.by<InfoRow[]>(() => {
    if (!term || !sections) return [];
    const rows: InfoRow[] = [
      { label: 'Kind', text: kind.charAt(0).toUpperCase() + kind.slice(1) },
      term.deprecated
        ? { label: 'Status', text: 'deprecated', badge: 'danger' }
        : { label: 'Status', text: 'current', badge: 'ok' },
    ];
    for (const g of sections.info)
      rows.push({
        label: g.kind,
        values: g.values,
        mono: g.kind === 'ID' || g.kind === 'Notation',
      });
    if (ontologyIri || term.ontologies.length) {
      const o = term.ontologies[0];
      rows.push({
        label: 'Ontology',
        text: ontologyName,
        note: [
          termHead?.head.version ? `version ${termHead.head.version}` : null,
          o ? (o.graph === 'default' ? 'default graph' : o.graph) : null,
        ]
          .filter(Boolean)
          .join(', '),
      });
    }
    const inst = term.incoming.find((i) => i.property === WELL_KNOWN.rdf + 'type');
    if (inst) rows.push({ label: 'Instances', text: inst.count.toLocaleString('en-US') });
    if (isSkos) {
      const ls = [...new Set(sections.labels.map((l) => l.lang).filter(Boolean))].sort();
      if (ls.length) rows.push({ label: 'Languages', text: ls.join(', ') });
      const cols = term.incoming.find((i) => i.property === SKOS + 'member');
      if (cols) rows.push({ label: 'Collections', values: cols.subjects });
    }
    return rows;
  });

  const exploreHref = $derived(
    iri
      ? `${base}/explore?${new URLSearchParams({ ds: name, iri, ...(branch ? { branch } : {}) })}`
      : '',
  );
  const editLink = $derived.by(() => {
    if (!term) return '';
    const h = editHref(base, name, term.iri, term.kinds, sel);
    return branch ? `${h}&branch=${encodeURIComponent(branch)}` : h;
  });
  /** The term in its Workbench editor, selected in the tree with its path open. */
  const workbenchLink = $derived(editLink);

  // --- edit mode (ROADMAP item 630) ------------------------------------------------------
  // A caller with write access switches the page into edit mode (`edit=1`), where its
  // sections become the editors' forms. Every change is one operation that OntoPreview
  // runs as a dry run with the term's entity tag and then commits, and EditSession keeps
  // the undo stack under the editors' own keys, so a tab shares it with the editor.
  /** The target the shown term was read from, for the live refresh. */
  let termTarget = $state('');
  const mode = $derived<EditMode>(isSkos ? 'skos' : 'owl');
  const editorKind = $derived(term ? editorKindOf(term.kinds) : null);
  const canWrite = $derived(auth.loaded && auth.can(name, 'write'));
  /** Edit mode needs write access, a branch head (not a past state) and the term's tag. */
  const canEdit = $derived(
    canWrite && !at && !!term?.etag && (isSkos || (!!editorKind && editorKind !== 'Datatype')),
  );
  const editing = $derived(wantEdit && canEdit);

  // the hierarchy card: open on a phone only on request or while editing, and its own scroll keeps the
  // entity in view
  let treeOpen = $state(false);
  let treeBox = $state<HTMLElement>();
  $effect(() => {
    const box = treeBox;
    void tree;
    void treeOpen;
    if (!box) return;
    void tick().then(() => {
      const here = box.querySelector<HTMLElement>('.here');
      if (!here || box.scrollHeight <= box.clientHeight) return;
      const top = here.getBoundingClientRect().top - box.getBoundingClientRect().top;
      box.scrollTop += top - box.clientHeight / 3;
    });
  });
  // a caller who may not edit keeps no `edit=1` in the address
  $effect(() => {
    if (wantEdit && term && auth.loaded && !canEdit) setEdit(false);
  });

  let branchList = $state<api.Branch[]>([]);
  $effect(() => {
    if (!editing || !name) return;
    const n = name;
    api.branches(n).then(
      (r) => {
        if (n === name) branchList = r.branches;
      },
      () => (branchList = []),
    );
  });
  const protectedBranch = $derived(
    !!branchList.find((b) => b.name === (branch ?? MAIN))?.protected,
  );
  /** The forms are read-only outside edit mode and on a protected branch. */
  const readonly = $derived(!editing || protectedBranch);
  const editorName = $derived(isSkos ? 'Thesaurus' : 'Ontology');

  function setEdit(on: boolean) {
    const u = new URL(page.url);
    if (on) {
      u.searchParams.set('edit', '1');
      u.searchParams.delete('view');
    } else u.searchParams.delete('edit');
    void goto(u, { replaceState: true, noScroll: true, keepFocus: true });
  }
  /** Whether a form holds a change that is not previewed yet. */
  const unsaved = () =>
    typeof document !== 'undefined' &&
    !!document.querySelector('[data-edit-form][data-dirty="true"]');
  function leaveEdit() {
    if (unsaved() && !confirm('Leave edit mode and discard the changes you have not previewed?'))
      return;
    session.notice = null;
    setEdit(false);
  }
  beforeNavigate((nav) => {
    if (!editing || !unsaved() || nav.type === 'leave') return;
    const to = nav.to?.url;
    // a change of view or mode keeps the forms; another entity, dataset, branch, graph,
    // state or page does not
    const same = (k: string) => to?.searchParams.get(k) === page.url.searchParams.get(k);
    if (
      to &&
      to.pathname === page.url.pathname &&
      ['ds', 'iri', 'branch', 'graph', 'at', 'imports'].every(same)
    )
      return;
    if (!confirm('Leave this page and discard the changes you have not previewed?')) nav.cancel();
  });
  onMount(() => {
    const unload = (e: BeforeUnloadEvent) => {
      if (editing && unsaved()) e.preventDefault();
    };
    window.addEventListener('beforeunload', unload);
    return () => window.removeEventListener('beforeunload', unload);
  });

  let sessionError = $state<string | null>(null);
  async function startSession() {
    sessionError = null;
    try {
      const n = await startSessionBranch(name, {
        from: branch,
        branches: branchList,
        user: auth.who?.principal?.name,
        prefix: isSkos ? SESSION_PREFIX : 'onto',
        note: isSkos ? 'thesaurus editing session' : 'ontology editing session',
      });
      await goto(
        browseHref(base, name, iri, { ...keep, branch: n === MAIN ? null : n, edit: '1' }),
      );
    } catch (e) {
      sessionError = api.errorMessage(e);
    }
  }

  /** Read the entity again after a commit, in place, keeping the forms' text. */
  async function refresh() {
    const t = target;
    const s = sel;
    const i = iri;
    if (!t || !i) return;
    try {
      const got = await fetchTerm(t, s, i);
      if (t !== target || i !== iri) return;
      term = got;
      termTarget = t;
      const kind = treeKindOf(got.kinds);
      treeKind = kind;
      if (kind) {
        const h = await fetchHierarchy(t, s, { kind, iri: i, ancestors: true, limit: 50 });
        if (t === target && i === iri) tree = h;
      }
    } catch (e) {
      toasts.error('Could not read the entity again', e);
    }
  }

  const session = new EditSession({
    reload: refresh,
    staleText: 'Another commit landed since the page read this entity.',
  });
  $effect(() => {
    const k = `sparkles.${mode === 'skos' ? 'skos' : 'onto'}.undo.${target}|${graph ?? ''}`;
    untrack(() => session.reset(k));
  });
  $effect(() => {
    if (!editing) return;
    const t = target;
    return session.follow(
      t,
      () => term?.commit ?? 0,
      () => !!term && termTarget === t,
    );
  });
  onMount(() => session.keys(() => readonly || session.previewOpen || launcher.open));
  const onedit: OnEdit = (op, label, after) => session.edit(op, label, after);
  const describe = (a: AxiomInput) =>
    describeAxiom(a, (i) => displayName(i, term?.names, prefixes), prefixes);

  // the thesaurus editor's reading of a SKOS resource, for its labels and links
  // kept with the target, graph and entity it was read for, so the forms never pair one
  // concept's labels with another's IRI and tag, and read again at each commit, dropping a
  // superseded answer; the forms stay mounted across a commit so their drafts survive
  let read = $state<{ key: string; commit: number; details: Details } | null>(null);
  let detailsTry = $state(0);
  const detailsKey = $derived(
    term && termTarget ? JSON.stringify([termTarget, graph, term.iri]) : '',
  );
  const details = $derived(read && read.key === detailsKey ? read.details : null);
  $effect(() => {
    if (!editing || !isSkos || !term || !detailsKey) return;
    const key = detailsKey;
    const t = termTarget!;
    const r = term.iri;
    const g = graph;
    const commit = term.commit;
    void detailsTry;
    let live = true;
    api.select(t, detailsQuery(r, g), { nocache: true }).then(
      (rows) => {
        if (live && key === detailsKey) read = { key, commit, details: parseDetails(r, rows) };
      },
      (e) => {
        if (live) toasts.error('Could not read the labels', e);
      },
    );
    return () => (live = false);
  });
  /**
   * A SKOS edit built from the details, previewed only when they were read at the commit the
   * page shows, so a label another writer changed is never replaced by its old reading.
   */
  function skosEdit(sparql: string, label: string) {
    if (!read || !term || read.commit !== term.commit) {
      toasts.push(
        'info',
        'Reading the labels again',
        'Another commit changed this concept. Try again in a moment.',
      );
      detailsTry++;
      return false;
    }
    onedit({ kind: 'update', sparql }, label);
    return true;
  }
  const skosLangs = $derived([
    ...new Set([
      ...(details?.labels ?? []).map((l) => l.label.lang).filter(Boolean),
      ...langs.map((l) => l.split('-')[0].toLowerCase()),
    ]),
  ] as string[]);
  const picker = $derived({ target, dataset: name, graph, prefixes, langs });
  /** The mapping kinds the caller chose to add, shown before they have a mapping. */
  let addedMappings = $state<string[]>([]);
  $effect(() => {
    void iri;
    addedMappings = [];
  });
  const linkWord = (p: string) =>
    p === 'broader' || p === 'narrower' || p === 'related'
      ? p
      : (MAPPING_NAME[p] ?? p).toLowerCase();
  function addLink(property: string, other: string, picked?: string) {
    if (!term) return;
    skosEdit(
      linkEdit(term.iri, property, other, false, graph),
      `Add ${picked ?? linker.name(other)} as ${linkWord(property)} of ${title}`,
    );
  }
  function removeLink(property: string, other: string) {
    if (!term) return;
    skosEdit(
      linkEdit(term.iri, property, other, true, graph),
      `Remove ${linker.name(other)} as ${linkWord(property)} of ${title}`,
    );
  }

  /** The statements with the annotation assertions that state them in the edited graph. */
  const stated = $derived(
    term ? withAssertions(term.iri, term.statements, term.axioms, graph) : [],
  );
  /** The definitions, the edited graph's first, then the others. */
  const definitions = $derived(
    term
      ? stated
          .filter((s) => DEFINITION_PROPS.includes(s.property))
          .sort((a, b) => Number(inGraph(b, graph)) - Number(inGraph(a, graph)))
      : [],
  );
  /**
   * Drafts of definitions that another writer's commit changed while they were typed, each
   * shown as a definition of its own until it is previewed or cancelled.
   */
  let orphans = $state<{ id: number; draft: DefinitionDraft }[]>([]);
  let orphanId = 0;
  function keepOrphan(draft: DefinitionDraft) {
    if (editing && term?.iri === iri) orphans = [...orphans, { id: ++orphanId, draft }];
  }
  const dropOrphan = (id: number) => (orphans = orphans.filter((o) => o.id !== id));
  $effect(() => {
    void iri;
    if (!editing) untrack(() => (orphans = []));
  });
  let addingDefinition = $state(false);
  const synonymStatements = $derived(
    stated.filter((s) => SYNONYM_KINDS.some(([p]) => p === s.property)),
  );
  const owned = new Set([...DEFINITION_PROPS, ...SYNONYM_KINDS.map(([p]) => p)]);
  const annotationStatements = $derived(
    stated.filter((s) => isAnnotation(s, owned, (p) => term?.names[p]?.kinds)),
  );
  const ownAnnotations = $derived(annotationStatements.filter((s) => inGraph(s, graph)));
  const annotationProperties = $derived(
    term
      ? Object.entries(term.names)
          .filter(([, n]) => n.kinds.includes('annotationProperty'))
          .map(([i]) => i)
      : [],
  );
  const shortName = (i: string) => term?.names[i]?.label ?? iriName(i, prefixes);
  const parents = $derived(
    term && tree?.ancestors && treeKind && parentsEditable(treeKind)
      ? parentRows(treeKind, term.iri, tree.ancestors.edges, term.axioms, graph)
      : [],
  );
  const completionNames = $derived.by(() => {
    const out: NameOption[] = [];
    for (const [i, n] of Object.entries(term?.names ?? {})) {
      const k = editorKindOf(n.kinds);
      if (!k) continue;
      const kind = KIND_SINGULAR[k];
      const shown = n.label ?? n.shortForm ?? iriName(i, prefixes);
      out.push({ name: shown, iri: i, kind });
      const short = iriName(i, prefixes);
      if (short !== shown) out.push({ name: short, iri: i, kind });
    }
    return out;
  });
  const parse = (text: string, as: ParseAs) =>
    parseManchester(target, { graph, imports: true }, text, as);
  const defaultLang = $derived(
    (
      sections?.definition?.lang ??
      term?.labelLang ??
      langs[0]?.split('-')[0] ??
      'en'
    ).toLowerCase(),
  );

  // --- the keyboard and the palette ------------------------------------------------------
  /** Press the Preview button of the form that holds focus, or of the only changed form. */
  function previewForm() {
    const active = document.activeElement;
    const here = active?.closest<HTMLElement>('[data-edit-form]') ?? null;
    // focus in a field of another form previews nothing rather than some other change
    if (!here && active?.closest('input, textarea, select, [contenteditable], .cm-editor')) return;
    const changed = [
      ...document.querySelectorAll<HTMLElement>('[data-edit-form][data-dirty="true"]'),
    ];
    const form = here ?? (changed.length === 1 ? changed[0] : null);
    const b = form?.querySelector<HTMLButtonElement>('[data-preview]');
    if (b && !b.disabled) b.click();
    else if (!form && changed.length > 1)
      toasts.push('info', 'Several forms have changes', 'Click into the one to preview first.');
  }
  /** Enter edit mode if needed, then focus the add field of a section. */
  async function focusAdd(which: 'synonym' | 'parent' | 'axiom') {
    if (!editing) setEdit(true);
    for (let i = 0; i < 40; i++) {
      await tick();
      const el = addField(which);
      if (el) {
        el.scrollIntoView({ block: 'center' });
        el.focus();
        return;
      }
      await new Promise((r) => setTimeout(r, 50));
    }
  }
  function addField(which: 'synonym' | 'parent' | 'axiom'): HTMLElement | null {
    if (which === 'axiom') return document.querySelector('#browse-add-axiom .cm-content');
    if (which === 'parent') {
      if (!isSkos) return document.getElementById('browse-add-parent');
      const b = document.querySelector<HTMLButtonElement>(
        '[data-section="broader"] button[aria-label="Add broader"]',
      );
      b?.click();
      return b ? document.querySelector('[data-section="broader"] input') : null;
    }
    if (!isSkos) return document.getElementById('browse-add-synonym');
    const form = document.querySelector('form[aria-label="Add a label"]');
    const kindSel = form?.querySelector<HTMLSelectElement>('select');
    if (kindSel && kindSel.value !== 'alt') {
      kindSel.value = 'alt';
      kindSel.dispatchEvent(new Event('change'));
    }
    return form?.querySelector<HTMLElement>('input[aria-label="Label"]') ?? null;
  }
  const inText = (el: EventTarget | null) => {
    const e = el as HTMLElement | null;
    return (
      !!e &&
      (e.tagName === 'INPUT' ||
        e.tagName === 'TEXTAREA' ||
        e.tagName === 'SELECT' ||
        e.isContentEditable ||
        !!e.closest?.('.cm-editor'))
    );
  };
  /** A dialog or the palette has the keyboard. */
  const covered = () => launcher.open || !!document.querySelector('dialog[open]');
  onMount(() => {
    const key = (e: KeyboardEvent) => {
      if (e.defaultPrevented || covered()) return;
      const mod = e.ctrlKey || e.metaKey;
      if (mod && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 's') {
        if (!editing) return;
        e.preventDefault();
        previewForm();
        return;
      }
      if (mod || e.altKey || inText(e.target)) return;
      if (e.key === 'Escape' && editing) {
        e.preventDefault();
        leaveEdit();
        return;
      }
      if (e.shiftKey || !term || !canEdit) return;
      const k = e.key.toLowerCase();
      if (k === 'e' && !editing) {
        e.preventDefault();
        setEdit(true);
      } else if (k === 's') {
        e.preventDefault();
        void focusAdd('synonym');
      } else if (k === 'p' && (isSkos ? treeKind === 'concept' : parentsEditable(treeKind))) {
        e.preventDefault();
        void focusAdd('parent');
      } else if (k === 'a' && !isSkos) {
        e.preventDefault();
        void focusAdd('axiom');
      }
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  });
  useCommands(() => [
    {
      id: 'browse.workbench',
      label: isSkos ? 'Open in Thesaurus' : 'Open in Ontology',
      icon: 'pencil',
      keywords: ['workbench', 'editor', 'tree', 'open in ontology', 'open in thesaurus'],
      active: () => !!iri && canWrite && !!workbenchLink,
      run: () => void goto(workbenchLink),
    },
    {
      id: 'browse.edit',
      label: 'Edit this page',
      icon: 'pencil',
      shortcut: 'E',
      keywords: ['edit', 'change', 'curate'],
      active: () => !!iri && canEdit && !editing,
      run: () => setEdit(true),
    },
    {
      id: 'browse.done',
      label: 'Leave edit mode',
      icon: 'check',
      shortcut: 'Escape',
      keywords: ['done', 'stop editing'],
      active: () => editing,
      run: leaveEdit,
    },
    {
      id: 'browse.preview',
      label: 'Preview change',
      icon: 'check',
      shortcut: 'Mod+S',
      keywords: ['save', 'commit'],
      active: () => editing && unsaved(),
      run: () => void tick().then(previewForm),
    },
    {
      id: 'browse.undo',
      label: 'Undo',
      icon: 'stepBack',
      shortcut: 'Mod+Z',
      active: () => editing && !readonly && session.done.length > 0,
      run: () => session.undoLast(),
    },
    {
      id: 'browse.redo',
      label: 'Redo',
      icon: 'stepForward',
      shortcut: 'Mod+Shift+Z',
      active: () => editing && !readonly && session.undone.length > 0,
      run: () => session.redoLast(),
    },
    {
      id: 'browse.synonym',
      label: 'Add synonym',
      icon: 'plus',
      shortcut: 'S',
      keywords: ['label', 'alternative'],
      active: () => !!iri && canEdit,
      run: () => void focusAdd('synonym'),
    },
    {
      id: 'browse.parent',
      label: 'Add parent',
      icon: 'plus',
      shortcut: 'P',
      keywords: ['superclass', 'broader', 'hierarchy'],
      active: () =>
        !!iri && canEdit && (isSkos ? treeKind === 'concept' : parentsEditable(treeKind)),
      run: () => void focusAdd('parent'),
    },
    {
      id: 'browse.axiom',
      label: 'Add axiom',
      icon: 'plus',
      shortcut: 'A',
      keywords: ['logical definition', 'manchester'],
      active: () => !!iri && canEdit && !isSkos,
      run: () => void focusAdd('axiom'),
    },
    {
      id: 'browse.session',
      label: 'Start session',
      icon: 'branch',
      keywords: ['edit', 'branch'],
      subtitle: 'Edit on a branch of your own, then merge it',
      active: () => editing && !branch,
      run: startSession,
    },
    {
      id: 'browse.editor',
      label: 'Open in the full editor',
      icon: 'tree',
      keywords: ['ontology', 'thesaurus', 'editor'],
      active: () => !!term,
      run: () => void goto(editLink),
    },
  ]);

  // --- the landing page ----------------------------------------------------------------
  let head = $state<{ head: OntologyHead; scheme: boolean } | null>(null);
  /** The labels of the header's annotation properties and IRI values, by IRI. */
  let aboutNames = $state<Map<string, string>>(new Map());
  let counts = $state<{ kind: string; label: string; count: number }[]>([]);
  let roots = $state<Hierarchy | null>(null);
  let rootItems = $state<TreeNode[]>([]);
  let rootDefs = $state<Map<string, string>>(new Map());
  let rootBusy = $state(false);
  let headGraphs = $state<string[]>([]);
  let landingError = $state<string | null>(null);
  let landingLoading = $state(false);

  $effect(() => {
    const t = target;
    const g = graph;
    const a = at;
    if (!t || iri) return;
    const ctl = new AbortController();
    untrack(() => void loadLanding(t, { graph: g, at: a }, ctl.signal));
    return () => ctl.abort();
  });

  async function loadLanding(t: string, s: BrowseSelection, signal: AbortSignal) {
    head = null;
    aboutNames = new Map();
    counts = [];
    roots = null;
    rootItems = [];
    rootDefs = new Map();
    landingError = null;
    landingLoading = true;
    const opts = { at: s.at ?? undefined, signal };
    try {
      const [h, c, g] = await Promise.all([
        ontologyHead(t, s, langs, signal),
        api.select(t, countsQuery(s.graph), opts).then((rows) => parseCounts(rows as never)),
        api
          .select(t, headGraphsQuery(), opts)
          .then((rows) => rows.flatMap((r) => (r.g?.type === 'uri' ? [r.g.value] : [])))
          .catch(() => [] as string[]),
      ]);
      if (signal.aborted) return;
      headGraphs = g;
      head = h;
      counts = c;
      if (h) void loadAboutNames(t, s, h.head, signal);
      const has = (k: string) => c.some((x) => x.kind === k);
      const kind: TreeKind = !has(OWL + 'Class') && has(SKOS + 'Concept') ? 'concept' : 'class';
      const r = await fetchHierarchy(t, s, { kind, limit: 50 }, signal);
      if (signal.aborted) return;
      roots = r;
      rootItems = r.children;
      void loadDefinitions(t, s, r.children, signal);
    } catch (e) {
      if (!signal.aborted) landingError = api.errorMessage(e);
    } finally {
      if (!signal.aborted) landingLoading = false;
    }
  }

  /** `key` in the pieces between its soft break points: after `_` or `-` and in camelCase. */
  const keyParts = (key: string) => key.split(/(?<=[_-])|(?<=[a-z])(?=[A-Z])/);

  /** Labels for the header's properties and IRI values, so they read as words, not IDs. */
  async function loadAboutNames(
    t: string,
    s: BrowseSelection,
    h: OntologyHead,
    signal: AbortSignal,
  ) {
    const iris = new Set<string>();
    for (const a of h.annotations.slice(0, 12)) {
      iris.add(a.property);
      if (!isLiteral(a.value) && /^https?:/.test(a.value)) iris.add(a.value);
    }
    if (!iris.size) return;
    try {
      const rows = await api.select(t, namesQuery([...iris], s.graph), {
        at: s.at ?? undefined,
        signal,
      });
      if (!signal.aborted) aboutNames = parseDefinitions(rows as never, langs);
    } catch {
      // the names stay short IRIs
    }
  }

  async function loadDefinitions(
    t: string,
    s: BrowseSelection,
    nodes: TreeNode[],
    signal?: AbortSignal,
  ) {
    if (!nodes.length) return;
    try {
      const rows = await api.select(
        t,
        definitionsQuery(
          nodes.map((n) => n.iri),
          s.graph,
        ),
        { at: s.at ?? undefined, signal },
      );
      if (signal?.aborted) return;
      rootDefs = new Map([...rootDefs, ...parseDefinitions(rows as never, langs)]);
    } catch {
      // the roots stand without their definitions
    }
  }

  async function moreRoots() {
    if (!roots || roots.next === null) return;
    rootBusy = true;
    try {
      const r = await fetchHierarchy(target, sel, {
        kind: roots.kind,
        limit: 50,
        offset: rootItems.length,
      });
      roots = r;
      rootItems = [...rootItems, ...r.children];
      void loadDefinitions(target, sel, r.children);
    } finally {
      rootBusy = false;
    }
  }

  const chips = $derived.by(() => {
    const has = (k: string) => counts.some((c) => c.kind === k);
    const out: { label: string; kind: EntitySearchKind }[] = [];
    if (has(OWL + 'Class')) out.push({ label: 'Classes', kind: 'class' });
    if (
      has(OWL + 'ObjectProperty') ||
      has(OWL + 'DatatypeProperty') ||
      has(OWL + 'AnnotationProperty')
    )
      out.push({ label: 'Properties', kind: 'property' });
    if (has(OWL + 'NamedIndividual')) out.push({ label: 'Individuals', kind: 'individual' });
    if (has(SKOS + 'Concept')) out.push({ label: 'Concepts', kind: 'concept' });
    return out;
  });
  const entityTotal = $derived(
    counts.filter((c) => c.kind !== 'deprecated').reduce((n, c) => n + c.count, 0),
  );
  const landingTitle = $derived(head?.head.title ?? head?.head.iri ?? name);
  const pageTitle = $derived(iri ? (term ? title : 'Browse') : landingTitle);
</script>

<svelte:head><title>{pageTitle || 'Browse'} | Sparkles</title></svelte:head>

<div class="wrap">
  {#if !name}
    <div class="page-container">
      <div class="empty">
        <p>No dataset selected.</p>
        <a class="btn" href="{base}/datasets">Go to Datasets</a>
      </div>
    </div>
  {:else if iri}
    {#if termError}
      <div class="page-container">
        <div class="empty" role="alert">
          {#if termError.status === 404}
            <p>The graphs of <span class="mono">{name}</span> do not mention this IRI.</p>
            <p class="mono faint iri-line">{iri}</p>
          {:else}
            <p>{termError.text}</p>
          {/if}
          <a class="btn" href={hrefOf(null)}>Back to {name}</a>
        </div>
      </div>
    {:else if !term || !sections}
      <div class="page-container"><p class="faint" aria-live="polite">Loading…</p></div>
    {:else}
      {#if editing}
        <EditBar
          variant="top"
          branch={branch ?? MAIN}
          {protectedBranch}
          canSession={!branch}
          undoLabel={session.done.at(-1)?.label ?? null}
          redoLabel={session.undone.at(-1)?.label ?? null}
          notice={session.notice}
          error={sessionError}
          editorHref={editLink}
          {editorName}
          onundo={() => session.undoLast()}
          onredo={() => session.redoLast()}
          onsession={startSession}
          ondone={leaveEdit}
        />
      {/if}
      <div
        class="browse"
        use:balance
        class:graph={view === 'graph' && !treeKind}
        class:editing
        data-kind={treeKind ?? 'none'}
      >
        <div class="b-head">
          <TermHeader
            name={term}
            {title}
            {kind}
            crumbs={[
              { label: ontologyName, href: hrefOf(null) },
              { label: plural[kind] ?? 'Entities', href: null },
            ]}
            definition={null}
            replacedBy={iris(OBO + 'IAO_0100001')}
            consider={values(OIO + 'consider')
              .map((v) => valueText(v))
              .map((v) => oboIri(v) ?? v)}
            {linker}
            {curieIri}
          >
            {#snippet actions()}
              {#if !treeKind && !editing}
                {#if view === 'graph'}
                  <a class="btn sm" href={hrefOf(iri)} data-sveltekit-noscroll
                    ><Icon name="book" size={14} />Page</a
                  >
                {:else}
                  <a class="btn sm" href={hrefOf(iri, { view: 'graph' })} data-sveltekit-noscroll
                    ><Icon name="graph" size={14} />Graph</a
                  >
                {/if}
              {/if}
              <a class="btn sm" href={exploreHref}
                ><Icon name="explore" size={14} />Open in Explore</a
              >
              {#if canWrite && workbenchLink}
                <a class="btn sm" href={workbenchLink}
                  ><Icon name="pencil" size={14} />Open in {editorOf(term?.kinds ?? []) ===
                  'thesaurus'
                    ? 'Thesaurus'
                    : 'Ontology'}</a
                >
              {/if}
              <!-- the drawer's context chip is the entity this page lends (ROADMAP item 633) -->
              <button
                type="button"
                class="btn sm"
                onclick={() => {
                  askThread.chipRemoved = false;
                  askThread.show();
                }}><Icon name="sparkle" size={14} />Ask about this</button
              >
              {#if canEdit && !editing}
                <button
                  type="button"
                  class="btn sm"
                  aria-pressed="false"
                  title="Edit this page (E)"
                  onclick={() => setEdit(true)}><Icon name="pencil" size={14} />Edit</button
                >
              {/if}
            {/snippet}
          </TermHeader>
          {#if editing}
            <div class="defs" aria-label="Definitions" role="group">
              {#each definitions as d (statementKey(d))}
                {#if inGraph(d, graph)}
                  <DefinitionField
                    {mode}
                    iri={term.iri}
                    {title}
                    before={d}
                    {graph}
                    {readonly}
                    {onedit}
                    onorphan={keepOrphan}
                  />
                {:else}
                  <p class="def-ro">
                    {valueText(d.value)}
                    <span class="from">from {d.graph}, not editable here</span>
                  </p>
                {/if}
              {/each}
              {#each orphans as o (o.id)}
                <DefinitionField
                  {mode}
                  iri={term.iri}
                  {title}
                  before={null}
                  {graph}
                  initial={o.draft}
                  note="Another commit changed this definition while you typed. Preview adds your text as a definition of its own."
                  {readonly}
                  onedit={(op, label, after) =>
                    onedit(op, label, () => {
                      after?.();
                      dropOrphan(o.id);
                    })}
                  oncancel={() => dropOrphan(o.id)}
                />
              {/each}
              {#if addingDefinition || !definitions.some((d) => inGraph(d, graph))}
                <DefinitionField
                  {mode}
                  iri={term.iri}
                  {title}
                  before={null}
                  {graph}
                  lang={defaultLang}
                  {readonly}
                  onedit={(op, label, after) =>
                    onedit(op, label, () => {
                      after?.();
                      addingDefinition = false;
                    })}
                  oncancel={addingDefinition ? () => (addingDefinition = false) : undefined}
                  autofocus={addingDefinition}
                />
              {:else if !readonly}
                <button
                  type="button"
                  class="btn sm ghost add-def"
                  onclick={() => (addingDefinition = true)}
                  ><Icon name="plus" size={14} />Add a definition in another language</button
                >
              {/if}
            </div>
          {/if}
        </div>

        <!-- a stat strip, then the term's cards in two columns that balance their heights;
             one column below a large screen (ROADMAP item 633) -->
        <dl class="figures b-strip" aria-label="Counts" style:--n={termCounts.length}>
          {#each termCounts as c (c.key)}
            <div>
              <dt><a href={c.href}>{c.label}</a></dt>
              <dd>{c.loading ? '…' : fmtInt(c.n)}</dd>
            </div>
          {/each}
        </dl>
        <!-- the counts as one row of chips under the header, each a link to its section -->
        <nav class="b-chips" aria-label="Counts">
          {#each termCounts as c (c.key)}
            <a class="chip" href={c.href}
              >{c.loading ? '…' : fmtInt(c.n)} {c.n === 1 ? c.one : c.many}</a
            >
          {/each}
        </nav>
        {#if view === 'graph' && piece && !treeKind}
          <div class="b-graph">
            <TermGraph
              ds={target}
              {piece}
              {linker}
              {exploreHref}
              onexpand={expand}
              epoch={graphEpoch}
            />
          </div>
        {:else}
          <div class="b-main" data-col="main">
            <section class="panel b-def" aria-labelledby="def-h">
              <div class="panel-head"><h2 id="def-h">Definition</h2></div>
              <div class="panel-body ruled">
                {#if sections.definition}<p class="lede">
                    {#if sections.definitionKind}<span class="badge lede-kind"
                        >{sections.definitionKind}</span
                      >{/if}{valueText(
                      sections.definition.value,
                    )}{#if sections.definition.sources.length}<span class="faint small lede-src">
                        {sections.definition.sources.join(', ')}</span
                      >{/if}
                  </p>{:else if !editing}<p class="lede none">
                    No definition in this ontology.
                  </p>{/if}
                {#if editing}
                  {#if !isSkos && editorKind}
                    <SynonymEditor
                      iri={term.iri}
                      {title}
                      synonyms={synonymStatements}
                      kinds={SYNONYM_KINDS}
                      {graph}
                      lang={defaultLang}
                      {readonly}
                      {onedit}
                    />
                  {:else if details}
                    <section class="term-sec labels-edit" aria-labelledby="labels-edit-h">
                      <h2 id="labels-edit-h">Labels and synonyms</h2>
                      <!-- the editor's label table is wider than a phone, so it scrolls in place -->
                      <div class="labels-scroll">
                        <SkosLabels
                          iri={term.iri}
                          name={title}
                          entries={details.labels}
                          {graph}
                          langs={skosLangs}
                          {readonly}
                          onedit={skosEdit}
                          scope={`${target}|${graph ?? ''}`}
                          isUsed={async (i) => {
                            const r = await api.query(target, usedQuery(i, graph), {
                              nocache: true,
                            });
                            return !!r.boolean;
                          }}
                        />
                      </div>
                    </section>
                    <section class="term-sec links-edit" aria-labelledby="links-edit-h">
                      <h2 id="links-edit-h">Related concepts and mappings</h2>
                      <SkosLinks
                        title="Related"
                        id="related"
                        links={details.related}
                        name={(i) => linker.name(i)}
                        sub={(i) => linker.short(i)}
                        {readonly}
                        paired
                        {picker}
                        onopen={(i) => void goto(hrefOf(i))}
                        onadd={(i, n) => addLink('related', i, n)}
                        onremove={(i) => removeLink('related', i)}
                        onrepair={(l) => addLink('related', l.iri)}
                      />
                      {#each MAPPINGS.filter((m) => addedMappings.includes(m) || details?.mappings.some((x) => x.property === m)) as m (m)}
                        <SkosLinks
                          title={MAPPING_NAME[m]}
                          id={m}
                          links={details.mappings
                            .filter((x) => x.property === m)
                            .map((x) => ({ iri: x.target, forward: true, inverse: true }))}
                          name={(i) => linker.name(i)}
                          sub={(i) => linker.short(i)}
                          {readonly}
                          {picker}
                          onopen={(i) => void goto(hrefOf(i))}
                          onadd={(i, n) => addLink(m, i, n)}
                          onremove={(i) => removeLink(m, i)}
                        />
                      {/each}
                      {#if !readonly && MAPPINGS.some((m) => !addedMappings.includes(m) && !details?.mappings.some((x) => x.property === m))}
                        <label class="add-map">
                          <span class="sr-only">Add a mapping</span>
                          <select
                            class="select sm"
                            value=""
                            onchange={(e) => {
                              const m = e.currentTarget.value;
                              e.currentTarget.value = '';
                              if (m) addedMappings = [...addedMappings, m];
                            }}
                          >
                            <option value="">Add a mapping…</option>
                            {#each MAPPINGS.filter((m) => !addedMappings.includes(m) && !details?.mappings.some((x) => x.property === m)) as m (m)}
                              <option value={m}>{MAPPING_NAME[m]}</option>
                            {/each}
                          </select>
                        </label>
                      {/if}
                    </section>
                  {:else}
                    <p class="faint" aria-live="polite">Loading the labels…</p>
                  {/if}
                  <section class="term-sec" aria-labelledby="ann-edit-h">
                    <h2 id="ann-edit-h">
                      Annotations {#if ownAnnotations.length}<span class="count-badge"
                          >{ownAnnotations.length}</span
                        >{/if}
                    </h2>
                    <p class="sec-note">
                      {isSkos ? 'Notes' : 'The label, comments, cross-references'} and the other annotations.
                      The definition and {isSkos ? 'labels' : 'synonyms'} are edited above.
                    </p>
                    <AnnotationRows
                      annotations={ownAnnotations.map(toAnnotation)}
                      name={shortName}
                      {prefixes}
                      properties={annotationProperties}
                      {readonly}
                      label={title}
                      onchange={(next, what) => {
                        if (!term) return;
                        const op = annotationsOp(mode, term.iri, ownAnnotations, next, graph);
                        if (op) onedit(op, what);
                      }}
                    />
                    {#if annotationStatements.length > ownAnnotations.length}
                      <p class="sec-note">
                        {annotationStatements.length - ownAnnotations.length} more come from other graphs
                        and are not editable here.
                      </p>
                    {/if}
                  </section>
                {:else}
                  {#if logicalDefinition.length}
                    <section class="term-sec" aria-labelledby="logic-h">
                      <h2 id="logic-h">Logical definition</h2>
                      {#each logicalDefinition as v, i (i)}
                        <p class="logic"><AxiomProse value={v} {linker} {prefixes} /></p>
                      {/each}
                    </section>
                  {/if}
                  <TermSynonyms synonyms={sections.synonyms} {linker} />
                  <TermNotes
                    notes={sections.otherDefinitions.length
                      ? [
                          {
                            kind: 'Definition in other languages',
                            values: sections.otherDefinitions,
                          },
                          ...sections.notes,
                        ]
                      : sections.notes}
                    {linker}
                    {langs}
                    {curieIri}
                  />
                  <TermLabels labels={sections.labels} />
                  {#if sections.related.length}
                    <section class="term-sec" aria-labelledby="rel-h">
                      <h2 id="rel-h">
                        Related concepts <span class="count-badge">{sections.related.length}</span>
                      </h2>
                      <div class="term-flow">
                        {#each sections.related as r (r)}<a href={linker.href(r)}
                            >{linker.name(r)}</a
                          >{/each}
                      </div>
                    </section>
                  {/if}
                  {#if sections.members.length}
                    <section class="term-sec" aria-labelledby="mem-h">
                      <h2 id="mem-h">
                        Members <span class="count-badge">{sections.members.length}</span>
                      </h2>
                      <div class="term-flow">
                        {#each sections.members as r (r)}<a href={linker.href(r)}
                            >{linker.name(r)}</a
                          >{/each}
                      </div>
                    </section>
                  {/if}
                {/if}
                {#if term.statementsTruncated}
                  <p class="faint">
                    This entity has more statements than the page reads. The download has them all.
                  </p>
                {/if}
              </div>
            </section>
            {#if !isSkos && editorKind}
              <!-- the entity's frame in Manchester syntax, after Definition on a phone -->
              {#snippet axiomEditor()}
                <AxiomEditor
                  {groups}
                  kind={editorKind!}
                  iri={term!.iri}
                  {title}
                  {linker}
                  {prefixes}
                  {graph}
                  {readonly}
                  names={() => completionNames}
                  {parse}
                  {onedit}
                  bare
                />
              {/snippet}
              <TermAxioms
                kind={editorKind}
                iri={term.iri}
                {groups}
                {linker}
                {prefixes}
                {target}
                {graph}
                historic={!!at}
                editor={editing ? axiomEditor : undefined}
              />
            {/if}
            {#if treeKind}
              <!-- the hierarchy is a card above the facts in the page's side column, and on a
                 phone a card that opens on request (ROADMAP item 633) -->
              {#snippet treeHead()}
                {#if !editing}
                  <button
                    type="button"
                    class="btn sm ghost icon tree-toggle"
                    aria-expanded={treeOpen}
                    aria-controls="tree-body"
                    aria-label={treeOpen ? 'Hide the hierarchy' : 'Show the hierarchy'}
                    onclick={() => (treeOpen = !treeOpen)}
                    ><Icon name={treeOpen ? 'chevronDown' : 'chevron'} size={16} /></button
                  >
                {/if}
                <h2 id="tree-h">Hierarchy</h2>
                <span class="spacer"></span>
                {#if !editing}
                  <span class="seg" role="group" aria-label="View">
                    <a
                      href={hrefOf(iri)}
                      aria-current={view === 'tree' ? 'true' : undefined}
                      data-sveltekit-noscroll>Tree</a
                    ><a
                      href={hrefOf(iri, { view: 'graph' })}
                      aria-current={view === 'graph' ? 'true' : undefined}
                      data-sveltekit-noscroll>Graph</a
                    >
                  </span>
                {/if}
              {/snippet}
              <aside
                class="panel b-tree"
                class:shut={!treeOpen && !editing}
                aria-labelledby="tree-h"
              >
                {#if view === 'graph' && piece && !editing}
                  <TermGraph
                    ds={target}
                    {piece}
                    {linker}
                    {exploreHref}
                    onexpand={expand}
                    epoch={graphEpoch}
                    lead={treeHead}
                    shut={!treeOpen}
                  />
                {:else}
                  <div class="panel-head">{@render treeHead()}</div>
                  <div class="tree-edit">
                    {#if editing && isSkos && treeKind === 'concept'}
                      <div class="skos-parents">
                        <SkosLinks
                          title="Broader"
                          id="broader"
                          links={details?.broader ?? []}
                          name={(i) => linker.name(i)}
                          sub={(i) => linker.short(i)}
                          {readonly}
                          paired
                          {picker}
                          onopen={(i) => void goto(hrefOf(i))}
                          onadd={(i, n) => addLink('broader', i, n)}
                          onremove={(i) => removeLink('broader', i)}
                          onrepair={(l) => addLink('broader', l.iri)}
                        />
                        <SkosLinks
                          title="Narrower"
                          id="narrower"
                          links={details?.narrower ?? []}
                          name={(i) => linker.name(i)}
                          sub={(i) => linker.short(i)}
                          {readonly}
                          paired
                          {picker}
                          onopen={(i) => void goto(hrefOf(i))}
                          onadd={(i, n) => addLink('narrower', i, n)}
                          onremove={(i) => removeLink('narrower', i)}
                          onrepair={(l) => addLink('narrower', l.iri)}
                        />
                      </div>
                    {:else if editing && parentsEditable(treeKind)}
                      <ParentEditor
                        kind={treeKind}
                        iri={term.iri}
                        {title}
                        rows={parents}
                        {linker}
                        {target}
                        {graph}
                        {prefixes}
                        {readonly}
                        {onedit}
                      />
                    {/if}
                  </div>
                  <div class="tree-body" id="tree-body" bind:this={treeBox}>
                    {#if tree}
                      <LineageTree
                        {target}
                        {sel}
                        kind={treeKind}
                        iri={term.iri}
                        {title}
                        answer={tree}
                        href={(i) => hrefOf(i)}
                      />
                    {:else}
                      <p class="faint">Loading…</p>
                    {/if}
                  </div>
                {/if}
              </aside>
            {/if}
            <section class="panel b-refs" aria-labelledby="refs-h" data-movable>
              <div class="panel-head"><h2 id="refs-h">References</h2></div>
              <div class="panel-body ruled">
                <TermXrefs
                  xrefs={sections.xrefs}
                  seeAlso={sections.seeAlso}
                  mappings={sections.mappings}
                  {linker}
                />
                <TermUsage
                  iri={term.iri}
                  groups={mentions}
                  total={term.mentions.total}
                  truncated={term.mentions.truncated}
                  incoming={term.incoming}
                  usage={term.usage}
                  {linker}
                  {prefixes}
                  more={moreIncoming}
                  skip={skipIncoming}
                />
                <TermReuse
                  found={reuse.found}
                  checked={reuse.checked}
                  loading={reuse.loading}
                  href={(d) => browseHref(base, d, term?.iri ?? null)}
                />
                {#if !editing}
                  <SimilarTerms ds={target} iri={term.iri} href={(i) => hrefOf(i)} />
                {/if}
                <!-- the statements no other section shows, folded away -->
                {#if sections.other.length && !editing}
                  <section class="term-sec">
                    <details class="ann">
                      <summary
                        >All other statements <span class="count-badge"
                          >{sections.other.reduce((n, o) => n + o.values.length, 0)}</span
                        ></summary
                      >
                      <div class="table-scroll">
                        <table class="data">
                          <tbody>
                            {#each sections.other as o (o.property)}
                              <tr>
                                <th scope="row" class="mono">{iriName(o.property, prefixes)}</th>
                                <td>
                                  {#each o.values as v, i (i)}{#if i > 0}<br
                                      />{/if}{#if isLiteral(v)}{v[
                                        '@value'
                                      ]}{#if v['@language']}<span class="lang-tag"
                                          >{v['@language']}</span
                                        >{/if}{:else if linker.href(v)}<a href={linker.href(v)}
                                        >{linker.name(v)}</a
                                      >{:else}<span class="mono">{v}</span>{/if}{/each}
                                </td>
                              </tr>
                            {/each}
                          </tbody>
                        </table>
                      </div>
                    </details>
                  </section>
                {/if}
              </div>
            </section>
          </div>
        {/if}
        <div class="b-side" data-col="side">
          <div class="b-info"><TermInfobox rows={infoRows} {linker} heading="Metadata" /></div>
          <section class="panel b-dl" aria-labelledby="dl-h" data-movable>
            <div class="panel-head"><h2 id="dl-h">Download</h2></div>
            <div class="panel-body">
              <DownloadButtons
                noun="this {kind}"
                href={(f) => branchPath(downloadHref(target, sel, term!.iri, f))}
              />
            </div>
          </section>
        </div>
      </div>
    {/if}
  {:else}
    <div class="landing">
      <header class="term-head">
        <nav class="crumbs" aria-label="Breadcrumbs">
          <span>{name}</span><span aria-hidden="true">›</span><span>{graph ?? 'default graph'}</span
          >
        </nav>
        <div class="title-row">
          <h1 class="term-title">{landingTitle}</h1>
          <div class="actions">
            {#if headGraphs.length}
              <label class="form-row">
                Graph
                <select
                  class="select"
                  value={graph ?? ''}
                  onchange={(e) =>
                    goto(
                      browseHref(base, name, null, {
                        ...keep,
                        graph: e.currentTarget.value || null,
                      }),
                    )}
                >
                  <option value="">default graph</option>
                  {#each headGraphs as g (g)}<option value={g}>{iriName(g, prefixes)}</option
                    >{/each}
                  {#if graph && !headGraphs.includes(graph)}<option value={graph}>{graph}</option
                    >{/if}
                </select>
              </label>
            {/if}
            <a
              class="btn sm"
              href="{base}/{head?.scheme ? 'thesaurus' : 'ontology'}?{new URLSearchParams({
                ds: name,
                ...(graph ? { graph } : {}),
              })}"
              ><Icon name="pencil" size={14} />Open in {head?.scheme ? 'Thesaurus' : 'Ontology'} editor</a
            >
          </div>
        </div>
        {#if head}
          <div class="ids">
            {#if head.head.iri}<span class="iri-text">{head.head.iri}</span>{/if}
            {#if head.head.version}<span class="faint">version {head.head.version}</span>{/if}
            {#if head.head.license}
              {#if /^https?:/.test(head.head.license)}<a
                  href={head.head.license}
                  rel="noopener noreferrer"
                  target="_blank">License</a
                >{:else}<span>{head.head.license}</span>{/if}
            {/if}
          </div>
          {#if head.head.description}<p class="lede">{head.head.description}</p>{/if}
        {/if}
      </header>
      {#if landingError}
        <div class="error-box" role="alert">
          <strong>Could not read the ontology.</strong>
          {landingError}
        </div>
      {/if}
      <!-- the entity search reads a branch head, so a past state has no search box (item 658) -->
      {#if !at}
        <EntitySearchList
          {target}
          {graph}
          href={(i) => hrefOf(i)}
          {chips}
          placeholder={entityTotal
            ? `Search ${entityTotal.toLocaleString('en-US')} entities by label, synonym, ID or definition`
            : 'Search by label, synonym, ID or definition'}
        />
      {/if}
      <div class="cols">
        {#if roots}
          <TopTerms
            title={roots.kind === 'concept' ? 'Top concepts' : 'Top classes'}
            roots={rootItems}
            kind={roots.kind}
            definitions={rootDefs}
            href={(i) => hrefOf(i)}
            from={roots.roots}
            deprecated={roots.deprecatedRoots ?? 0}
            more={roots.next !== null ? moreRoots : undefined}
            busy={rootBusy}
          />
        {:else if landingLoading}
          <p class="faint" aria-live="polite">Loading…</p>
        {:else}
          <div></div>
        {/if}
        <aside class="side">
          {#if counts.length}
            <section class="panel" aria-labelledby="contents-h">
              <div class="panel-head"><h3 id="contents-h">Contents</h3></div>
              <div class="panel-body">
                <dl class="counts">
                  {#each counts as c (c.kind)}
                    <dt class:sub={c.kind === 'deprecated'}>{c.label}</dt>
                    <dd class:sub={c.kind === 'deprecated'}>{c.count.toLocaleString('en-US')}</dd>
                  {/each}
                </dl>
              </div>
            </section>
          {/if}
          {#if head?.head.annotations.length}
            <section class="panel" aria-labelledby="about-h">
              <div class="panel-head">
                <h3 id="about-h">About this {head.scheme ? 'thesaurus' : 'ontology'}</h3>
              </div>
              <div class="panel-body">
                <dl class="about">
                  {#each head.head.annotations.slice(0, 12) as a, i (i)}
                    {@const key =
                      aboutNames.get(a.property) ?? displayName(a.property, undefined, prefixes)}
                    <!-- a long key breaks only between words: at a space, after _ or -, or
                         between the words of a camelCase name -->
                    <dt title={a.property}>
                      {#each keyParts(key) as part, j (j)}{#if j}<wbr />{/if}{part}{/each}
                    </dt>
                    <dd>
                      {#if isLiteral(a.value)}{a.value['@value']}{:else if aboutNames.has(a.value)}
                        <!-- a term of this graph opens its own page -->
                        <a href={browseHref(base, name, a.value, keep)} title={a.value}
                          >{aboutNames.get(a.value)}</a
                        >{:else if /^https?:/.test(a.value)}<a
                          href={a.value}
                          title={a.value}
                          rel="noopener noreferrer"
                          target="_blank">{iriName(a.value, prefixes)}</a
                        >{:else}{a.value}{/if}
                    </dd>
                  {/each}
                </dl>
              </div>
            </section>
          {/if}
        </aside>
      </div>
    </div>
  {/if}
</div>

{#if editing && term}
  <EditBar
    variant="bottom"
    branch={branch ?? MAIN}
    {protectedBranch}
    canSession={!branch}
    undoLabel={session.done.at(-1)?.label ?? null}
    redoLabel={session.undone.at(-1)?.label ?? null}
    notice={session.notice}
    error={sessionError}
    editorHref={editLink}
    {editorName}
    onundo={() => session.undoLast()}
    onredo={() => session.redoLast()}
    onsession={startSession}
    ondone={leaveEdit}
  />
{/if}

<OntoPreview
  bind:open={session.previewOpen}
  title={session.previewTitle}
  {target}
  sel={{ graph, imports: true }}
  op={session.previewOp}
  etag={term?.etag ?? null}
  head={term?.commit ?? null}
  {describe}
  oncommitted={(op, a) => void session.committed(op, a)}
  onstale={() => void session.stale()}
/>

<style>
  .labels-edit {
    min-width: 0;
  }
  .labels-scroll {
    overflow-x: auto;
  }
  .wrap {
    container: browse / inline-size;
    min-width: 0;
  }
  /* one column on a phone and a medium screen: the head, the counts, Definition, Axioms,
     Metadata, Hierarchy, References and Download (ROADMAP item 633) */
  .browse {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--gap);
    padding: var(--gutter) var(--gutter) var(--sp-10);
    max-width: 1360px;
    min-width: 0;
  }
  .b-head {
    order: 0;
    min-width: 0;
  }
  .b-main,
  .b-side {
    display: contents;
  }
  .b-strip {
    order: 1;
  }
  .b-def,
  .b-graph {
    order: 2;
    min-width: 0;
  }
  .browse :global(.b-ax) {
    order: 3;
  }
  .logic {
    margin: 0;
    font-size: var(--fs-md);
    line-height: 1.5;
  }
  .b-info {
    order: 4;
    min-width: 0;
  }
  .b-tree {
    order: 5;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .b-refs {
    order: 6;
  }
  .b-dl {
    order: 7;
  }
  /* the tiles share the row whatever their number, so no cell stands empty */
  .b-strip {
    grid-template-columns: repeat(var(--n, 5), minmax(0, 1fr));
  }
  .b-strip {
    display: none;
  }
  .b-chips {
    display: flex;
    order: 1;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: calc(-1 * var(--sp-2));
  }
  .b-chips .chip {
    display: inline-flex;
    align-items: center;
    height: 28px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--surface);
    color: var(--text-2);
    font-size: var(--fs-sm);
    text-decoration: none;
    font-variant-numeric: tabular-nums;
  }
  .b-chips .chip:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }
  /* on a phone the definition's lead sits close under its heading */
  @container browse (max-width: 639px) {
    .b-def :global(.panel-body) {
      padding-top: var(--sp-3);
    }
  }
  .b-strip dt a {
    color: inherit;
    text-decoration: none;
  }
  .b-strip dt a:hover {
    text-decoration: underline;
  }
  /* a card's sections, split by rules as in the Workbench's details panel */
  .ruled {
    display: grid;
  }
  .ruled > :global(*) {
    min-width: 0;
  }
  .ruled > :global(:empty) {
    display: none;
  }
  .ruled > :global(section) {
    padding: var(--sp-4) 0;
    border-top: 1px solid var(--border);
  }
  .ruled > :global(section:first-child),
  .ruled > :global(.lede + section) {
    border-top: 0;
  }
  .ruled > :global(section:first-child) {
    padding-top: 0;
  }
  .ruled > :global(section:last-child) {
    padding-bottom: 0;
  }
  .ruled :global(.term-sec > h2) {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    margin-bottom: var(--sp-2);
  }
  /* the definition's sources follow it after a gap, whatever whitespace the markup keeps */
  .lede-src {
    margin-left: var(--sp-2);
  }
  .lede-kind {
    margin-right: var(--sp-2);
    vertical-align: 2px;
  }
  .ruled > .lede.none {
    color: var(--text-3, var(--text-2));
    font-size: var(--fs-md);
  }
  .ruled > .lede {
    margin: 0 0 var(--sp-3);
    font-size: 15px;
    line-height: 1.6;
  }
  /* two columns on a large screen: the term's own cards on the left and Metadata and Download
     on the right, with References or Download moving to the shorter column (balance.ts) */
  @container browse (min-width: 1100px) {
    .browse {
      /* the side column takes what its cards need, and the term's own cards the rest */
      grid-template-columns: minmax(0, 1fr) var(--side-column);
      grid-template-areas: 'head head' 'chips chips' 'main side';
      align-items: start;
    }
    .b-head {
      grid-area: head;
    }
    .b-strip {
      grid-area: strip;
    }
    .b-chips {
      grid-area: chips;
    }
    .b-main,
    .b-side {
      display: grid;
      gap: var(--gap);
      min-width: 0;
      align-content: start;
    }
    .b-main,
    .b-graph {
      grid-area: main;
    }
    .b-side {
      grid-area: side;
    }
    /* in two columns a card keeps its place in its column, where balance.ts puts it */
    .browse .b-main > :global(*),
    .browse .b-side > :global(*) {
      order: 0;
    }
    /* the shorter column stays in view while the page scrolls */
    .b-side:global([data-stick]) {
      position: sticky;
      top: var(--gutter);
    }
  }
  .b-tree h2 {
    font-size: var(--fs-md);
  }
  .tree-edit:not(:empty) {
    padding: var(--sp-3) 14px 0;
  }
  /* the card scrolls on its own, about half the window at most */
  .tree-body {
    position: relative;
    max-height: 50vh;
    overflow: auto;
    padding: var(--sp-3) 14px;
    overscroll-behavior: contain;
  }
  .browse .b-tree .tree-toggle {
    display: none;
    margin-left: -6px;
  }
  @container browse (max-width: 639px) {
    .browse .b-tree .tree-toggle {
      display: inline-flex;
    }
    .b-tree.shut .tree-body,
    .b-tree.shut .tree-edit {
      display: none;
    }
    .b-tree.shut .panel-head {
      border-bottom: 0;
    }
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--r);
    overflow: hidden;
    background: var(--surface);
  }
  .seg a {
    display: inline-flex;
    align-items: center;
    height: var(--ctl-h-sm);
    padding: 0 10px;
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--text-2);
    text-decoration: none;
  }
  .seg a[aria-current='true'] {
    background: var(--surface-3);
    color: var(--text);
  }
  details.ann summary {
    cursor: pointer;
    color: var(--text-2);
    font-weight: 500;
  }
  details.ann .table-scroll {
    margin-top: var(--sp-3);
  }
  details.ann th {
    font-weight: 500;
    color: var(--text-2);
    white-space: nowrap;
    vertical-align: top;
  }
  details.ann td a {
    color: var(--iri);
  }
  .iri-line {
    overflow-wrap: anywhere;
  }

  /* the landing page */
  .landing {
    padding: var(--gutter) var(--gutter) var(--sp-10);
    display: grid;
    gap: var(--sp-8);
    max-width: 1180px;
    min-width: 0;
  }
  .crumbs {
    font-size: var(--fs-sm);
    color: var(--text-3);
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    margin-bottom: var(--sp-2);
  }
  .title-row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    flex-wrap: wrap;
  }
  .term-title {
    font-size: var(--fs-display);
    line-height: 1.12;
    letter-spacing: -0.025em;
    font-weight: 600;
    margin: 0;
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .actions {
    margin-left: auto;
    display: flex;
    gap: var(--sp-2);
    flex-wrap: wrap;
  }
  .ids {
    display: flex;
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
    flex-wrap: wrap;
    margin-top: var(--sp-2);
  }
  .iri-text {
    font: 12px var(--font-mono);
    color: var(--iri);
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .lede {
    margin-top: var(--sp-5);
    font-size: 15px;
    line-height: 1.6;
    max-width: 68ch;
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--sp-6);
  }
  @container browse (min-width: 640px) {
    .cols {
      grid-template-columns: minmax(0, 1fr) var(--side-column);
    }
  }
  @container browse (min-width: 1040px) {
    .cols {
      gap: var(--sp-8);
    }
  }
  @container browse (max-width: 639px) {
    .actions {
      margin-left: 0;
      width: 100%;
    }
    .lede {
      font-size: 14px;
    }
  }
  .side {
    display: grid;
    gap: var(--gap);
    align-content: start;
    min-width: 0;
  }
  .counts,
  .about {
    display: grid;
    gap: 6px var(--sp-4);
    margin: 0;
  }
  .counts {
    grid-template-columns: minmax(0, 1fr) auto;
  }
  .counts dt {
    color: var(--text-2);
  }
  .counts dd {
    margin: 0;
    text-align: right;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
  .counts .sub {
    color: var(--text-3);
    font-weight: 400;
    font-size: var(--fs-sm);
  }
  .counts dt.sub {
    padding-left: var(--sp-3);
  }
  .about {
    grid-template-columns: minmax(8rem, 1fr) minmax(0, 1.4fr);
  }
  /* a long key such as hasOBOFormatVersion wraps at its soft break points, never inside a word */
  .about dt {
    min-width: 0;
    overflow-wrap: normal;
    word-break: normal;
    color: var(--text-3);
    font-size: var(--fs-sm);
  }
  .about dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  /* edit mode (item 630) */
  .browse.editing {
    padding-top: var(--sp-5);
  }
  .defs {
    display: grid;
    gap: var(--sp-4);
    margin-top: var(--sp-5);
    min-width: 0;
  }
  .def-ro {
    margin: 0;
    max-width: 68ch;
    color: var(--text-2);
    line-height: 1.6;
  }
  .from {
    font-size: var(--fs-xs);
    color: var(--text-3);
    margin-left: 6px;
  }
  .add-def {
    justify-self: start;
  }
  .sec-note {
    font-size: var(--fs-sm);
    color: var(--text-3);
    margin: 0 0 var(--sp-2);
  }
  .skos-parents {
    margin: 0 0 var(--sp-4);
    padding: 0 0 var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  .links-edit {
    display: grid;
    gap: var(--sp-3);
  }
  .links-edit h2 {
    margin-bottom: 0;
  }
  .add-map {
    justify-self: start;
  }
  @media (max-width: 760px) {
    .browse.editing {
      padding-bottom: calc(96px + env(safe-area-inset-bottom));
    }
  }
</style>

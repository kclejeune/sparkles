<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onDestroy, onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
  import { routes } from '$lib/routes';
  import { assistantSettings } from '$lib/ask-api';
  import type { Term } from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import * as ex from '$lib/explore';
  import { fmtCompact, fmtInt } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import { displayIri, isVectorLiteral, literalText, RDF_TYPE, termKey } from '$lib/rdf';
  import { Generation, isAbort, LatestRun } from '$lib/supersede';
  import { parseCompact } from '$lib/compact';
  import {
    authorsOf,
    entityChanges,
    entityCommits,
    isReadable,
    netChanges,
    selectorSeq,
    type EntityChange,
  } from '$lib/entity-history';
  import { unrecordedNote } from '$lib/history';
  import { timeFailure, type Range } from '$lib/timeline';
  import {
    confirmPlan,
    deltaText,
    edgeIdOf,
    planDiff,
    schemaAnnouncement,
    schemaChanges,
  } from '$lib/explore-time';
  import type { EdgeTriple, SchemaChanges } from '$lib/explore-time';
  import ClassTree from '$components/ClassTree.svelte';
  import GeoMapCard from '$components/GeoMapCard.svelte';
  import GraphMap from '$components/GraphMap.svelte';
  import GraphView, { type GEdge, type GNode } from '$components/GraphView.svelte';
  import LargeGraphView, { type LEdge, type LNode } from '$components/LargeGraphView.svelte';
  import ExploreFilters, {
    freshFilters,
    type FilterState,
    type GroupEntry,
  } from '$components/ExploreFilters.svelte';
  import ExploreGroup from '$components/ExploreGroup.svelte';
  import * as grp from '$lib/explore-groups';
  import {
    applyFilters,
    classFacet,
    HELD_LIMIT,
    parallelEdges,
    predicateFacet,
    VISIBLE_BUDGET,
    WEBGL_FROM,
    wantWebgl,
    type Filters,
  } from '$lib/explore-filters';
  import { hasWebGL2, seed } from '$lib/overview';
  import SelectionBar from '$components/SelectionBar.svelte';
  import FacetPanel from '$components/FacetPanel.svelte';
  import { selectionQuery, setQuery, VALUES_CAP, type Brush } from '$lib/facets';
  import type { FacetSetSpec } from '$lib/api';
  import { selection } from '$lib/selection.svelte';
  import { useAskContext, useAskTarget } from '$lib/ask-thread.svelte';
  import Icon from '$components/Icon.svelte';
  import SchemaDiffDialog from '$components/SchemaDiffDialog.svelte';
  import ShapesDraftDialog from '$components/ShapesDraftDialog.svelte';
  import { coverage, objectKinds, shortenExpression, valuesRange } from '$lib/schema-history';
  import SimilarPanel from '$components/SimilarPanel.svelte';
  import TermView from '$components/TermView.svelte';
  import TimeSlider from '$components/TimeSlider.svelte';
  import MemoryTab from '$components/MemoryTab.svelte';
  import OverviewView, { type OverviewCommunity } from '$components/OverviewView.svelte';
  import CommunityReport from '$components/CommunityReport.svelte';
  import * as gr from '$lib/graphrag';
  import { branchParam, onBranch } from '$lib/branches';
  import PathPanel from '$components/PathPanel.svelte';
  import type { Picked } from '$components/EntityPicker.svelte';
  import * as pf from '$lib/paths';
  import DiffGraph from '$components/DiffGraph.svelte';
  import { quadIds, seqOf, stepAnnouncement, STEP_QUADS } from '$lib/diff-graph';
  import TextSearchView from '$components/TextSearchView.svelte';
  import GraphListView, { type ListRow } from '$components/GraphListView.svelte';
  import LensMenu from '$components/LensMenu.svelte';
  import LensLegend from '$components/LensLegend.svelte';
  import { describe as lensDescribe, entryIds, OTHER } from '$lib/lens';
  import { Lenses } from '$lib/lenses.svelte';
  import ValidationOverlay from '$components/ValidationOverlay.svelte';
  import WhyPanel from '$components/WhyPanel.svelte';
  import WhyNotPanel from '$components/WhyNotPanel.svelte';
  import type { WhyTriple } from '$lib/why';
  import ShapeForm from '$components/ShapeForm.svelte';
  import { getForm } from '$lib/forms-api';
  import ValidationSource from '$components/ValidationSource.svelte';
  import { type Badge, type GuardProbe, type VResult } from '$lib/validation-overlay';
  import { load as loadPref, loadRaw, save as savePref } from '$lib/storage';
  import { useHint, usePageSearch, useProvider, type LauncherItem } from '$lib/launcher.svelte';

  type ENode = {
    id: string;
    term: Term;
    iri: string;
    /** An explicit RDF label. Fallback captions follow the current dataset prefixes. */
    label?: string;
    expanded: boolean;
    lit: boolean;
    /** A group node (ROADMAP item 725): the neighbours of `owner` through one predicate. */
    group?: grp.GroupSpec;
  };
  type EEdge = GEdge & { lit: boolean };

  const RDFS_COMMENT = 'http://www.w3.org/2000/01/rdf-schema#comment';
  const INFERRED_GRAPH = 'urn:x-sparkles:inferred';

  const ds = $derived(app.current);
  const prefixes = $derived(app.prefixes(ds));

  /** Whether the dataset answers questions, for the search box's Ask hint (C18 §6.1). */
  let canAsk = $state(false);
  $effect(() => {
    const name = ds;
    canAsk = false;
    if (!name) return;
    const ctl = new AbortController();
    assistantSettings(name, ctl.signal)
      .then((s) => {
        if (ds === name) canAsk = !!s?.status?.ask;
      })
      .catch(() => {});
    return () => ctl.abort();
  });

  // the selected resource is the context of a question asked from the Ask drawer
  useAskContext(() => {
    const n = selectedId ? nodes[selectedId] : null;
    if (!n || n.lit || n.term.type !== 'uri') return null;
    return {
      kind: 'entity',
      iri: n.iri,
      label: n.label ?? shortLabel(n.iri, prefixes),
      type: 'resource',
    };
  });
  // the branch and the state the page reads
  useAskTarget(() => ({ branch: overviewBranch, at: timeActive && exAt ? exAt : null }));

  type Tab = 'graph' | 'schema' | 'search';
  const urlTab = page.url.searchParams.get('tab');
  let tab = $state<Tab>(urlTab === 'schema' || urlTab === 'search' ? urlTab : 'graph');
  /** The view of the selected resource: its properties or its memory (`&view=memory`). */
  let sideTab = $state<'props' | 'memory'>(
    page.url.searchParams.get('view') === 'memory' ? 'memory' : 'props',
  );
  /** Full-text query of the Search tab (`&q=`). */
  let textQuery = $state(page.url.searchParams.get('q') ?? '');
  /** The drawn graph as the node-link canvas or on a map (`&map=1`, Shift+M). */
  let canvasMode = $state<'graph' | 'map'>(
    page.url.searchParams.get('map') === '1' ? 'map' : 'graph',
  );
  /** The facet panel beside the canvas (`&facets=1`, Shift+F, U11 section 2.3). */
  let facetsOpen = $state(page.url.searchParams.get('facets') === '1');
  /** The graph toolbar's View menu: literals and rdf:type edges. */
  let viewOpen = $state(false);
  /** The set the facets count: `class:` and a class IRI, or `drawn`; empty for the default. */
  let facetSetKey = $state('');

  // The overview grouped by the GraphRAG communities (`&overview=communities`), opened
  // down to a community (`&community=`), with the report of the community it shows in the
  // side panel and the entities of a chosen finding marked on the canvas (U06 with C22).
  let overviewMode = $state<'classes' | 'communities'>(
    page.url.searchParams.get('overview') === 'communities' ? 'communities' : 'classes',
  );
  /** The community the page was opened at, for the first dataset only. */
  let overviewFocus = $state(page.url.searchParams.get('community'));
  /** The reports belong to main: a page on another branch draws no communities. */
  const overviewBranch = $derived(branchParam(page.url.searchParams.get('branch')));
  const resourceTarget = $derived(ds ? onBranch(ds, overviewBranch) : null);
  let lastDs: string | null = null;
  // Set the URL family before any branch-target effect can launch a read.
  $effect(() => {
    const urlDs = page.url.searchParams.get('ds');
    untrack(() => {
      if (urlDs && urlDs !== app.current && app.datasets.some((d) => d.name === urlDs)) {
        lastDs = urlDs;
        app.setDataset(urlDs);
        void loadStarters(urlDs);
      } else if (urlDs && urlDs !== app.current && !app.datasetsLoaded) {
        app.setDataset(urlDs);
        lastDs = urlDs;
      }
    });
  });

  let previousResourceTarget: string | null | undefined = undefined;
  $effect(() => {
    const target = resourceTarget;
    if (previousResourceTarget === undefined) {
      previousResourceTarget = target;
      return;
    }
    if (previousResourceTarget === target) return;
    previousResourceTarget = target;
    untrack(() => {
      graphRuns.claim('graph');
      histRuns.claim('history');
      graphReading = false;
      graphPlaying = false;
      histFor = '';
      hist = null;
      histError = null;
      pathsOpen = false;
      pathFrom = null;
      pathTo = null;
      clearPaths();
      resetGraph();
      focusId = null;
      selectedId = null;
      editable = {};
      creatable = {};
      editing = false;
      const iri = page.url.searchParams.get('iri');
      if (iri) void focusOn(iri, undefined, true);
    });
  });

  let community = $state<OverviewCommunity | null>(null);
  let communityDetail = $state<gr.Community | null>(null);
  let communityError = $state<unknown>(null);
  let communityLoading = $state(false);
  let finding = $state<number | null>(null);
  let communitySeq = 0;
  const marked = $derived.by(() => {
    const f = finding != null ? communityDetail?.report?.findings[finding] : undefined;
    return f ? gr.findingEntities(f) : null;
  });

  async function showCommunity(c: OverviewCommunity | null) {
    const seq = ++communitySeq;
    community = c;
    finding = null;
    communityError = null;
    if (communityDetail?.iri !== c?.iri) communityDetail = null;
    if (!c || !ds) return;
    communityLoading = true;
    try {
      const d = await gr.community(ds, c.iri);
      if (seq === communitySeq) communityDetail = d;
    } catch (e) {
      if (seq === communitySeq) communityError = e;
    } finally {
      if (seq === communitySeq) communityLoading = false;
    }
  }
  let nodes = $state<Record<string, ENode>>({});
  let edges = $state<Record<string, EEdge>>({});
  let focusId = $state<string | null>(null);
  let selectedId = $state<string | null>(null);
  let showLiterals = $state(false);
  let hideTypes = $state(false);
  let expanding = $state<Record<string, number>>({});
  let detailCache = $state<Record<string, ex.Details | { error: string }>>({});
  // Bumped whenever the graph is reset (refocus, clear, dataset switch): responses to
  // requests started for an older graph are discarded instead of mutating the new one.
  const graphGen = new Generation();
  /** Aborts the expansions of the graph that was reset. */
  let expandCtl = new AbortController();

  /** The nodes the person expanded, in order, which a re-read at another commit replays. */
  let expandLog: string[] = [];
  /** Every resource the graph has drawn since it was focused, so that a replay can find it again. */
  let seenTerms = new Map<string, Term>();
  /**
   * The members the person drew from each group node, by group id, with what the group
   * stood for, so that a replay draws them again at a commit where the group exists.
   */
  type DrawnGroup = {
    owner: string;
    ownerTerm: Term;
    predicate: string;
    incoming: boolean;
    members: Map<string, grp.Member>;
  };
  let drawLog = new Map<string, DrawnGroup>();

  function resetGraph() {
    graphGen.bump();
    expandCtl.abort();
    expandCtl = new AbortController();
    nodes = {};
    edges = {};
    expanding = {};
    detailCache = {};
    expandLog = [];
    seenTerms = new Map();
    drawLog = new Map();
    changes = null;
    selection.clear();
    lensPick = null;
    // the nodes of a hidden group belong to the graph that is gone
    if (fstate.hiddenGroups.size) fstate = { ...fstate, hiddenGroups: new Map() };
    vShown = new Set();
    graphEpoch++;
  }
  let graphView: GraphView | undefined = $state();
  let graphToolbar: HTMLDivElement | undefined = $state();
  let gtoolsWidth = $state(0);

  // --- the validation overlay (docs/specs/U07-data-quality-and-schema-diagram.md §11) ---
  /**
   * The person's last choice of the validation lens, on or off, or `null` for the default:
   * on when the dataset has a write-time guard (U07 §11.2).
   */
  let vChoice = $state<boolean | null>(
    loadPref<boolean | null>('sparkles.explore.validation', null),
  );
  let vLive = $state(false);
  /** The lens's source, which the lens menu's picker sets and the overlay validates with. */
  let vSource = $state<'guard' | 'graph' | 'editor'>('guard');
  let vGraphIri = $state('');
  let vProbe = $state<GuardProbe | null>(null);
  let vProbeFor = $state('');
  let vHasEditor = $state(false);
  let vBadges = $state(new Map<string, Badge>());
  let vMarked = $state<{ nodes: string[]; edges: string[] } | null>(null);
  let vPlaying = $state(false);
  /** The elements Show triples drew, which stay drawn while the paths are drawn alone. */
  let vShown = $state(new Set<string>());
  let overlay: ValidationOverlay | undefined = $state();
  /** Show triples: draw the result's focus node, path edge and value, and mark them. */
  function showResultTriples(r: VResult) {
    const sid = termKey(r.focus);
    // a node the path finder drew joins the graph, so that its triples can be drawn
    const drawnNode = gNodes.find((n) => n.id === sid && n.change !== 'removed');
    if (!nodes[sid] && drawnNode) ensureNode(r.focus, pathLabels.get(sid));
    const out = { nodes: nodes[sid] ? [sid] : [], edges: [] as string[] };
    const shown = () => {
      // paths alone hide the explored graph, but not the triples of a result
      vShown = new Set([...vShown, ...out.nodes, ...out.edges]);
      return out;
    };
    if (!r.path || !r.value || !nodes[sid]) return shown();
    const lit = r.value.type === 'literal';
    if (lit) showLiterals = true;
    const oid = lit ? `lit:${sid}|${r.path}|${termKey(r.value)}` : termKey(r.value);
    ensureNode(r.value, undefined, oid);
    const [s, o] = r.inverse ? [oid, sid] : [sid, oid];
    addEdge(s, r.path, o, lit);
    out.nodes.push(oid);
    out.edges.push(`${s}|${r.path}|${o}`);
    return shown();
  }
  // --- Why is this inferred? (docs/specs/U10-canvas-lenses.md §2.4) -----------------
  /** The inferred triple the Why? panel explains, or null when it is closed. */
  let whyFor = $state<WhyTriple | null>(null);
  /** The premises Show on graph drew, highlighted on the canvas. */
  let whyMarked = $state<{ nodes: string[]; edges: string[] } | null>(null);
  /** Open Why? for a drawn edge that the inferred-edge lens marks; false for another edge. */
  function openWhyEdge(id: string): boolean {
    const e = gEdges.find((x) => x.id === id);
    if (!e?.triple || !lenses.encoding.inferred.has(id)) return false;
    const [s, p, o] = e.triple;
    whyMarked = null;
    whyFor = { s, p, o };
    return true;
  }
  function closeWhy() {
    whyFor = null;
    whyMarked = null;
  }
  /** Show on graph: draw the premises and highlight them with the explained edge. */
  function showWhy(triples: WhyTriple[]) {
    const out = { nodes: new Set<string>(), edges: new Set<string>() };
    const root = whyFor;
    for (const t of root ? [root, ...triples] : triples) {
      if (t.s.type === 'literal' || t.p.type !== 'uri') continue;
      const sid = termKey(t.s);
      ensureNode(t.s);
      const lit = t.o.type === 'literal';
      if (lit) showLiterals = true;
      if (t.p.value === RDF_TYPE) hideTypes = false;
      const oid = lit ? `lit:${sid}|${t.p.value}|${termKey(t.o)}` : termKey(t.o);
      ensureNode(t.o, undefined, oid);
      addEdge(sid, t.p.value, oid, lit);
      out.nodes.add(sid);
      out.nodes.add(oid);
      out.edges.add(`${sid}|${t.p.value}|${oid}`);
    }
    whyMarked = { nodes: [...out.nodes], edges: [...out.edges] };
  }
  /** The edge id of a value of the selected resource, as the canvas draws it. */
  function valueEdge(sid: string, p: string, o: Term, incoming = false): string {
    if (incoming) return `${termKey(o)}|${p}|${sid}`;
    const oid = o.type === 'literal' ? `lit:${sid}|${p}|${termKey(o)}` : termKey(o);
    return `${sid}|${p}|${oid}`;
  }
  // the reads of a view that goes away stop with it
  onDestroy(() => {
    selection.clear();
    lenses.destroy();
    expandCtl.abort();
    for (const r of [graphRuns, detailRuns, schemaRuns, profileRuns, histRuns]) r.abortAll();
  });

  // --- time: a resource's history strip (docs/specs/U01-time-slider.md) ----------------

  const urlAt = page.url.searchParams.get('at');
  const urlFrom = page.url.searchParams.get('from');
  const urlAgainst = page.url.searchParams.get('against');
  // --- the diff graph (docs/specs/U01-time-slider.md, Phase 4) -------------------------

  type DiffView = { to: string; from: string; branch: string; group: boolean };
  const diffOfUrl = (u: URL): DiffView | null => {
    const to = u.searchParams.get('diff');
    if (!to) return null;
    return {
      to,
      from: u.searchParams.get('diffFrom') ?? '',
      branch: u.searchParams.get('diffBranch') ?? '',
      group: u.searchParams.get('group') === 'class',
    };
  };
  /** The diff drawn as a graph, in place of the resource graph and the overview. */
  let diffView = $state<DiffView | null>(diffOfUrl(page.url));
  /** Bumped when the address names another diff, so that the view reads it again. */
  let diffKey = $state(0);
  $effect(() => {
    const d = diffOfUrl(page.url);
    untrack(() => {
      const same =
        (!d && !diffView) ||
        (d &&
          diffView &&
          d.to === diffView.to &&
          d.from === diffView.from &&
          d.branch === diffView.branch &&
          d.group === diffView.group);
      if (same) return;
      // a move inside the diff graph keeps it; another address reads the new diff
      diffView = d;
      diffKey++;
      if (d) {
        tab = 'graph';
        // the diff graph takes the canvas, so the path finder closes with its overlay
        closePaths();
      }
    });
  });
  function openDiff(to: string, from: string) {
    closePaths();
    diffView = { to, from, branch: overviewBranch ?? '', group: false };
    diffKey++;
    syncUrl(focusId && nodes[focusId] ? nodes[focusId].iri : null);
  }
  function exitDiff() {
    diffView = null;
    syncUrl(focusId && nodes[focusId] ? nodes[focusId].iri : null);
  }

  /** The History strip of the selected resource. */
  let stripOpen = $state(
    !!(urlAt || urlFrom || urlAgainst) && urlTab !== 'schema' && urlTab !== 'search',
  );
  /** The state the resource's properties are read at (`at=`): empty for the head. */
  let exAt = $state(urlAt ?? '');
  /** Compare mode: the earlier state, and what changed since it. */
  let exFrom = $state(urlFrom ?? '');
  let exCompare = $state(!!(urlFrom || urlAgainst));
  /** Compare with the head of another branch instead of an earlier commit. */
  let exAgainst = $state(urlAgainst ?? '');
  /** The branches of the dataset, to compare the graph and the schema across. */
  let branchNames = $state<string[]>([]);
  $effect(() => {
    const name = ds;
    const selected = overviewBranch ?? 'main';
    branchNames = [];
    if (!name) return;
    const ctl = new AbortController();
    api
      .branches(name, ctl.signal)
      .then((l) => {
        if (ds === name && selected === (overviewBranch ?? 'main'))
          branchNames = l.branches.map((b) => b.name).filter((n) => n !== selected);
      })
      .catch(() => {});
    return () => ctl.abort();
  });
  let strip: TimeSlider | undefined = $state();
  /** The slider over the whole graph, in the graph toolbar. It shares `exAt` and `exFrom`. */
  let graphTimeOpen = $state(
    !!(urlAt || urlFrom || urlAgainst) && urlTab !== 'schema' && urlTab !== 'search',
  );
  let graphSlider: TimeSlider | undefined = $state();
  /** Either slider is open, so the properties are read at the state they name. */
  const timeActive = $derived(stripOpen || graphTimeOpen);
  /**
   * The canvas shows a past state, where the route's tree of the latest inferences may not
   * hold, so the panel says so and the premises it drew are no longer marked.
   */
  const whyAtPast = $derived(timeActive && exAt !== '');
  $effect(() => {
    if (whyAtPast) untrack(() => (whyMarked = null));
  });
  type EntityHistory = {
    iri: string;
    changes: EntityChange[];
    commits: api.Commit[];
    authors: Map<number, string>;
    head: number;
    readable: Range[];
    truncated: boolean;
    note: string;
  };
  let hist = $state<EntityHistory | null>(null);
  let histError = $state<string | null>(null);
  let histFor = '';
  const histRuns = new LatestRun();

  /** The commits that changed a resource, as subject or object, from the change log. */
  async function loadHistory(name: string, iri: string) {
    const owns = histRuns.claim('history');
    const signal = histRuns.signal('history');
    if (histFor && histFor !== iri) {
      exAt = '';
      exFrom = '';
      exCompare = false;
    }
    histFor = iri;
    hist = null;
    histError = null;
    try {
      const [subj, obj, head] = await Promise.all([
        api.historyChanges(onBranch(name, overviewBranch), {
          subject: iri,
          order: 'asc',
          limit: 5000,
          signal,
        }),
        api.historyChanges(onBranch(name, overviewBranch), {
          object: iri,
          order: 'asc',
          limit: 5000,
          signal,
        }),
        api.commits(onBranch(name, overviewBranch), { limit: 1, signal }),
      ]);
      if (!owns()) return;
      const changes = entityChanges(subj.changes, obj.changes);
      const kinds = new Map([...subj.changes, ...obj.changes].map((c) => [c.commit, c.kind]));
      hist = {
        iri,
        changes,
        commits: entityCommits(changes, kinds),
        authors: authorsOf(changes),
        head: head.head,
        readable: head.reconstructable ?? [{ from: 0, to: head.head }],
        truncated: subj.truncated || obj.truncated,
        note: [unrecordedNote(subj), unrecordedNote(obj)].filter(Boolean)[0] ?? '',
      };
    } catch (e) {
      if (owns() && !isAbort(e)) {
        histError = api.errorMessage(e);
        strip?.fail();
      }
    }
  }

  $effect(() => {
    const name = ds;
    const iri = selected?.term.type === 'uri' ? selected.iri : null;
    if (!stripOpen || !name || !iri) return;
    untrack(() => void loadHistory(name, iri));
  });

  /** What changed between the two handles, with who changed it and why. */
  const histNet = $derived.by(() => {
    if (!hist || !stripOpen || !exCompare) return null;
    const to = selectorSeq(exAt, hist.head);
    const from = selectorSeq(exFrom, hist.head);
    return to == null || from == null ? null : netChanges(hist.changes, from, to);
  });

  /** The IRI of a predicate the change log gives in N-Triples. */
  function predIri(nt: string): string {
    const t = parseCompact(nt, {});
    return t.type === 'uri' ? t.value : nt;
  }

  /** Reset the shared state unless the other slider still uses it. */
  function resetTime() {
    if (stripOpen || graphTimeOpen || schemaTimeOpen) return;
    exAt = '';
    exFrom = '';
    exCompare = false;
    exAgainst = '';
  }

  function closeStrip() {
    stripOpen = false;
    resetTime();
  }

  function openGraphTime() {
    graphTimeOpen = true;
  }

  function closeGraphTime() {
    graphTimeOpen = false;
    vPlaying = false;
    resetTime();
  }

  // the properties are read again when a slider moves
  $effect(() => {
    void exAt;
    void timeActive;
    const id = selectedId;
    if (id) untrack(() => void loadDetails(id));
  });

  // --- editing forms (U02) ---------------------------------------------------------
  /** Whether a shape describes a resource and the caller may save its form, by IRI. */
  let editable = $state<Record<string, boolean>>({});
  /** The properties panel shows the form of the selected resource. */
  let editing = $state(false);
  $effect(() => {
    // only the selection, the dataset and the slider: the graph changes as it grows
    const id = selectedId;
    const name = ds;
    const past = timeActive;
    const n = untrack(() => (id ? nodes[id] : null));
    untrack(() => (editing = false));
    if (!n || n.term.type !== 'uri' || !name || past) return;
    const iri = n.iri;
    if (untrack(() => editable[`${name}|${iri}`]) !== undefined) return;
    const c = new AbortController();
    getForm(onBranch(name, overviewBranch), iri, { signal: c.signal }).then(
      (m) => (editable[`${name}|${iri}`] = m.editable && m.fields.some((f) => f.editable)),
      () => {},
    );
    return () => c.abort();
  });

  /** Whether a shape would describe a new instance of a class the caller may save, by class. */
  let creatable = $state<Record<string, boolean>>({});
  $effect(() => {
    const c = selectedClass;
    const name = ds;
    if (!c || !name) return;
    if (untrack(() => creatable[`${name}|${c}`]) !== undefined) return;
    const ctl = new AbortController();
    getForm(name, '', { class: c, signal: ctl.signal }).then(
      (m) => (creatable[`${name}|${c}`] = m.editable && m.fields.some((f) => f.editable)),
      () => {},
    );
    return () => ctl.abort();
  });

  /** After a save: read the resource's properties again. */
  function formSaved(id: string) {
    const keep = Object.fromEntries(
      Object.entries(detailCache).filter(([k]) => !k.startsWith(`${id}|`)),
    );
    detailCache = keep;
    void loadDetails(id);
  }

  // --- graph model -----------------------------------------------------------

  /**
   * Comparing two commits: what the later state added to the drawn graph, and the ghosts of
   * what it removed (nodes and edges only the earlier state had).
   */
  let changes = $state<{
    addedNodes: Set<string>;
    addedEdges: Set<string>;
    ghostNodes: Record<string, ENode>;
    ghostEdges: Record<string, EEdge>;
  } | null>(null);

  // The graph the page holds is `allNodes` and `allEdges`. The filters and the budget of
  // visible nodes (ROADMAP item 725) choose `gNodes` and `gEdges` from them, and those are
  // what the canvases, the lenses and the lists draw.
  const allNodes = $derived.by((): GNode[] => {
    const toG = (n: ENode, change?: 'added' | 'removed'): GNode =>
      n.group
        ? {
            id: n.id,
            label: n.label ?? '',
            kind: 'group',
            title: `${n.group.total.toLocaleString('en-US')} resources through ${n.group.predicate}`,
            focus: false,
            expanded: false,
            via: n.group.predicate,
          }
        : {
            id: n.id,
            label: labelFor(n.term, n.label),
            kind: n.term.type === 'uri' ? 'iri' : n.term.type,
            title: n.term.type === 'uri' ? n.term.value : undefined,
            focus: n.id === focusId,
            expanded: n.expanded,
            change,
            term: n.term,
          };
    const real = Object.values(nodes)
      .filter((n) => showLiterals || !n.lit)
      .map((n) => toG(n, changes?.addedNodes.has(n.id) ? 'added' : undefined));
    const ghosts = Object.values(changes?.ghostNodes ?? {})
      .filter((n) => showLiterals || !n.lit)
      .map((n) => toG(n, 'removed'));
    if (!pathOverlay) return [...real, ...ghosts];
    // the path finder's overlay: on the drawn nodes, plus the nodes it adds
    const on = new Map(pathOverlay.nodes.map((n) => [n.key, n]));
    const drawn = pathsAlone ? real.filter((n) => vShown.has(n.id)) : [...real, ...ghosts];
    const have = new Set(drawn.map((n) => n.id));
    const marked = drawn.map((n) => (on.has(n.id) ? { ...n, paths: on.get(n.id)!.paths } : n));
    const extra = pathOverlay.nodes
      .filter((n) => !have.has(n.key))
      .map((n): GNode => ({
        id: n.key,
        label: pathLabels.get(n.key) ?? labelFor(n.term),
        kind: n.term.type === 'uri' ? 'iri' : n.term.type,
        title: n.term.type === 'uri' ? n.term.value : undefined,
        paths: n.paths,
        order: n.order,
        near: pathNear.get(n.key),
        // the lenses read the path's nodes as they read the drawn ones
        term: n.term,
      }));
    return [...marked, ...extra];
  });
  const allEdges = $derived.by((): GEdge[] => {
    // the type edges stay in the held edges whether or not they are drawn, since the class
    // facet and the class filter read a node's classes from them
    const keep = (e: EEdge) => showLiterals || !e.lit;
    const tripleOf = (e: EEdge): EdgeTriple | undefined => {
      if (nodes[e.source]?.group || nodes[e.target]?.group) return undefined;
      const s = nodes[e.source]?.term;
      const o = nodes[e.target]?.term;
      return s && o && e.title ? [s, { type: 'uri', value: e.title }, o] : undefined;
    };
    const real = Object.values(edges)
      .filter(keep)
      .map((e) => ({
        ...e,
        label: e.title ? shortLabel(e.title, prefixes) : e.label,
        triple: tripleOf(e),
        ...(changes?.addedEdges.has(e.id) ? { change: 'added' as const } : {}),
      }));
    const ghosts = Object.values(changes?.ghostEdges ?? {})
      .filter(keep)
      .map((e) => ({
        ...e,
        label: e.title ? shortLabel(e.title, prefixes) : e.label,
        change: 'removed' as const,
      }));
    if (!pathOverlay) return [...real, ...ghosts];
    const on = new Map(pathOverlay.edges.map((e) => [e.id, e]));
    const drawn = pathsAlone ? real.filter((e) => vShown.has(e.id)) : [...real, ...ghosts];
    const have = new Set(drawn.map((e) => e.id));
    const marked = drawn.map((e) => (on.has(e.id) ? { ...e, paths: on.get(e.id)!.paths } : e));
    const extra = pathOverlay.edges
      .filter((e) => !have.has(e.id))
      .map((e): GEdge => ({
        id: e.id,
        source: termKey(e.s),
        target: termKey(e.o),
        label: shortLabel(e.p, prefixes),
        title: e.p,
        isType: e.p === RDF_TYPE,
        paths: e.paths,
        order: e.order,
        change: pathChange.get(e.id),
        triple: [e.s, { type: 'uri', value: e.p }, e.o],
      }));
    return [...marked, ...extra];
  });
  const nodeCount = $derived(Object.keys(nodes).length);

  // --- what is drawn of the held graph (ROADMAP item 725) -----------------------------------

  /** The nodes the filters always draw: the selection, the focus and the path finder's. */
  const keepIds = $derived.by(
    () =>
      new Set<string>([
        ...(selectedId ? [selectedId] : []),
        ...(focusId ? [focusId] : []),
        ...(pathOverlay?.nodes.map((n) => n.key) ?? []),
      ]),
  );
  let fstate = $state<FilterState>(freshFilters(VISIBLE_BUDGET));
  /** Without WebGL the canvas keeps to what it can draw. */
  let webglOff = $state<string | null>(
    typeof document !== 'undefined' && !hasWebGL2() ? 'This browser has no WebGL 2.' : null,
  );
  const filters = $derived<Filters>({
    predicates: fstate.predicates,
    classes: fstate.classes,
    minDegree: fstate.minDegree,
    hideLeaves: fstate.hideLeaves,
    hiddenIds: new Set([...fstate.hiddenGroups.values()].flatMap((g) => g.ids)),
    budget: webglOff ? Math.min(fstate.budget, WEBGL_FROM) : fstate.budget,
    // the type edges give nodes their classes whether or not they are drawn
    hideTypes,
  });
  const drawn = $derived(applyFilters(allNodes, allEdges, filters, keepIds));
  const gNodes = $derived(drawn.nodes);
  const gEdges = $derived(drawn.edges);
  /** The visible node count at which the page draws with WebGL, `localStorage` for tests. */
  const webglFrom = loadPref<number>('sparkles.explore.webglFrom', WEBGL_FROM);
  let useWebgl = $state(false);
  $effect(() => {
    const n = gNodes.length;
    const on = untrack(() => useWebgl);
    const want = !webglOff && wantWebgl(n, on, webglFrom);
    if (want !== on) useWebgl = want;
  });
  const predicateFacets = $derived(predicateFacet(allEdges));
  const classFacets = $derived(classFacet(allNodes, allEdges));
  const maxDegree = $derived.by(() => {
    const d = new Map<string, number>();
    for (const e of allEdges) {
      d.set(e.source, (d.get(e.source) ?? 0) + 1);
      d.set(e.target, (d.get(e.target) ?? 0) + 1);
    }
    return Math.max(0, ...d.values());
  });
  let filtersOpen = $state(page.url.searchParams.get('filters') === '1');
  const filtering = $derived(
    fstate.predicates.size > 0 ||
      fstate.classes.size > 0 ||
      fstate.minDegree > 0 ||
      fstate.hideLeaves ||
      fstate.hiddenGroups.size > 0,
  );

  // --- the validation overlay's drawn nodes and list column ---
  // the nodes the canvas draws, path finder nodes included and ghosts left out
  const vItems = $derived(
    gNodes.flatMap((n) =>
      n.term && n.change !== 'removed' ? [{ id: n.id, term: n.term, label: n.label }] : [],
    ),
  );

  // --- the path finder (docs/specs/U09-path-finder.md) ----------------------------------

  /** The most nodes a path overlay adds to the graph. */
  const MAX_PATH_NODES = 400;
  let pathsOpen = $state(false);
  let pathFrom = $state<Picked | null>(null);
  let pathTo = $state<Picked | null>(null);
  let pathHighlight = $state<number | null>(null);
  let pathOverlay = $state<ReturnType<typeof pf.overlay> | null>(null);
  let pathLabels = $state(new Map<string, string>());
  /** Each new node's neighbours on its paths, between which it is placed. */
  let pathNear = $state(new Map<string, string[]>());
  /** The paths are drawn without the graph, which could not take them. */
  let pathsAlone = $state(false);
  /** The path edges that a comparison marks, by the overlay's edge id. */
  let pathChange = $state(new Map<string, 'added' | 'removed'>());
  // comparing: a path edge that is not drawn is marked as the drawn ones are. Every path
  // edge is checked, whether or not the graph draws it now, since the graph can change
  // under the paths. An edge is added when it holds at the later state and not at the
  // earlier one, and removed the other way round. Both are read, because the paths may come
  // from a third state, and an edge
  // the check cannot read, such as one with a blank node, is found at neither and stays
  // unmarked.
  $effect(() => {
    const o = pathOverlay;
    const name = ds;
    const comparing = graphTimeOpen && exCompare && !!(exFrom || exAgainst);
    const against = exAgainst;
    const from = exFrom;
    const later = exAt;
    if (!o || !name || !comparing || !o.edges.length) {
      pathChange = new Map();
      return;
    }
    const c = new AbortController();
    const triples = o.edges.map((e): [Term, Term, Term] => [e.s, { type: 'uri', value: e.p }, e.o]);
    Promise.all([
      ex.presentEdges(
        against ? `${name}@${against}` : onBranch(name, overviewBranch),
        triples,
        against ? undefined : from,
        c.signal,
      ),
      ex.presentEdges(onBranch(name, overviewBranch), triples, later || undefined, c.signal),
    ])
      .then(([before, after]) => {
        if (c.signal.aborted) return;
        const marks = new Map<string, 'added' | 'removed'>();
        o.edges.forEach((e, i) => {
          const id = edgeIdOf(triples[i]);
          if (after.has(id) && !before.has(id)) marks.set(e.id, 'added');
          else if (before.has(id) && !after.has(id)) marks.set(e.id, 'removed');
        });
        pathChange = marks;
      })
      .catch(() => {
        // the marks are a nicety: the paths stand without them
        if (!c.signal.aborted) pathChange = new Map();
      });
    return () => c.abort();
  });
  let pathPanel: PathPanel | undefined = $state();
  /** The node selected before the current one: with both, Paths fills From and To. */
  let prevSelectedId: string | null = null;
  let menu = $state<{ id: string; x: number; y: number } | null>(null);

  const pickedOf = (id: string): Picked | null => {
    const n = nodes[id];
    return n && n.term.type === 'uri' ? { term: n.term, label: labelFor(n.term, n.label) } : null;
  };

  function openPaths(opts: { from?: string; to?: string } = {}) {
    if (opts.from || opts.to) {
      if (opts.from) pathFrom = pickedOf(opts.from) ?? pathFrom;
      if (opts.to) pathTo = pickedOf(opts.to) ?? pathTo;
    } else if (!pathsOpen && selectedId) {
      // two nodes chosen one after the other fill both ends
      const cur = pickedOf(selectedId);
      const prev =
        prevSelectedId && prevSelectedId !== selectedId ? pickedOf(prevSelectedId) : null;
      if (cur && prev) {
        pathFrom = prev;
        pathTo = cur;
      } else if (cur && !pathFrom) pathFrom = cur;
    }
    tab = 'graph';
    // the paths are drawn on the graph, not on the map or the diff graph
    if (diffView) exitDiff();
    if (canvasMode === 'map') setCanvasMode('graph');
    // and on the canvas, not the list
    listView = false;
    pathsOpen = true;
    menu = null;
    queueMicrotask(() => pathPanel?.focus());
  }

  function closePaths() {
    pathsOpen = false;
    clearPaths();
  }

  function clearPaths() {
    pathOverlay = null;
    pathHighlight = null;
    pathsAlone = false;
  }

  function showPaths(r: { paths: pf.FoundPath[]; labels: Map<string, string> } | null) {
    if (!r) return clearPaths();
    const o = pf.overlay(r.paths);
    const labels = new Map<string, string>();
    for (const n of o.nodes)
      if (n.term.type === 'uri' && r.labels.has(n.term.value))
        labels.set(n.key, r.labels.get(n.term.value)!);
    const near = new Map<string, string[]>();
    for (const p of r.paths)
      p.nodes.forEach((t, i) => {
        const k = termKey(t);
        const around = [p.nodes[i - 1], p.nodes[i + 1]].filter(Boolean).map((x) => termKey(x!));
        near.set(k, [...new Set([...(near.get(k) ?? []), ...around])]);
      });
    const fresh = o.nodes.filter((n) => !nodes[n.key]).length;
    pathsAlone = nodeCount + Math.min(fresh, MAX_PATH_NODES) > HELD_LIMIT;
    if (pathsAlone)
      toasts.push(
        'info',
        'Showing paths only',
        `The graph already has ${nodeCount} nodes, so the paths are shown on their own.`,
      );
    if (o.nodes.length > MAX_PATH_NODES) {
      const keep = new Set(o.nodes.slice(0, MAX_PATH_NODES).map((n) => n.key));
      o.nodes = o.nodes.filter((n) => keep.has(n.key));
      o.edges = o.edges.filter((e) => keep.has(termKey(e.s)) && keep.has(termKey(e.o)));
    }
    pathLabels = labels;
    pathNear = near;
    pathHighlight = null;
    pathOverlay = o;
  }

  /** The paths become ordinary elements of the graph, and the panel closes. */
  function addPathsToGraph() {
    const o = pathOverlay;
    if (!o) return;
    if (pathsAlone) resetGraph();
    for (const n of o.nodes) ensureNode(n.term, pathLabels.get(n.key), n.key);
    for (const e of o.edges) addEdge(termKey(e.s), e.p, termKey(e.o), e.o.type === 'literal');
    if (o.nodes.some((n) => n.term.type === 'literal')) showLiterals = true;
    if (!focusId && o.nodes[0]) focusId = o.nodes[0].key;
    pathsOpen = false;
    clearPaths();
  }

  function fitPath(p: pf.FoundPath) {
    view.fitTo(p.nodes.map((t) => termKey(t)));
  }

  // --- the WebGL view past about 1,000 visible nodes (ROADMAP item 725) ----------------------

  let largeView: LargeGraphView | undefined = $state();
  /** Whether the WebGL view's layout has settled, after which new nodes are placed in place. */
  let largeSettled = $state(false);
  $effect(() => {
    void graphEpoch;
    void useWebgl;
    largeSettled = false;
  });
  function graphColors(): Record<string, string> {
    const css = typeof document === 'undefined' ? null : getComputedStyle(document.documentElement);
    const v = (n: string, d: string) => css?.getPropertyValue(n).trim() || d;
    return {
      iri: v('--iri', '#2459c7'),
      literal: v('--literal', '#0b7d69'),
      bnode: v('--bnode', '#7d45c9'),
      triple: v('--text-2', '#525b6d'),
      group: v('--spark', '#e5a117'),
      added: v('--added', '#1a7f37'),
      removed: v('--removed', '#c62828'),
    };
  }
  const largeNodes = $derived.by((): LNode[] => {
    if (!useWebgl) return [];
    const colors = graphColors();
    const radius = 100 + Math.sqrt(gNodes.length) * 4;
    // a picture of thousands of nodes has room for small dots only: at the default size the
    // members of a hub run together into one disc that looks like a single large node
    const dense = gNodes.length > WEBGL_FROM;
    const dot = dense ? 1.8 : 3.5;
    return gNodes.map((g) => {
      const p = seed(g.id, undefined, radius);
      return {
        key: g.id,
        label: g.label,
        size:
          g.kind === 'group'
            ? 6.5
            : g.kind === 'literal'
              ? dot * 0.7
              : g.focus
                ? 5.5
                : g.expanded && dense
                  ? 4
                  : dot,
        color:
          g.change === 'added'
            ? colors.added
            : g.change === 'removed'
              ? colors.removed
              : (colors[g.kind] ?? colors.iri),
        x: p.x,
        y: p.y,
        ...(g.paths?.length ? { paths: g.paths } : {}),
      };
    });
  });
  const largeEdges = $derived.by((): LEdge[] => {
    if (!useWebgl) return [];
    const colors = graphColors();
    return gEdges.map((e) => ({
      key: e.id,
      source: e.source,
      target: e.target,
      size: 0.6,
      ...(e.change === 'added'
        ? { color: colors.added }
        : e.change === 'removed'
          ? { color: colors.removed }
          : {}),
      ...(e.paths?.length ? { paths: e.paths } : {}),
    }));
  });
  /**
   * The edges of each pair of nodes, by the key of the first, which is the one the WebGL view
   * draws for the pair: it draws one line for the predicates between two nodes.
   */
  const largePairs = $derived(useWebgl ? parallelEdges(gEdges) : new Map<string, GEdge[]>());
  /** A click on a line of the WebGL view: the explanation of an inferred edge, else the predicates it stands for. */
  function openLargeEdge(id: string) {
    const group = largePairs.get(id);
    if (!group) return void openWhyEdge(id);
    const inferred = group.find((e) => lenses.encoding.inferred.has(e.id));
    if (inferred && openWhyEdge(inferred.id)) return;
    toasts.push(
      'info',
      `${group.length} predicates join these nodes`,
      group.map((e) => e.label).join(', '),
    );
  }
  /** The nodes with their neighbours loaded and the group nodes, which the overlay rings. */
  const expandedIds = $derived(new Set(gNodes.filter((n) => n.expanded).map((n) => n.id)));
  const groupIds = $derived(new Set(gNodes.filter((n) => n.kind === 'group').map((n) => n.id)));
  /** An active selection narrows the lens's marks rather than giving way to them. */
  const largeMarked = $derived.by((): string[] | null => {
    const extra = whyFor ? (whyMarked?.nodes ?? []) : [];
    const base = !selection.active
      ? lensMarked
      : !lensMarked
        ? [...selection.ids]
        : lensMarked.filter((k) => selection.has(k));
    return base || extra.length ? [...(base ?? []), ...extra] : null;
  });
  const view = {
    fit: () => (useWebgl ? largeView?.fit() : graphView?.fit()),
    relayout: () => (useWebgl ? largeView?.relayout() : graphView?.relayout()),
    center: (id: string) => (useWebgl ? largeView?.reveal(id) : graphView?.center(id)),
    fitTo: (ids: string[]) => (useWebgl ? largeView?.fitTo(ids) : graphView?.fitTo(ids)),
  };

  // A handle for the performance measurements (ROADMAP item 725), installed only when
  // `localStorage['sparkles.explore.perfHook']` is '1'. Its members read the page's state
  // when they are called, so it costs nothing while nobody measures.
  onMount(() => {
    if (loadRaw('sparkles.explore.perfHook') !== '1') return;
    (window as unknown as { __perfHook: unknown }).__perfHook = {
      nodeCount: () => Object.keys(nodes).length,
      drawnCount: () => gNodes.length,
      nodeIds: () => Object.keys(nodes),
      expand: (id: string) => expand(id),
      renderer: () => (useWebgl ? 'webgl' : 'cytoscape'),
      viewport: () => (useWebgl ? largeView?.viewport() : graphView?.viewport()),
      screenOf: (id: string) => (useWebgl ? largeView?.screenOf(id) : null),
    };
    return () => {
      delete (window as unknown as { __perfHook?: unknown }).__perfHook;
    };
  });

  // --- lenses (docs/specs/U10-canvas-lenses.md) ------------------------------------------

  const lenses = new Lenses();
  /** The drawn graph as a list, with a column per active lens. */
  let listView = $state(false);
  /** The legend entry whose nodes are selected. */
  let lensPick = $state<string | null>(null);
  /** Its nodes while the entry exists. */
  const lensMarked = $derived(entryIds(lenses.results, lensPick));
  /** Bumped whenever the graph is drawn from new reads, so that lens values are read again. */
  let graphEpoch = $state(0);
  const dsInfo = $derived(app.datasets.find((d) => d.name === ds));
  /** Materialized inferences or RDFS on read: the inferred-edge lens exists (§2.7). */
  const hasReasoning = $derived(!!dsInfo?.reasoning || !!dsInfo?.rdfs);
  $effect(() => {
    const name = resourceTarget;
    if (!name) return;
    lenses.setInput({
      ds: name,
      at: graphTimeOpen && exAt ? exAt : undefined,
      epoch: graphEpoch,
      nodes: gNodes.map((n) => ({ id: n.id, term: n.term })),
      // the ghosts of a comparison are not in the state the lenses read
      edges: gEdges
        .filter((e) => e.change !== 'removed')
        .map((e) => ({ id: e.id, source: e.source, target: e.target, triple: e.triple })),
      reasoning: hasReasoning,
      label: (id) => {
        const n = nodes[id];
        return n ? labelFor(n.term, n.label) : id;
      },
      short: (iri) => shortLabel(iri, prefixes),
      // the left handle of a comparison in time, not one against another branch (§4.3)
      compareAt: graphTimeOpen && exCompare && exFrom && !exAgainst ? exFrom : undefined,
      playing: graphPlaying,
    });
  });
  // the validation lens of the badge channel (U07 §11.2): the overlay validates, and the
  // lens carries its badges to the legend, the list column and the descriptions
  const vOn = $derived(lenses.choice.badge.lens === 'validation');
  lenses.onchoose = (ch, c, remember) => {
    if (ch !== 'badge') return;
    vChoice = c.lens === 'validation';
    savePref('sparkles.explore.validation', vChoice);
    // Shift+3 turns this lens on again, also after the page opens again
    savePref('sparkles.explore.badgeLens', remember.lens);
    if (!vChoice) vMarked = null;
  };
  {
    const last = loadPref<string | null>('sparkles.explore.badgeLens', null);
    if (last === 'validation' || last === 'inferred') lenses.setLast('badge', { lens: last });
  }
  $effect(() => {
    const p = vProbe;
    const name = ds;
    const forDs = vProbeFor;
    const choice = vChoice;
    untrack(() => {
      // until the overlay has probed this dataset, the lens is not offered
      if (!p || !name || forDs !== name) {
        lenses.setValidation(false, null);
        return;
      }
      const ok = p.state !== 'restricted' && p.state !== 'unavailable';
      lenses.setValidation(ok, ok ? null : `${p.message}.`);
      if (!ok) return;
      const guard = p.state === 'shacl' || p.state === 'shex';
      // the person's own choice comes back where the lens exists again
      if (choice === true) lenses.restore('badge', { lens: 'validation' });
      else if (choice === null && guard) lenses.suggest('badge', { lens: 'validation' });
    });
  });
  $effect(() => {
    lenses.setValidationBadges(vBadges);
  });
  $effect(() => {
    if (!vOn) vMarked = null;
  });

  /** The list view's rows: the drawn nodes, ordered by the size lens when one is on. */
  const listRows = $derived.by((): ListRow[] => {
    const size = lenses.results.size;
    const degree = new Map<string, number>();
    for (const e of gEdges) {
      degree.set(e.source, (degree.get(e.source) ?? 0) + 1);
      degree.set(e.target, (degree.get(e.target) ?? 0) + 1);
    }
    const look = lenses.encoding.nodes;
    return gNodes.map((n) => {
      const v = size?.values.get(n.id);
      const c = look.get(n.id)?.colour;
      return {
        key: n.id,
        label: n.label,
        kind: n.change === 'removed' ? 'removed' : n.kind === 'iri' ? 'resource' : n.kind,
        size: typeof v === 'number' ? v : -Infinity,
        degree: degree.get(n.id) ?? 0,
        color:
          c == null
            ? `var(--${n.kind === 'iri' ? 'iri' : n.kind})`
            : c === OTHER
              ? 'var(--lens-other)'
              : `var(--lens-${c + 1})`,
        openable: n.kind === 'iri',
        marked: !!lensMarked?.includes(n.id),
        title: [n.title ?? n.label, lensDescribe(n.id, lenses.results)].filter(Boolean).join('\n'),
      };
    });
  });

  function labelFor(t: Term, label?: string) {
    if (label) return label;
    if (t.type === 'uri') return shortLabel(t.value, prefixes);
    if (t.type === 'bnode') return `_:${t.value.slice(0, 8)}`;
    if (t.type === 'literal') {
      const s = literalText(t);
      return s.length > 48 ? s.slice(0, 45) + '…' : s;
    }
    return '<<triple>>';
  }

  function ensureNode(term: Term, label?: string, id = termKey(term)): ENode {
    const existing = nodes[id];
    if (existing) {
      if (label && existing.label !== label && term.type === 'uri') existing.label = label;
      return existing;
    }
    const n: ENode = {
      id,
      term,
      iri: term.type === 'uri' ? term.value : '',
      label,
      expanded: false,
      lit: term.type === 'literal',
    };
    nodes[id] = n;
    seenTerms.set(id, term);
    return nodes[id];
  }

  function addEdge(s: string, p: string, o: string, lit: boolean) {
    const id = `${s}|${p}|${o}`;
    if (edges[id]) return;
    edges[id] = {
      id,
      source: s,
      target: o,
      label: shortLabel(p, prefixes),
      title: p,
      isType: p === RDF_TYPE,
      lit,
    };
  }

  /** Draw the neighbours of a node as `neighbours` answered them. */
  function populate(n: ENode, r: Awaited<ReturnType<typeof ex.neighbours>>) {
    const { out, inc, label } = r;
    const id = n.id;
    if (label) n.label = label;
    for (const nb of out) {
      const lit = nb.term.type === 'literal';
      // Label literals are already the node's caption.
      if (lit && ex.LABEL_IRIS.includes(nb.predicate) && nb.term.value === n.label) continue;
      const oid = lit ? `lit:${id}|${nb.predicate}|${termKey(nb.term)}` : termKey(nb.term);
      ensureNode(nb.term, nb.label, oid);
      addEdge(id, nb.predicate, oid, lit);
    }
    for (const nb of inc) {
      const sid = termKey(nb.term);
      ensureNode(nb.term, nb.label, sid);
      addEdge(sid, nb.predicate, id, false);
    }
  }

  /**
   * Read a resource's neighbourhood as Explore draws it: the predicates with few edges as
   * neighbours, and each predicate with many as one group node (ROADMAP item 725).
   */
  async function readNeighbourhood(target: string, iri: string, at?: string, signal?: AbortSignal) {
    const plan = grp.planFanout(await grp.fanout(target, iri, at, signal));
    // a label is read from the resource's own edges, so those predicates are never grouped
    const keep = (f: grp.Fan) => !f.incoming && ex.LABEL_IRIS.includes(f.predicate);
    const groups = plan.groups.filter((f) => !keep(f));
    const skip = (incoming: boolean) =>
      groups.filter((f) => f.incoming === incoming).map((f) => f.predicate);
    const r = await ex.neighbours(target, iri, at, signal, { out: skip(false), inc: skip(true) });
    return { r, groups };
  }

  /** Draw a group node for the edges of one predicate at `owner`. */
  function addGroup(owner: ENode, f: grp.Fan) {
    const id = grp.groupId(owner.id, f.predicate, f.incoming);
    const caption = grp.groupCaption(f.count, shortLabel(f.predicate, prefixes), f.incoming);
    const existing = nodes[id];
    if (existing) {
      existing.label = caption;
      if (existing.group) existing.group = { ...existing.group, total: f.count };
      return;
    }
    nodes[id] = {
      id,
      term: { type: 'bnode', value: id },
      iri: '',
      label: caption,
      expanded: false,
      lit: false,
      group: {
        owner: owner.id,
        ownerIri: owner.iri,
        predicate: f.predicate,
        incoming: f.incoming,
        total: f.count,
      },
    };
    const eid = `${f.incoming ? id : owner.id}|${f.predicate}|${f.incoming ? owner.id : id}`;
    edges[eid] = {
      id: eid,
      source: f.incoming ? id : owner.id,
      target: f.incoming ? owner.id : id,
      label: shortLabel(f.predicate, prefixes),
      title: f.predicate,
      isType: f.predicate === RDF_TYPE,
      lit: false,
    };
  }

  /** Draw members the group panel chose, linked to the group's owner through its predicate. */
  function drawMembers(gid: string, members: grp.Member[]) {
    const g = nodes[gid]?.group;
    if (!g) return;
    const ownerTerm = nodes[g.owner]?.term;
    const logged = drawLog.get(gid) ?? {
      owner: g.owner,
      ownerTerm: ownerTerm as Term,
      predicate: g.predicate,
      incoming: g.incoming,
      members: new Map<string, grp.Member>(),
    };
    if (ownerTerm) drawLog.set(gid, logged);
    for (const m of members) {
      const lit = m.term.type === 'literal';
      const oid = lit ? `lit:${g.owner}|${g.predicate}|${termKey(m.term)}` : termKey(m.term);
      logged.members.set(oid, m);
      ensureNode(m.term, m.label, oid);
      if (lit) showLiterals = true;
      if (g.incoming) addEdge(oid, g.predicate, g.owner, false);
      else addEdge(g.owner, g.predicate, oid, lit);
    }
  }

  async function expand(id: string) {
    const n = nodes[id];
    if (!n || n.group || n.term.type !== 'uri' || !ds || expanding[id]) return;
    const gen = graphGen.current;
    const target = resourceTarget;
    const stale = () => target !== resourceTarget || !graphGen.isCurrent(gen) || nodes[id] !== n;
    if (nodeCount > HELD_LIMIT) {
      toasts.push(
        'error',
        'Graph is getting large',
        `Over ${HELD_LIMIT.toLocaleString('en-US')} nodes. Remove some or refocus before expanding.`,
      );
      return;
    }
    expanding[id] = gen;
    const signal = expandCtl.signal;
    try {
      const { r, groups } = await readNeighbourhood(
        target!,
        n.iri,
        timeActive && graphTimeOpen ? exAt || undefined : undefined,
        signal,
      );
      if (stale()) return;
      populate(n, r);
      for (const f of groups) addGroup(n, f);
      n.expanded = true;
      if (!expandLog.includes(id)) expandLog.push(id);
      // comparing: the new neighbours are marked against the earlier state too
      if (graphTimeOpen && exCompare) await markChanges(() => !stale(), signal);
      if (r.out.length >= 100 || r.inc.length >= 100) {
        toasts.push(
          'info',
          'Showing the first 100 edges each way',
          'Open the resource in Query to see everything.',
        );
      }
    } catch (e) {
      if (!stale() && !isAbort(e)) toasts.error(`Could not expand ${labelFor(n.term, n.label)}`, e);
    } finally {
      if (expanding[id] === gen) delete expanding[id];
    }
  }

  // --- the graph at a commit (docs/specs/U01-time-slider.md) ---------------------------------

  const graphRuns = new LatestRun();
  const detailRuns = new LatestRun();
  let graphReading = $state(false);
  let graphReadMs = $state(0);
  let graphNote = $state('');
  let graphAnnounce = $state('');
  /** What the graph slider last drew, so that only a move re-reads the graph. */
  let drawnKey = 'head';

  const timeKey = () =>
    graphTimeOpen && (exAt || exCompare)
      ? `${exAt}|${exCompare ? exFrom : ''}|${exCompare ? exAgainst : ''}`
      : 'head';

  /** Mark what changed between the two handles, from the diff of the drawn resources. */
  async function markChanges(owns: () => boolean, signal?: AbortSignal) {
    const name = ds;
    if (!name || !graphTimeOpen || !exCompare || !(exFrom || exAgainst)) {
      changes = null;
      return;
    }
    const iris = Object.values(nodes)
      .filter((n) => n.term.type === 'uri')
      .map((n) => n.iri)
      .slice(0, 1000);
    const d = await api.diff(onBranch(name, overviewBranch), {
      // Across branches, compare the selected branch with the other's head.
      ...(exAgainst
        ? { fromBranch: exAgainst, toBranch: overviewBranch ?? 'main' }
        : { from: exFrom }),
      to: exAt || 'head',
      terms: iris,
      quads: true,
      limit: 5000,
      signal,
    });
    if (!owns()) return;
    const drawnEdges = Object.values(edges);
    let plan = planDiff(d.quads ?? [], new Set(Object.keys(nodes)), drawnEdges, focusId);
    // the diff lists the changes of every graph and the drawn graph shows one view of them, so
    // a marked edge is checked against the state it was added to, and a ghost against the
    // state it was removed from, read as the graph is read
    if (plan.addedTriples.size || plan.ghostEdges.length) {
      const leftDs = exAgainst ? `${name}@${exAgainst}` : onBranch(name, overviewBranch);
      const leftAt = exAgainst ? undefined : exFrom;
      const [before, after] = await Promise.all([
        ex.presentEdges(leftDs, [...plan.addedTriples.values()], leftAt, signal),
        ex.presentEdges(
          onBranch(name, overviewBranch),
          plan.ghostEdges.map((g) => g.triple),
          exAt || undefined,
          signal,
        ),
      ]);
      if (!owns()) return;
      plan = confirmPlan(plan, drawnEdges, focusId, before, after);
    }
    const listed = d.quads?.length ?? 0;
    const partial = listed < d.added + d.removed;
    const ghostNodes: Record<string, ENode> = {};
    for (const g of plan.ghostNodes) {
      const lit = g.term.type === 'literal';
      ghostNodes[g.id] = {
        id: g.id,
        term: g.term,
        iri: g.term.type === 'uri' ? g.term.value : '',
        expanded: false,
        lit,
      };
    }
    const ghostEdges: Record<string, EEdge> = {};
    for (const g of plan.ghostEdges) {
      ghostEdges[g.id] = {
        id: g.id,
        source: g.source,
        target: g.target,
        label: shortLabel(g.predicate, prefixes),
        title: g.predicate,
        isType: g.predicate === RDF_TYPE,
        lit: g.lit,
      };
    }
    changes = { addedNodes: plan.addedNodes, addedEdges: plan.addedEdges, ghostNodes, ghostEdges };
    const added = plan.addedEdges.size;
    const removed = plan.ghostEdges.length;
    graphAnnounce = `Commit ${d.to.commit.seq}: ${added} ${added === 1 ? 'edge' : 'edges'} added, ${removed} removed`;
    graphNote = [
      plan.truncated ? `Only ${plan.ghostEdges.length} removed edges are drawn.` : '',
      partial ? `Only ${fmtInt(listed)} of ${fmtInt(d.added + d.removed)} changes are marked.` : '',
    ]
      .filter(Boolean)
      .join(' ');
  }

  /** The members drawn from group nodes whose edge holds at the state `at` names, by group. */
  async function drawnMembersAt(
    target: string,
    at: string | undefined,
    signal?: AbortSignal,
  ): Promise<{ gid: string; owner: string; members: grp.Member[] }[]> {
    const wanted: { gid: string; m: grp.Member; triple: EdgeTriple }[] = [];
    for (const [gid, g] of drawLog) {
      const p: Term = { type: 'uri', value: g.predicate };
      for (const m of g.members.values())
        wanted.push({
          gid,
          m,
          triple: g.incoming ? [m.term, p, g.ownerTerm] : [g.ownerTerm, p, m.term],
        });
    }
    if (!wanted.length) return [];
    const found = await ex.presentEdges(
      target,
      wanted.map((w) => w.triple),
      at,
      signal,
    );
    const by = new Map<string, { gid: string; owner: string; members: grp.Member[] }>();
    for (const w of wanted) {
      // a blank node or a triple term cannot be checked, so its member stays
      const checkable = w.triple.every((t) => t.type === 'uri' || t.type === 'literal');
      if (checkable && !found.has(edgeIdOf(w.triple))) continue;
      const d = by.get(w.gid) ?? { gid: w.gid, owner: drawLog.get(w.gid)!.owner, members: [] };
      d.members.push(w.m);
      by.set(w.gid, d);
    }
    return [...by.values()];
  }

  /**
   * Read the graph again at the state the graph slider names: the focus and the nodes the
   * person expanded, in the order they were expanded, are read at that commit, and an
   * expanded node that did not exist then is skipped.
   */
  async function rereadGraph() {
    const name = ds;
    const target = resourceTarget;
    const root = focusId;
    if (!name || !target || !root) return;
    const requestOwns = graphRuns.claim('graph');
    const owns = () => requestOwns() && target === resourceTarget;
    const signal = graphRuns.signal('graph');
    const at = graphTimeOpen && exAt ? exAt : undefined;
    graphReading = true;
    graphNote = '';
    const t0 = performance.now();
    try {
      const present = new Set([root]);
      const fetched = new Map<string, Awaited<ReturnType<typeof readNeighbourhood>>>();
      // the members drawn from a group stay when their edge holds at that commit
      const drawnAt = await drawnMembersAt(target!, at, signal);
      if (!owns()) return;
      for (const id of [...expandLog]) {
        const term = seenTerms.get(id);
        if (!present.has(id) || !term || term.type !== 'uri') continue;
        const nh = await readNeighbourhood(target!, term.value, at, signal);
        if (!owns()) return;
        fetched.set(id, nh);
        for (const nb of [...nh.r.out, ...nh.r.inc]) {
          if (nb.term.type === 'literal') continue;
          present.add(termKey(nb.term));
        }
        for (const d of drawnAt) {
          if (d.owner !== id) continue;
          for (const m of d.members) if (m.term.type !== 'literal') present.add(termKey(m.term));
        }
      }
      graphEpoch++;
      // the graph is rebuilt in one step, so that what stays keeps its place
      const rootTerm = seenTerms.get(root);
      if (!rootTerm) return;
      const keepLabels = new Map(Object.values(nodes).map((n) => [n.id, n.label]));
      nodes = {};
      edges = {};
      const rn = ensureNode(rootTerm, keepLabels.get(root), root);
      const replayed = new Set<string>();
      // an expansion makes its groups, whose drawn members can be expanded in turn, so the
      // passes go on until one expands nothing more
      for (let more = true; more;) {
        more = false;
        for (const id of expandLog) {
          const nh = fetched.get(id);
          const n = id === root ? rn : nodes[id];
          if (!nh || !n || replayed.has(id)) continue;
          replayed.add(id);
          more = true;
          populate(n, nh.r);
          for (const f of nh.groups) addGroup(n, f);
          n.expanded = true;
        }
        for (const d of drawnAt) drawMembers(d.gid, d.members);
      }
      for (const [id, n] of Object.entries(nodes)) {
        const kept = keepLabels.get(id);
        if (kept && n.term.type === 'uri' && !n.label) n.label = kept;
      }
      await markChanges(owns, signal);
      if (!owns()) return;
      if (!graphTimeOpen) changes = null;
      graphSlider?.accept();
      graphReadMs = performance.now() - t0;
      if (!(graphTimeOpen && exCompare)) {
        graphAnnounce = at
          ? `Commit ${at.replace(/^commit:/, '')}: ${Object.keys(nodes).length} nodes`
          : 'Latest commit';
      }
    } catch (e) {
      if (!owns() || isAbort(e)) return;
      const gone = at ? timeFailure(e) : null;
      if (gone) {
        toasts.push('error', gone.title, gone.detail);
        graphSlider?.reject({ seq: gone.seq, readable: gone.readable });
        strip?.reject({ seq: gone.seq, readable: gone.readable });
        drawnKey = timeKey();
        return;
      }
      graphSlider?.fail();
      strip?.fail();
      toasts.error('Could not load the graph at that commit', e);
    } finally {
      if (owns()) graphReading = false;
    }
  }

  // --- the time-lapse on the graph (U01 Phase 4) ---------------------------------------

  /** Play re-reads the graph at each commit, or applies each step's changes to it. */
  let graphPlayMode = $state<'reread' | 'lapse'>('reread');
  let graphPlaying = $state(false);

  /**
   * One time-lapse step from `fromAt` to `toAt`: the changes that touch drawn resources (the
   * `term` filter, up to 1,000 of them) are applied to the graph. An added triple draws its
   * edge and a new end next to its neighbour, so the picture grows as the data did. A removed
   * one leaves a ghost for this step, and a node left without edges goes with it.
   */
  async function lapseGraph(fromAt: string, toAt: string) {
    const name = ds;
    const target = resourceTarget;
    if (!name || !target || !focusId) return;
    const requestOwns = graphRuns.claim('graph');
    const owns = () => requestOwns() && target === resourceTarget;
    const signal = graphRuns.signal('graph');
    graphReading = true;
    graphNote = '';
    const t0 = performance.now();
    try {
      const iris = Object.values(nodes)
        .filter((n) => n.term.type === 'uri')
        .map((n) => n.iri)
        .slice(0, 1000);
      const d = await api.diff(target!, {
        from: fromAt,
        to: toAt,
        terms: iris,
        quads: true,
        limit: STEP_QUADS,
        signal,
      });
      if (!owns()) return;
      const quads = d.quads ?? [];
      // a step that cannot be applied whole is read instead, so no change is lost for the
      // steps after it: one that listed only some of its changes, or one past the node cap
      const fresh = new Set<string>();
      for (const q of quads) {
        if (q.op !== '+') continue;
        const { sid, oid } = quadIds(q);
        if (!nodes[sid] && !nodes[oid]) continue;
        for (const id of [sid, oid]) if (!nodes[id]) fresh.add(id);
      }
      if (
        quads.length < d.added + d.removed ||
        Object.keys(nodes).length + fresh.size > HELD_LIMIT
      ) {
        graphReading = false;
        await rereadGraph();
        if (graphTimeOpen)
          graphNote = `Commit ${d.to.commit.seq} changed too much to apply in one step, so the graph was reloaded.`;
        return;
      }
      // the diff lists every graph's changes and the drawn graph reads one view of them, so
      // each change is checked against the state it leads to, read as the graph is read
      const candidates = new Map<string, EdgeTriple>();
      for (const q of quads) {
        const { s, o, p, sid, oid, edge } = quadIds(q);
        if (q.op === '+' ? !nodes[sid] && !nodes[oid] : !edges[edge]) continue;
        candidates.set(edge, [s, { type: 'uri', value: p }, o]);
      }
      // a blank node or a quoted triple cannot be checked by value: read the graph instead
      if (
        [...candidates.values()].some((t) =>
          t.some((x) => x.type !== 'uri' && x.type !== 'literal'),
        )
      ) {
        graphReading = false;
        await rereadGraph();
        return;
      }
      // the state the diff resolved to, which a write after it does not move
      const holds = candidates.size
        ? await ex.presentEdges(
            target!,
            [...candidates.values()],
            `commit:${d.to.commit.seq}`,
            signal,
          )
        : new Set<string>();
      if (!owns()) return;
      const addedNodes = new Set<string>();
      const addedEdges = new Set<string>();
      const ghostEdges: Record<string, EEdge> = {};
      const ghostNodes: Record<string, ENode> = {};
      let added = 0;
      let removed = 0;
      for (const q of quads) {
        const { s, o, p, sid, oid, edge, lit } = quadIds(q);
        if (q.op === '+') {
          if (!nodes[sid] && !nodes[oid]) continue;
          // added in a graph the drawn view does not read
          if (!holds.has(edge)) continue;
          if (!nodes[sid]) {
            ensureNode(s, undefined, sid);
            addedNodes.add(sid);
          }
          if (!nodes[oid]) {
            ensureNode(o, undefined, oid);
            addedNodes.add(oid);
          }
          if (!edges[edge]) {
            addEdge(sid, p, oid, lit);
            addedEdges.add(edge);
            added++;
          }
          continue;
        }
        const e = edges[edge];
        // a triple removed from one graph that the drawn view still reads stays
        if (e && !holds.has(edge)) {
          ghostEdges[edge] = e;
          delete edges[edge];
          removed++;
        }
      }
      const incident = new Set(Object.values(edges).flatMap((e) => [e.source, e.target]));
      for (const [id, n] of Object.entries(nodes)) {
        if (id === focusId || incident.has(id)) continue;
        ghostNodes[id] = n;
        delete nodes[id];
      }
      changes = { addedNodes, addedEdges, ghostNodes, ghostEdges };
      graphSlider?.accept();
      graphReadMs = performance.now() - t0;
      const a = seqOf(fromAt) ?? 0;
      graphAnnounce = stepAnnouncement({ from: a, to: d.to.commit.seq }, added, removed);
    } catch (e) {
      if (!owns() || isAbort(e)) return;
      const gone = timeFailure(e);
      if (gone) {
        toasts.push('error', gone.title, gone.detail);
        graphSlider?.reject({ seq: gone.seq, readable: gone.readable });
        drawnKey = timeKey();
        return;
      }
      graphSlider?.fail();
      toasts.error('Could not load the changes in that commit', e);
    } finally {
      if (owns()) graphReading = false;
    }
  }

  // the graph follows the graph slider: a time-lapse step applies its changes, any other
  // move reads the graph again
  $effect(() => {
    const key = timeKey();
    untrack(() => {
      if (key === drawnKey) return;
      const prev = drawnKey;
      drawnKey = key;
      if (!focusId) return;
      const before = prev === 'head' ? null : prev.split('|')[0];
      const a = seqOf(before);
      const b = seqOf(exAt);
      // the head is always after the state drawn
      const forward = a != null && (b != null ? b > a : exAt === '');
      if (graphPlayMode === 'lapse' && graphPlaying && !exCompare && forward)
        void lapseGraph(`commit:${a}`, b != null ? `commit:${b}` : 'head');
      else void rereadGraph();
    });
  });

  async function focusOn(iri: string, label?: string, fromUrl = false) {
    resetGraph();
    const n = ensureNode({ type: 'uri', value: iri }, label);
    focusId = n.id;
    selectedId = n.id;
    if (!fromUrl) {
      tab = 'graph';
      syncUrl(iri);
    }
    await expand(n.id);
    void loadDetails(n.id);
  }

  function syncUrl(iri: string | null) {
    const p = new URLSearchParams();
    if (ds) p.set('ds', ds);
    if (iri) p.set('iri', iri);
    if (tab !== 'graph') p.set('tab', tab);
    if (canvasMode === 'map' && tab === 'graph') p.set('map', '1');
    if (facetsOpen && tab === 'graph') p.set('facets', '1');
    if (filtersOpen && tab === 'graph') p.set('filters', '1');
    if (tab === 'search' && textQuery.trim()) p.set('q', textQuery.trim());
    if (iri && sideTab === 'memory') p.set('view', 'memory');
    if (!iri && tab === 'graph' && overviewMode === 'communities') {
      p.set('overview', 'communities');
      if (community) p.set('community', community.iri);
    }
    if (overviewBranch) p.set('branch', overviewBranch);
    if (diffView && tab === 'graph') {
      p.set('diff', diffView.to);
      if (diffView.from) p.set('diffFrom', diffView.from);
      if (diffView.branch) p.set('diffBranch', diffView.branch);
      if (diffView.group) p.set('group', 'class');
    }
    const timed =
      tab === 'schema'
        ? schemaTimeOpen
        : tab === 'graph' && (graphTimeOpen || (!!iri && stripOpen && sideTab === 'props'));
    if (timed) {
      if (exAt) p.set('at', exAt);
      if (exCompare && exFrom) p.set('from', exFrom);
      if (exCompare && exAgainst) p.set('against', exAgainst);
    }
    goto(`${resolve('/explore')}?${p}`, { replaceState: true, keepFocus: true, noScroll: true });
  }

  function select(id: string | null) {
    if (id && selectedId && id !== selectedId) prevSelectedId = selectedId;
    selectedId = id;
    menu = null;
    if (id) void loadDetails(id);
    // a click on the canvas is a selection the map shows too
    if (id) selection.set([id], 'the graph');
    else selection.clear();
  }

  function setCanvasMode(m: 'graph' | 'map') {
    if (canvasMode === m) return;
    canvasMode = m;
    syncUrl(focusId && nodes[focusId] ? nodes[focusId].iri : null);
  }

  /** The state the graph's map reads positions at, and the earlier one in compare mode. */
  const mapAt = $derived(timeActive && graphTimeOpen ? exAt : '');
  const mapFromAt = $derived(graphTimeOpen && exCompare && !exAgainst ? exFrom : '');

  /**
   * Draw the entities of the selection as the graph: the first one is the focus and each
   * is expanded, up to 25 of them. Larger sets are drawn as nodes to expand by hand.
   */
  async function openSelectionAsGraph(ids: string[]) {
    if (!ds) return;
    const terms = ids
      .filter((id) => id.startsWith('<') && id.endsWith('>'))
      .slice(0, VISIBLE_BUDGET)
      .map((id): Term => ({ type: 'uri', value: id.slice(1, -1) }));
    if (!terms.length) return;
    resetGraph();
    canvasMode = 'graph';
    const made = terms.map((t) => ensureNode(t));
    focusId = made[0].id;
    selectedId = made[0].id;
    syncUrl(made[0].iri);
    const many = made.length > 25;
    if (many)
      toasts.push(
        'info',
        `Drew ${made.length} entities`,
        'Double-click a node to load its neighbours.',
      );
    await Promise.all(many ? [expand(made[0].id)] : made.map((n) => expand(n.id)));
    void loadDetails(made[0].id);
  }

  async function loadDetails(id: string) {
    const n = nodes[id];
    const at = timeActive ? exAt : '';
    const key = `${id}|${at}`;
    if (!n || n.term.type !== 'uri' || !ds || detailCache[key]) return;
    const gen = graphGen.current;
    // a newer read of properties supersedes this one, which stops on the server too
    detailRuns.claim('entity');
    const signal = detailRuns.signal('entity');
    let d: ex.Details | { error: string };
    try {
      d = await ex.details(resourceTarget!, n.iri, at || undefined, signal);
      if (timeActive && at === exAt) strip?.accept();
    } catch (e) {
      if (isAbort(e)) return;
      // a state the server can no longer read: the handle goes back to the last one it read
      const gone = at ? timeFailure(e) : null;
      if (gone) {
        toasts.push('error', gone.title, gone.detail);
        strip?.reject({ seq: gone.seq, readable: gone.readable });
        graphSlider?.reject({ seq: gone.seq, readable: gone.readable });
        return;
      }
      if (at) {
        strip?.fail();
        graphSlider?.fail();
      }
      d = { error: api.errorMessage(e) };
    }
    if (graphGen.isCurrent(gen) && nodes[id] === n) detailCache[key] = d;
  }

  function removeNode(id: string) {
    const keep = Object.fromEntries(
      Object.entries(edges).filter(([, e]) => e.source !== id && e.target !== id),
    );
    edges = keep;
    const { [id]: _removed, ...rest } = nodes;
    void _removed;
    // Drop literal nodes and group nodes that belonged to the removed node.
    nodes = Object.fromEntries(
      Object.entries(rest).filter(([k, n]) => !k.startsWith(`lit:${id}|`) && n.group?.owner !== id),
    );
    // what the person removed is not drawn again by a replay
    for (const [gid, g] of drawLog) {
      if (g.owner === id) drawLog.delete(gid);
      else
        for (const oid of [...g.members.keys()])
          if (oid === id || oid.startsWith(`lit:${id}|`)) g.members.delete(oid);
    }
    if (selectedId === id) selectedId = null;
    if (focusId === id) focusId = null;
  }

  /** Add a value from the properties panel as a node linked to the selected resource. */
  function addLinked(from: string, predicate: string, term: Term, incoming = false) {
    const target = ensureNode(term);
    if (incoming) addEdge(target.id, predicate, from, false);
    else addEdge(from, predicate, target.id, false);
    select(target.id);
    queueMicrotask(() => view.center(target.id));
  }

  function clearGraph() {
    resetGraph();
    clearPaths();
    focusId = null;
    selectedId = null;
    syncUrl(null);
  }

  function describeInQuery(iri: string) {
    app.pendingQuery = { query: `DESCRIBE <${iri}>`, title: shortLabel(iri, prefixes) };
    goto(resolve('/query'));
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toasts.push('success', 'Copied IRI', undefined, 1400);
    } catch (e) {
      toasts.error('Could not copy', e);
    }
  }

  // --- selected resource panel ---------------------------------------------------

  const selected = $derived(selectedId ? nodes[selectedId] : null);
  const detail = $derived(
    selectedId ? detailCache[`${selectedId}|${timeActive ? exAt : ''}`] : undefined,
  );
  const grouped = $derived.by(() => {
    if (!detail || 'error' in detail) return [];
    const by: Record<string, Term[]> = {};
    for (const { p, o } of detail.props) (by[p] ??= []).push(o);
    const order = (p: string) =>
      p === RDF_TYPE ? 0 : ex.LABEL_IRIS.includes(p) ? 1 : p === RDFS_COMMENT ? 2 : 3;
    return Object.entries(by).sort((a, b) => order(a[0]) - order(b[0]) || a[0].localeCompare(b[0]));
  });
  const types = $derived(grouped.find(([p]) => p === RDF_TYPE)?.[1] ?? []);
  /** The types that make a resource a term with a page in Browse: a class, property or concept. */
  const TERM_TYPES = new Set([
    'http://www.w3.org/2002/07/owl#Class',
    'http://www.w3.org/2000/01/rdf-schema#Class',
    'http://www.w3.org/1999/02/22-rdf-syntax-ns#Property',
    'http://www.w3.org/2002/07/owl#ObjectProperty',
    'http://www.w3.org/2002/07/owl#DatatypeProperty',
    'http://www.w3.org/2002/07/owl#AnnotationProperty',
    'http://www.w3.org/2004/02/skos/core#Concept',
    'http://www.w3.org/2004/02/skos/core#ConceptScheme',
    'http://www.w3.org/2004/02/skos/core#Collection',
  ]);
  const isTerm = (ts: Term[]) => ts.some((t) => t.type === 'uri' && TERM_TYPES.has(t.value));

  // --- facets -------------------------------------------------------------------

  /** The drawn entities, as the term keys the selection and the facets use. */
  const drawnIds = $derived(gNodes.filter((n) => n.id.startsWith('<')).map((n) => n.id));
  /** The sets the facets can count: each class of the selected entity, and the drawn ones. */
  const facetSets = $derived.by(() => {
    const out: { key: string; label: string; within: FacetSetSpec; name: string }[] = [];
    for (const t of types)
      if (t.type === 'uri')
        out.push({
          key: `class:${t.value}`,
          label: `Instances of ${shortLabel(t.value, prefixes)}`,
          within: { class: t.value },
          name: `instances of ${shortLabel(t.value, prefixes)}`,
        });
    if (drawnIds.length)
      out.push({
        key: 'drawn',
        label: `The ${drawnIds.length} drawn entities`,
        within: { nodes: drawnIds.slice(0, 20_000).map((id) => id.slice(1, -1)) },
        name: 'drawn entities',
      });
    return out;
  });
  /** The chosen set: the one picked, else the class of the selected entity when it has
   * exactly one, else the drawn entities. */
  const facetSet = $derived.by(() => {
    const picked = facetSets.find((x) => x.key === facetSetKey);
    if (picked) return picked;
    const classes = facetSets.filter((x) => x.key.startsWith('class:'));
    return classes.length === 1 ? classes[0] : facetSets.find((x) => x.key === 'drawn');
  });
  // the drawn set's IRIs, kept stable while the same nodes are drawn so the panel does not
  // count again for an unrelated change
  let facetWithin = $state<FacetSetSpec | null>(null);
  $effect(() => {
    const w = facetsOpen ? (facetSet?.within ?? null) : null;
    if (JSON.stringify(w) !== JSON.stringify(untrack(() => facetWithin))) facetWithin = w;
  });

  // the filters and the facets share the panel beside the canvas
  function toggleFilters() {
    filtersOpen = !filtersOpen;
    if (filtersOpen && facetsOpen) {
      facetsOpen = false;
      if (selection.source === 'the facets') selection.clear();
    }
    syncUrl(selected?.term.type === 'uri' ? selected.iri : null);
  }
  function openFilters() {
    if (!filtersOpen) toggleFilters();
  }

  // the Communities section is offered where the dataset has GraphRAG communities
  $effect(() => {
    if (filtersOpen && nodeCount > 0) void lenses.loadAlgoOptions();
  });

  /** The categories of the colour lens, such as communities, which the filters can hide. */
  const COMMUNITY_LENSES = new Set(['wcc', 'labelPropagation', 'louvain', 'leiden', 'graphrag']);
  const communityGroups = $derived.by((): GroupEntry[] => {
    const r = lenses.results.colour;
    if (!r || !r.legend.some((e) => e.key.startsWith('cat-'))) return [];
    return r.legend.map((e) => ({
      key: `${r.lens}:${e.key}`,
      text: e.text,
      count: e.count,
      ids: e.ids,
    }));
  });
  const communityTitle = $derived.by(() => {
    const r = lenses.results.colour;
    if (!r || !communityGroups.length) return 'Communities';
    return COMMUNITY_LENSES.has(r.lens) ? 'Communities' : `Groups by ${r.name}`;
  });
  /** Colour by the GraphRAG communities of the memory store, whose legend then groups the graph. */
  function groupByCommunities() {
    const levels = lenses.algo.graphrag?.levels ?? [];
    lenses.set('colour', { lens: 'graphrag', ...(levels.length ? { level: levels.at(-1) } : {}) });
  }

  function toggleFacets() {
    facetsOpen = !facetsOpen;
    if (facetsOpen && filtersOpen) filtersOpen = false;
    if (!facetsOpen && selection.source === 'the facets') selection.clear();
    syncUrl(selected?.term.type === 'uri' ? selected.iri : null);
  }

  /** The drawn entities the brushes keep become the selection, which dims the rest. */
  function facetMatch(ids: Set<string> | null) {
    if (ids) selection.set(ids, 'the facets', { keepEmpty: true });
    else if (selection.source === 'the facets') selection.clear();
  }

  /** Use in query: the selected entities as the VALUES block of a new query, 1,000 at most. */
  function useSelection(ids: string[]) {
    const list = ids.slice(0, VALUES_CAP);
    if (ids.length > list.length)
      toasts.push(
        'info',
        `The query names the first ${VALUES_CAP.toLocaleString('en-US')} entities`,
        `${(ids.length - list.length).toLocaleString('en-US')} more were left out.`,
      );
    app.pendingQuery = { query: selectionQuery(list), title: 'Selection' };
    goto(resolve('/query'));
  }

  /** Explore's Apply to query: a query for the set under the brushes, in a new Query tab. */
  function applyFacets(brushes: Brush[]) {
    if (!facetWithin) return;
    const list = Object.entries(prefixes).map(([prefix, iri]) => ({ prefix, iri }));
    app.pendingQuery = { query: setQuery(facetWithin, brushes, list), title: 'Facets' };
    goto(resolve('/query'));
  }
  const comment = $derived(grouped.find(([p]) => p === RDFS_COMMENT)?.[1]);

  // --- search -------------------------------------------------------------------

  /** The IRI that a search text names: `<…>`, an absolute IRI or a known prefixed name. */
  function directIriOf(q: string): string | null {
    const m = /^<(.+)>$/.exec(q);
    if (m) return m[1];
    if (/^(https?|urn|mailto|file):/i.test(q)) return q;
    const pn = /^([A-Za-z][\w.-]*)?:(\S*)$/.exec(q);
    if (pn && prefixes[pn[1] ?? ''] != null) return prefixes[pn[1] ?? ''] + pn[2];
    return null;
  }

  // The resource finder lives in the top bar's launcher: while Explore is open its hits come
  // first, each focusing the resource in the graph, with "Open <IRI>" for a typed IRI. The
  // Ask drawer's own provider adds "Ask a question…" when the dataset answers questions.
  useProvider({
    id: 'resources',
    label: ({ ds: name }) => (name ? `Resources in ${name}` : 'Resources'),
    order: 1,
    modes: ['@'],
    recent: false,
    active: ({ ds: name }) => !!name,
    // the Open row needs the prefixes, which can arrive after the launcher asked
    watch: () => [Object.keys(prefixes).length],
    async search(raw, { ds: name, signal }) {
      const q = raw.trim();
      if (!name || q.length < 2) return [];
      const direct = directIriOf(q);
      const out: LauncherItem[] = [];
      if (direct)
        out.push({
          id: `!${direct}`,
          title: `Open ${displayIri(direct, prefixes)}`,
          icon: 'target',
          run: () => void focusOn(direct),
        });
      // a failed label search leaves the Open row, which needs no answer
      let hits: Awaited<ReturnType<typeof ex.search>> = [];
      try {
        hits = await ex.search(name, q, signal);
      } catch (e) {
        if (signal.aborted || (e instanceof DOMException && e.name === 'AbortError')) throw e;
      }
      for (const h of hits)
        if (h.iri !== direct)
          out.push({
            id: h.iri,
            title: h.label ?? shortLabel(h.iri, prefixes),
            subtitle: displayIri(h.iri, prefixes),
            icon: 'explore',
            run: () => void focusOn(h.iri, h.label),
          });
      return out;
    },
  });
  useHint(() =>
    ds && tab === 'graph'
      ? `Find a resource in ${ds} by label or IRI${canAsk ? ', or ask a question' : ''}`
      : null,
  );
  // `/` focuses the text-search tab's own field there, and opens the top bar elsewhere
  usePageSearch(() => {
    if (tab !== 'search') return false;
    const el = document.querySelector<HTMLInputElement>('.text-search-field');
    el?.focus();
    return !!el;
  });

  // --- starting points ------------------------------------------------------------

  /** Named graphs of the dataset (schema graph selector). */
  let namedGraphs = $state<string[]>([]);
  async function loadStarters(name: string) {
    try {
      const s = await api.datasetStats(name);
      namedGraphs = s.graphs
        .map((g) => g.name)
        .filter((g): g is string => !!g && g !== INFERRED_GRAPH);
    } catch {
      namedGraphs = [];
    }
  }

  async function sampleOf(cls: string) {
    if (!ds) return;
    try {
      const rows = await api.select(ds, `SELECT ?s WHERE { ?s a <${cls}> } LIMIT 1`);
      const s = rows[0]?.s;
      if (s?.type === 'uri') void focusOn(s.value);
      else toasts.push('info', 'No IRI instances of that class');
    } catch (e) {
      toasts.error('Could not load an instance', e);
    }
  }

  // --- schema --------------------------------------------------------------------

  let schema = $state<ex.Schema | null>(null);
  let schemaFor = $state<string | null>(null);
  let schemaErr = $state<string | null>(null);
  let schemaLoading = $state(false);
  let openClasses = $state(new Set<string>());
  let selectedClass = $state<string | null>(null);
  let classFilter = $state('');
  let propFilter = $state('');
  let schemaView = $state<'classes' | 'properties'>('classes');
  /** Graph whose triples are counted: `default`, `union` or a graph IRI. */
  let schemaGraph = $state('default');
  /** Count materialized inferences (only offered when the dataset has them). */
  let schemaInferences = $state(true);
  /** The "Draft shapes" dialog. */
  let draftOpen = $state(false);
  /** The "Compare" dialog (schema diffs). */
  let diffOpen = $state(false);
  /** The observed profile of the selected class. */
  let profile = $state<api.ClassProfile | null>(null);
  let profileFor = $state<string | null>(null);
  let profileErr = $state<string | null>(null);
  const profileRuns = new LatestRun();
  let showBuiltinProps = $state(false);
  const hasInferences = $derived(!!app.datasets.find((d) => d.name === ds)?.reasoning);
  // A newer load (reload, other graph, dataset switch) supersedes an older one.
  const schemaRuns = new LatestRun();
  /** The slider over the schema tab. It shares `exAt` and `exFrom` with the graph's. */
  let schemaTimeOpen = $state(!!(urlAt || urlFrom || urlAgainst) && urlTab === 'schema');
  let schemaSlider: TimeSlider | undefined = $state();
  /** Comparing two commits: the change of each count, and what came and went. */
  let schemaDelta = $state<SchemaChanges | null>(null);
  let schemaAnnounce = $state('');
  let schemaReadMs = $state(0);
  let schemaDrawnKey = 'head';
  const schemaKey = () =>
    schemaTimeOpen && (exAt || exCompare)
      ? `${exAt}|${exCompare ? exFrom : ''}|${exCompare ? exAgainst : ''}`
      : 'head';

  async function loadSchema(name: string) {
    const owns = schemaRuns.claim('schema');
    const signal = schemaRuns.signal('schema');
    schemaLoading = true;
    schemaErr = null;
    const at = schemaTimeOpen && exAt ? exAt : undefined;
    const from = schemaTimeOpen && exCompare && exFrom && !exAgainst ? exFrom : undefined;
    const other = schemaTimeOpen && exCompare && exAgainst ? exAgainst : undefined;
    const t0 = performance.now();
    schemaDrawnKey = schemaKey();
    try {
      const o = {
        graph: schemaGraph,
        reasoning: hasInferences ? schemaInferences : undefined,
        signal,
      };
      const [s, before] = await Promise.all([
        ex.loadSchema(name, { ...o, at }),
        from
          ? ex.loadSchema(name, { ...o, at: from })
          : other
            ? ex.loadSchema(`${name}@${other}`, o)
            : Promise.resolve(null),
      ]);
      if (!owns()) return;
      const first = schemaFor !== name;
      schema = s;
      schemaFor = name;
      schemaDelta = before ? schemaChanges(before, s) : null;
      schemaAnnounce = schemaDelta ? schemaAnnouncement(schemaDelta) : '';
      schemaReadMs = performance.now() - t0;
      if (schemaTimeOpen) schemaSlider?.accept();
      // Open the first two levels by default.
      if (first || !schemaTimeOpen) {
        const open = new Set<string>();
        for (const r of schema.roots) {
          open.add(r);
          for (const s of schema.classes.get(r)?.subs ?? []) open.add(s);
        }
        openClasses = open;
      }
    } catch (e) {
      if (!owns() || isAbort(e)) return;
      const gone = at || from ? timeFailure(e) : null;
      if (gone) {
        toasts.push('error', gone.title, gone.detail);
        schemaSlider?.reject({ seq: gone.seq, readable: gone.readable });
        schemaDrawnKey = schemaKey();
      } else {
        schemaSlider?.fail();
        schemaErr = api.errorMessage(e);
      }
    } finally {
      if (owns()) schemaLoading = false;
    }
  }

  // the schema follows its slider
  $effect(() => {
    const key = schemaKey();
    untrack(() => {
      if (key === schemaDrawnKey || !ds) return;
      void loadSchema(ds);
    });
  });

  function openSchemaTime(from?: string, to?: string) {
    if (from && to) {
      exFrom = from;
      exAt = to === 'head' ? '' : to;
      exAgainst = '';
      exCompare = true;
    }
    schemaTimeOpen = true;
    syncUrl(null);
  }

  function closeSchemaTime() {
    schemaTimeOpen = false;
    resetTime();
    syncUrl(null);
  }

  function setSchemaGraph(g: string) {
    schemaGraph = g;
    if (ds) void loadSchema(ds);
  }

  function toggleClass(iri: string) {
    const next = new Set(openClasses);
    if (next.has(iri)) next.delete(iri);
    else next.add(iri);
    openClasses = next;
  }

  function revealClass(iri: string) {
    if (!schema) return;
    // Open every ancestor so the class is visible in the tree.
    const next = new Set(openClasses);
    const walk = (c: string, seen: Set<string>) => {
      for (const s of schema!.classes.get(c)?.supers ?? [])
        if (!seen.has(s)) (seen.add(s), next.add(s), walk(s, seen));
    };
    walk(iri, new Set());
    openClasses = next;
    selectedClass = iri;
    schemaView = 'classes';
    classFilter = '';
  }

  const classMatches = $derived.by(() => {
    if (!schema || !classFilter.trim()) return null;
    const f = classFilter.trim().toLowerCase();
    return [...schema.classes.values()]
      .filter((c) => (c.label ?? '').toLowerCase().includes(f) || c.iri.toLowerCase().includes(f))
      .map((c) => c.iri)
      .slice(0, 300);
  });
  const propMatches = $derived.by(() => {
    if (!schema) return [];
    const f = propFilter.trim().toLowerCase();
    return schema.properties.filter(
      (p) =>
        (showBuiltinProps || !p.builtin) &&
        (!f || (p.label ?? '').toLowerCase().includes(f) || p.iri.toLowerCase().includes(f)),
    );
  });
  const builtinProps = $derived(schema ? schema.properties.filter((p) => p.builtin).length : 0);
  const cls = $derived(selectedClass && schema ? schema.classes.get(selectedClass) : undefined);
  const clsProps = $derived.by(() => {
    if (!cls || !schema) return { domain: [], range: [] };
    return {
      domain: schema.properties.filter((p) => p.domains.includes(cls.iri)),
      range: schema.properties.filter((p) => p.ranges.includes(cls.iri)),
    };
  });
  // the profile of the selected class, read again when the class or the selection changes
  $effect(() => {
    const iri = selectedClass;
    const name = ds;
    // the profile is of the state the schema shows
    const at = schemaTimeOpen && exAt ? exAt : undefined;
    const key = `${name} ${schemaGraph} ${schemaInferences} ${schemaFor} ${iri} ${at ?? ''}`;
    if (!iri || !name || !schema || profileFor === key) return;
    untrack(() => {
      const owns = profileRuns.claim('profile');
      const signal = profileRuns.signal('profile');
      profileFor = key;
      profile = null;
      profileErr = null;
      api
        .schemaProfiles(name, {
          graph: schemaGraph,
          reasoning: hasInferences ? schemaInferences : undefined,
          classes: [iri],
          at,
          signal,
        })
        .then((p) => {
          if (owns()) profile = p.classes.find((c) => c.class === iri) ?? null;
        })
        .catch((e) => {
          if (owns() && !isAbort(e)) profileErr = api.errorMessage(e);
        });
    });
  });

  const clsConstraints = $derived(
    cls && schema ? schema.constraints.filter((l) => l.class === cls.iri) : [],
  );
  const short = (iri: string) => shortLabel(iri, prefixes);
  const kindName = (k: string) => shortLabel(k, prefixes).replace(/^owl:|^rdf:/, '');

  function classLabel(iri: string) {
    return schema?.classes.get(iri)?.label ?? shortLabel(iri, prefixes);
  }

  // --- lifecycle / url sync -----------------------------------------------------

  $effect(() => {
    const name = ds;
    if (name === lastDs) return;
    const first = lastDs === null;
    lastDs = name;
    untrack(() => {
      if (!name) return;
      void loadStarters(name);
      if (!first) {
        // Dataset switched in the sidebar: start over.
        resetGraph();
        pathsOpen = false;
        pathFrom = null;
        pathTo = null;
        clearPaths();
        focusId = null;
        selectedId = null;
        schema = null;
        schemaFor = null;
        schemaGraph = 'default';
        selectedClass = null;
        // the report beside the overview belongs to the dataset left, and a pending
        // read of it is dropped by the new sequence number
        overviewFocus = null;
        void showCommunity(null);
        syncUrl(null);
      }
      if (tab === 'schema') void loadSchema(name);
    });
  });

  $effect(() => {
    if (tab === 'schema' && ds && schemaFor !== ds && !schemaLoading)
      untrack(() => loadSchema(ds!));
  });

  // Focus only after initial time controls have reconciled, preserving a past-state URL.
  $effect(() => {
    const iri = page.url.searchParams.get('iri');
    const urlTab = page.url.searchParams.get('tab');
    void page.url.searchParams.get('ds');
    untrack(() => {
      if (!iri) return;
      // a link to a resource, such as a top bar search hit, shows it in the graph, also
      // when it is the resource already focused; the page's own tabs keep `tab` in the URL
      if (!urlTab && tab !== 'graph') tab = 'graph';
      if (!focusId || nodes[focusId]?.iri !== iri) void focusOn(iri, undefined, true);
    });
  });

  // the first vector index of the dataset, for the link to its embedding map; a caller
  // whose access covers part of the dataset gets 403 here and sees no link
  let mapIndex = $state<string | null>(null);
  $effect(() => {
    const name = ds;
    mapIndex = null;
    if (!name) return;
    let live = true;
    api
      .vectorStatus(name)
      .then((s) => {
        if (live && name === ds) mapIndex = s.indexes[0]?.name ?? null;
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  });

  // `?selection=1` (Open as graph on another page, such as the embedding map) draws the
  // shared selection once the dataset is open
  let selectionOpened = false;
  $effect(() => {
    if (selectionOpened || !ds || page.url.searchParams.get('selection') !== '1') return;
    selectionOpened = true;
    untrack(() => void openSelectionAsGraph([...selection.ids]));
  });

  function setTab(t: Tab) {
    tab = t;
    syncUrl(focusId && nodes[focusId] ? nodes[focusId].iri : null);
  }

  // keep the text query in the URL (debounced: it changes with every keystroke)
  let qSync: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    if (tab !== 'search') return;
    void textQuery;
    clearTimeout(qSync);
    qSync = setTimeout(
      () => untrack(() => syncUrl(focusId && nodes[focusId] ? nodes[focusId].iri : null)),
      500,
    );
    return () => clearTimeout(qSync);
  });

  onMount(() => {
    const onDoc = (e: MouseEvent) => {
      if (viewOpen && !(e.target as HTMLElement).closest('.view-menu')) viewOpen = false;
    };
    // H opens and closes the History strip of the selected resource, Shift+H the one over the graph
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const target = e.target as HTMLElement;
      if (target?.closest?.('input, textarea, select, [contenteditable]')) return;
      // Shift+M shows the drawn graph on a map and back; paths are drawn on the graph only,
      // so the map waits until the path finder closes
      if (e.key === 'M' && e.shiftKey) {
        if (tab !== 'graph' || !nodeCount || pathsOpen || diffView) return;
        e.preventDefault();
        setCanvasMode(canvasMode === 'map' ? 'graph' : 'map');
        return;
      }
      // Shift+G opens and closes the filters
      if (e.key === 'G' && e.shiftKey) {
        if (tab !== 'graph' || (!nodeCount && !filtersOpen)) return;
        e.preventDefault();
        toggleFilters();
        return;
      }
      // Shift+F opens and closes the facet panel
      if (e.key === 'F' && e.shiftKey) {
        if (tab !== 'graph' || (!nodeCount && !facetsOpen)) return;
        e.preventDefault();
        toggleFacets();
        return;
      }
      // Shift+P opens the path finder; with it open, the graph takes its keys too
      if (e.key === 'P' && e.shiftKey && ds && tab !== 'schema') {
        e.preventDefault();
        openPaths();
        return;
      }
      if (pathsOpen && tab === 'graph' && !target?.closest?.('.paths')) {
        const count = pathOverlay ? new Set(pathOverlay.edges.flatMap((x) => x.paths)).size : 0;
        if (/^[1-9]$/.test(e.key) && Number(e.key) <= count) {
          e.preventDefault();
          pathHighlight = Number(e.key);
          return;
        }
        if ((e.key === 'a' || e.key === 'A') && pathOverlay) {
          e.preventDefault();
          pathHighlight = null;
          return;
        }
        if (e.key === 'Escape') {
          e.preventDefault();
          if (menu) menu = null;
          else if (pathHighlight != null) pathHighlight = null;
          else closePaths();
          return;
        }
      }
      // W opens Why? for the focused inferred edge of the resource's properties (§2.5)
      if ((e.key === 'w' || e.key === 'W') && !e.shiftKey && tab === 'graph') {
        const el = target?.closest?.('[data-why-edge]') as HTMLElement | null;
        if (el?.dataset.whyEdge && openWhyEdge(el.dataset.whyEdge)) e.preventDefault();
        return;
      }
      if (e.key !== 'h' && e.key !== 'H') return;
      if (tab !== 'graph') return;
      if (e.shiftKey) {
        if (!nodeCount) return;
        e.preventDefault();
        if (graphTimeOpen) closeGraphTime();
        else openGraphTime();
        return;
      }
      if (selected?.term.type !== 'uri' || sideTab !== 'props') return;
      e.preventDefault();
      if (stripOpen) closeStrip();
      else stripOpen = true;
    };
    document.addEventListener('mousedown', onDoc);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDoc);
      document.removeEventListener('keydown', onKey);
    };
  });
</script>

<svelte:head><title>Explore | Sparkles</title></svelte:head>

<h1 class="sr-only">Explore</h1>
<div class="page">
  <header class="bar">
    <div class="tabs" role="tablist">
      <button class="tab" role="tab" aria-selected={tab === 'graph'} onclick={() => setTab('graph')}
        ><Icon name="graph" size={14} /> Graph</button
      >
      <button
        class="tab"
        role="tab"
        aria-selected={tab === 'schema'}
        onclick={() => setTab('schema')}><Icon name="tree" size={14} /> Schema</button
      >
      <button
        class="tab"
        role="tab"
        aria-selected={tab === 'search'}
        onclick={() => setTab('search')}
        title="Full-text search (text:query)"><Icon name="filter" size={14} /> Text search</button
      >
    </div>
  </header>

  {#if !ds}
    <div class="empty">
      <Icon name="database" size={22} />
      <p>No dataset selected.</p>
      <a class="btn" href={resolve('/datasets')}>Go to Datasets</a>
    </div>
  {:else if tab === 'search'}
    {#key ds}
      <TextSearchView
        {ds}
        {prefixes}
        bind:query={textQuery}
        onopen={(iri, label) => focusOn(iri, label)}
      />
    {/key}
  {:else if tab === 'graph' && diffView}
    <div class="diff-host">
      {#key `${ds}|${diffKey}`}
        <DiffGraph
          ds={diffView.branch ? `${ds}@${diffView.branch}` : ds}
          from={diffView.from}
          to={diffView.to}
          group={diffView.group}
          {prefixes}
          onexit={exitDiff}
          onchange={(c) => {
            if (!diffView) return;
            diffView = { ...diffView, from: c.from, to: c.to, group: c.group };
            syncUrl(focusId && nodes[focusId] ? nodes[focusId].iri : null);
          }}
          onopen={(iri) => {
            diffView = null;
            void focusOn(iri);
          }}
        />
      {/key}
    </div>
  {:else if tab === 'graph'}
    <div
      class="split"
      class:overview={nodeCount === 0 && !graphTimeOpen && !community && !pathsOpen}
      class:reporting={nodeCount === 0 && !!community && !pathsOpen}
      class:mapmode={canvasMode === 'map' && nodeCount > 0 && !pathsOpen}
      class:faceted={(facetsOpen || filtersOpen) && nodeCount > 0 && !pathsOpen}
    >
      <div class="canvas">
        {#if nodeCount > 0 || graphTimeOpen || pathsOpen}
          <div
            class="toolbar gtools"
            class:narrow={gtoolsWidth > 0 && gtoolsWidth < 860}
            bind:this={graphToolbar}
            bind:clientWidth={gtoolsWidth}
          >
            <div class="toolbar-group">
              <button
                class="btn sm"
                aria-pressed={pathsOpen}
                aria-label="Paths"
                title="Find the paths between two entities (Shift+P)"
                onclick={() => (pathsOpen ? closePaths() : openPaths())}
                ><Icon name="route" size={13} /><span class="lbl">Paths</span></button
              >
              {#if nodeCount}
                <div class="segmented" role="group" aria-label="Show the graph as">
                  <button
                    aria-pressed={canvasMode === 'graph'}
                    onclick={() => setCanvasMode('graph')}>Graph</button
                  >
                  <button
                    aria-pressed={canvasMode === 'map'}
                    disabled={pathsOpen}
                    title={pathsOpen
                      ? 'Close the path finder to show the graph on a map'
                      : 'Show the drawn entities on a map, with arcs for their relations (Shift+M)'}
                    onclick={() => setCanvasMode('map')}><Icon name="map" size={13} /> Map</button
                  >
                </div>
                {#if mapIndex && ds}
                  <a
                    class="btn sm"
                    title="The entities of the vector index {mapIndex} projected to two dimensions"
                    href={routes.similarMap(ds, { index: mapIndex })}>Embedding map</a
                  >
                {/if}
                {#if canvasMode === 'graph'}
                  <LensMenu
                    {lenses}
                    anchor={graphToolbar}
                    short={(iri) => shortLabel(iri, prefixes)}
                    keys={tab === 'graph' && canvasMode === 'graph'}
                  >
                    {#snippet extra()}
                      {#if vOn && vProbe}
                        <ValidationSource
                          probe={vProbe}
                          editor={vHasEditor}
                          bind:kind={vSource}
                          bind:graphIri={vGraphIri}
                        />
                      {/if}
                    {/snippet}
                  </LensMenu>
                {/if}
                <button
                  class="btn sm"
                  aria-pressed={facetsOpen}
                  aria-label="Facets"
                  title="Histograms of the properties of the entities, which brush the graph (Shift+F)"
                  onclick={toggleFacets}
                  ><Icon name="filter" size={13} /><span class="lbl">Facets</span></button
                >
                <button
                  class="btn sm"
                  aria-pressed={filtersOpen}
                  aria-label="Filters"
                  title="Choose which edge types, classes and nodes are drawn (Shift+G)"
                  onclick={toggleFilters}
                  ><Icon name="sliders" size={13} /><span class="lbl">Filters</span></button
                >
                <button
                  class="btn sm"
                  aria-pressed={graphTimeOpen}
                  aria-label="Graph history"
                  title="Move the whole graph through the commits (Shift+H)"
                  onclick={() => (graphTimeOpen ? closeGraphTime() : openGraphTime())}
                  ><Icon name="clock" size={13} /><span class="lbl">History</span></button
                >
              {/if}
              <div class="view-menu">
                <button
                  class="btn sm"
                  aria-haspopup="true"
                  aria-expanded={viewOpen}
                  aria-label="View"
                  title="What the graph draws"
                  onclick={() => (viewOpen = !viewOpen)}
                  ><Icon name="eye" size={13} /><span class="lbl">View</span>
                  <Icon name="chevronDown" size={13} /></button
                >
                {#if viewOpen}
                  <div class="dropdown-menu">
                    <label class="dropdown-item check"
                      ><input type="checkbox" bind:checked={showLiterals} /> Literals</label
                    >
                    <label class="dropdown-item check"
                      ><input type="checkbox" bind:checked={hideTypes} /> Hide rdf:type</label
                    >
                  </div>
                {/if}
              </div>
            </div>
            <div class="toolbar-end">
              <span class="note" data-testid="graph-counts"
                >{gNodes.length} nodes, {gEdges.length} edges{#if useWebgl}
                  <span
                    class="renderer"
                    title="Drawn with WebGL, since the graph shows over {webglFrom.toLocaleString(
                      'en-US',
                    )} nodes">WebGL</span
                  >{/if}</span
              >
              {#if drawn.hidden > 0}
                <button
                  class="btn sm hidden-chip"
                  data-testid="hidden-chip"
                  title="The graph holds {nodeCount.toLocaleString(
                    'en-US',
                  )} nodes. The filters and the budget leave these out of the picture."
                  onclick={openFilters}>{drawn.hidden.toLocaleString('en-US')} hidden</button
                >
              {/if}
              {#if nodeCount && canvasMode === 'graph'}
                {#if !listView}
                  <button
                    class="btn sm icon graph-control"
                    aria-label="Fit to view"
                    title="Fit to view"
                    onclick={() => view.fit()}><Icon name="fit" size={14} /></button
                  >
                  <button
                    class="btn sm icon graph-control"
                    aria-label="Re-run layout"
                    title="Re-run layout"
                    onclick={() => view.relayout()}><Icon name="refresh" size={14} /></button
                  >
                {/if}
                <button
                  class="btn sm"
                  aria-pressed={listView}
                  aria-label="List"
                  title="The drawn nodes as a list, with a column per lens"
                  onclick={() => (listView = !listView)}
                  ><Icon name="table" size={13} /><span class="lbl">List</span></button
                >
              {/if}
              {#if nodeCount}
                <button
                  class="btn sm ghost"
                  aria-label="Clear"
                  title="Clear the graph"
                  onclick={clearGraph}
                  ><Icon name="trash" size={13} /><span class="lbl">Clear</span></button
                >
              {/if}
            </div>
          </div>
          <SelectionBar onopengraph={openSelectionAsGraph} onuse={useSelection} />
          {#if ds && vItems.length}
            <ValidationOverlay
              bind:this={overlay}
              {ds}
              items={vItems}
              {prefixes}
              on={vOn}
              bind:sourceKind={vSource}
              bind:graphIri={vGraphIri}
              bind:probe={vProbe}
              bind:probeFor={vProbeFor}
              bind:hasEditor={vHasEditor}
              at={mapAt}
              branch={overviewBranch ?? ''}
              paused={vPlaying}
              head={app.datasets.find((d) => d.name === ds)?.head ?? null}
              {selectedId}
              onselect={(id) => {
                select(id);
                view.center(id);
              }}
              onshowtriples={showResultTriples}
              onopenquery={(text) => {
                app.pendingQuery = { query: text, title: 'Suggested fix' };
                goto(resolve('/query'));
              }}
              bind:live={vLive}
              bind:badges={vBadges}
              bind:marked={vMarked}
            />
          {/if}
        {/if}
        {#if graphTimeOpen && ds}
          <div class="graph-time">
            <TimeSlider
              bind:this={graphSlider}
              ds={resourceTarget ?? ds}
              bind:at={exAt}
              bind:from={exFrom}
              bind:compare={exCompare}
              memory={app.datasets.find((d) => d.name === ds)?.type === 'mem'}
              busy={graphReading}
              lastReadMs={graphReadMs}
              announcement={graphAnnounce}
              branches={branchNames}
              bind:against={exAgainst}
              label="Graph history"
              canFollow={false}
              lapse
              bind:playMode={graphPlayMode}
              bind:playing={graphPlaying}
              onshowdiff={(from, to) => openDiff(to, from)}
              onplaying={(on) => (vPlaying = on)}
              onchange={() => syncUrl(selected?.term.type === 'uri' ? selected.iri : null)}
              onclose={() => {
                closeGraphTime();
                syncUrl(selected?.term.type === 'uri' ? selected.iri : null);
              }}
            />
            {#if graphNote}<p class="faint small graph-note">{graphNote}</p>{/if}
          </div>
        {/if}
        {#if nodeCount === 0 && !pathOverlay}
          {#if pathsOpen}
            <div class="cy-host paths-empty faint">
              <p>Choose two entities and find the paths between them.</p>
            </div>
          {:else}
            <div class="ov-host">
              {#key ds}
                <OverviewView
                  {ds}
                  {prefixes}
                  onresource={(iri, label) => focusOn(iri, label)}
                  onschema={() => setTab('schema')}
                  branch={overviewBranch}
                  start={overviewMode}
                  focus={overviewFocus}
                  {marked}
                  oncommunity={(c) => {
                    void showCommunity(c);
                    syncUrl(null);
                  }}
                  onmode={(m) => {
                    overviewMode = m;
                    syncUrl(null);
                  }}
                />
              {/key}
            </div>
          {/if}
        {:else}
          {#if canvasMode === 'map' && ds && !pathsOpen}
            <div class="cy-host">
              <GraphMap
                ds={resourceTarget ?? ds}
                nodes={gNodes}
                edges={gEdges}
                at={mapAt}
                fromAt={mapFromAt}
                fitKey={focusId ?? ''}
                onopen={(iri) => focusOn(iri)}
              />
            </div>
          {:else}
            <div class="cy-host">
              {#if listView}
                <GraphListView
                  rows={listRows}
                  plain
                  label="Drawn nodes"
                  lensColumns={lenses.columns}
                  selected={selectedId}
                  onselect={select}
                  openLabel="Focus"
                  onopen={(id) => {
                    const n = nodes[id];
                    if (n?.term.type === 'uri') void focusOn(n.iri, n.label);
                  }}
                />
              {:else if useWebgl}
                <LargeGraphView
                  bind:this={largeView}
                  nodes={largeNodes}
                  edges={largeEdges}
                  still={largeSettled}
                  relayoutBulk
                  onlayout={() => (largeSettled = true)}
                  selected={selectedId}
                  label="Explored graph"
                  onselect={select}
                  onexpand={expand}
                  onmenu={(id, at) =>
                    (menu =
                      nodes[id]?.term.type === 'uri' && !nodes[id]?.group ? { id, ...at } : null)}
                  onedge={openLargeEdge}
                  expanded={expandedIds}
                  groups={groupIds}
                  lens={lenses.encoding}
                  marked={largeMarked}
                  dimEmpty={selection.active}
                  pathFocus={pathHighlight}
                  onfail={(reason) => (webglOff = reason)}
                />
              {:else}
                <GraphView
                  bind:this={graphView}
                  showControls={!nodeCount}
                  animate={graphTimeOpen}
                  nodes={gNodes}
                  edges={gEdges}
                  selected={selectedId}
                  onselect={select}
                  onexpand={expand}
                  highlight={selection.active ? selection.ids : null}
                  pathFocus={pathHighlight}
                  onmenu={(id, at) =>
                    (menu =
                      nodes[id]?.term.type === 'uri' && !nodes[id]?.group ? { id, ...at } : null)}
                  lens={lenses.encoding}
                  marked={lensMarked}
                  onedge={(id) => void openWhyEdge(id)}
                  whyMarked={whyFor ? whyMarked : null}
                  validation={vLive && vOn
                    ? { badges: vBadges, marked: vMarked, onbadge: (id) => overlay?.open(id) }
                    : null}
                />
              {/if}
              {#if menu && !listView}
                <div
                  class="node-menu"
                  role="menu"
                  aria-label="Node actions"
                  style="left: {menu.x}px; top: {menu.y}px"
                >
                  <button
                    role="menuitem"
                    class="btn sm ghost"
                    onclick={() => openPaths({ from: menu!.id })}>Paths from here</button
                  >
                  <button
                    role="menuitem"
                    class="btn sm ghost"
                    onclick={() => openPaths({ to: menu!.id })}>Paths to here</button
                  >
                  <button
                    role="menuitem"
                    class="btn sm ghost"
                    onclick={() => {
                      const id = menu!.id;
                      menu = null;
                      void expand(id);
                    }}>Expand</button
                  >
                </div>
              {/if}
              <LensLegend
                results={lenses.results}
                {ds}
                selected={lensMarked ? lensPick : null}
                onselect={(key) => (lensPick = key)}
              />
            </div>
            <div class="hint faint">
              Double-click a node to load its neighbours. {useWebgl
                ? 'Ringed nodes are expanded.'
                : 'Blue-ringed nodes are expanded.'} A dashed box is a group of neighbours, which the
              panel opens a page at a time.
            </div>
          {/if}
        {/if}
      </div>

      {#if filtersOpen && !facetsOpen && nodeCount > 0 && !pathsOpen && ds}
        <aside class="side side-facets" aria-label="Filters">
          <ExploreFilters
            bind:value={fstate}
            predicates={predicateFacets}
            classes={classFacets}
            predicateLabel={(iri) => shortLabel(iri, prefixes)}
            classLabel={(id) => {
              const n = nodes[id];
              return n ? labelFor(n.term, n.label) : id;
            }}
            held={{ nodes: allNodes.length, edges: allEdges.length }}
            drawn={{ nodes: gNodes.length, edges: gEdges.length }}
            hidden={drawn.hidden}
            overBudget={drawn.overBudget}
            groups={communityGroups}
            groupsTitle={communityTitle}
            canCommunities={!!lenses.algo.graphrag}
            oncommunities={groupByCommunities}
            {maxDegree}
            webglNote={webglOff
              ? `WebGL is off (${webglOff}), so the canvas draws at most ${WEBGL_FROM.toLocaleString('en-US')} nodes.`
              : null}
            onclose={toggleFilters}
          />
        </aside>
      {/if}
      {#if facetsOpen && nodeCount > 0 && !pathsOpen && ds}
        <aside class="side side-facets" aria-label="Facets">
          <FacetPanel
            ds={resourceTarget ?? ds}
            within={facetWithin}
            setLabel={facetSet?.name ?? 'entities'}
            at={timeActive ? exAt : ''}
            {prefixes}
            drawn={drawnIds}
            onmatch={facetMatch}
            onapply={applyFacets}
            onclose={toggleFacets}
          >
            {#snippet header()}
              {#if facetSets.length > 1}
                <label class="row facet-set form-row"
                  >Count
                  <select
                    class="select sm"
                    value={facetSet?.key ?? ''}
                    onchange={(e) => (facetSetKey = (e.currentTarget as HTMLSelectElement).value)}
                  >
                    {#each facetSets as x (x.key)}
                      <option value={x.key}>{x.label}</option>
                    {/each}
                  </select></label
                >
              {:else if facetSet}
                <span class="faint">{facetSet.label}</span>
              {/if}
            {/snippet}
          </FacetPanel>
        </aside>
      {/if}
      {#if pathsOpen && ds}
        <aside class="side side-paths" aria-label="Path finder">
          {#key resourceTarget}
            <PathPanel
              bind:this={pathPanel}
              ds={resourceTarget ?? ds}
              {prefixes}
              branch={overviewBranch ?? 'main'}
              at={graphTimeOpen ? exAt : ''}
              bind:from={pathFrom}
              bind:to={pathTo}
              bind:highlight={pathHighlight}
              onresult={showPaths}
              onadd={addPathsToGraph}
              onclear={clearPaths}
              onclose={closePaths}
              onfit={fitPath}
            />
          {/key}
        </aside>
      {:else if nodeCount > 0 || graphTimeOpen}
        <aside class="side" aria-label="Resource details">
          {#if whyFor && ds}
            <WhyPanel
              ds={resourceTarget ?? ds}
              triple={whyFor}
              {prefixes}
              past={whyAtPast}
              onshow={showWhy}
              onopen={(iri) => focusOn(iri)}
              onclose={closeWhy}
            />
          {:else if selected?.group}
            {@const g = selected.group}
            <ExploreGroup
              ds={resourceTarget ?? ds}
              at={timeActive && graphTimeOpen ? exAt : ''}
              spec={g}
              predicateLabel={shortLabel(g.predicate, prefixes)}
              ownerLabel={nodes[g.owner]
                ? labelFor(nodes[g.owner].term, nodes[g.owner].label)
                : g.ownerIri}
              termLabel={(t) => labelFor(t)}
              drawn={(t) =>
                !!nodes[termKey(t)] || !!nodes[`lit:${g.owner}|${g.predicate}|${termKey(t)}`]}
              ondraw={(m) => drawMembers(selected!.id, m)}
              onremove={() => removeNode(selected!.id)}
            />
          {:else if selected}
            {@const n = selected}
            <div class="side-head">
              <div class="side-title-row">
                <div class="side-title">
                  <h2 title={labelFor(n.term, n.label)}>{labelFor(n.term, n.label)}</h2>
                  {#if n.term.type === 'uri'}
                    <button class="iri-line mono" onclick={() => copy(n.iri)} title="Copy IRI">
                      {n.iri}
                      <Icon name="copy" size={12} />
                    </button>
                  {:else}
                    <div class="mono faint">
                      {n.term.type === 'literal' ? 'literal' : n.term.type}
                    </div>
                  {/if}
                </div>
                {#if n.term.type === 'uri'}
                  <button
                    class="btn sm ghost icon"
                    title="Remove from graph"
                    aria-label="Remove from graph"
                    onclick={() => removeNode(n.id)}
                  >
                    <Icon name="x" size={14} />
                  </button>
                {/if}
              </div>
              {#if types.length}
                <div class="types">
                  {#each types as t (termKey(t))}
                    <button
                      class="badge iri"
                      onclick={() => t.type === 'uri' && (setTab('schema'), revealClass(t.value))}
                      title="Show class in schema"
                    >
                      {t.type === 'uri' ? shortLabel(t.value, prefixes) : t.value}
                    </button>
                  {/each}
                </div>
              {/if}
              {#if comment?.length}<p class="comment">{ex.pickLabel(comment)}</p>{/if}
              {#if n.term.type === 'uri'}
                <div class="side-actions">
                  <button
                    class="btn sm"
                    onclick={() => expand(n.id)}
                    disabled={n.expanded || expanding[n.id] !== undefined}
                  >
                    {#if expanding[n.id]}<span class="spinner"></span>{:else}<Icon
                        name="expand"
                        size={13}
                      />{/if}
                    {n.expanded ? 'Expanded' : 'Expand'}
                  </button>
                  {#if focusId !== n.id}
                    <button class="btn sm" onclick={() => focusOn(n.iri, n.label)}
                      ><Icon name="target" size={13} /> Focus</button
                    >
                  {/if}
                  <button class="btn sm" onclick={() => describeInQuery(n.iri)}
                    ><Icon name="query" size={13} /> Describe</button
                  >
                  {#if ds && isTerm(types)}
                    <a
                      class="btn sm"
                      href={routes.browse(ds, {
                        iri: n.iri,
                        branch: overviewBranch,
                        at: timeActive && exAt ? exAt : null,
                      })}
                      title="This term's page in Browse"><Icon name="book" size={13} /> Open page</a
                    >
                  {/if}
                  {#if ds && editable[`${ds}|${n.iri}`] && !timeActive}
                    <button
                      class="btn sm"
                      aria-pressed={editing}
                      title="Edit this resource in a form generated from its shapes"
                      onclick={() => {
                        editing = !editing;
                        sideTab = 'props';
                      }}><Icon name="pencil" size={13} /> Edit</button
                    >
                  {/if}
                  <button
                    class="btn sm"
                    title="Find paths from this resource (Shift+P)"
                    onclick={() => openPaths({ from: n.id })}
                    ><Icon name="route" size={13} /> Paths from here</button
                  >
                  <button class="btn sm" onclick={() => openPaths({ to: n.id })}
                    ><Icon name="route" size={13} /> Paths to here</button
                  >
                  <button
                    class="btn sm"
                    aria-pressed={stripOpen}
                    title="Move through the commits that changed this resource (H)"
                    onclick={() => (stripOpen ? closeStrip() : (stripOpen = true))}
                    ><Icon name="clock" size={13} /> History</button
                  >
                </div>
                <div class="segmented side-tabs" role="group" aria-label="Resource views">
                  <button aria-pressed={sideTab === 'props'} onclick={() => (sideTab = 'props')}
                    >Properties</button
                  >
                  <button
                    aria-pressed={sideTab === 'memory'}
                    onclick={() => (sideTab = 'memory')}
                    title="Facts agents recorded about this resource, with citations"
                    ><Icon name="brain" size={13} /> Memory</button
                  >
                </div>
                {#if stripOpen && sideTab === 'props'}
                  {#if hist}
                    <TimeSlider
                      bind:this={strip}
                      ds={resourceTarget ?? ds}
                      commits={hist.commits}
                      head={hist.head}
                      authors={hist.authors}
                      readable={hist.readable}
                      bind:at={exAt}
                      bind:from={exFrom}
                      bind:compare={exCompare}
                      busy={!detail}
                      label="Resource history"
                      canFollow={false}
                      onchange={() => syncUrl(n.iri)}
                      onclose={() => {
                        closeStrip();
                        syncUrl(n.iri);
                      }}
                    />
                    {#if hist.commits.length === 0}
                      <p class="faint small">No changes to this resource.</p>
                    {/if}
                    {#if hist.truncated}
                      <p class="faint small">
                        Only the first 5,000 changes of each kind are listed.
                      </p>
                    {/if}
                    {#if hist.note}<p class="faint small">{hist.note}</p>{/if}
                  {:else if histError}
                    <div class="error-box">{histError}</div>
                  {:else}
                    <p class="faint row"><span class="spinner"></span> Loading history…</p>
                  {/if}
                {/if}
              {/if}
            </div>

            {#if n.term.type === 'literal'}
              <div class="side-body">
                {#if isVectorLiteral(n.term)}
                  <p class="faint">{literalText(n.term)}</p>
                {/if}
                <pre class="literal">{n.term.value}</pre>
              </div>
            {:else if n.term.type !== 'uri'}
              <div class="side-body faint">
                Blank nodes can only be explored through their neighbours.
              </div>
            {:else if editing && sideTab === 'props' && ds && !timeActive}
              <div class="side-body scroll">
                {#key n.iri}
                  <ShapeForm
                    {ds}
                    branch={overviewBranch}
                    iri={n.iri}
                    {prefixes}
                    title={labelFor(n.term, n.label)}
                    oncancel={() => (editing = false)}
                    onsaved={() => formSaved(n.id)}
                    ondraft={() => (draftOpen = true)}
                  />
                {/key}
              </div>
            {:else if sideTab === 'memory' && ds}
              <div class="side-body scroll">
                {#key n.iri}
                  <MemoryTab
                    ds={resourceTarget ?? ds}
                    iri={n.iri}
                    {prefixes}
                    onopen={(iri) => focusOn(iri)}
                  />
                {/key}
              </div>
            {:else if !detail}
              <div class="side-body row faint">
                <span class="spinner"></span> Loading properties…
              </div>
            {:else if 'error' in detail}
              <div class="side-body"><div class="error-box">{detail.error}</div></div>
            {:else}
              <div class="side-body scroll">
                {#if histNet && hist}
                  <section class="hist-net" aria-label="Changes between the two commits">
                    <h3 class="sub">
                      Changed since commit {selectorSeq(exFrom, hist.head)}
                      <span class="faint">{histNet.length}</span>
                    </h3>
                    {#if histNet.length === 0}
                      <p class="faint small">No differences between these commits.</p>
                    {:else}
                      <ul>
                        {#each histNet as c (c.op + c.dir + c.predicate + c.term + c.graph)}
                          <li class="net {c.op}">
                            <span class="sign" aria-label={c.op === 'add' ? 'added' : 'removed'}
                              >{c.op === 'add' ? '+' : '−'}</span
                            >
                            <span class="mono net-body">
                              <span class="faint">{c.dir === 'out' ? '' : 'referenced by '}</span
                              ><span class="t-iri"
                                >{displayIri(predIri(c.predicate), prefixes)}</span
                              >
                              <TermView term={parseCompact(c.term, prefixes)} {prefixes} />
                            </span>
                            <span class="faint small net-by">
                              commit {c.commit}{c.author ? ` · ${c.author}` : ''}{c.message
                                ? ` · “${c.message}”`
                                : ''}{isReadable(c.commit, hist.readable)
                                ? ''
                                : ' · state unavailable'}
                            </span>
                          </li>
                        {/each}
                      </ul>
                    {/if}
                  </section>
                {/if}
                <h3 class="sub">
                  Properties <span class="count-badge">{fmtInt(detail.props.length)}</span>
                </h3>
                {#if grouped.length === 0}
                  <p class="faint">No outgoing triples. This IRI is only used as an object.</p>
                {/if}
                <table class="props">
                  <tbody>
                    {#each grouped as [p, values] (p)}
                      <tr>
                        <th title={p}><span class="t-iri">{displayIri(p, prefixes)}</span></th>
                        <td>
                          {#each values.slice(0, 20) as o (termKey(o))}
                            {@const eid = valueEdge(n.id, p, o)}
                            {@const inferred = lenses.encoding.inferred.has(eid)}
                            <div class="val" data-why-edge={inferred ? eid : undefined}>
                              <TermView
                                term={o}
                                {prefixes}
                                expandable
                                onopen={(iri) => addLinked(n.id, p, { type: 'uri', value: iri })}
                              />
                              {#if inferred}
                                <button
                                  class="why-btn"
                                  title="Why is this inferred? (W)"
                                  aria-label="Why is {displayIri(p, prefixes)} inferred?"
                                  onclick={() => openWhyEdge(eid)}>ⓘ Why?</button
                                >
                              {/if}
                            </div>
                          {/each}
                          {#if values.length > 20}<div class="faint">
                              +{values.length - 20} more
                            </div>{/if}
                        </td>
                      </tr>
                    {/each}
                  </tbody>
                </table>

                {#key n.iri}
                  <GeoMapCard
                    ds={resourceTarget ?? ds}
                    iri={n.iri}
                    props={detail.props}
                    {prefixes}
                  />
                  <SimilarPanel
                    ds={resourceTarget ?? ds}
                    iri={n.iri}
                    props={detail.props}
                    {prefixes}
                  />
                  {#if ds && !timeActive}<WhyNotPanel
                      ds={resourceTarget ?? ds}
                      subject={n.term}
                      {prefixes}
                      onopen={(iri) => focusOn(iri)}
                    />{/if}
                {/key}

                <h3 class="sub">
                  Referenced by <span class="count-badge">{fmtInt(detail.incomingTotal)}</span>
                </h3>
                {#if detail.incoming.length === 0}
                  <p class="faint">Nothing points to this resource.</p>
                {:else}
                  <table class="props">
                    <tbody>
                      {#each detail.incoming.slice(0, 100) as r (termKey(r.s) + r.p)}
                        <tr>
                          <td class="inc-s">
                            <TermView
                              term={r.s}
                              {prefixes}
                              onopen={(iri) =>
                                addLinked(n.id, r.p, { type: 'uri', value: iri }, true)}
                            />
                          </td>
                          <th title={r.p}><span class="t-iri">{displayIri(r.p, prefixes)}</span></th
                          >
                        </tr>
                      {/each}
                    </tbody>
                  </table>
                  {#if detail.incomingTotal > 100}<p class="faint more-note">
                      Showing 100 of {fmtInt(detail.incomingTotal)}.
                    </p>{/if}
                {/if}
              </div>
            {/if}
          {:else}
            <div class="side-empty faint">
              <Icon name="info" size={18} />
              <p>Select a node to see its properties.</p>
            </div>
          {/if}
        </aside>
      {:else if community && ds}
        <aside class="side" aria-label="Community report" data-testid="community-side">
          <div class="side-body scroll">
            {#if communityDetail}
              <CommunityReport
                {ds}
                detail={communityDetail}
                {prefixes}
                heading="h2"
                {finding}
                onfinding={(i) => (finding = i)}
              >
                {#snippet actions()}
                  <a
                    class="btn sm"
                    href={gr.reportHref(resolve('/datasets/[name]', { name: ds }), community!.iri)}
                    title="Review this report in the dataset's Communities tab"
                    ><Icon name="sparkle" size={12} /> Open report</a
                  >
                  <button
                    class="btn sm ghost"
                    onclick={() => void showCommunity(null)}
                    aria-label="Close the report">Close</button
                  >
                {/snippet}
              </CommunityReport>
            {:else if communityError}
              <h2 class="side-title-plain">{community.title}</h2>
              <div class="error-box">{api.errorMessage(communityError)}</div>
            {:else if communityLoading}
              <p class="faint">
                <span class="spinner"></span> Loading the report for {community.title}…
              </p>
            {/if}
          </div>
        </aside>
      {/if}
    </div>
  {:else}
    <!-- schema -->
    <div class="split">
      <div class="schema-main">
        <div class="ontology">
          {#if schema?.ontology}
            <strong>{schema.ontology.label ?? shortLabel(schema.ontology.iri, prefixes)}</strong>
            {#if schema.ontology.version}<span class="badge">v{schema.ontology.version}</span>{/if}
            <span class="mono faint">{schema.ontology.iri}</span>
          {/if}
          {#if schema}
            <span
              class="faint small"
              title="Exact counts at this snapshot ({fmtInt(schema.totals.triples)} triples)"
              >{ex.snapshotLine(schema.snapshot)}</span
            >
          {/if}
          <span class="spacer"></span>
          <label class="row small">
            <span class="faint">Graph</span>
            <select
              class="select sm"
              value={schemaGraph}
              onchange={(e) => setSchemaGraph(e.currentTarget.value)}
            >
              <option value="default">default graph</option>
              <option value="union">all graphs (union)</option>
              {#each namedGraphs as g (g)}<option value={g}>{displayIri(g, prefixes)}</option
                >{/each}
            </select>
          </label>
          {#if hasInferences}
            <label class="row small" title="Count materialized inferences (reasoning=)">
              <input
                type="checkbox"
                bind:checked={schemaInferences}
                onchange={() => ds && loadSchema(ds)}
              />
              <span>include inferences</span>
            </label>
          {/if}
          <button
            class="btn sm"
            onclick={() => (draftOpen = true)}
            disabled={!ds}
            title="Draft SHACL shapes or a ShEx schema from the data"
          >
            <Icon name="wand" size={13} /> Draft shapes
          </button>
          <button
            class="btn sm"
            onclick={() => (diffOpen = true)}
            disabled={!ds}
            title="What changed in the schema since an earlier commit or snapshot"
          >
            <Icon name="clock" size={13} /> Compare
          </button>
          <button
            class="btn sm"
            aria-pressed={schemaTimeOpen}
            aria-label="Schema history"
            onclick={() => (schemaTimeOpen ? closeSchemaTime() : openSchemaTime())}
            disabled={!ds}
            title="Read the schema at an earlier commit and see how its counts changed"
          >
            <Icon name="clock" size={13} /> History
          </button>
        </div>
        <div class="schema-tools">
          <div class="tabs" role="tablist">
            <button
              class="tab"
              role="tab"
              aria-selected={schemaView === 'classes'}
              onclick={() => (schemaView = 'classes')}
            >
              Classes <span class="count">{schema ? schema.classes.size : ''}</span>
            </button>
            <button
              class="tab"
              role="tab"
              aria-selected={schemaView === 'properties'}
              onclick={() => (schemaView = 'properties')}
            >
              Properties <span class="count"
                >{schema
                  ? schema.properties.length - (showBuiltinProps ? 0 : builtinProps)
                  : ''}</span
              >
            </button>
          </div>
          <span class="spacer"></span>
          {#if schemaView === 'classes'}
            <input class="input sm" placeholder="Filter classes" bind:value={classFilter} />
          {:else}
            {#if builtinProps}
              <label class="row small" title="rdf:, rdfs:, owl:, xsd: and sh: predicates">
                <input type="checkbox" bind:checked={showBuiltinProps} />
                <span>built-in ({builtinProps})</span>
              </label>
            {/if}
            <input class="input sm" placeholder="Filter properties" bind:value={propFilter} />
          {/if}
          <button
            class="btn sm ghost icon"
            title="Reload schema"
            aria-label="Reload schema"
            onclick={() => ds && loadSchema(ds)}
          >
            <Icon name="refresh" size={13} />
          </button>
        </div>
        {#if schemaTimeOpen && ds}
          <TimeSlider
            bind:this={schemaSlider}
            {ds}
            bind:at={exAt}
            bind:from={exFrom}
            bind:compare={exCompare}
            memory={app.datasets.find((d) => d.name === ds)?.type === 'mem'}
            busy={schemaLoading}
            lastReadMs={schemaReadMs}
            announcement={schemaAnnounce}
            branches={branchNames}
            bind:against={exAgainst}
            label="Schema history"
            canFollow={false}
            onchange={() => syncUrl(null)}
            onclose={closeSchemaTime}
          />
        {/if}
        <div class="schema-body scroll">
          {#if schemaLoading && !schema}
            <div class="empty"><span class="spinner"></span> Loading schema…</div>
          {:else if schemaErr}
            <div class="pad">
              <div class="error-box"><strong>Could not load the schema.</strong> {schemaErr}</div>
            </div>
          {:else if schema}
            {#if schemaView === 'classes'}
              {#if schema.classes.size === 0}
                <div class="empty">
                  No classes found. Declare some with owl:Class or rdfs:subClassOf, or type your
                  resources with rdf:type.
                </div>
              {:else if classMatches}
                <ClassTree
                  {schema}
                  iris={classMatches}
                  {prefixes}
                  open={new Set()}
                  selected={selectedClass}
                  onselect={(i) => (selectedClass = i)}
                  ontoggle={revealClass}
                  changes={schemaDelta?.classes}
                />
              {:else}
                <div class="legend faint"><span>Class</span><span>subclasses, instances</span></div>
                {#if schemaDelta?.removedClasses.length}
                  <section class="gone-list" aria-label="Classes removed since the earlier commit">
                    {#each schemaDelta.removedClasses as c (c.iri)}
                      <div class="gone-row" title={c.iri}>
                        <span class="sign" aria-label="removed">−</span>
                        <span class="gone-name">{c.label ?? shortLabel(c.iri, prefixes)}</span>
                        <span class="faint small">removed, had {fmtInt(c.instances)}</span>
                      </div>
                    {/each}
                  </section>
                {/if}
                <ClassTree
                  {schema}
                  iris={schema.roots}
                  {prefixes}
                  open={openClasses}
                  selected={selectedClass}
                  onselect={(i) => (selectedClass = i)}
                  ontoggle={toggleClass}
                  changes={schemaDelta?.classes}
                />
              {/if}
            {:else}
              <table class="data plist">
                <thead>
                  <tr>
                    <th>Property</th>
                    <th class="num" title="Distinct triples in the selected graphs">Triples</th>
                    <th class="num" title="Distinct subjects / distinct objects">Subj. / obj.</th>
                    <th>Objects</th>
                    <th>Kind</th>
                    <th>Domain</th>
                    <th>Range</th>
                  </tr>
                </thead>
                <tbody>
                  {#each schemaDelta?.removedProperties ?? [] as p (p.iri)}
                    <tr class="gone-prop" title={p.iri}>
                      <td>
                        <div class="pname">
                          <span class="sign" aria-label="removed">−</span>
                          {p.label ?? shortLabel(p.iri, prefixes)}
                        </div>
                        <div class="mono faint small">{displayIri(p.iri, prefixes)}</div>
                      </td>
                      <td class="num">{fmtInt(p.triples)}</td>
                      <td colspan="5" class="faint small">removed since the earlier commit</td>
                    </tr>
                  {/each}
                  {#each propMatches as p (p.iri)}
                    {@const o = p.observed}
                    {@const ch = schemaDelta?.properties.get(p.iri)}
                    <tr>
                      <td title={p.iri}>
                        <div class="pname">{p.label ?? shortLabel(p.iri, prefixes)}</div>
                        <div class="mono faint small">{displayIri(p.iri, prefixes)}</div>
                      </td>
                      <td class="num"
                        >{o.triples ? fmtInt(o.triples) : '—'}{#if ch}
                          <span
                            class="delta"
                            class:add={ch.delta > 0 || ch.mark === 'added'}
                            class:del={ch.delta < 0}
                            >{#if ch.mark === 'added'}<span class="tag">added</span>{/if}{deltaText(
                              ch.delta,
                            )}</span
                          >{/if}</td
                      >
                      <td class="num small"
                        >{o.triples
                          ? `${fmtCompact(o.distinctSubjects)} / ${fmtCompact(o.distinctObjects)}`
                          : ''}</td
                      >
                      <td class="objects">
                        {#if o.triples}
                          {@const segs = ex.objectSegments(o)}
                          <div class="mix" aria-hidden="true">
                            {#each segs as sg, i (i)}<span
                                class="seg {sg.kind}"
                                style:width="{Math.max(sg.share * 100, 2)}%"
                              ></span>{/each}
                          </div>
                          <div class="mix-legend">
                            {#each segs as sg, i (i)}
                              <span
                                class="mix-item"
                                title="{fmtInt(sg.triples)} triples ({Math.round(sg.share * 100)}%)"
                                ><span class="dot {sg.kind}"></span>{sg.datatype
                                  ? shortLabel(sg.datatype, prefixes)
                                  : sg.kind}</span
                              >
                              {#each sg.languages.slice(0, 6) as l (l)}<span class="lang">@{l}</span
                                >{/each}
                              {#if sg.languages.length > 6}<span class="faint small"
                                  >+{sg.languages.length - 6}</span
                                >{/if}
                            {/each}
                          </div>
                        {:else}
                          <span class="faint small">not used in this graph</span>
                        {/if}
                      </td>
                      <td>
                        {#each p.kinds.filter((k) => k !== ex.OWL_FUNCTIONAL) as k (k)}<span
                            class="badge">{kindName(k)}</span
                          >
                        {/each}
                        {#each ex.cardinalityChips(p) as c (c.kind)}<span
                            class="badge {c.kind === 'observed' ? 'measure' : 'iri'}"
                            title={c.title}>{c.text}</span
                          >
                        {/each}
                        {#each ex.constraintChips(schema.constraints, p.iri, short) as c, i (i)}<span
                            class="badge shacl {c.enforcement}"
                            title={c.title}>{c.text}</span
                          >
                        {/each}
                      </td>
                      <td>
                        {#each p.domains as d (d)}
                          <button class="cls-link" onclick={() => revealClass(d)} title={d}
                            >{classLabel(d)}</button
                          >
                        {:else}<span class="faint">—</span>{/each}
                      </td>
                      <td>
                        {#each p.ranges as r (r)}
                          {#if schema.classes.has(r)}
                            <button class="cls-link" onclick={() => revealClass(r)} title={r}
                              >{classLabel(r)}</button
                            >
                          {:else}
                            <span class="mono t-literal small" title={r}
                              >{shortLabel(r, prefixes)}</span
                            >
                          {/if}
                        {:else}<span class="faint">—</span>{/each}
                      </td>
                    </tr>
                  {:else}
                    <tr><td colspan="7" class="faint">No properties match.</td></tr>
                  {/each}
                </tbody>
              </table>
            {/if}
          {/if}
        </div>
      </div>

      <aside class="side" aria-label="Class details">
        {#if cls && schema}
          <div class="side-head">
            <div class="side-title">
              <h2>{cls.label ?? shortLabel(cls.iri, prefixes)}</h2>
              <button class="iri-line mono" onclick={() => copy(cls.iri)} title="Copy IRI"
                >{cls.iri} <Icon name="copy" size={12} /></button
              >
            </div>
            {#if cls.comment}<p class="comment">{cls.comment}</p>{/if}
            <div class="side-actions">
              <button class="btn sm" onclick={() => sampleOf(cls.iri)} disabled={!cls.instances}
                ><Icon name="explore" size={13} /> Explore an instance</button
              >
              <button class="btn sm" onclick={() => focusOn(cls.iri, cls.label)}
                ><Icon name="graph" size={13} /> Class as graph</button
              >
              {#if ds && creatable[`${ds}|${cls.iri}`]}
                <a
                  class="btn sm"
                  href="{resolve('/datasets/[name]/edit', { name: ds })}?class={encodeURIComponent(
                    cls.iri,
                  )}"
                  title="Make a resource of this class in a form generated from its shapes"
                  ><Icon name="plus" size={13} /> New instance</a
                >
              {/if}
            </div>
          </div>
          <div class="side-body scroll">
            <dl class="facts">
              <dt>Instances</dt>
              <dd>{fmtInt(cls.instances)}</dd>
              <dt>Declared</dt>
              <dd>{cls.declared ? 'yes' : 'no, only used'}</dd>
            </dl>
            <h3 class="sub">Superclasses <span class="faint small">asserted</span></h3>
            <div class="chips left">
              {#each cls.assertedSupers as s (s)}<button
                  class="cls-link"
                  onclick={() => revealClass(s)}
                  title={s}>{classLabel(s)}</button
                >{:else}<span class="faint">None (root class)</span>{/each}
            </div>
            {#each cls.superExpressions ?? [] as x (x)}
              <div class="mono small expr" title={x}>⊑ {shortenExpression(x, short)}</div>
            {/each}
            {#each cls.equivalentExpressions ?? [] as x (x)}
              <div class="mono small expr" title={x}>≡ {shortenExpression(x, short)}</div>
            {/each}
            {#if cls.cycle.length}
              <p class="faint small">
                <Icon name="cycle" size={12} /> On a subClassOf cycle with {cls.cycle
                  .map(classLabel)
                  .join(', ')}
              </p>
            {/if}
            {#if cls.equivalents.length}
              <h3 class="sub">Equivalent classes</h3>
              <div class="chips left">
                {#each cls.equivalents as s (s)}<button
                    class="cls-link"
                    onclick={() => revealClass(s)}
                    title={s}>{classLabel(s)}</button
                  >{/each}
              </div>
            {/if}
            {#if cls.disjoint.length}
              <h3 class="sub">Disjoint with</h3>
              <div class="chips left">
                {#each cls.disjoint as s (s)}<button
                    class="cls-link"
                    onclick={() => revealClass(s)}
                    title={s}>{classLabel(s)}</button
                  >{/each}
              </div>
            {/if}
            <h3 class="sub">Subclasses</h3>
            <div class="chips left">
              {#each cls.subs as s (s)}<button class="cls-link" onclick={() => revealClass(s)}
                  >{classLabel(s)}</button
                >{:else}<span class="faint">None</span>{/each}
            </div>
            <h3 class="sub">
              Used properties <span class="faint small">observed</span>
            </h3>
            {#if profileErr}
              <p class="error small">{profileErr}</p>
            {:else if !profile}
              <p class="faint small">{cls.instances ? 'Counting…' : 'No instances.'}</p>
            {:else}
              {#each profile.properties as p (p.predicate)}
                <div class="profile-line">
                  <div class="row">
                    <span class="t-iri mono">{displayIri(p.predicate, prefixes)}</span>
                    <span class="faint small" title="Instances with a value"
                      >{coverage(p, profile.instances)}</span
                    >
                  </div>
                  <div class="faint small">
                    {valuesRange(p)} per instance · {objectKinds(
                      p,
                      short,
                    )}{#if p.objectClasses.length}
                      · → {p.objectClasses
                        .slice(0, 3)
                        .map((k) => classLabel(k.class))
                        .join(', ')}{/if}
                  </div>
                </div>
              {:else}<p class="faint small">
                  Its instances have no properties besides rdf:type.
                </p>{/each}
              {#if profile.incoming.length}
                <h3 class="sub">Pointed at by <span class="faint small">observed</span></h3>
                {#each profile.incoming as i (i.predicate)}
                  <div class="prop-line">
                    <span class="t-iri mono">{displayIri(i.predicate, prefixes)}</span>
                    <span class="faint small"
                      >{fmtInt(i.triples)} triples · {fmtInt(i.instances)} instances</span
                    >
                  </div>
                {/each}
              {/if}
            {/if}
            {#if clsConstraints.length}
              <h3 class="sub">
                Constraints <span class="faint small">declared by SHACL shapes</span>
              </h3>
              {#each clsConstraints as l, i (i)}
                {@const e = ex.enforcementText(l.constraint.enforcement)}
                <div class="prop-line">
                  <span class="t-iri mono">{displayIri(l.constraint.path, prefixes)}</span>
                  <span class="small">{ex.constraintSummary(l.constraint, short)}</span>
                  <span class="badge shacl {l.constraint.enforcement}" title={e.title}
                    >{e.text}</span
                  >
                </div>
              {/each}
            {/if}
            <h3 class="sub">
              Properties with this domain <span class="faint">{clsProps.domain.length}</span>
            </h3>
            {#each clsProps.domain as p (p.iri)}
              <div class="prop-line">
                <span class="t-iri mono">{displayIri(p.iri, prefixes)}</span>
                <span class="faint">→</span>
                {#each p.ranges as r (r)}<span class="mono small">{shortLabel(r, prefixes)}</span
                  >{:else}<span class="faint">any</span>{/each}
              </div>
            {:else}<p class="faint">None declared.</p>{/each}
            <h3 class="sub">
              Properties with this range <span class="faint">{clsProps.range.length}</span>
            </h3>
            {#each clsProps.range as p (p.iri)}
              <div class="prop-line">
                {#each p.domains as d (d)}<span class="mono small">{shortLabel(d, prefixes)}</span
                  >{:else}<span class="faint">any</span>{/each}
                <span class="faint">→</span>
                <span class="t-iri mono">{displayIri(p.iri, prefixes)}</span>
              </div>
            {:else}<p class="faint">None declared.</p>{/each}
          </div>
        {:else}
          <div class="side-empty faint">
            <Icon name="tree" size={18} />
            <p>Select a class to see its place in the hierarchy and the properties that use it.</p>
          </div>
        {/if}
      </aside>
    </div>
  {/if}
</div>

{#if ds}
  <ShapesDraftDialog
    bind:open={draftOpen}
    {ds}
    graph={schemaGraph}
    reasoning={hasInferences && schemaInferences}
    {prefixes}
  />
  <SchemaDiffDialog
    bind:open={diffOpen}
    {ds}
    graph={schemaGraph}
    reasoning={hasInferences ? schemaInferences : undefined}
    {prefixes}
    onslider={(from, to) => openSchemaTime(from, to)}
  />
{/if}

<style>
  .page {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
  }
  .bar {
    display: flex;
    align-items: flex-end;
    padding: var(--sp-1) var(--gutter) 0;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .diff-host {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .split {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 380px;
  }
  /* the facets beside the properties on a wide screen */
  .split.faceted {
    grid-template-columns: minmax(0, 1fr) 320px 380px;
  }
  .renderer {
    margin-left: 6px;
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: var(--fs-xs);
    color: var(--text-2);
  }
  .hidden-chip {
    border-radius: 999px;
    color: var(--text-2);
  }
  .facet-set select {
    max-width: 100%;
  }
  .canvas {
    position: relative;
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    /* node labels near the edge must not paint over the details panel */
    overflow: hidden;
  }
  .gtools {
    padding-top: 6px;
    padding-bottom: 6px;
  }
  /* a narrow canvas, such as beside the details panel on a laptop: every tool keeps its
     icon and tooltip, so the tools stay on one row and the canvas keeps its height. A
     measured width rather than a container query, since a container's layout containment
     would hold the toolbar's fixed popovers */
  .gtools.narrow {
    /* and a very narrow one, such as with Facets open as well, wraps its tools to more
       rows rather than clipping them */
    & .toolbar-group,
    & .toolbar-end {
      flex-wrap: wrap;
      min-width: 0;
    }
    & .toolbar-end {
      flex-shrink: 1;
      justify-content: flex-end;
    }
    & .lbl {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip: rect(0 0 0 0);
      white-space: nowrap;
    }
  }
  .view-menu {
    position: relative;
  }
  /* beside the details panel the canvas is narrow: the end group's buttons keep their icons
     and tooltips, so the tools stay on one row */
  @media (max-width: 1599px) {
    .gtools .toolbar-end .lbl {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip: rect(0 0 0 0);
      white-space: nowrap;
    }
  }
  .check {
    gap: 5px;
    cursor: pointer;
  }
  .cy-host {
    flex: 1;
    min-height: 0;
    /* the lens legend docks in its corner */
    position: relative;
  }
  .ov-host {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .node-menu {
    position: absolute;
    z-index: 20;
    display: grid;
    padding: 4px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: 6px;
    box-shadow: var(--shadow, 0 6px 18px rgb(0 0 0 / 0.15));
  }
  .node-menu .btn {
    justify-content: flex-start;
  }
  .paths-empty {
    display: grid;
    place-items: center;
    text-align: center;
    padding: 24px;
  }
  .split.overview {
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr);
  }
  .graph-time {
    flex: none;
    min-width: 0;
  }
  .delta {
    margin-left: 6px;
    font-size: var(--fs-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .delta.add {
    color: var(--added);
  }
  .delta.del {
    color: var(--removed);
  }
  .delta .tag {
    margin-right: 4px;
    padding: 0 5px;
    border: 1px solid var(--added);
    border-radius: 8px;
    font-size: 10px;
    font-weight: 400;
  }
  .gone-list {
    padding: 2px 4px 6px;
    border-bottom: 1px dashed var(--border);
  }
  .gone-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 2px 10px;
    color: var(--removed);
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--removed) 60%, transparent);
  }
  .gone-row .faint {
    text-decoration: none;
  }
  .gone-prop td {
    border-bottom: 1px dashed var(--border);
    color: var(--removed);
    background: var(--removed-soft);
  }
  .sign {
    font-weight: 700;
    margin-right: 2px;
  }
  .graph-note {
    margin: 0;
    padding: 2px 12px;
  }
  .hint {
    padding: 4px 12px;
    font-size: var(--fs-xs);
    border-top: 1px solid var(--border);
    background: var(--surface);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 6px;
    max-width: 640px;
    margin: 6px 0;
  }
  .chips.left {
    justify-content: flex-start;
    margin: 0 0 4px;
  }
  .chip {
    display: inline-flex;
    gap: 8px;
    align-items: center;
    padding: 4px 10px;
    border: 1px solid var(--border);
    border-radius: 14px;
    background: var(--surface);
    cursor: pointer;
    font: inherit;
    font-size: var(--fs-sm);
  }
  .chip:hover {
    border-color: var(--iri);
  }
  .side {
    border-left: 1px solid var(--border);
    background: var(--surface);
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .side-head {
    padding: 14px 16px 12px;
    border-bottom: 1px solid var(--border);
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 8px;
  }
  .side-title-row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
  }
  .side-title {
    flex: 1;
    min-width: 0;
  }
  .side-title h2 {
    font-size: var(--fs-lg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .iri-line {
    all: unset;
    display: flex;
    gap: 6px;
    align-items: center;
    margin-top: 2px;
    font-size: 11px;
    color: var(--text-3);
    word-break: break-all;
    cursor: pointer;
  }
  .iri-line:hover {
    color: var(--text);
  }
  .types {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .types .badge {
    border: 0;
    cursor: pointer;
    font-family: var(--font-mono);
    font-weight: 500;
  }
  .comment {
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .side-actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
  }
  .side-tabs {
    margin-top: 8px;
    align-self: start;
  }
  .hist-net ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
    font-size: 12px;
  }
  .net {
    display: grid;
    grid-template-columns: 1.4em minmax(0, 1fr);
    column-gap: 4px;
    padding-left: 6px;
    border-left: 3px solid var(--border);
  }
  .net.add {
    border-left-color: var(--added);
  }
  .net.remove {
    border-left-color: var(--removed);
  }
  .net .sign {
    font-weight: 700;
  }
  .net.add .sign {
    color: var(--added);
  }
  .net.remove .sign {
    color: var(--removed);
  }
  .net.remove .net-body {
    text-decoration: line-through;
    text-decoration-thickness: 1px;
  }
  .net-body {
    overflow-wrap: anywhere;
  }
  .net-by {
    grid-column: 2;
  }
  .side-body {
    padding: 8px 16px 16px;
    min-height: 0;
    flex: 1;
  }
  .side-empty {
    flex: 1;
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 8px;
    padding: 24px;
    text-align: center;
  }
  .sub {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 14px 0 6px;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  .props {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
    table-layout: fixed;
  }
  .props th,
  .props td {
    padding: 4px 0;
    border-bottom: 1px solid var(--border);
    vertical-align: top;
    text-align: left;
  }
  .props th {
    width: 38%;
    padding-right: 10px;
    font-weight: 400;
    font-family: var(--font-mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .props td {
    font-family: var(--font-mono);
    word-break: break-word;
  }
  .why-btn {
    margin-left: 6px;
    border: 1px dashed var(--border-strong);
    border-radius: 999px;
    background: transparent;
    color: var(--text-2);
    font-size: var(--fs-xs);
    padding: 0 6px;
    cursor: pointer;
  }
  .why-btn:hover,
  .why-btn:focus-visible {
    color: var(--text);
    border-color: var(--accent, var(--text-2));
  }
  .props .val + .val {
    margin-top: 2px;
  }
  .inc-s {
    padding-right: 10px !important;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* a <button> is an atomic inline box, so the cell's ellipsis never applies to it */
  .inc-s :global(.link) {
    display: block;
    max-width: 100%; /* buttons shrink-wrap their content even as blocks */
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .more-note {
    margin-top: 6px;
    font-size: var(--fs-sm);
  }
  .literal {
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--literal);
    font-size: 12px;
  }
  .schema-main {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    background: var(--surface);
  }
  .ontology {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 10px 14px;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .ontology .mono {
    font-size: 11px;
    overflow-wrap: anywhere;
  }
  .ontology > label {
    min-width: 0;
    max-width: 100%;
  }
  .schema-tools {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 12px;
    border-bottom: 1px solid var(--border);
  }
  .input.sm {
    height: 26px;
    width: 220px;
    font-size: var(--fs-sm);
  }
  .schema-body {
    flex: 1;
    min-height: 0;
    padding: 6px 8px;
  }
  .legend {
    display: flex;
    justify-content: space-between;
    padding: 2px 10px 4px 26px;
    font-size: var(--fs-xs);
  }
  .pad {
    padding: 8px;
  }
  .plist td {
    vertical-align: top;
  }
  .plist .num {
    text-align: right;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .objects {
    min-width: 160px;
  }
  .mix {
    display: flex;
    height: 6px;
    margin-top: 4px;
    border-radius: 3px;
    overflow: hidden;
    background: var(--surface-3);
  }
  .seg.iri,
  .dot.iri {
    background: var(--iri);
  }
  .seg.literal,
  .dot.literal {
    background: var(--literal);
  }
  .seg.blank,
  .dot.blank,
  .seg.triple,
  .dot.triple {
    background: var(--bnode);
  }
  .seg + .seg {
    border-left: 1px solid var(--surface);
  }
  .mix-legend {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 8px;
    margin-top: 3px;
    font-size: 11px;
  }
  .mix-item {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }
  .lang {
    font-family: var(--font-mono);
    color: var(--literal);
  }
  /* declared by a SHACL shape: solid when write-time validation enforces it */
  .badge.shacl {
    background: var(--spark-soft);
    color: var(--spark-ink);
    font-weight: 500;
  }
  .badge.shacl.validated-on-request {
    background: transparent;
    border: 1px solid var(--spark-ink);
  }
  /* an observation of the current data, not a declared constraint */
  .badge.measure {
    background: transparent;
    border: 1px dashed var(--text-3);
    color: var(--text-2);
    font-weight: 500;
  }
  .ontology .spacer {
    flex: 1;
  }
  .pname {
    font-weight: 500;
  }
  .small {
    font-size: 11px;
  }
  .cls-link {
    all: unset;
    display: inline-block;
    margin: 0 4px 2px 0;
    padding: 1px 7px;
    border-radius: 10px;
    font-size: var(--fs-sm);
    background: color-mix(in srgb, var(--iri) 10%, transparent);
    color: var(--iri);
    cursor: pointer;
  }
  .cls-link:hover {
    background: color-mix(in srgb, var(--iri) 18%, transparent);
  }
  .cls-link:focus-visible {
    box-shadow: var(--focus);
  }
  .facts {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 16px;
    margin: 6px 0 0;
    font-size: var(--fs-sm);
  }
  .facts dt {
    color: var(--text-2);
  }
  .facts dd {
    margin: 0;
    font-weight: 600;
  }
  .expr {
    margin: 2px 0;
    overflow-wrap: anywhere;
  }
  .profile-line {
    padding: 3px 0;
    border-bottom: 1px solid var(--border);
    overflow-wrap: anywhere;
  }
  .profile-line .row {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  .prop-line {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
    padding: 3px 0;
    font-size: 12px;
    border-bottom: 1px solid var(--border);
  }
  @media (max-width: 1000px) {
    .split,
    .split.faceted {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: minmax(320px, 1fr) auto;
    }
    .split.faceted {
      grid-template-rows: minmax(320px, 1fr) auto auto;
    }
    /* the facets take the height they need and the page scrolls to them */
    .side-facets {
      max-height: none;
    }
    .side-facets :global(.facets),
    .side-facets :global(.filters) {
      height: auto;
    }
    /* the panels stack under the canvas, each at the height its content needs */
    .side-facets {
      min-height: min-content;
    }
    /* the toolbars of a phone take most of the row, so the drawn graph gets more of it */
    .split:not(.overview, .mapmode, .reporting) {
      grid-template-rows: minmax(560px, 1fr) auto;
      /* the rows take their content's height and the page scrolls */
      flex: none;
    }
    .split.faceted:not(.overview, .mapmode, .reporting) {
      grid-template-rows: minmax(560px, 1fr) auto auto;
    }
    .side-facets :global(.fbody) {
      flex: none;
      overflow: visible;
    }
    /* the map takes the height it needs and the page scrolls to the details below it */
    .split.mapmode {
      grid-template-rows: auto auto;
    }
    .split.mapmode .canvas {
      overflow: visible;
    }
    .split.mapmode .cy-host {
      flex: none;
    }
    .side {
      border-left: 0;
      border-top: 1px solid var(--border);
      max-height: 50vh;
    }
    /* the overview's toolbars and notes sit above its canvas, so it needs more room
       than the resource graph to keep the picture usable above a report */
    .split.reporting {
      grid-template-rows: minmax(min(480px, 62vh), 1fr) auto;
    }
  }
  @media (max-width: 760px) {
    /* the counts are in the filter panel and the list, so the end group's buttons share the
       last row of the tools and the canvas keeps the height the counts would take */
    .gtools .note {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip: rect(0 0 0 0);
      white-space: nowrap;
    }
    .gtools .toolbar-end {
      flex: 1 1 auto;
      flex-wrap: nowrap;
    }
    /* taps need 44 px targets */
    .graph-control {
      min-height: 44px;
      min-width: 44px;
    }
    /* the path finder is a sheet over the graph, which folds to its first path */
    .split {
      position: relative;
    }
    .side-paths {
      position: absolute;
      left: 0;
      right: 0;
      bottom: 0;
      z-index: 15;
      max-height: 85%;
      border-top: 1px solid var(--border-strong);
      border-radius: 12px 12px 0 0;
      box-shadow: 0 -6px 18px rgb(0 0 0 / 0.12);
    }
  }
</style>

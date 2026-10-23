<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onDestroy, onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
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
    planDiff,
    schemaAnnouncement,
    schemaChanges,
  } from '$lib/explore-time';
  import type { EdgeTriple, SchemaChanges } from '$lib/explore-time';
  import ClassTree from '$components/ClassTree.svelte';
  import GeoMapCard from '$components/GeoMapCard.svelte';
  import GraphView, { type GEdge, type GNode } from '$components/GraphView.svelte';
  import Icon from '$components/Icon.svelte';
  import SchemaDiffDialog from '$components/SchemaDiffDialog.svelte';
  import ShapesDraftDialog from '$components/ShapesDraftDialog.svelte';
  import { coverage, objectKinds, shortenExpression, valuesRange } from '$lib/schema-history';
  import SimilarPanel from '$components/SimilarPanel.svelte';
  import TermView from '$components/TermView.svelte';
  import TimeSlider from '$components/TimeSlider.svelte';
  import MemoryTab from '$components/MemoryTab.svelte';
  import OverviewView from '$components/OverviewView.svelte';
  import DiffGraph from '$components/DiffGraph.svelte';
  import { quadIds, seqOf, stepAnnouncement, STEP_QUADS } from '$lib/diff-graph';
  import TextSearchView from '$components/TextSearchView.svelte';

  type ENode = {
    id: string;
    term: Term;
    iri: string;
    label: string;
    expanded: boolean;
    lit: boolean;
  };
  type EEdge = GEdge & { lit: boolean };

  const RDFS_COMMENT = 'http://www.w3.org/2000/01/rdf-schema#comment';
  const MAX_NODES = 1500;
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

  /** Take the search text to the query page's Ask bar. */
  function askInQuery() {
    app.pendingQuery = { query: '', ask: query.trim() };
    goto(resolve('/query'));
  }

  type Tab = 'graph' | 'schema' | 'search';
  const urlTab = page.url.searchParams.get('tab');
  let tab = $state<Tab>(urlTab === 'schema' || urlTab === 'search' ? urlTab : 'graph');
  /** The view of the selected resource: its properties or its memory (`&view=memory`). */
  let sideTab = $state<'props' | 'memory'>(
    page.url.searchParams.get('view') === 'memory' ? 'memory' : 'props',
  );
  /** Full-text query of the Search tab (`&q=`). */
  let textQuery = $state(page.url.searchParams.get('q') ?? '');
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
    changes = null;
  }
  let graphView: GraphView | undefined = $state();
  // the reads of a view that goes away stop with it
  onDestroy(() => {
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
      if (d) tab = 'graph';
    });
  });
  function openDiff(to: string, from: string) {
    diffView = { to, from, branch: '', group: false };
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
    branchNames = [];
    if (!name) return;
    const ctl = new AbortController();
    api
      .branches(name, ctl.signal)
      .then((l) => {
        if (ds === name) branchNames = l.branches.map((b) => b.name).filter((n) => n !== 'main');
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
        api.historyChanges(name, { subject: iri, order: 'asc', limit: 5000, signal }),
        api.historyChanges(name, { object: iri, order: 'asc', limit: 5000, signal }),
        api.commits(name, { limit: 1, signal }),
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
    resetTime();
  }

  // the properties are read again when a slider moves
  $effect(() => {
    void exAt;
    void timeActive;
    const id = selectedId;
    if (id) untrack(() => void loadDetails(id));
  });

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

  const gNodes = $derived.by((): GNode[] => {
    const toG = (n: ENode, change?: 'added' | 'removed'): GNode => ({
      id: n.id,
      label: n.label,
      kind: n.term.type === 'uri' ? 'iri' : n.term.type,
      title: n.term.type === 'uri' ? n.term.value : undefined,
      focus: n.id === focusId,
      expanded: n.expanded,
      change,
    });
    const real = Object.values(nodes)
      .filter((n) => showLiterals || !n.lit)
      .map((n) => toG(n, changes?.addedNodes.has(n.id) ? 'added' : undefined));
    const ghosts = Object.values(changes?.ghostNodes ?? {})
      .filter((n) => showLiterals || !n.lit)
      .map((n) => toG(n, 'removed'));
    return [...real, ...ghosts];
  });
  const gEdges = $derived.by((): GEdge[] => {
    const keep = (e: EEdge) => (showLiterals || !e.lit) && (!hideTypes || !e.isType);
    const real = Object.values(edges)
      .filter(keep)
      .map((e) => (changes?.addedEdges.has(e.id) ? { ...e, change: 'added' as const } : e));
    const ghosts = Object.values(changes?.ghostEdges ?? {})
      .filter(keep)
      .map((e) => ({ ...e, change: 'removed' as const }));
    return [...real, ...ghosts];
  });
  const nodeCount = $derived(Object.keys(nodes).length);

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
      label: labelFor(term, label),
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

  async function expand(id: string) {
    const n = nodes[id];
    if (!n || n.term.type !== 'uri' || !ds || expanding[id]) return;
    const gen = graphGen.current;
    const stale = () => !graphGen.isCurrent(gen) || nodes[id] !== n;
    if (nodeCount > MAX_NODES) {
      toasts.push(
        'error',
        'Graph is getting large',
        `Over ${MAX_NODES} nodes. Remove some or refocus before expanding.`,
      );
      return;
    }
    expanding[id] = gen;
    const signal = expandCtl.signal;
    try {
      const r = await ex.neighbours(
        ds,
        n.iri,
        timeActive && graphTimeOpen ? exAt || undefined : undefined,
        signal,
      );
      if (stale()) return;
      populate(n, r);
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
      if (!stale() && !isAbort(e)) toasts.error(`Could not expand ${n.label}`, e);
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
    const d = await api.diff(name, {
      // across branches, the later side is this branch (main here) and the earlier one the other's head
      ...(exAgainst ? { fromBranch: exAgainst, toBranch: 'main' } : { from: exFrom }),
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
      const leftDs = exAgainst ? `${name}@${exAgainst}` : name;
      const leftAt = exAgainst ? undefined : exFrom;
      const [before, after] = await Promise.all([
        ex.presentEdges(leftDs, [...plan.addedTriples.values()], leftAt, signal),
        ex.presentEdges(
          name,
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
        label: labelFor(g.term),
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
      partial
        ? `The comparison listed ${fmtInt(listed)} of ${fmtInt(d.added + d.removed)} changes, so the marks are partial.`
        : '',
    ]
      .filter(Boolean)
      .join(' ');
  }

  /**
   * Read the graph again at the state the graph slider names: the focus and the nodes the
   * person expanded, in the order they were expanded, are read at that commit, and an
   * expanded node that did not exist then is skipped.
   */
  async function rereadGraph() {
    const name = ds;
    const root = focusId;
    if (!name || !root) return;
    const owns = graphRuns.claim('graph');
    const signal = graphRuns.signal('graph');
    const at = graphTimeOpen && exAt ? exAt : undefined;
    graphReading = true;
    graphNote = '';
    const t0 = performance.now();
    try {
      const present = new Set([root]);
      const fetched = new Map<string, Awaited<ReturnType<typeof ex.neighbours>>>();
      for (const id of [...expandLog]) {
        const term = seenTerms.get(id);
        if (!present.has(id) || !term || term.type !== 'uri') continue;
        const r = await ex.neighbours(name, term.value, at, signal);
        if (!owns()) return;
        fetched.set(id, r);
        for (const nb of [...r.out, ...r.inc]) {
          if (nb.term.type === 'literal') continue;
          present.add(termKey(nb.term));
        }
      }
      // the graph is rebuilt in one step, so that what stays keeps its place
      const rootTerm = seenTerms.get(root);
      if (!rootTerm) return;
      const keepLabels = new Map(Object.values(nodes).map((n) => [n.id, n.label]));
      nodes = {};
      edges = {};
      const rn = ensureNode(rootTerm, keepLabels.get(root), root);
      for (const id of expandLog) {
        const r = fetched.get(id);
        const n = id === root ? rn : nodes[id];
        if (!r || !n) continue;
        populate(n, r);
        n.expanded = true;
      }
      for (const [id, n] of Object.entries(nodes)) {
        const kept = keepLabels.get(id);
        if (kept && n.term.type === 'uri' && n.label === labelFor(n.term)) n.label = kept;
      }
      await markChanges(owns, signal);
      if (!owns()) return;
      if (!graphTimeOpen) changes = null;
      graphSlider?.accept();
      graphReadMs = performance.now() - t0;
      if (!(graphTimeOpen && exCompare)) {
        graphAnnounce = at
          ? `Commit ${at.replace(/^commit:/, '')}: ${Object.keys(nodes).length} nodes`
          : 'The graph at the head';
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
      toasts.error('Could not read the graph at that commit', e);
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
    if (!name || !focusId) return;
    const owns = graphRuns.claim('graph');
    const signal = graphRuns.signal('graph');
    graphReading = true;
    graphNote = '';
    const t0 = performance.now();
    try {
      const iris = Object.values(nodes)
        .filter((n) => n.term.type === 'uri')
        .map((n) => n.iri)
        .slice(0, 1000);
      const d = await api.diff(name, {
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
        Object.keys(nodes).length + fresh.size > MAX_NODES
      ) {
        graphReading = false;
        await rereadGraph();
        if (graphTimeOpen)
          graphNote = `Commit ${d.to.commit.seq} changed more than a step can apply, so the graph was read again at it.`;
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
      const holds = candidates.size
        ? await ex.presentEdges(
            name,
            [...candidates.values()],
            toAt === 'head' ? undefined : toAt,
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
      toasts.error('Could not read the changes of that commit', e);
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
    if (tab === 'search' && textQuery.trim()) p.set('q', textQuery.trim());
    if (iri && sideTab === 'memory') p.set('view', 'memory');
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
    selectedId = id;
    if (id) void loadDetails(id);
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
      d = await ex.details(ds, n.iri, at || undefined, signal);
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
    // Drop literal nodes that belonged to the removed node.
    nodes = Object.fromEntries(Object.entries(rest).filter(([k]) => !k.startsWith(`lit:${id}|`)));
    if (selectedId === id) selectedId = null;
    if (focusId === id) focusId = null;
  }

  /** Add a value from the properties panel as a node linked to the selected resource. */
  function addLinked(from: string, predicate: string, term: Term, incoming = false) {
    const target = ensureNode(term);
    if (incoming) addEdge(target.id, predicate, from, false);
    else addEdge(from, predicate, target.id, false);
    select(target.id);
    queueMicrotask(() => graphView?.center(target.id));
  }

  function clearGraph() {
    resetGraph();
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
  const comment = $derived(grouped.find(([p]) => p === RDFS_COMMENT)?.[1]);

  // --- search -------------------------------------------------------------------

  let query = $state('');
  let hits = $state<ex.SearchHit[]>([]);
  let searching = $state(false);
  let searchOpen = $state(false);
  let searchErr = $state<string | null>(null);
  let active = $state(0);
  let searchCtl: AbortController | null = null;
  let debounce: ReturnType<typeof setTimeout> | undefined;

  const directIri = $derived.by(() => {
    const q = query.trim();
    if (!q) return null;
    const m = /^<(.+)>$/.exec(q);
    if (m) return m[1];
    if (/^(https?|urn|mailto|file):/i.test(q)) return q;
    const pn = /^([A-Za-z][\w.-]*)?:(\S*)$/.exec(q);
    if (pn && prefixes[pn[1] ?? ''] != null) return prefixes[pn[1] ?? ''] + pn[2];
    return null;
  });
  const options = $derived<{ iri: string; label?: string; direct?: boolean }[]>([
    ...(directIri ? [{ iri: directIri, direct: true }] : []),
    ...hits.filter((h) => h.iri !== directIri),
  ]);

  function onSearchInput() {
    clearTimeout(debounce);
    searchOpen = true;
    active = 0;
    const q = query.trim();
    if (q.length < 2 || !ds) {
      hits = [];
      searching = false;
      return;
    }
    // Show the spinner (not "Nothing matches") while the debounce is pending.
    searching = true;
    debounce = setTimeout(async () => {
      searchCtl?.abort();
      const ctl = (searchCtl = new AbortController());
      searching = true;
      searchErr = null;
      try {
        hits = await ex.search(ds!, q, ctl.signal);
      } catch (e) {
        if (!(e instanceof DOMException && e.name === 'AbortError'))
          searchErr = api.errorMessage(e);
      } finally {
        if (searchCtl === ctl) searching = false;
      }
    }, 250);
  }

  function pick(o: { iri: string; label?: string }) {
    searchOpen = false;
    query = '';
    hits = [];
    void focusOn(o.iri, o.label);
  }

  function onSearchKey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      active = Math.min(active + 1, options.length - 1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      active = Math.max(active - 1, 0);
    } else if (e.key === 'Enter' && options[active]) {
      e.preventDefault();
      pick(options[active]);
    } else if (e.key === 'Escape') {
      searchOpen = false;
    }
  }

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

  let lastDs: string | null = null;
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
        focusId = null;
        selectedId = null;
        schema = null;
        schemaFor = null;
        schemaGraph = 'default';
        selectedClass = null;
        syncUrl(null);
      }
      if (tab === 'schema') void loadSchema(name);
    });
  });

  $effect(() => {
    if (tab === 'schema' && ds && schemaFor !== ds && !schemaLoading)
      untrack(() => loadSchema(ds!));
  });

  // React to ?iri= changes (e.g. clicking an IRI on the Query page).
  $effect(() => {
    const iri = page.url.searchParams.get('iri');
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
      if (iri && (!focusId || nodes[focusId]?.iri !== iri)) void focusOn(iri, undefined, true);
    });
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
      if (searchOpen && !(e.target as HTMLElement).closest('.search')) searchOpen = false;
    };
    // H opens and closes the History strip of the selected resource, Shift+H the one over the graph
    const onKey = (e: KeyboardEvent) => {
      if ((e.key !== 'h' && e.key !== 'H') || e.metaKey || e.ctrlKey || e.altKey) return;
      if ((e.target as HTMLElement)?.closest?.('input, textarea, select, [contenteditable]'))
        return;
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

<div class="page">
  <header class="bar">
    <div class="search">
      <Icon name="search" size={15} />
      <input
        class="search-input"
        placeholder={ds
          ? `Find a resource in ${ds} by label or IRI${canAsk ? ', or ask a question' : ''}`
          : 'Choose a dataset first'}
        bind:value={query}
        oninput={onSearchInput}
        onfocus={() => (searchOpen = true)}
        onkeydown={onSearchKey}
        disabled={!ds}
        role="combobox"
        aria-expanded={searchOpen && options.length > 0}
        aria-controls="search-results"
        aria-autocomplete="list"
      />
      {#if searching}<span class="spinner"></span>{/if}
      {#if searchOpen && query.trim().length >= 2}
        <div class="results" id="search-results" role="listbox">
          {#each options as o, i (o.iri + (o.direct ? '!' : ''))}
            <button
              role="option"
              aria-selected={i === active}
              class="hit"
              class:active={i === active}
              onmouseenter={() => (active = i)}
              onclick={() => pick(o)}
            >
              {#if o.direct}
                <Icon name="target" size={14} />
                <span>Open <span class="mono t-iri">{displayIri(o.iri, prefixes)}</span></span>
              {:else}
                <span class="hit-label">{o.label ?? shortLabel(o.iri, prefixes)}</span>
                <span class="hit-iri mono">{displayIri(o.iri, prefixes)}</span>
              {/if}
            </button>
          {:else}
            {#if !searching}
              <div class="hit none">{searchErr ?? `Nothing matches “${query.trim()}”.`}</div>
            {/if}
          {/each}
          {#if canAsk}
            <button
              role="option"
              aria-selected="false"
              class="hit ask-hint"
              onclick={askInQuery}
              title="Open the query page with this as a question"
            >
              <Icon name="sparkle" size={14} />
              <span>Ask a question… <span class="faint">“{query.trim()}”</span></span>
            </button>
          {/if}
        </div>
      {/if}
    </div>
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
    <div class="split" class:overview={nodeCount === 0 && !graphTimeOpen}>
      <div class="canvas">
        {#if nodeCount > 0 || graphTimeOpen}
          <div class="gtools">
            <label class="row check"
              ><input type="checkbox" bind:checked={showLiterals} /> Literals</label
            >
            <label class="row check"
              ><input type="checkbox" bind:checked={hideTypes} /> Hide rdf:type</label
            >
            <span class="spacer"></span>
            <span class="faint">{gNodes.length} nodes, {gEdges.length} edges</span>
            {#if nodeCount}
              <button
                class="btn sm"
                aria-pressed={graphTimeOpen}
                aria-label="Graph history"
                title="Move the whole graph through the commits (Shift+H)"
                onclick={() => (graphTimeOpen ? closeGraphTime() : openGraphTime())}
                ><Icon name="clock" size={13} /> History</button
              >
              <button class="btn sm ghost" onclick={clearGraph}
                ><Icon name="trash" size={13} /> Clear</button
              >
            {/if}
          </div>
        {/if}
        {#if graphTimeOpen && ds}
          <div class="graph-time">
            <TimeSlider
              bind:this={graphSlider}
              {ds}
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
              onchange={() => syncUrl(selected?.term.type === 'uri' ? selected.iri : null)}
              onclose={() => {
                closeGraphTime();
                syncUrl(selected?.term.type === 'uri' ? selected.iri : null);
              }}
            />
            {#if graphNote}<p class="faint small graph-note">{graphNote}</p>{/if}
          </div>
        {/if}
        {#if nodeCount === 0}
          <div class="ov-host">
            {#key ds}
              <OverviewView
                {ds}
                {prefixes}
                onresource={(iri, label) => focusOn(iri, label)}
                onschema={() => setTab('schema')}
              />
            {/key}
          </div>
        {:else}
          <div class="cy-host">
            <GraphView
              bind:this={graphView}
              animate={graphTimeOpen}
              nodes={gNodes}
              edges={gEdges}
              selected={selectedId}
              onselect={select}
              onexpand={expand}
            />
          </div>
          <div class="hint faint">
            Double-click a node to load its neighbours. Blue-ringed nodes are expanded.
          </div>
        {/if}
      </div>

      {#if nodeCount > 0 || graphTimeOpen}
        <aside class="side" aria-label="Resource details">
          {#if selected}
            {@const n = selected}
            <div class="side-head">
              <div class="side-title">
                <h2 title={n.label}>{n.label}</h2>
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
                  <button
                    class="btn sm"
                    aria-pressed={stripOpen}
                    title="Move through the commits that changed this resource (H)"
                    onclick={() => (stripOpen ? closeStrip() : (stripOpen = true))}
                    ><Icon name="clock" size={13} /> History</button
                  >
                  <button
                    class="btn sm ghost icon"
                    title="Remove from graph"
                    aria-label="Remove from graph"
                    onclick={() => removeNode(n.id)}
                  >
                    <Icon name="x" size={13} />
                  </button>
                </div>
                <div class="tabs side-tabs" role="tablist" aria-label="Resource views">
                  <button
                    class="tab"
                    role="tab"
                    aria-selected={sideTab === 'props'}
                    onclick={() => (sideTab = 'props')}>Properties</button
                  >
                  <button
                    class="tab"
                    role="tab"
                    aria-selected={sideTab === 'memory'}
                    onclick={() => (sideTab = 'memory')}
                    title="What the memory you can read holds about it, with citations"
                    ><Icon name="brain" size={13} /> Memory</button
                  >
                </div>
                {#if stripOpen && sideTab === 'props'}
                  {#if hist}
                    <TimeSlider
                      bind:this={strip}
                      {ds}
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
                      <p class="faint small">The change log holds no change of this resource.</p>
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
                    <p class="faint row"><span class="spinner"></span> Reading the history…</p>
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
            {:else if sideTab === 'memory' && ds}
              <div class="side-body scroll">
                {#key n.iri}
                  <MemoryTab {ds} iri={n.iri} {prefixes} onopen={(iri) => focusOn(iri)} />
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
                      <p class="faint small">Nothing about this resource differs between them.</p>
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
                                : ' · state not readable'}
                            </span>
                          </li>
                        {/each}
                      </ul>
                    {/if}
                  </section>
                {/if}
                <h3 class="sub">Properties <span class="faint">{detail.props.length}</span></h3>
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
                            <div class="val">
                              <TermView
                                term={o}
                                {prefixes}
                                expandable
                                onopen={(iri) => addLinked(n.id, p, { type: 'uri', value: iri })}
                              />
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
                  <GeoMapCard {ds} iri={n.iri} props={detail.props} {prefixes} />
                  <SimilarPanel {ds} iri={n.iri} props={detail.props} {prefixes} />
                {/key}

                <h3 class="sub">
                  Referenced by <span class="faint">{fmtInt(detail.incomingTotal)}</span>
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
              title="Every count is exact at this snapshot of the data (triples {fmtInt(
                schema.totals.triples,
              )})">{ex.snapshotLine(schema.snapshot)}</span
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
            <div class="empty"><span class="spinner"></span> Reading the ontology…</div>
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
    align-items: center;
    gap: 16px;
    padding: 8px 12px 0;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .search {
    position: relative;
    flex: 1;
    min-width: 0;
    max-width: 620px;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 32px;
    padding: 0 10px;
    margin-bottom: 8px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--bg);
    color: var(--text-3);
  }
  .search:focus-within {
    border-color: var(--iri);
    box-shadow: 0 0 0 3px rgba(36, 89, 199, 0.15);
  }
  .search-input {
    flex: 1;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--text);
    font: inherit;
    min-width: 0;
  }
  .results {
    position: absolute;
    left: -1px;
    right: -1px;
    top: calc(100% + 4px);
    z-index: 40;
    max-height: 380px;
    overflow: auto;
    padding: 4px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r);
    box-shadow: var(--shadow-pop);
  }
  .hit {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    padding: 6px 8px;
    border-radius: 4px;
    cursor: pointer;
    color: var(--text);
  }
  .hit.active {
    background: color-mix(in srgb, var(--iri) 12%, transparent);
  }
  .hit.none {
    cursor: default;
    color: var(--text-2);
  }
  .hit-label {
    font-weight: 500;
    white-space: nowrap;
  }
  .hit-iri {
    font-size: 11px;
    color: var(--text-3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
  .canvas {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    /* node labels near the edge must not paint over the details panel */
    overflow: hidden;
  }
  .gtools {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 6px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
    font-size: var(--fs-sm);
  }
  .check {
    gap: 5px;
    cursor: pointer;
  }
  .cy-host {
    flex: 1;
    min-height: 0;
  }
  .ov-host {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
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
    gap: 4px;
  }
  .side-tabs {
    margin-top: 8px;
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
  .bar > .tabs {
    flex-shrink: 0;
  }
  @media (max-width: 1000px) {
    .bar {
      gap: 8px;
    }
    .bar > .tabs {
      flex-shrink: 1;
    }
    .split {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: minmax(320px, 1fr) auto;
    }
    .side {
      border-left: 0;
      border-top: 1px solid var(--border);
      max-height: 50vh;
    }
  }
  /* a phone: the search box on a row of its own, the tabs under it */
  @media (max-width: 600px) {
    .bar {
      flex-wrap: wrap;
    }
    .search {
      flex-basis: 100%;
      max-width: none;
    }
  }
</style>

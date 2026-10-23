<script lang="ts" module>
  /** A GraphRAG community the overview shows, for the page's report panel. */
  export type OverviewCommunity = { iri: string; title: string; level?: number };
</script>

<script lang="ts">
  /**
   * The graph overview (spec U06 §2.3): the dataset's classes and the predicates between
   * them, from the class profiles, and semantic zoom into one class or group at a time
   * through `POST /$/graph/{ds}/groups`. A click or tap opens a class or a group in place,
   * a member opens in the resource view, and the breadcrumb (or Backspace) closes levels.
   * The list view (L) is the accessible companion of the canvas, and the view falls back
   * to Cytoscape without WebGL.
   *
   * The group-by picker can start the overview from the GraphRAG communities (C22)
   * instead: the communities of one level of the hierarchy, sized by their members and
   * coloured by their report's status, which open into the level below and, at the
   * finest level, into their members. The selected or opened community goes to
   * `oncommunity`, so the page can show its report beside the canvas, and `marked`
   * entities, such as those a chosen finding is about, are ringed on the canvas.
   *
   * The overview moves through time (U01 Phase 4): `Shift+H` opens the time slider, which
   * reads the class level at its commit, labels each class and edge with its change in
   * compare mode, and plays a time-lapse that applies each step's grouped diff to the counts
   * while the positions hold still.
   * The history reads the classes only, so it is not offered while the communities are
   * drawn.
   */
  import { onDestroy, onMount, untrack } from 'svelte';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { ApiError, type ClassProfile, type GroupsAnswer, type GroupsBy } from '$lib/api';
  import { fmtCompact, fmtInt } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import * as gr from '$lib/graphrag';
  import type { PrefixMap } from '$lib/rdf';
  import { countsText, markOf, seqOf } from '$lib/diff-graph';
  import { timeFailure } from '$lib/timeline';
  import {
    applyOverviewChanges,
    overviewChanges,
    withRemoved,
    type OvChanges,
    classLevel,
    communityLevel,
    describe,
    fallbackNode,
    hasWebGL2,
    isReports,
    MAX_CLASSES,
    nodeRadius,
    seed,
    visible,
    type OvGraph,
    type OvLevel,
    type OvNode,
  } from '$lib/overview';
  import GraphListView, { type ListRow } from './GraphListView.svelte';
  import GraphView, { type GEdge, type GNode } from './GraphView.svelte';
  import Icon from './Icon.svelte';
  import LargeGraphView, { type LEdge, type LNode } from './LargeGraphView.svelte';
  import TimeSlider from './TimeSlider.svelte';

  let {
    ds,
    prefixes,
    onresource,
    onschema,
    branch = null,
    start = 'classes',
    focus = null,
    marked = null,
    oncommunity,
    onmode,
  }: {
    ds: string;
    prefixes: PrefixMap;
    /** Open a member in the resource view. */
    onresource: (iri: string, label?: string) => void;
    /** Show the schema tab. */
    onschema?: () => void;
    /** The branch the page shows, null for main. The GraphRAG reports belong to main. */
    branch?: string | null;
    /** What the overview starts from. */
    start?: 'classes' | 'communities';
    /** A community to open the communities down to, and select. */
    focus?: string | null;
    /** IRIs of entities to ring on the canvas. */
    marked?: string[] | null;
    /** The community selected, or opened when none is selected; null for none. */
    oncommunity?: (c: OverviewCommunity | null) => void;
    /** The starting point changed. */
    onmode?: (m: 'classes' | 'communities') => void;
  } = $props();

  let profiles = $state<ClassProfile[] | null>(null);
  let loadError = $state<string | null>(null);
  let loading = $state(false);
  let loadMs = $state(0);
  let builtins = $state(true);
  let path = $state<OvLevel[]>([]);
  let opening = $state<string | null>(null);
  let openError = $state<string | null>(null);
  /** The commit the base level was read at: every level must come from it. */
  let baseCommit: number | null = null;
  let changed = $state(false);
  let selected = $state<string | null>(null);
  let listView = $state(false);
  let webgl = $state(true);
  let failReason = $state<string | null>(null);
  let by = $state<string>('auto');
  let palette = $state<string[]>(['#2459c7']);
  let wrap: HTMLDivElement | undefined = $state();
  let large: LargeGraphView | undefined = $state();
  /** Positions of the nodes laid out so far, kept across levels. */
  const positions = new Map<string, { x: number; y: number }>();
  let controller: AbortController | undefined;

  // the GraphRAG communities
  let mode = $state<'classes' | 'communities'>(untrack(() => start));
  let reports = $state<GroupsAnswer | null>(null);
  let reportsError = $state<string | null>(null);
  let reportsLoading = $state(false);
  let colorBy = $state<'status' | 'community'>('status');
  let statusColors = $state<Record<gr.CommunityStatus, string>>({
    reviewed: '#1f8a4c',
    unreviewed: '#2459c7',
    stale: '#b7791f',
    pending: '#8a8f98',
    rejected: '#c53030',
    failed: '#c53030',
  });

  const label = (iri: string) => shortLabel(iri, prefixes);
  const communities = $derived(mode === 'communities');
  const offMain = $derived(communities && !!branch);
  const reportsReady = $derived(communities && !offMain && reports?.available === true);

  // --- the overview through time (U01 Phase 4) ------------------------------------------

  let timeOpen = $state(false);
  let ovAt = $state('');
  let ovFrom = $state('');
  let ovCompare = $state(false);
  let playMode = $state<'reread' | 'lapse'>('reread');
  let playing = $state(false);
  let timeBusy = $state(false);
  let timeMs = $state(0);
  let timeAnnounce = $state('');
  let timeNote = $state('');
  let slider: TimeSlider | undefined = $state();
  /** The changes the labels show: of the compared range, or of the last time-lapse step. */
  let marks = $state<OvChanges | null>(null);
  /** The class level as the time-lapse has moved it, in place of the profiles'. */
  let lapsed = $state<OvGraph | null>(null);
  /** The state the picture shows, which a time-lapse step starts from. */
  let drawnAt = '';
  let timeCtl: AbortController | undefined;

  /** The classes the profiles have instances of, drawn or filtered out. */
  const presentClasses = $derived(
    new Set((profiles ?? []).filter((p) => p.instances > 0).map((p) => p.class)),
  );
  const profileBase = $derived.by(() =>
    profiles ? classLevel(profiles, { builtins, label }) : { nodes: [], edges: [], hidden: 0 },
  );
  const base = $derived.by((): OvGraph & { hidden: number } => {
    if (communities) {
      return reportsReady && reports
        ? { ...communityLevel(reports, label), hidden: 0 }
        : { nodes: [], edges: [], hidden: 0 };
    }
    if (lapsed) return { ...lapsed, hidden: profileBase.hidden };
    // a comparison also draws the classes and edges it removed entirely, with their marks
    if (ovCompare && marks)
      return {
        ...withRemoved(profileBase, marks, label, presentClasses),
        hidden: profileBase.hidden,
      };
    return profileBase;
  });
  const picture: OvGraph = $derived(visible(base, path, label));
  const startLevel = $derived(reports?.within.reports?.level ?? 0);
  const announce = $derived(
    communities
      ? reportsReady
        ? describe(picture, path, fmtInt, startLevel)
        : ''
      : profiles
        ? describe(picture, path, fmtInt)
        : '',
  );
  const classColor = $derived.by(() => {
    const m = new Map<string, string>();
    base.nodes.forEach((n, i) => m.set(n.cls, palette[i % palette.length]));
    return m;
  });
  const maxSize = $derived(Math.max(1, ...picture.nodes.map((n) => n.size)));
  const degree = $derived.by(() => {
    const d = new Map<string, number>();
    for (const e of picture.edges) {
      d.set(e.source, (d.get(e.source) ?? 0) + e.count);
      d.set(e.target, (d.get(e.target) ?? 0) + e.count);
    }
    return d;
  });
  const byKey = $derived(new Map(picture.nodes.map((n) => [n.key, n])));
  const sel: OvNode | undefined = $derived(selected ? byKey.get(selected) : undefined);
  const current = $derived(path.length ? path[path.length - 1] : null);

  /** The colour of a node: its report's status, or its class or community. */
  function colorOf(n: OvNode): string {
    if (communities && colorBy === 'status') {
      if (n.status) return statusColors[n.status];
      return 'var(--text-3)';
    }
    return classColor.get(n.cls) ?? palette[0];
  }
  /** A drawn colour: a CSS variable is no colour to WebGL. */
  function drawn(c: string): string {
    return c.startsWith('var(') ? statusColors.pending : c;
  }

  /** The community the page shows: the selected one, else the one last opened. */
  const shownCommunity = $derived.by((): OverviewCommunity | null => {
    if (!communities) return null;
    if (sel?.community) return { iri: sel.community, title: sel.label, level: sel.level };
    const w = current?.answer.within;
    if (!sel && w?.community)
      return { iri: w.community, title: w.title ?? current!.label, level: w.level };
    return null;
  });
  let lastShown: string | null = null;
  $effect(() => {
    const c = shownCommunity;
    const key = c?.iri ?? null;
    untrack(() => {
      if (key === lastShown) return;
      lastShown = key;
      oncommunity?.(c);
    });
  });

  const markedKeys = $derived.by(() => {
    if (!marked?.length) return null;
    const want = new Set(marked.map((i) => `m:${i}`));
    return picture.nodes.filter((n) => want.has(n.key)).map((n) => n.key);
  });

  function nodeLabel(n: OvNode) {
    if (n.kind === 'member') return n.label;
    const c = n.kind === 'class' ? marks?.classes.get(n.cls) : undefined;
    const change = c ? countsText(c.added, c.removed) : '';
    return `${n.label} (${fmtCompact(n.size)})${change ? ` ${change}` : ''}`;
  }
  function nodeMark(n: OvNode) {
    const c = n.kind === 'class' ? marks?.classes.get(n.cls) : undefined;
    return c ? markOf(c.added, c.removed) : undefined;
  }
  function edgeMark(key: string) {
    const c = marks?.edges.get(key);
    return c ? markOf(c.added, c.removed) : undefined;
  }
  let changeColors = $state({ added: '#1a7f37', removed: '#cf222e' });
  const tint = (m: string | undefined, fallback: string) =>
    m === 'added' ? changeColors.added : m === 'removed' ? changeColors.removed : fallback;

  const lnodes: LNode[] = $derived.by(() => {
    // a new level is placed around the node it opened from
    const anchor = current ? positions.get(current.key) : undefined;
    const spread = anchor ? 40 + Math.sqrt(picture.nodes.length) * 6 : 200;
    return picture.nodes.map((n) => {
      const known = positions.get(n.key);
      const p = known ?? seed(n.key, anchor, spread);
      return {
        key: n.key,
        label: nodeLabel(n),
        size: nodeRadius(n.size, maxSize, n.kind),
        color: tint(nodeMark(n), drawn(colorOf(n))),
        x: p.x,
        y: p.y,
        fixed: !!known,
      };
    });
  });
  const ledges: LEdge[] = $derived.by(() => {
    const max = Math.max(1, ...picture.edges.map((e) => e.count));
    return picture.edges.map((e) => ({
      key: e.key,
      source: e.source,
      target: e.target,
      size: 0.6 + (2.4 * Math.log1p(e.count)) / Math.log1p(max),
      ...(edgeMark(e.key) ? { color: tint(edgeMark(e.key), '') } : {}),
    }));
  });
  const markedSet = $derived(new Set(markedKeys ?? []));
  const rows: ListRow[] = $derived(
    picture.nodes.map((n) => ({
      key: n.key,
      // a class row also names its change in the history; a community row only its title
      label: communities ? n.label : nodeLabel(n),
      kind: n.kind,
      size: n.size,
      cls: communities ? (n.status ?? '') : label(n.cls),
      degree: degree.get(n.key) ?? 0,
      color: colorOf(n),
      openable: n.openable,
      marked: markedSet.has(n.key),
    })),
  );
  // the Cytoscape fallback
  const gnodes: GNode[] = $derived(
    picture.nodes.map((n) => ({
      ...fallbackNode(n, nodeLabel(n), drawn(colorOf(n)), markedSet.has(n.key)),
      change: nodeMark(n),
    })),
  );
  const gedges: GEdge[] = $derived(
    picture.edges.map((e) => ({
      id: e.key,
      source: e.source,
      target: e.target,
      label: `${e.predicates[0]?.iri ? label(e.predicates[0].iri) : ''}${e.predicates.length > 1 ? ` +${e.predicates.length - 1}` : ''} · ${fmtCompact(e.count)}${marks?.edges.get(e.key) ? ` ${countsText(marks.edges.get(e.key)!.added, marks.edges.get(e.key)!.removed)}` : ''}`,
      change: edgeMark(e.key),
    })),
  );

  function readPalette() {
    if (!wrap) return;
    const css = getComputedStyle(wrap);
    const names = ['--iri', '--literal', '--bnode', '--num', '--var', '--ok', '--spark', '--kw'];
    palette = names.map((n) => css.getPropertyValue(n).trim()).filter(Boolean);
    if (!palette.length) palette = ['#2459c7'];
    const v = (n: string, fallback: string) => css.getPropertyValue(n).trim() || fallback;
    statusColors = {
      reviewed: v('--ok', statusColors.reviewed),
      unreviewed: v('--iri', statusColors.unreviewed),
      stale: v('--warn', statusColors.stale),
      pending: v('--text-3', statusColors.pending),
      rejected: v('--danger', statusColors.rejected),
      failed: v('--danger', statusColors.failed),
    };
    changeColors = {
      added: css.getPropertyValue('--added').trim() || changeColors.added,
      removed: css.getPropertyValue('--removed').trim() || changeColors.removed,
    };
  }

  function errorText(e: unknown): string {
    if (e instanceof ApiError) {
      if (e.status === 507)
        return `${e.message}. The group stays closed: it needs more than your memory budget allows.`;
      if (e.status === 408) return `${e.message}. The group stays closed.`;
      return e.message;
    }
    return e instanceof Error ? e.message : String(e);
  }

  /** Read the class profiles at `at` and draw them; a failure is the caller's. */
  async function readProfiles(at: string | undefined, signal: AbortSignal) {
    const t = performance.now();
    const p = await api.schemaProfiles(ds, { signal, ...(at ? { at } : {}) });
    if (signal.aborted) throw new DOMException('superseded', 'AbortError');
    profiles = p.classes;
    if (mode === 'classes') baseCommit = p.snapshot.commit ?? null;
    loadMs = performance.now() - t;
    // a dataset of built-in classes only keeps them, others start without them
    builtins = !p.classes.some((c) => !c.builtin && c.instances > 0);
  }

  async function load(at?: string) {
    controller?.abort();
    controller = new AbortController();
    loading = true;
    loadError = null;
    try {
      await readProfiles(at, controller.signal);
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') return;
      loadError = errorText(e);
    } finally {
      loading = false;
    }
  }

  /** The communities of the whole dataset at `level` (the coarsest by default). */
  async function loadCommunities(level?: number) {
    if (branch) return;
    controller?.abort();
    controller = new AbortController();
    reportsLoading = true;
    reportsError = null;
    const t = performance.now();
    try {
      const a = await api.graphGroups(
        ds,
        { by: { reports: level != null ? { level } : {} } },
        controller.signal,
      );
      reports = a;
      baseCommit = a.commit;
      loadMs = performance.now() - t;
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') return;
      reportsError = errorText(e);
    } finally {
      reportsLoading = false;
    }
  }

  function setMode(m: 'classes' | 'communities') {
    if (m === mode) return;
    controller?.abort();
    opening = null;
    openError = null;
    changed = false;
    path = [];
    selected = null;
    by = 'auto';
    mode = m;
    onmode?.(m);
    // the history reads the classes only
    if (m === 'communities' && timeOpen) resetTime();
    if (m === 'communities') {
      if (reports) baseCommit = reports.commit;
      else void loadCommunities();
    } else {
      // the classes are read again, at a commit the levels below can be compared with
      void load();
    }
  }

  function byOf(value: string): GroupsBy {
    if (value === 'auto' || value === 'members') return value;
    if (value.startsWith('community:')) return { community: value.slice(10) };
    return { property: value.slice(9) };
  }

  /** Open a class or a group: the path keeps the levels above it. */
  async function open(key: string, how: GroupsBy = 'auto') {
    // opening a class pauses the time-lapse; groups are read at the head
    if (playing) slider?.pause();
    if (timeOpen && (ovAt || ovCompare || lapsed) && !key.startsWith('m:')) {
      await closeTime();
      timeNote = 'Groups are read at the head, so the overview went back to it.';
    }
    if (key.startsWith('m:')) {
      onresource(key.slice(2), byKey.get(key)?.label);
      return;
    }
    const target = byKey.get(key);
    if (!target && current?.key !== key) return;
    if (target && !target.openable) return;
    controller?.abort();
    controller = new AbortController();
    opening = key;
    openError = null;
    try {
      const within = key.startsWith('c:') ? { class: key.slice(2) } : { group: key.slice(2) };
      const answer = await api.graphGroups(ds, { within, by: how }, controller.signal);
      changed = false;
      // after a write, the levels already drawn and this answer count different data:
      // read the base level again, and keep only an opened class, whose answer is new
      if (baseCommit != null && answer.commit !== baseCommit) {
        path = [];
        selected = null;
        if (communities) {
          await loadCommunities(startLevel);
          changed = true;
          return;
        }
        await load();
        changed = true;
        // the classes may have moved on again: keep the answer only at their commit
        if (key.startsWith('c:') && answer.commit === baseCommit)
          path = [{ key, label: target?.label ?? key, answer }];
        return;
      }
      if (answer.available === false) {
        openError = answer.reason ?? 'The GraphRAG communities are not available.';
        return;
      }
      // a node of an earlier level closes the levels below it first
      let keep = path.length;
      for (let i = 0; i < path.length; i++) {
        const nodes = path[i].answer;
        const inLevel =
          nodes.groups.some((g) => `g:${g.id}` === key) ||
          (nodes.members ?? []).some((m) => `m:${m.iri}` === key);
        if (inLevel) {
          keep = i + 1;
          break;
        }
      }
      if (key.startsWith('c:') || base.nodes.some((n) => n.key === key)) keep = 0;
      if (current?.key === key) keep = path.length - 1;
      const lbl = target?.label ?? current?.label ?? key;
      path = [...path.slice(0, keep), { key, label: lbl, answer }];
      by =
        typeof how === 'string'
          ? how
          : 'community' in how
            ? `community:${how.community}`
            : 'property' in how
              ? `property:${how.property}`
              : 'auto';
      selected = null;
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') return;
      openError = errorText(e);
    } finally {
      opening = null;
    }
  }

  /** Close the levels from index `i` on (0 closes everything). */
  function closeTo(i: number) {
    controller?.abort();
    opening = null;
    openError = null;
    const closed = path[i]?.key;
    path = path.slice(0, i);
    selected = closed ?? null;
    by = 'auto';
  }

  function regroup(value: string) {
    if (value === 'mode:classes') return setMode('classes');
    if (value === 'mode:communities') return setMode('communities');
    if (!current) return;
    void open(current.key, byOf(value));
  }

  /** The class profile of the open class, for the grouping choices. */
  const openClass = $derived(
    current && !communities ? (current.answer.within.class ?? null) : null,
  );
  const groupChoices = $derived.by(() => {
    if (!openClass || !profiles) return [];
    const p = profiles.find((c) => c.class === openClass);
    if (!p) return [];
    return p.properties
      .filter((x) => !x.predicate.endsWith('#type'))
      .slice(0, 40)
      .map((x) => ({ value: `property:${x.predicate}`, label: label(x.predicate) }));
  });
  /** The picker's value: the starting point at the base level, else the grouping. */
  const pickerValue = $derived(current ? by : `mode:${mode}`);

  /** Open the communities down to `iri` and select it. */
  async function focusOn(iri: string) {
    try {
      const chain: string[] = [];
      const lvl = startLevel;
      for (let c: string | null = iri; c && chain.length < 16;) {
        chain.unshift(c);
        const d = await gr.community(ds, c);
        if (d.level <= lvl) break;
        c = d.parent;
      }
      for (const c of chain.slice(0, -1)) {
        const n = picture.nodes.find((x) => x.community === c);
        if (!n) return;
        await open(n.key);
      }
      const last = picture.nodes.find((x) => x.community === iri);
      if (last) selected = last.key;
    } catch (e) {
      openError = errorText(e);
    }
  }

  let focused: string | null = null;
  $effect(() => {
    const f = focus;
    if (!f || !reportsReady) return;
    untrack(() => {
      if (focused === f) return;
      focused = f;
      void focusOn(f);
    });
  });

  /** Draw the members of the community the page shows, to see the marked entities. */
  function showMembers() {
    const c = shownCommunity;
    if (!c) return;
    const key = sel?.community === c.iri ? sel.key : current?.key;
    if (key) void open(key, 'members');
  }

  // --- reading through time ---------------------------------------------------------------

  function closeTime() {
    resetTime();
    return load();
  }

  /** Close the history without reading the classes again. */
  function resetTime() {
    timeOpen = false;
    playing = false;
    ovAt = '';
    ovFrom = '';
    ovCompare = false;
    marks = null;
    lapsed = null;
    timeCtl?.abort();
    drawnAt = '';
  }

  /** Read the class level at the slider's state, or apply one time-lapse step to it. */
  async function readTime() {
    timeCtl?.abort();
    timeCtl = new AbortController();
    const signal = timeCtl.signal;
    const at = ovAt;
    const prev = drawnAt;
    const a = seqOf(prev);
    const b = seqOf(at);
    timeBusy = true;
    timeNote = '';
    const t0 = performance.now();
    try {
      // the head is always after the state drawn
      const forward = a != null && (b != null ? b > a : at === '');
      if (playMode === 'lapse' && playing && !ovCompare && forward) {
        // one step: the grouped diff applied to the counts, positions held
        const g = await api.diffGroups(ds, {
          from: `commit:${a}`,
          to: b != null ? `commit:${b}` : undefined,
          signal,
        });
        const c = overviewChanges(g);
        lapsed = applyOverviewChanges(lapsed ?? profileBase, c, label);
        marks = c;
        timeAnnounce = `Commit ${g.to.commit.seq}: ${fmtInt(g.added)} added, ${fmtInt(g.removed)} removed`;
        if (g.truncated)
          timeNote = `The step has ${fmtInt(g.groupCount)} groups, of which the ${fmtInt(g.groups.length)} largest are counted.`;
      } else {
        path = [];
        // a failed read rejects the move instead of drawing the state before it
        await readProfiles(at || undefined, signal);
        lapsed = null;
        loadError = null;
        marks = null;
        if (ovCompare && ovFrom) {
          const g = await api.diffGroups(ds, { from: ovFrom, to: at || undefined, signal });
          if (signal.aborted) return;
          marks = overviewChanges(g);
          timeAnnounce = `Commit ${g.to.commit.seq}: ${fmtInt(g.added)} added, ${fmtInt(g.removed)} removed since commit ${g.from.commit.seq}`;
        } else {
          timeAnnounce = at
            ? `Class overview at commit ${at.replace(/^commit:/, '')}`
            : 'Class overview at the head';
        }
      }
      if (signal.aborted) return;
      drawnAt = at;
      slider?.accept();
      timeMs = performance.now() - t0;
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') return;
      const gone = timeFailure(e);
      if (gone) {
        slider?.reject({ seq: gone.seq, readable: gone.readable });
        timeNote = `${gone.title}. ${gone.detail}`;
      } else if (e instanceof ApiError && e.status === 507) {
        slider?.fail();
        timeNote = `${e.message}. The counts of this step could not be grouped within the budget.`;
      } else {
        slider?.fail();
        timeNote = errorText(e);
      }
    } finally {
      // a read that a newer one superseded leaves the busy state to it
      if (timeCtl?.signal === signal) timeBusy = false;
    }
  }

  /**
   * The time-lapse moves the counts from each grouped diff, which cannot see links that a
   * reclassified resource keeps or a type that two graphs hold. When it stops, the profiles
   * of the state it reached are read, so what stays on screen is exact.
   */
  $effect(() => {
    // not while a read runs; a move of the slider starts its read in the same update, so the
    // check waits for that to begin, and a failed read leaves the state drawn to reconcile
    if (playing || !lapsed || !timeOpen || timeBusy) return;
    untrack(() => queueMicrotask(reconcile));
  });

  function reconcile() {
    if (playing || !lapsed || !timeOpen || timeBusy) return;
    {
      timeCtl?.abort();
      timeCtl = new AbortController();
      const signal = timeCtl.signal;
      void readProfiles(drawnAt || undefined, signal)
        .then(() => {
          if (!signal.aborted) lapsed = null;
        })
        .catch((e) => {
          if (!(e instanceof DOMException && e.name === 'AbortError')) timeNote = errorText(e);
        });
    }
  }

  let timeKey = '';
  $effect(() => {
    if (!timeOpen) return;
    const key = `${ovAt}|${ovCompare ? ovFrom : ''}`;
    if (key === timeKey) return;
    timeKey = key;
    void readTime();
  });

  function onKey(e: KeyboardEvent) {
    if ((e.target as HTMLElement)?.closest?.('input, textarea, select, [contenteditable]')) return;
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    // Shift+H: the overview through time
    if ((e.key === 'H' || e.key === 'h') && e.shiftKey) {
      if (communities) return;
      e.preventDefault();
      if (timeOpen) closeTime();
      else {
        timeKey = '|';
        timeOpen = true;
      }
      return;
    }
    if (e.key === 'l' || e.key === 'L') {
      e.preventDefault();
      listView = !listView;
    } else if (e.key === 'Backspace' && path.length) {
      e.preventDefault();
      closeTo(path.length - 1);
    }
  }

  let themeObserver: MutationObserver | undefined;
  let media: MediaQueryList | undefined;

  onMount(() => {
    webgl = hasWebGL2();
    if (!webgl) failReason = 'WebGL is not available in this browser';
    readPalette();
    themeObserver = new MutationObserver(readPalette);
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme', 'class'],
    });
    media = matchMedia('(prefers-color-scheme: dark)');
    media.addEventListener('change', readPalette);
    if (mode === 'communities') void loadCommunities();
    else void load();
  });

  onDestroy(() => {
    controller?.abort();
    themeObserver?.disconnect();
    media?.removeEventListener('change', readPalette);
  });

  function onlayout(p: Map<string, { x: number; y: number }>) {
    for (const [k, v] of p) positions.set(k, v);
  }

  const crumbs = $derived(path.map((l, i) => ({ i, label: l.label })));
  const note = $derived.by(() => {
    if (!current) {
      if (communities)
        return [reports?.note, reports?.truncated ? 'Some communities or links were left out.' : '']
          .filter(Boolean)
          .join(' ');
      return base.hidden ? `The ${fmtInt(MAX_CLASSES)} largest classes are drawn.` : '';
    }
    const a = current.answer;
    const parts: string[] = [];
    if (a.note) parts.push(a.note);
    if (a.grouping.kind === 'values' || a.grouping.kind === 'ranges') {
      parts.push(
        `Grouped by ${label(a.grouping.predicate ?? '')}${a.communities ? '' : ', since no F14 communities were written for these instances'}.`,
      );
    }
    if (a.truncated)
      parts.push(
        a.grouping.kind === 'members'
          ? `Showing ${fmtInt(a.members?.length ?? 0)} of ${fmtInt(a.within.size)} members.`
          : 'Some groups or links were left out.',
      );
    return parts.join(' ');
  });
  const markNote = $derived.by(() => {
    if (!marked?.length || !communities) return '';
    const n = markedKeys?.length ?? 0;
    const of = marked.length;
    if (n === of) return `${fmtInt(n)} ${n === 1 ? 'entity' : 'entities'} of the finding marked.`;
    return `${fmtInt(n)} of the ${fmtInt(of)} entities of the finding are drawn and marked.`;
  });
  const legend = $derived.by(() => {
    if (!communities || colorBy !== 'status') return [];
    const seen = new Set(picture.nodes.map((n) => n.status).filter(Boolean));
    return gr.COMMUNITY_STATUSES.filter((s) => seen.has(s));
  });
  const communitiesHref = $derived(`${resolve('/datasets/[name]', { name: ds })}?tab=communities`);
  const shownLevels = $derived(reports?.levels ?? []);
</script>

<svelte:window onkeydown={onKey} />

<div class="ov" bind:this={wrap} data-testid="overview" data-mode={mode}>
  <div class="ov-bar">
    <nav class="crumbs" aria-label="Overview levels">
      <button
        class="crumb"
        class:cur={!path.length}
        onclick={() => closeTo(0)}
        disabled={!path.length}
        ><Icon name={communities ? 'sparkle' : 'graph'} size={13} />
        {communities ? 'Communities' : 'Overview'}</button
      >
      {#each crumbs as c (c.i)}
        <span class="sep" aria-hidden="true">›</span>
        <button
          class="crumb"
          class:cur={c.i === path.length - 1}
          aria-current={c.i === path.length - 1 ? 'location' : undefined}
          onclick={() => (c.i === path.length - 1 ? undefined : closeTo(c.i + 1))}
          title={c.label}>{c.label}</button
        >
      {/each}
      {#if opening}<span class="spinner" aria-label="Opening"></span>{/if}
    </nav>
    <span class="spacer"></span>
    <label class="by faint small">
      <span class="hide-xs">Group by</span>
      <select
        value={pickerValue}
        onchange={(e) => regroup((e.currentTarget as HTMLSelectElement).value)}
        aria-label="Group by"
      >
        {#if !current}
          <option value="mode:classes">Classes</option>
          <option value="mode:communities">GraphRAG communities</option>
        {:else if communities}
          <option value="auto">Sub-communities</option>
          <option value="members">Members</option>
          <option value="mode:classes">Classes</option>
        {:else}
          <option value="auto">Automatic</option>
          <option value="members">Members</option>
          {#if current?.answer.communities && current.answer.grouping.predicate}
            <option value={`community:${current.answer.grouping.predicate}`}>Communities</option>
          {/if}
          {#each groupChoices as g (g.value)}
            <option value={g.value}>{g.label}</option>
          {/each}
          <option value="mode:communities">GraphRAG communities</option>
        {/if}
      </select>
    </label>
    {#if communities && reportsReady}
      <label class="by faint small">
        <span class="hide-xs">Colour</span>
        <select bind:value={colorBy} aria-label="Colour by">
          <option value="status">Report status</option>
          <option value="community">Community</option>
        </select>
      </label>
      {#if !path.length && shownLevels.length > 1}
        <label class="by faint small">
          <span class="hide-xs">Level</span>
          <select
            value={String(startLevel)}
            onchange={(e) => loadCommunities(Number((e.currentTarget as HTMLSelectElement).value))}
            aria-label="Level"
          >
            {#each shownLevels as l (l.level)}
              <option value={String(l.level)}>{l.level}</option>
            {/each}
          </select>
        </label>
      {/if}
    {/if}
    {#if !communities && !path.length && profiles?.some((c) => c.builtin && c.instances > 0)}
      <label class="row check small"
        ><input type="checkbox" bind:checked={builtins} /> Built-in</label
      >
    {/if}
    {#if !communities}
      <button
        class="btn sm"
        aria-pressed={timeOpen}
        aria-label="Overview history"
        title="Move the overview through the commits (Shift+H)"
        onclick={() => (timeOpen ? closeTime() : ((timeKey = '|'), (timeOpen = true)))}
        ><Icon name="clock" size={13} /> <span class="hide-xs">History</span></button
      >
    {/if}
    <button
      class="btn sm"
      aria-pressed={listView}
      onclick={() => (listView = !listView)}
      title="List view (L)"
      ><Icon name="table" size={13} /> <span class="hide-xs">List</span></button
    >
    {#if !listView && webgl && !failReason}
      <button
        class="btn sm ghost icon"
        onclick={() => large?.fit()}
        title="Fit (0)"
        aria-label="Fit"><Icon name="fit" size={13} /></button
      >
    {/if}
  </div>
  {#if timeOpen}
    <div class="ov-time">
      <TimeSlider
        bind:this={slider}
        {ds}
        bind:at={ovAt}
        bind:from={ovFrom}
        bind:compare={ovCompare}
        bind:playMode
        bind:playing
        lapse
        busy={timeBusy || loading}
        lastReadMs={timeMs}
        announcement={timeAnnounce}
        label="Overview history"
        canFollow={false}
        onclose={closeTime}
      />
    </div>
  {/if}
  {#if note || openError || failReason || changed || markNote || legend.length || timeNote}
    <div class="ov-notes small">
      {#if timeNote}<p class="faint" role="status">{timeNote}</p>{/if}
      {#if openError}<p class="err" role="alert">{openError}</p>{/if}
      {#if changed}<p class="faint">
          The dataset changed since the overview was read, so its {communities
            ? 'communities'
            : 'classes'} were read again.
        </p>{/if}
      {#if note}<p class="faint">{note}</p>{/if}
      {#if legend.length}
        <p class="legend faint" aria-label="Colours by report status">
          {#each legend as s (s)}
            <span class="key"
              ><span class="dot" style:background={statusColors[s]} aria-hidden="true"
              ></span>{s}</span
            >
          {/each}
        </p>
      {/if}
      {#if markNote}
        <p class="faint" data-testid="overview-marked">
          {markNote}
          {#if (markedKeys?.length ?? 0) < (marked?.length ?? 0) && shownCommunity}
            <button class="link" onclick={showMembers}>Draw the members</button>
          {/if}
        </p>
      {/if}
      {#if failReason && !listView}<p class="faint">
          {failReason}; the overview is drawn with Cytoscape.
        </p>{/if}
    </div>
  {/if}
  <p class="sr-only" aria-live="polite">{announce}</p>

  <div class="ov-body">
    {#if communities && offMain}
      <div class="ov-empty" data-testid="overview-communities-off-main">
        <Icon name="branch" size={20} />
        <p>
          The GraphRAG communities belong to main, and this page shows the
          <span class="mono">{branch}</span> branch. Switch to main to draw them.
        </p>
      </div>
    {:else if communities && reportsLoading && !reports}
      <div class="ov-empty"><span class="spinner"></span> Reading the communities…</div>
    {:else if communities && reportsError}
      <div class="ov-empty">
        <Icon name="alert" size={20} />
        <p>The communities could not be read: {reportsError}</p>
        <button class="btn sm" onclick={() => loadCommunities()}
          ><Icon name="refresh" size={13} /> Retry</button
        >
      </div>
    {:else if communities && reports && reports.available === false}
      <div class="ov-empty" data-testid="overview-communities-unavailable">
        <Icon name={reports.unavailable === 'access' ? 'lock' : 'sparkle'} size={22} />
        <h2>
          {reports.unavailable === 'access'
            ? 'The communities are not available to you'
            : 'No GraphRAG communities'}
        </h2>
        <p class="muted">{reports.reason}</p>
        {#if reports.unavailable === 'none'}
          <p class="muted">
            A dataset admin builds them in the <a href={communitiesHref}>Communities tab</a>.
          </p>
        {/if}
        <button class="btn sm" onclick={() => setMode('classes')}>Show the classes</button>
      </div>
    {:else if !communities && loading && !profiles}
      <div class="ov-empty"><span class="spinner"></span> Reading the classes…</div>
    {:else if !communities && loadError}
      <div class="ov-empty">
        <Icon name="alert" size={20} />
        <p>The overview could not be read: {loadError}</p>
        <button class="btn sm" onclick={() => load(ovAt || undefined)}
          ><Icon name="refresh" size={13} /> Retry</button
        >
      </div>
    {:else if !communities && profiles && !base.nodes.length && !path.length}
      <div class="ov-empty">
        <Icon name="explore" size={24} />
        <h2>No classes to draw</h2>
        <p class="muted">
          The overview draws the classes of the dataset, and no resource in it has an
          <span class="mono">rdf:type</span>{builtins ? '' : ' outside the built-in vocabularies'}.
          Search above or paste an IRI to start from a resource.
        </p>
        {#if !builtins && profiles.some((c) => c.instances > 0)}
          <button class="btn sm" onclick={() => (builtins = true)}>Show built-in classes</button>
        {/if}
      </div>
    {:else if (communities && reportsReady) || (!communities && profiles)}
      {#if listView}
        <GraphListView
          {rows}
          {selected}
          clsLabel={communities ? 'Status' : 'Class'}
          onselect={(k) => (selected = k)}
          onopen={(k) => open(k)}
        />
      {:else if webgl && !failReason}
        <LargeGraphView
          bind:this={large}
          nodes={lnodes}
          edges={ledges}
          {selected}
          marked={markedKeys}
          label={communities ? 'GraphRAG communities' : 'Graph overview'}
          still={!!lapsed}
          onselect={(k) => (selected = k)}
          onopen={(k) => open(k)}
          {onlayout}
          onfail={(r) => (failReason = r)}
        />
      {:else}
        <GraphView
          nodes={gnodes}
          edges={gedges}
          {selected}
          onselect={(k) => (selected = k)}
          onexpand={(k) => open(k)}
        />
      {/if}
    {/if}
  </div>

  {#if sel}
    <div class="ov-detail" role="region" aria-label="Selected node">
      <span class="dot" style:background={colorOf(sel)} aria-hidden="true"></span>
      <div class="d-main">
        <div class="d-title" title={sel.iri ?? sel.label}>{sel.label}</div>
        <div class="faint small">
          {#if sel.kind === 'class'}Class · {fmtInt(sel.size)} instances
          {:else if sel.kind === 'group' && sel.status}{sel.community
              ? `Community at level ${sel.level ?? 0}`
              : 'Members of no community below'} · {fmtInt(sel.size)} members · {sel.status}
          {:else if sel.kind === 'group'}Group of {label(sel.cls)} · {fmtInt(sel.size)} instances
          {:else if communities}Member
          {:else}{label(sel.cls)}{/if}
          · {fmtInt(degree.get(sel.key) ?? 0)} links{#if sel.internal}, {fmtInt(sel.internal)} inside{/if}
          {#if sel.top?.length}
            · e.g. {sel.top.map((t) => t.label ?? label(t.iri)).join(', ')}
          {/if}
        </div>
      </div>
      {#if sel.openable}
        <button class="btn sm" onclick={() => open(sel.key)} disabled={!!opening}
          ><Icon name="expand" size={13} /> Open</button
        >
      {:else}
        <button class="btn sm" onclick={() => open(sel.key)}
          ><Icon name="target" size={13} /> Explore</button
        >
      {/if}
    </div>
  {/if}
  {#if !path.length && !communities && profiles && base.nodes.length}
    <div class="ov-foot faint small">
      {fmtInt(base.nodes.length)} classes · read in {fmtInt(Math.round(loadMs))} ms · tap a class to open
      it
      {#if onschema}· <button class="link" onclick={onschema}>Browse the schema</button>{/if}
    </div>
  {:else if !path.length && communities && reportsReady}
    <div class="ov-foot faint small">
      {fmtInt(base.nodes.length)}
      {base.nodes.length === 1 ? 'community' : 'communities'} at level {startLevel} · read in {fmtInt(
        Math.round(loadMs),
      )} ms · tap a community to open it
    </div>
  {/if}
</div>

<style>
  .ov {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    position: relative;
  }
  .ov-time {
    flex: none;
    min-width: 0;
  }
  .ov-bar {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
    min-height: 38px;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
    flex-wrap: wrap;
  }
  .crumb {
    background: none;
    border: 0;
    color: var(--iri);
    cursor: pointer;
    font: inherit;
    font-size: var(--fs-sm);
    padding: 4px 6px;
    border-radius: var(--r-sm);
    max-width: 22ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    display: inline-flex;
    gap: 4px;
    align-items: center;
  }
  .crumb:hover:not(:disabled) {
    background: var(--hover);
  }
  .crumb.cur,
  .crumb:disabled {
    color: var(--text);
    font-weight: 600;
    cursor: default;
  }
  .sep {
    color: var(--text-3);
  }
  .spacer {
    flex: 1;
  }
  .by {
    display: inline-flex;
    gap: 4px;
    align-items: center;
  }
  .by select {
    /* room for "GraphRAG communities" and the arrow */
    max-width: 24ch;
  }
  .ov-notes {
    padding: 4px 12px;
    border-bottom: 1px solid var(--border);
  }
  .ov-notes p {
    margin: 2px 0;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 12px;
  }
  .legend .key {
    display: inline-flex;
    gap: 4px;
    align-items: center;
  }
  .legend .dot {
    width: 9px;
    height: 9px;
  }
  .err {
    color: var(--danger);
  }
  .ov-body {
    flex: 1;
    min-height: 0;
    position: relative;
  }
  .ov-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    height: 100%;
    padding: 24px 16px;
    text-align: center;
    color: var(--text-2);
  }
  .ov-empty h2 {
    margin: 0;
    font-size: var(--fs-lg);
    color: var(--text);
  }
  .ov-empty p {
    max-width: 52ch;
    margin: 0;
  }
  .ov-detail {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    border-top: 1px solid var(--border);
    background: var(--surface);
  }
  .d-main {
    flex: 1;
    min-width: 0;
  }
  .d-title {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dot {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    flex: none;
  }
  .ov-foot {
    padding: 4px 12px;
    border-top: 1px solid var(--border);
  }
  .link {
    background: none;
    border: 0;
    padding: 0;
    color: var(--iri);
    cursor: pointer;
    font: inherit;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  @media (max-width: 480px) {
    .hide-xs {
      display: none;
    }
    .by select {
      max-width: 13ch;
    }
  }
</style>

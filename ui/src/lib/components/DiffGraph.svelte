<script lang="ts">
  /**
   * The diff graph (docs/specs/U01-time-slider.md, Phase 4): the changes between two commits
   * drawn as a graph of their own, added triples solid with `+` and removed ones as dashed
   * ghosts with `−`. A diff of more than 500 changes draws the first 500 and offers
   * **Group by class**, which draws the grouped diff as classes and predicates. ◀ and ▶ move
   * the diff one commit and keep its width, and **Play** applies one commit's changes per
   * step, one request at a time, from the start of the range to its end.
   */
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import * as api from '$lib/api';
  import type { Term } from '$lib/api';
  import {
    applyStep,
    canvasElements,
    changedResources,
    changeRows,
    classOfKey,
    countsText,
    diffCanvas,
    emptyCanvas,
    groupedElements,
    lapseGroupsStep,
    lapseSteps,
    MAX_CANVAS_NODES,
    seqOf,
    SPEEDS,
    STEP_QUADS,
    stepAnnouncement,
    stepDelay,
    tooLarge,
    withContext,
    withCounts,
    type Canvas,
    type ContextEdge,
    type LapseStep,
    type LiveGroup,
    type Speed,
  } from '$lib/diff-graph';
  import * as ex from '$lib/explore';
  import { fmtInt } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import { literalText, type PrefixMap } from '$lib/rdf';
  import { isAbort } from '$lib/supersede';
  import { timeFailure } from '$lib/timeline';
  import GraphView, { type GEdge, type GNode } from './GraphView.svelte';
  import Icon from './Icon.svelte';

  let {
    ds,
    from = '',
    to,
    group = false,
    prefixes,
    onexit,
    onchange,
    onopen,
  }: {
    /** The dataset on its branch, as the API client takes it (`ds` or `ds@branch`). */
    ds: string;
    /** The earlier state, a selector of `at`; empty for the commit before `to`. */
    from?: string;
    /** The later state. */
    to: string;
    /** Draw the grouped diff. */
    group?: boolean;
    prefixes: PrefixMap;
    onexit: () => void;
    /** The diff moved or changed its grouping, for the address. */
    onchange?: (c: { from: string; to: string; group: boolean }) => void;
    /** Open a resource in the resource graph. */
    onopen?: (iri: string) => void;
  } = $props();

  // --- what is shown ----------------------------------------------------------------------

  let showAdded = $state(true);
  let showRemoved = $state(true);
  let showContext = $state(false);
  let showLiterals = $state(true);
  let grouped = $state(untrack(() => group));
  let listView = $state(false);
  /** At 390 px: the graph or the list of changes, and the sheet of options. */
  let narrowTab = $state<'graph' | 'changes'>('graph');
  let sheetOpen = $state(false);

  let fromSel = $state(untrack(() => from));
  let toSel = $state(untrack(() => to));

  let diff = $state<api.Diff | null>(null);
  /** The changes the list shows: of the diff, of a time-lapse step or of a class's members. */
  let listQuads = $state.raw<api.DiffQuad[]>([]);
  let groups = $state<api.GroupedDiff | null>(null);
  let canvas = $state.raw<Canvas>(emptyCanvas());
  let context = $state.raw<Map<string, ContextEdge[]>>(new Map());
  /** The diff the context was read for. */
  let contextFor = $state('');
  const diffKey = (d: api.Diff) => `${d.from.commit.seq}..${d.to.commit.seq}|${listQuads.length}`;
  let loading = $state(false);
  let error = $state<string | null>(null);
  let note = $state('');
  let announce = $state('');
  /** The class whose changed members are drawn, in place of the grouped picture. */
  let members = $state<{ cls: string; offset: number; more: boolean } | null>(null);
  let selected = $state<string | null>(null);
  let ctl: AbortController | undefined;

  const label = (t: Term) => {
    if (t.type === 'uri') return shortLabel(t.value, prefixes);
    if (t.type === 'bnode') return `_:${t.value.slice(0, 8)}`;
    if (t.type === 'literal') {
      const s = literalText(t);
      return s.length > 40 ? s.slice(0, 37) + '…' : s;
    }
    return '<<triple>>';
  };
  const short = (iri: string) => shortLabel(iri, prefixes);
  /** Who made a commit, where the server says (the catalog of a commit may not). */
  const authorOf = (c: api.Commit) => (c as api.Commit & { author?: string }).author;

  const shown = $derived({
    added: showAdded,
    removed: showRemoved,
    context: showContext,
    literals: showLiterals,
  });
  const drawnCanvas = $derived(showContext ? withContext(canvas, context) : canvas);
  const classView = $derived(grouped && !members && groups ? groupedElements(groups, short) : null);
  const elements = $derived.by((): { nodes: GNode[]; edges: GEdge[] } => {
    if (classView) {
      const keepE = (e: { change?: string }) =>
        (e.change !== 'added' || showAdded) && (e.change !== 'removed' || showRemoved);
      const edges = classView.edges.filter(keepE);
      const ends = new Set(edges.flatMap((e) => [e.source, e.target]));
      return { nodes: classView.nodes.filter((n) => ends.has(n.id) || keepE(n)), edges };
    }
    return canvasElements(drawnCanvas, shown, label, short);
  });

  /** The commits the range resolved to, which a time-lapse keeps while it clears the diff. */
  let range = $state<{ from: number; to: number } | null>(null);
  const fromSeq = $derived(
    diff?.from.commit.seq ?? range?.from ?? groups?.from.commit.seq ?? seqOf(fromSel),
  );
  const toSeq = $derived(diff?.to.commit.seq ?? range?.to ?? groups?.to.commit.seq ?? seqOf(toSel));
  const total = $derived(
    diff ? diff.added + diff.removed : groups ? groups.added + groups.removed : 0,
  );
  const listed = $derived(diff?.quads?.length ?? 0);
  const head = $derived(diff ?? groups);
  const commitLine = $derived.by(() => {
    const c = head?.to.commit;
    if (!c) return '';
    const who = authorOf(c);
    return [c.kind, who ? `by ${who}` : '', c.message ? `“${c.message}”` : '']
      .filter(Boolean)
      .join(' ');
  });

  // --- reading ------------------------------------------------------------------------------

  function errorText(e: unknown): string {
    const gone = timeFailure(e);
    if (gone) return `${gone.title}. ${gone.detail}`;
    return api.errorMessage(e);
  }

  async function read() {
    ctl?.abort();
    ctl = new AbortController();
    const signal = ctl.signal;
    stopLapse();
    // a new range: Play starts from its first commit, and context is read again for it
    steps = [];
    stepIndex = -1;
    stepCount = 0;
    lapseGroups = null;
    range = null;
    context = new Map();
    contextFor = '';
    listQuads = [];
    loading = true;
    error = null;
    note = '';
    members = null;
    try {
      if (grouped) {
        try {
          const g = await api.diffGroups(ds, { from: fromSel || undefined, to: toSel, signal });
          groups = g;
          range = { from: g.from.commit.seq, to: g.to.commit.seq };
          diff = null;
          canvas = emptyCanvas();
          const omitted = groupedElements(g, short).omitted;
          note = [
            g.truncated
              ? `The ${fmtInt(g.groups.length)} largest of ${fmtInt(g.groupCount)} groups are drawn.`
              : '',
            omitted
              ? `${fmtInt(omitted)} smaller groups are left out to stay within ${fmtInt(MAX_CANVAS_NODES)} nodes.`
              : '',
          ]
            .filter(Boolean)
            .join(' ');
          announce = `Commit ${g.to.commit.seq}: ${fmtInt(g.added)} added, ${fmtInt(g.removed)} removed, in ${fmtInt(g.groupCount)} groups`;
          return;
        } catch (e) {
          if (isAbort(e)) throw e;
          // a grouping past the budget falls back to the first changes
          if (e instanceof api.ApiError && e.status === 507) {
            grouped = false;
            note = `Grouping needs more than the server allows (${e.message}), so the first ${STEP_QUADS} changes are drawn.`;
          } else throw e;
        }
      }
      const d = await api.diff(ds, {
        from: fromSel || undefined,
        to: toSel,
        quads: true,
        limit: STEP_QUADS,
        signal,
      });
      diff = d;
      range = { from: d.from.commit.seq, to: d.to.commit.seq };
      groups = null;
      listQuads = d.quads ?? [];
      const r = diffCanvas(d.quads ?? []);
      canvas = r.overflow ? emptyCanvas() : r.canvas;
      const n = d.added + d.removed;
      const more = (d.quads?.length ?? 0) < n;
      note = [
        note,
        more ? `Showing ${fmtInt(d.quads?.length ?? 0)} of ${fmtInt(n)} changes.` : '',
        r.overflow ? `The changes need more than ${fmtInt(MAX_CANVAS_NODES)} nodes.` : '',
      ]
        .filter(Boolean)
        .join(' ');
      announce = `Commit ${d.to.commit.seq}: ${fmtInt(d.added)} added, ${fmtInt(d.removed)} removed`;
    } catch (e) {
      if (isAbort(e)) return;
      error = errorText(e);
    } finally {
      if (!signal.aborted) loading = false;
    }
  }

  /** The unchanged edges of the changed resources at the `to` state. */
  async function readContext(signal: AbortSignal) {
    const quads = diff?.quads ?? [];
    // the state the diff resolved to, which a moving head does not change
    const seq = diff?.to.commit.seq;
    const at = seq != null ? `commit:${seq}` : undefined;
    const out = new Map<string, ContextEdge[]>();
    for (const iri of changedResources(quads, 30)) {
      const r = await ex.neighbours(ds, iri, at, signal);
      const me: Term = { type: 'uri', value: iri };
      out.set(iri, [
        ...r.out.map((n) => ({ s: me, p: n.predicate, o: n.term })),
        ...r.inc.map((n) => ({ s: n.term, p: n.predicate, o: me })),
      ]);
    }
    if (!signal.aborted && diff) {
      context = out;
      contextFor = diffKey(diff);
    }
  }

  /** Members of a class read from each state at a time. */
  const MEMBER_PAGE = 500;

  /** The changed members of a class of the grouped picture, 500 changes at a time. */
  async function openMembers(cls: string, offset = 0) {
    ctl?.abort();
    ctl = new AbortController();
    const signal = ctl.signal;
    loading = true;
    error = null;
    try {
      // a removal counts under the class its subject had before, an addition under the one
      // it has after, so the members are read at both states
      const ends = [groups?.from.commit.seq, groups?.to.commit.seq].map((n) =>
        n != null ? `commit:${n}` : undefined,
      );
      // a page of each state, so their union fits the diff's 1,000 terms and paging both
      // states by a page skips no member of either
      const query = `SELECT DISTINCT ?s WHERE { ?s a <${cls}> } ORDER BY ?s LIMIT ${MEMBER_PAGE} OFFSET ${offset}`;
      const reads = await Promise.all(
        ends.map((at) => api.select(ds, query, { ...(at ? { at } : {}), signal })),
      );
      const full = reads.some((rows) => rows.length === MEMBER_PAGE);
      const terms = [
        ...new Set(reads.flat().flatMap((r) => (r.s?.type === 'uri' ? [r.s.value] : []))),
      ];
      const d = terms.length
        ? await api.diff(ds, {
            from: fromSel || undefined,
            to: toSel,
            quads: true,
            limit: STEP_QUADS,
            terms,
            signal,
          })
        : null;
      const r = diffCanvas(d?.quads ?? []);
      canvas = r.canvas;
      listQuads = d?.quads ?? [];
      const cut = d != null && (d.quads?.length ?? 0) < d.added + d.removed;
      members = { cls, offset, more: full };
      note = d
        ? `The changes of ${fmtInt(terms.length)} members of ${short(cls)}${cut ? `, the first ${fmtInt(d.quads?.length ?? 0)} of ${fmtInt(d.added + d.removed)}, which touch other resources too` : ''}.`
        : `No changed members of ${short(cls)} to draw.`;
      announce = note;
    } catch (e) {
      if (!isAbort(e)) error = errorText(e);
    } finally {
      if (!signal.aborted) loading = false;
    }
  }

  function setGrouped(on: boolean) {
    if (grouped === on && !members) return;
    grouped = on;
    members = null;
    onchange?.({ from: fromSel, to: toSel, group: grouped });
    void read();
  }

  /** Move the diff `by` commits and keep its width. */
  function move(by: number) {
    const f = fromSeq;
    const t = toSeq;
    if (f == null || t == null) return;
    const nf = f + by;
    const nt = t + by;
    if (nf < 0) return;
    fromSel = `commit:${nf}`;
    toSel = `commit:${nt}`;
    onchange?.({ from: fromSel, to: toSel, group: grouped });
    void read();
  }

  // --- the time-lapse ----------------------------------------------------------------------

  let playing = $state(false);
  let speed = $state<Speed>(1);
  let keepGhosts = $state(false);
  let steps: LapseStep[] = [];
  let stepIndex = $state(-1);
  let stepCount = $state(0);
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** Requests the time-lapse made, and the most in flight at once, for the tests. */
  let requests = 0;
  let inFlight = 0;
  let maxInFlight = 0;
  let frameMs = $state(0);
  let frameMax = $state(0);
  /** The frame time of each step drawn, for the tests. */
  let frames: number[] = [];
  let lapseCaption = $state('');
  /** The time-lapse switched to grouping, adding each step's groups. */
  let lapseGroups: Map<string, LiveGroup> | null = null;
  let wrap: HTMLDivElement | undefined = $state();

  function stopLapse() {
    playing = false;
    clearTimeout(timer);
  }

  async function startLapse() {
    const f = fromSeq;
    const t = toSeq;
    if (f == null || t == null || t <= f) return;
    if (stepIndex < 0 || stepIndex >= steps.length - 1) {
      // a range of more than 500 commits steps through the timeline's buckets
      let buckets: api.TimelineBucket[] | undefined;
      if (t - f > 500) {
        const tl = await api
          .commitTimeline(ds, { from: f + 1, to: t, buckets: 200 })
          .catch(() => null);
        buckets = tl?.buckets;
      }
      steps = lapseSteps(f, t, buckets);
      // the catalog's counts say which steps are too large to draw, before asking for them
      if (t - f <= 500) {
        const page = await api.commits(ds, { before: t + 1, limit: t - f }).catch(() => null);
        if (page) steps = withCounts(steps, page.commits);
      }
      stepCount = steps.length;
      stepIndex = -1;
      canvas = emptyCanvas();
      lapseGroups = null;
      requests = 0;
      maxInFlight = 0;
      frameMax = 0;
      frames = [];
      members = null;
      if (grouped) {
        grouped = false;
      }
      groups = null;
      diff = null;
      listQuads = [];
      context = new Map();
      contextFor = '';
      note = '';
    }
    playing = true;
    void nextStep();
  }

  async function nextStep() {
    if (!playing) return;
    const i = stepIndex + 1;
    const s = steps[i];
    if (!s) {
      stopLapse();
      return;
    }
    ctl?.abort();
    ctl = new AbortController();
    const signal = ctl.signal;
    const t0 = performance.now();
    requests++;
    inFlight++;
    maxInFlight = Math.max(maxInFlight, inFlight);
    if (wrap) {
      wrap.dataset.requests = String(requests);
      wrap.dataset.maxInFlight = String(maxInFlight);
    }
    loading = true;
    try {
      let added = 0;
      let removed = 0;
      let commit: api.Commit | undefined;
      if (!lapseGroups && tooLarge(s)) {
        lapseGroups = new Map();
        note = `Commit ${s.to} has ${fmtInt(s.changes ?? 0)} changes, more than the ${fmtInt(STEP_QUADS)} a step draws, so the time-lapse is grouped by class from here on.`;
      } else if (!lapseGroups) {
        const d = await api.diff(ds, {
          from: `commit:${s.from}`,
          to: `commit:${s.to}`,
          quads: true,
          limit: STEP_QUADS,
          signal,
        });
        inFlight--;
        commit = d.to.commit;
        const listedAll = (d.quads?.length ?? 0) >= d.added + d.removed;
        const r = listedAll ? applyStep(canvas, d.quads ?? [], { keepGhosts }) : null;
        if (!r || r.overflow) {
          // a step too large to draw: from here on, by class
          lapseGroups = new Map();
          note = `Commit ${s.to} has ${fmtInt(d.added + d.removed)} changes, more than ${!r ? `the ${fmtInt(STEP_QUADS)} a step draws` : `${fmtInt(MAX_CANVAS_NODES)} nodes hold`}, so the time-lapse is grouped by class from here on.`;
        } else {
          const t1 = performance.now();
          canvas = r.canvas;
          listQuads = d.quads ?? [];
          await frame();
          frameMs = performance.now() - t1;
          frameMax = Math.max(frameMax, frameMs);
          frames.push(frameMs);
          if (wrap) {
            wrap.dataset.frameMs = frameMs.toFixed(1);
            wrap.dataset.frameMax = frameMax.toFixed(1);
            const sorted = [...frames].sort((x, y) => x - y);
            wrap.dataset.frameP50 = sorted[Math.floor(sorted.length / 2)].toFixed(1);
            wrap.dataset.frameP95 = sorted[Math.floor(sorted.length * 0.95)].toFixed(1);
            wrap.dataset.frameFirst = frames[0].toFixed(1);
          }
          added = r.added;
          removed = r.removed;
        }
      }
      if (lapseGroups) {
        if (inFlight === 0) {
          requests++;
          inFlight++;
          maxInFlight = Math.max(maxInFlight, inFlight);
          if (wrap) {
            wrap.dataset.requests = String(requests);
            wrap.dataset.maxInFlight = String(maxInFlight);
          }
        }
        const g = await api.diffGroups(ds, {
          from: `commit:${s.from}`,
          to: `commit:${s.to}`,
          signal,
        });
        inFlight--;
        commit = g.to.commit;
        const next = lapseGroupsStep(lapseGroups, g.groups, keepGhosts);
        lapseGroups = next.live;
        listQuads = [];
        groups = {
          ...g,
          groups: next.groups,
          groupCount: next.groups.length,
          // the running totals since the start, for the caption
          added: (groups?.added ?? canvas.added) + g.added,
          removed: (groups?.removed ?? canvas.removed) + g.removed,
        };
        grouped = true;
        added = g.added;
        removed = g.removed;
      }
      stepIndex = i;
      const totalAdded = lapseGroups ? (groups?.added ?? 0) : canvas.added;
      const totalRemoved = lapseGroups ? (groups?.removed ?? 0) : canvas.removed;
      const who = commit ? authorOf(commit) : undefined;
      lapseCaption = [
        s.to - s.from === 1 ? `commit ${s.to}` : `commits ${s.from + 1}–${s.to}`,
        who ? `by ${who}` : '',
        commit?.message ? `“${commit.message}”` : '',
        `· ${countsText(totalAdded, totalRemoved) || 'no change'} since the start`,
      ]
        .filter(Boolean)
        .join(' ');
      announce = stepAnnouncement(s, added, removed);
      const spent = performance.now() - t0;
      if (i >= steps.length - 1) {
        stopLapse();
        return;
      }
      clearTimeout(timer);
      timer = setTimeout(() => void nextStep(), stepDelay(speed, spent));
    } catch (e) {
      inFlight = 0;
      if (isAbort(e)) return;
      stopLapse();
      const gone = timeFailure(e);
      if (gone) {
        const last = steps[stepIndex]?.to;
        note = `${gone.title}. The time-lapse stopped${last != null ? ` at commit ${last}` : ''}. ${gone.detail}`;
      } else error = errorText(e);
    } finally {
      if (!signal.aborted) loading = false;
    }
  }

  /** The next frame after the graph drew the step. */
  async function frame() {
    await tick();
    await new Promise<void>((r) =>
      typeof requestAnimationFrame === 'function' ? requestAnimationFrame(() => r()) : r(),
    );
  }

  function togglePlay() {
    if (playing) stopLapse();
    else void startLapse();
  }

  // --- keys ----------------------------------------------------------------------------------

  function onKey(e: KeyboardEvent) {
    if ((e.target as HTMLElement)?.closest?.('input, textarea, select, [contenteditable]')) return;
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const k = e.key;
    if (k === 'Escape') {
      e.preventDefault();
      stopLapse();
      onexit();
    } else if (k === 'a' || k === 'A') {
      e.preventDefault();
      showAdded = !showAdded;
    } else if (k === 'r' || k === 'R') {
      e.preventDefault();
      showRemoved = !showRemoved;
    } else if (k === 'x' || k === 'X') {
      e.preventDefault();
      showContext = !showContext;
    } else if (k === 'g' || k === 'G') {
      e.preventDefault();
      setGrouped(!grouped || !!members);
    } else if (k === 'l' || k === 'L') {
      e.preventDefault();
      listView = !listView;
    } else if (k === 'ArrowLeft' && !(e.target as HTMLElement)?.closest?.('[role="slider"]')) {
      e.preventDefault();
      move(-1);
    } else if (k === 'ArrowRight' && !(e.target as HTMLElement)?.closest?.('[role="slider"]')) {
      e.preventDefault();
      move(1);
    } else if (k === ' ' && !(e.target as HTMLElement)?.closest?.('button')) {
      e.preventDefault();
      togglePlay();
    }
  }

  $effect(() => {
    if (showContext && diff && !grouped && !playing && contextFor !== diffKey(diff)) {
      const c = new AbortController();
      untrack(() => void readContext(c.signal).catch(() => {}));
      return () => c.abort();
    }
  });

  onMount(() => {
    void read();
  });
  onDestroy(() => {
    stopLapse();
    ctl?.abort();
  });

  const rows = $derived(members || !grouped ? changeRows(listQuads).slice(0, STEP_QUADS) : []);
  const emptyChanges = $derived(!loading && !error && head && total === 0);

  function selectNode(id: string | null) {
    selected = id;
  }
  function expandNode(id: string) {
    const cls = classOfKey(id);
    if (classView && cls) {
      stopLapse();
      void openMembers(cls);
      return;
    }
    if (!classView && id.startsWith('<') && onopen) onopen(id.slice(1, -1));
  }
  const selectedClass = $derived(selected && classView ? classOfKey(selected) : undefined);
</script>

<svelte:window onkeydown={onKey} />

<div class="dg" bind:this={wrap} data-requests="0" data-max-in-flight="0">
  <header class="dg-head">
    <div class="title">
      <strong>Diff</strong>
      <span class="mono">
        commit {fromSeq ?? '…'} → {toSeq ?? '…'}
      </span>
      {#if commitLine}<span class="faint hide-xs">· {commitLine}</span>{/if}
      {#if head}
        <span
          class="counts mono"
          aria-label="{fmtInt(head.added)} added, {fmtInt(head.removed)} removed"
        >
          <span class="ins">+{fmtInt(head.added)}</span>
          <span class="del">−{fmtInt(head.removed)}</span>
        </span>
      {/if}
    </div>
    <span class="spacer"></span>
    <button
      class="btn sm sheet-toggle"
      aria-expanded={sheetOpen}
      onclick={() => (sheetOpen = !sheetOpen)}><Icon name="filter" size={13} /> Options</button
    >
    <button class="btn sm" onclick={onexit} title="Leave the diff (Esc)"
      ><Icon name="x" size={13} /> Exit diff</button
    >
  </header>

  <div class="dg-tools" class:open={sheetOpen} role="toolbar" aria-label="Diff options">
    <span class="faint small">Show</span>
    <label class="check small" title="Added triples (A)"
      ><input type="checkbox" bind:checked={showAdded} /> added</label
    >
    <label class="check small" title="Removed triples (R)"
      ><input type="checkbox" bind:checked={showRemoved} /> removed</label
    >
    <label class="check small" title="Unchanged edges of the changed resources (X)"
      ><input type="checkbox" bind:checked={showContext} disabled={grouped && !members} /> context</label
    >
    <label class="check small"><input type="checkbox" bind:checked={showLiterals} /> literals</label
    >
    <label class="small by">
      Group by
      <select
        class="select sm"
        value={grouped && !members ? 'class' : 'none'}
        onchange={(e) => setGrouped((e.currentTarget as HTMLSelectElement).value === 'class')}
        aria-label="Group by (G)"
      >
        <option value="none">none</option>
        <option value="class">class</option>
      </select>
    </label>
    <button
      class="btn sm"
      aria-pressed={listView}
      onclick={() => (listView = !listView)}
      title="List the changes (L)"><Icon name="table" size={13} /> List</button
    >
  </div>

  <div class="dg-play" role="group" aria-label="Diff steps">
    <button
      class="btn ghost icon sm"
      aria-label="First step"
      title="Back to the start of the range"
      disabled={playing}
      onclick={() => {
        stepIndex = -1;
        steps = [];
        void read();
      }}><Icon name="skipBack" size={13} /></button
    >
    <button
      class="btn ghost icon sm"
      aria-label="One commit back"
      title="One commit back, keeping the width (←)"
      disabled={playing || !fromSeq}
      onclick={() => move(-1)}><Icon name="stepBack" size={13} /></button
    >
    <button
      class="btn ghost icon sm"
      aria-label="One commit forward"
      title="One commit forward, keeping the width (→)"
      disabled={playing}
      onclick={() => move(1)}><Icon name="stepForward" size={13} /></button
    >
    <button
      class="btn sm"
      aria-pressed={playing}
      aria-label={playing ? 'Pause the time-lapse' : 'Play the time-lapse'}
      title="Apply one commit's changes per step (Space)"
      disabled={fromSeq == null || toSeq == null || toSeq <= (fromSeq ?? 0)}
      onclick={togglePlay}
      ><Icon name={playing ? 'pause' : 'play'} size={13} /> {playing ? 'Pause' : 'Play'}</button
    >
    <select class="select sm" aria-label="Steps a second" bind:value={speed}>
      {#each SPEEDS as s (s)}<option value={s}>{s}/s</option>{/each}
    </select>
    <label class="check small" title="Ghosts stay until the end"
      ><input type="checkbox" bind:checked={keepGhosts} /> Keep ghosts</label
    >
    {#if stepCount}<span class="faint small mono"
        >step {Math.max(0, stepIndex + 1)}/{stepCount}</span
      >{/if}
    {#if lapseCaption}<span class="small lapse-cap">{lapseCaption}</span>{/if}
    {#if loading}<span class="spinner" aria-label="Reading"></span>{/if}
  </div>

  <div class="tabs narrow-tabs" role="tablist" aria-label="Diff views">
    <button
      class="tab"
      role="tab"
      aria-selected={narrowTab === 'graph'}
      onclick={() => (narrowTab = 'graph')}>Graph</button
    >
    <button
      class="tab"
      role="tab"
      aria-selected={narrowTab === 'changes'}
      onclick={() => (narrowTab = 'changes')}>Changes</button
    >
  </div>

  {#if error}
    <div class="error-box" role="alert">{error}</div>
  {/if}
  <p class="sr-only" aria-live="polite">{announce}</p>

  <div class="dg-body" class:list={listView} data-tab={narrowTab}>
    <div class="dg-canvas" data-frame-ms={frameMs.toFixed(1)}>
      {#if emptyChanges}
        <div class="dg-empty">
          <Icon name="info" size={20} />
          <p>No changes you can read between these commits.</p>
        </div>
      {:else}
        <GraphView
          nodes={elements.nodes}
          edges={elements.edges}
          {selected}
          animate={!playing || speed <= 2}
          layoutNew={!playing || speed <= 2}
          onselect={selectNode}
          onexpand={expandNode}
          emptyText={loading ? 'Reading the changes…' : 'Nothing to draw'}
        />
      {/if}
    </div>
    {#if listView || narrowTab === 'changes'}
      <div class="dg-list" role="region" aria-label="Changes">
        {#if classView}
          <table class="tbl">
            <thead
              ><tr
                ><th>Subject class</th><th>Predicate</th><th>Object</th><th class="num">+</th><th
                  class="num">−</th
                ></tr
              ></thead
            >
            <tbody>
              {#each groups?.groups ?? [] as g, i (i)}
                <tr>
                  <td class="mono">{g.subjectClass ? short(g.subjectClass) : 'no class'}</td>
                  <td class="mono">{short(g.predicate)}</td>
                  <td class="mono"
                    >{'class' in g.object
                      ? g.object.class
                        ? short(g.object.class)
                        : 'no class'
                      : 'literal' in g.object
                        ? short(g.object.literal)
                        : 'IRI'}</td
                  >
                  <td class="num ins">{g.added ? `+${fmtInt(g.added)}` : ''}</td>
                  <td class="num del">{g.removed ? `−${fmtInt(g.removed)}` : ''}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {:else}
          <table class="tbl">
            <thead><tr><th></th><th>Subject</th><th>Predicate</th><th>Object</th></tr></thead>
            <tbody>
              {#each rows as r, i (i)}
                <tr class:ins={r.sign === '+'} class:del={r.sign === '−'}>
                  <td class="mono sign">{r.sign}</td>
                  <td class="mono">{r.s}</td>
                  <td class="mono">{r.p}</td>
                  <td class="mono">{r.o}</td>
                </tr>
              {:else}
                <tr><td colspan="4" class="faint">No changes listed.</td></tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>
    {/if}
  </div>

  <footer class="dg-foot small">
    {#if note}<span class="faint">{note}</span>{/if}
    {#if !grouped && diff && listed < total}
      <button class="btn sm" onclick={() => setGrouped(true)}>Group by class</button>
      <span class="faint">to see all</span>
    {/if}
    {#if members}
      <button class="btn sm ghost" onclick={() => setGrouped(true)}>Back to the classes</button>
      {#if members.more}
        <button
          class="btn sm ghost"
          onclick={() => members && openMembers(members.cls, members.offset + MEMBER_PAGE)}
          >Next members</button
        >
      {/if}
    {/if}
    {#if selectedClass}
      <button class="btn sm" onclick={() => selectedClass && openMembers(selectedClass)}
        >Open the changed members of {short(selectedClass)}</button
      >
    {/if}
  </footer>
</div>

<style>
  .dg {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    gap: 6px;
  }
  .dg-head,
  .dg-tools,
  .dg-play,
  .dg-foot {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    padding: 0 10px;
  }
  .dg-head {
    padding-top: 8px;
  }
  .title {
    display: flex;
    align-items: baseline;
    gap: 6px;
    flex-wrap: wrap;
    min-width: 0;
  }
  .counts .ins,
  td.ins,
  tr.ins .sign {
    color: var(--added);
  }
  .counts .del,
  td.del,
  tr.del .sign {
    color: var(--removed);
  }
  .spacer {
    flex: 1;
  }
  .check {
    display: inline-flex;
    gap: 4px;
    align-items: center;
  }
  .lapse-cap {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 52ch;
  }
  .dg-body {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: 1fr;
  }
  .dg-body.list {
    grid-template-columns: minmax(0, 3fr) minmax(0, 2fr);
  }
  .dg-canvas {
    position: relative;
    min-height: 260px;
  }
  .dg-list {
    overflow: auto;
    border-left: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .dg-list td {
    max-width: 28ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dg-empty {
    display: grid;
    place-items: center;
    align-content: center;
    gap: 6px;
    height: 100%;
    color: var(--text-2);
  }
  .sheet-toggle,
  .narrow-tabs {
    display: none;
  }
  .num {
    text-align: right;
  }
  @media (max-width: 640px) {
    .sheet-toggle {
      display: inline-flex;
    }
    .hide-xs {
      display: none;
    }
    /* the options fold into a sheet */
    .dg-tools {
      display: none;
    }
    .dg-tools.open {
      display: flex;
      position: absolute;
      z-index: 5;
      left: 8px;
      right: 8px;
      top: 48px;
      padding: 10px;
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: 8px;
      box-shadow: var(--shadow, 0 4px 18px rgb(0 0 0 / 0.18));
    }
    .dg {
      position: relative;
    }
    /* the play controls stay in a bar at the bottom */
    .dg-play {
      order: 10;
      position: sticky;
      bottom: 0;
      padding: 6px 10px;
      background: var(--surface);
      border-top: 1px solid var(--border);
    }
    .lapse-cap {
      max-width: 100%;
      flex-basis: 100%;
    }
    .narrow-tabs {
      display: flex;
      padding: 0 10px;
    }
    .dg-body,
    .dg-body.list {
      grid-template-columns: 1fr;
    }
    .dg-body[data-tab='changes'] .dg-canvas {
      display: none;
    }
    .dg-body[data-tab='graph'] .dg-list {
      display: none;
    }
    .dg-list {
      border-left: 0;
    }
  }
</style>

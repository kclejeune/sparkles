<script lang="ts" module>
  import { LayoutStore } from '$lib/pattern-layout';

  /** The layout maps of every editor tab, shared by the pane and the result's tab. */
  export const layouts = new LayoutStore();

  /** What **Count matches** answers: a count, or that the count ran out of time. */
  export type CountAnswer = number | 'timeout';

  /** A node added on the canvas that no triple uses yet (§2.2). */
  type Waiting = {
    id: string;
    kind: 'var' | 'iri';
    /** the term an edge writes */
    text: string;
    /** for a class node, the class as an edge writes it in `?x a C` */
    cls?: string;
  };

  /** The waiting nodes of each editor tab, in memory only, so a pane mounted again for
   * the tab, after the phone's Text tab or another editor tab, still has them. */
  const waitingByTab = new Map<string, Waiting[]>();
</script>

<script lang="ts">
  // The Pattern pane of spec U08: the drawing of a query's pattern beside the editor, or
  // of the query that produced a result (`readonly`). The text is the only source of
  // truth. The pane parses it 150 ms after it stops changing, with the browser module of
  // sparkles-fmt, and draws the answer. A canvas edit is a list of changes the page applies
  // to the editor as one transaction, after which the pane redraws from the new text like
  // after any typing. Text that does not parse leaves the last pattern on screen, dimmed,
  // and the pane refuses edits until it parses again.
  //
  // Phase 2 adds the edits of §2.2. **+ Node** puts a waiting node on the canvas, which
  // only the pane holds until an edge uses it, since SPARQL has no free variables.
  // **+ Edge** writes a triple, with `?x a C` first when an end is a waiting class node.
  // The panel of an element holds its edits, and the strip under the canvas holds the
  // projection, DISTINCT, LIMIT, OFFSET and **Count matches**.
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import type { ClassProfile, CursorPlan, PlanNode } from '$lib/api';
  import {
    badge,
    filterLabel,
    label as planLabel,
    misestimate,
    LEGEND,
    overlay as overlayOf,
    spoken,
    under,
    width as planWidth,
    type Overlay,
  } from '$lib/plan-pattern';
  import { LocalUnavailable, patternLocal, patternNow } from '$lib/fmt-wasm';
  import {
    announcement,
    applyChanges,
    checkTerm,
    compact,
    describe,
    effectivePrefixes,
    drawing as drawingOf,
    EditError,
    lockedOf,
    MAX_TEXT,
    missingPrefixes,
    overCaps,
    RDF_TYPE,
    renamed,
    resolveTerm,
    stillNames,
    termKey,
    textOrder,
    triplesOf,
    type Change,
    type Drawing,
    type PatternAnswer,
    type PatternError,
    type PatternOk,
    type PNode,
    type Prefixes,
  } from '$lib/pattern';
  import {
    agreesWith,
    boundVars,
    checkVarName,
    countQuery,
    describeEdit,
    editAny,
    filterKeys,
    lockedBlocks,
    outerFilterVars,
    pathPrefixes,
    patternVars,
    renameReach,
    starVars,
    unboundUses,
    type AnyEdit,
    type Phase2Edit,
    type Words,
  } from '$lib/pattern-edits';
  import { kept, type Layout } from '$lib/pattern-layout';
  import {
    datatypesOf,
    narrow,
    ProfileCache,
    rankClasses,
    rankPredicates,
    varName,
    type Choice,
    type SchemaSource,
  } from '$lib/pattern-schema';
  import type { PrefixMap } from '$lib/rdf';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';
  import PatternCanvas, { type CanvasOverlay, type Focusable } from './PatternCanvas.svelte';
  import PatternPanel from './PatternPanel.svelte';
  import PatternPicker from './PatternPicker.svelte';

  let {
    text,
    tab = null,
    readonly = false,
    phone = false,
    prefixes = {},
    schema = null,
    count,
    countKey = '',
    onedit,
    onundo,
    onredo,
    onescape,
    onclose,
    debounce = 150,
    plan = null,
    planText = null,
    planExecuted = true,
    planHeader = null,
    planSelected = null,
    onplanselect,
    onshowplan,
    planSwitch = null,
    onopen,
  }: {
    text: string;
    /** the editor tab whose layout the pane keeps; null for a result's pattern */
    tab?: string | null;
    readonly?: boolean;
    phone?: boolean;
    /** the dataset's declared prefixes, for a prefixed name the query does not declare */
    prefixes?: PrefixMap;
    /** the schema behind the pickers, at the page's branch and state */
    schema?: SchemaSource | null;
    /** run a count query under the five-second timeout of §2.2 */
    count?: (sparql: string, signal: AbortSignal) => Promise<CountAnswer>;
    /** the dataset, branch and state a count reads; a change clears the count */
    countKey?: string;
    /** apply changes to the editor as one transaction */
    onedit?: (changes: Change[]) => void;
    onundo?: () => void;
    onredo?: () => void;
    /** return focus to the editor */
    onescape?: () => void;
    onclose?: () => void;
    debounce?: number;
    /** the plan to draw on the pattern (spec U04 §11), when **Plan on pattern** is on */
    plan?: PlanNode | CursorPlan | null;
    /** the query text the plan is of: the overlay shows only while the pane draws it */
    planText?: string | null;
    /** the plan ran, so it has actual rows and times */
    planExecuted?: boolean;
    /** the commit and branch the plan's numbers describe */
    planHeader?: string | null;
    /** the operator selected in the plan tree, whose elements light up */
    planSelected?: string | null;
    /** an element was chosen whose operator the plan tree should select */
    onplanselect?: (id: string) => void;
    /** show this operator in the plan tree (the phone's sheet) */
    onshowplan?: (id: string) => void;
    /** the shared **Plan on pattern** switch, shown in the header when a plan exists */
    planSwitch?: { on: boolean; set: (on: boolean) => void } | null;
    /** a result's pattern: put the query in the editor and open the Pattern pane there */
    onopen?: () => void;
  } = $props();

  /** What an empty editor is edited as: the first edit writes the whole query. */
  const BLANK = 'SELECT * WHERE {\n}\n';
  const source = (t: string) => (t.trim() === '' ? BLANK : t);

  let answer = $state<PatternOk | null>(null);
  let error = $state<PatternError | null>(null);
  let unavailable = $state<string | null>(null);
  let tooLarge = $state(false);
  let drawn = $state<Drawing | null>(null);
  let layout = $state<Layout>({});
  /** the text the drawing is of */
  let drawnText = $state('');
  let mode = $state<'canvas' | 'list'>('canvas');
  let selected = $state<Focusable | null>(null);
  /** what the panel opens with: the filter form, after `F` */
  let intent = $state<'filter' | null>(null);
  // the page keys a pane per tab, so the tab it starts with is the tab it keeps
  let waiting = $state<Waiting[]>(untrack(() => (tab && waitingByTab.get(tab)) || []));
  $effect(() => {
    if (!tab) return;
    if (waiting.length) waitingByTab.set(tab, waiting);
    else waitingByTab.delete(tab);
  });
  // the edge form: from, predicate, to
  let edgeOpen = $state(false);
  let edgeS = $state('');
  let edgeP = $state('');
  let edgeO = $state('');
  let edgeError = $state<string | null>(null);
  // the node form
  let nodeOpen = $state(false);
  let nodeText = $state('');
  let nodeClass = $state(false);
  let nodeError = $state<string | null>(null);
  /** the two-tap edge of the phone (§2.6) */
  let tapEdge = $state<null | { stage: 'source' } | { stage: 'target'; source: string }>(null);
  let optionsOpen = $state(false);
  let confirm = $state<{
    title: string;
    lead: string;
    blocks: string[];
    actions: { label: string; danger?: boolean; run: () => void }[];
  } | null>(null);
  let countText = $state<string | null>(null);
  let counting = $state(false);
  let countAbort: AbortController | null = null;
  let live = $state('');
  let canvas: PatternCanvas | undefined = $state();
  let root: HTMLElement;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let seq = 0;
  /** timings of the last redraw, for the performance test (§4.6) */
  let timing = $state({ parseMs: 0, drawMs: 0 });
  /** bumped when the schema or a profile arrives, so the pickers read again */
  let schemaRev = $state(0);
  /** Profiles of the current schema source; a new source starts with an empty cache. */
  let profiles: ProfileCache | null = null;
  /** The classes `datatypes()` reads at most, far under the cache's bound. */
  const DATATYPE_CLASSES = 20;

  // ------------------------------------------------- the plan on the pattern (U04 §11) ----

  /** Show each operator's own time in place of its rows (`T`). */
  let planTime = $state(false);
  /** the element under the pointer or the focus, for a join badge's highlight */
  let hovered = $state<string | null>(null);
  let notDrawnOpen = $state(false);

  /** The plan matched with the drawing, when the pane draws the text the plan is of. */
  const planMatch = $derived.by<Overlay | null>(() => {
    if (!plan || planText == null || !answer || !drawn || error || planText !== drawnText)
      return null;
    try {
      return overlayOf(answer, drawn, plan, planExecuted);
    } catch {
      return null;
    }
  });
  /** A plan is given but the text differs from the one that ran (A19). */
  const planStale = $derived(!!plan && planText != null && !!drawn && planText !== drawnText);

  /** The elements an operator's subtree reads: edges, their ends, chips, classes and joins. */
  function elementsUnder(m: Overlay, opId: string): string[] {
    const out = new Set<string>();
    for (const [id, st] of m.edges)
      if (st.ops.some((o) => under(o.id, opId))) {
        out.add(id);
        const e = drawn?.edges.find((x) => x.id === id);
        if (e) {
          out.add(e.source);
          out.add(e.target);
        }
      }
    for (const [id, list] of m.classes)
      if (list.some((st) => st.ops.some((o) => under(o.id, opId)))) out.add(id);
    for (const j of m.joins)
      if (under(j.op.id, opId)) for (const v of j.drawnVars) out.add(`v:${v}`);
    for (const [key, f] of m.filters)
      if (under(f.op.id, opId))
        for (const n of shown?.nodes ?? []) if (n.filters?.includes(key)) out.add(n.id);
    for (const [id, st] of m.locked) if (under(st.op.id, opId)) out.add(id);
    return [...out];
  }

  /** The operator an element of the canvas stands for: an edge's, a badge's, a chip's. */
  function opOf(m: Overlay, el: Focusable): string | null {
    if ('edge' in el) return m.edges.get(el.edge.id)?.op.id ?? null;
    const n = el.node;
    const join = m.joins.find((j) => j.drawnVars.some((v) => n.id === `v:${v}`));
    if (join) return join.op.id;
    for (const key of n.filters ?? []) {
      const f = m.filters.get(key);
      if (f) return f.op.id;
    }
    return m.locked.get(n.id)?.op.id ?? m.classes.get(n.id)?.[0]?.op.id ?? null;
  }

  const canvasOverlay = $derived.by<CanvasOverlay | null>(() => {
    const m = planMatch;
    if (!m || !shown) return null;
    const o = { time: planTime, phone };
    const edges: CanvasOverlay['edges'] = {};
    for (const [id, st] of m.edges) {
      const shared = st.shared ? (phone ? ' ⋈' : ' · shared') : '';
      edges[id] = {
        text: planLabel(st, o) + shared,
        width: planWidth(m.executed && st.rows >= 0 ? st.rows : st.est),
        warn: !planTime && !!st.ratio,
        hot: planTime && m.hot === st.op.id,
        spoken:
          spoken(st, planTime) +
          (st.shared
            ? `, read by ${st.op.node.operator} with ${st.shared} other ${st.shared === 1 ? 'edge' : 'edges'}`
            : '') +
          (st.ops.length > 1 ? `, read by ${st.ops.length} operators` : ''),
      };
    }
    const nodes: CanvasOverlay['nodes'] = {};
    const add = (id: string, line: string, warn: boolean, hot: boolean, said: string) => {
      const n = (nodes[id] ??= { lines: [], warn: false, hot: false, spoken: '' });
      n.lines.push(hot ? `${line} ●` : line);
      n.warn ||= warn;
      n.hot ||= hot;
      n.spoken = n.spoken ? `${n.spoken}, ${said}` : said;
    };
    for (const [id, list] of m.classes)
      for (const st of list)
        add(
          id,
          `a ${planLabel(st, o)}`,
          !planTime && !!st.ratio,
          planTime && m.hot === st.op.id,
          `class ${spoken(st, planTime)}`,
        );
    for (const [key, f] of m.filters) {
      const n = shown.nodes.find((x) => x.filters?.includes(key));
      if (!n) continue;
      const text = filterLabel(f, o);
      if (text)
        add(
          n.id,
          `filter ${text}`,
          false,
          planTime && m.hot === f.op.id,
          f.passed >= 0 ? `filter passed ${f.passed} of ${f.reached} rows` : `filter ${text}`,
        );
    }
    for (const [id, st] of m.locked)
      add(
        id,
        `${st.op.node.operator} · ${planLabel(st, o)}`,
        !planTime && !!st.ratio,
        planTime && m.hot === st.op.id,
        `${st.op.node.operator}, ${spoken(st, planTime)}`,
      );
    const joins: string[] = [];
    for (const j of m.joins) {
      for (const v of j.drawnVars) {
        const id = `v:${v}`;
        const warn = !planTime && misestimate(j.op.node, m.executed) != null;
        add(
          id,
          badge(j, o, m.executed) + (warn ? ' ⚠' : ''),
          warn,
          planTime && m.hot === j.op.id,
          `join ${j.n}, ${j.op.node.operator} on ?${v}` +
            (m.executed && j.op.node.actualRows >= 0 ? `, ${j.op.node.actualRows} rows` : ''),
        );
        if (!joins.includes(id)) joins.push(id);
      }
    }
    return { edges, nodes, joins };
  });

  /** What lights up: the edges below a hovered or focused join badge, else the elements of
   * the operator selected in the plan tree. */
  const planLit = $derived.by<string[] | null>(() => {
    const m = planMatch;
    if (!m) return null;
    const hoverJoins = hovered
      ? m.joins.filter((j) => j.drawnVars.some((v) => `v:${v}` === hovered))
      : [];
    if (hoverJoins.length)
      return [...new Set(hoverJoins.flatMap((j) => elementsUnder(m, j.op.id)))];
    if (planSelected) {
      const els = elementsUnder(m, planSelected);
      return els.length ? els : null;
    }
    return null;
  });

  /** What the panel of the selected element shows of the plan. */
  const planInfo = $derived.by(() => {
    const m = planMatch;
    if (!m || !selected) return null;
    const id = opOf(m, selected);
    const op = id ? m.ops.find((o) => o.id === id) : undefined;
    if (!op) return null;
    const n = op.node;
    const lines: string[] = [];
    if (n.estimatedRows >= 0)
      lines.push(`Estimated ${n.estimatedRows.toLocaleString('en-US')} rows`);
    if (m.executed && n.actualRows >= 0)
      lines.push(`Actual ${n.actualRows.toLocaleString('en-US')} rows`);
    if (m.executed) lines.push(`Own time ${op.self.toFixed(op.self < 1 ? 2 : 1)} ms`);
    const st = 'edge' in selected ? m.edges.get(selected.edge.id) : undefined;
    if (st?.ratio)
      lines.push(
        `⚠ ${st.ratio}: ${st.rows > st.est ? 'more than ten times' : 'under a tenth of'} the estimate`,
      );
    if (st?.shared)
      lines.push(
        `${n.operator} also reads ${st.shared} other ${st.shared === 1 ? 'edge' : 'edges'} of this pattern`,
      );
    if (st && st.ops.length > 1) lines.push(`${st.ops.length} operators read this edge`);
    return { id: op.id, operator: n.operator, description: n.description, lines };
  });

  const canEdit = $derived(!readonly && !!answer && !error && !!onedit);
  const over = $derived(!!answer && !!drawn && overCaps(answer, drawn));
  const shown = $derived(drawn && answer ? decorate(drawn, answer, waiting) : null);
  const rows = $derived(shown ? textOrder(shown) : []);
  /** The dataset's prefixes, apart from those whose names the query declares for its own
   * namespaces, which a picked IRI must not be written under. */
  const datasetPrefixes = $derived<Prefixes>(
    effectivePrefixes(answer?.prologue.prefixes ?? [], prefixes),
  );
  const schemaNote = $derived.by(() => {
    void schemaRev;
    if (!schema) return 'No schema is available here, so type an IRI or a prefixed name.';
    return schema.error
      ? `The schema did not load (${schema.error}), so type an IRI or a prefixed name.`
      : null;
  });

  /**
   * The drawing as shown: locked blocks that use a variable nothing binds get the warning
   * mark of §4.2 rule 4, and the waiting nodes are added, dimmed.
   */
  function decorate(d: Drawing, a: PatternOk, w: Waiting[]): Drawing {
    const blocks = new Map(lockedBlocks(a).map((b) => [b.key, b.block]));
    const bound = boundVars(a);
    const nodes: PNode[] = d.nodes.map((n) => {
      const b = n.kind === 'locked' ? blocks.get(n.id) : undefined;
      // a locked filter warns of a variable it reads outside its EXISTS patterns
      const unbound = b
        ? unboundUses(a, b)
        : n.kind === 'chip' && n.locked
          ? outerFilterVars(n.locked.text).filter((v) => !bound.has(v))
          : [];
      if (!unbound.length) return n;
      const names = unbound.map((v) => `?${v}`).join(', ');
      return {
        ...n,
        warn: true,
        subtitle: [...n.subtitle, `uses ${names}, which nothing binds now`],
      };
    });
    const have = new Set(nodes.map((n) => n.id));
    w.forEach((x, i) => {
      if (have.has(x.id)) return;
      nodes.push({
        id: x.id,
        kind: x.kind,
        label: x.text,
        subtitle: x.cls ? [x.cls] : [],
        title: x.cls
          ? `${x.text}, a ${x.cls} once an edge uses it`
          : `${x.text}, waiting for an edge`,
        at: 1e9 + i,
        waiting: true,
      });
    });
    return { ...d, nodes };
  }

  // a waiting node leaves the list once a triple uses it
  $effect(() => {
    const d = drawn;
    if (!d) return;
    const left = waiting.filter((w) => !d.nodes.some((n) => n.id === w.id));
    if (left.length !== waiting.length) waiting = left;
  });

  function show(text: string, a: PatternAnswer, parseMs: number) {
    const t0 = performance.now();
    if (!a.ok) {
      error = a;
      return;
    }
    error = null;
    const next = drawingOf(a);
    if (tab && drawn) {
      // a variable renamed in the editor keeps its place, with its literals
      for (const [from, to] of renamed(drawn, next) ?? []) layouts.rename(tab, from, to);
    }
    // the open panel follows its element by id, or closes when it is gone
    if (selected) {
      const id = 'node' in selected ? selected.node.id : selected.edge.id;
      const node = next.nodes.find((n) => n.id === id);
      const edge =
        node || !drawn || !stillNames(drawn, next, id)
          ? undefined
          : next.edges.find((x) => x.id === id);
      const still = 'node' in selected && selected.node.waiting && waiting.some((w) => w.id === id);
      selected = node ? { node } : edge ? { edge } : still ? selected : null;
    }
    const map = tab ? layouts.get(tab, text) : (layouts.stored(text) ?? {});
    layout = kept(map, [...next.nodes.map((n) => n.id), ...waiting.map((w) => w.id)]);
    answer = a;
    drawn = next;
    drawnText = text;
    timing = { parseMs, drawMs: performance.now() - t0 };
  }

  async function parse(raw: string) {
    const mine = ++seq;
    if (raw.length > MAX_TEXT) {
      tooLarge = true;
      return;
    }
    tooLarge = false;
    const text = readonly ? raw : source(raw);
    const t0 = performance.now();
    const now = patternNow(text);
    if (now) return show(text, now, performance.now() - t0);
    try {
      const a = await patternLocal(text);
      if (mine === seq) show(text, a, performance.now() - t0);
      unavailable = null;
    } catch (e) {
      if (!(e instanceof LocalUnavailable)) throw e;
      unavailable = e.message;
    }
  }

  // parse 150 ms after the text stops changing; the first time at once
  let first = true;
  $effect(() => {
    const t = text;
    clearTimeout(timer);
    if (first) {
      first = false;
      void parse(t);
      return;
    }
    timer = setTimeout(() => void parse(t), debounce);
  });

  // a count is of the text, the drawing and the state it ran on
  $effect(() => {
    void text;
    void drawnText;
    void countKey;
    countAbort?.abort();
    countText = null;
    counting = false;
  });

  // the schema loads once the pane can edit, and profiles belong to their source
  $effect(() => {
    const s = schema;
    profiles = s ? new ProfileCache((cls) => s.profile(cls)) : null;
    if (!s || readonly) return;
    void s.load().then(() => s === schema && schemaRev++);
  });

  onDestroy(() => {
    clearTimeout(timer);
    countAbort?.abort();
  });

  function onlayout(l: Layout) {
    layout = l;
    if (tab) layouts.set(tab, drawnText, l);
  }

  const parseNow = (t: string): PatternAnswer =>
    patternNow(t) ?? {
      ok: false,
      code: 'unavailable',
      error: 'The browser module is not ready',
      line: 0,
      column: 0,
    };

  /** The edge or node an edit names, which must still be there after typing. */
  function named(e: AnyEdit): { edge?: string; node?: string } {
    if (e.kind === 'delete' || e.kind === 'optional' || e.kind === 'predicate')
      return { edge: e.edge };
    if (e.kind === 'delete-node') return { node: e.node };
    return {};
  }

  /** What a filter's or a locked block's key names in a pattern: its text and where it
   * sits, so a key that typing moved onto another copy is noticed. */
  function keyed(p: PatternOk, e: AnyEdit): string | null | undefined {
    if (e.kind === 'filter-edit' || e.kind === 'filter-delete') {
      const f = filterKeys(p).find((x) => x.key === e.filter);
      if (!f) return null;
      const depth = triplesOf(p).find((t) => t.path.at(-1) === f.group)?.path.length ?? 1;
      return `${f.filter.text}|${f.group.kind}|${depth}`;
    }
    if (e.kind === 'delete-locked') {
      const b = lockedBlocks(p).find((x) => x.key === e.block);
      return b ? `${b.block.text}|${b.block.range.from - (p.root?.range.from ?? 0)}` : null;
    }
    return undefined;
  }

  /** The nodes and edges of a drawing, as an announcement ends. */
  const counts = (d: Drawing) =>
    `${d.nodes.filter((n) => n.kind !== 'chip').length} nodes, ${d.edges.filter((x) => x.style !== 'dotted').length} edges`;

  /** Make an edit: the changes go to the editor, which the pane then redraws from. */
  function apply(e: AnyEdit, refuse: (why: string) => void = (why) => (live = why)): boolean {
    if (!answer || !canEdit) return false;
    // typing since the last redraw: the ranges are of the text as it is now
    const now = source(text);
    if (now !== drawnText) {
      const a = patternNow(now);
      if (!a?.ok) return false;
      const prev = drawn;
      const was = keyed(answer, e);
      show(now, a, 0);
      if (was !== undefined && (was === null || was !== keyed(a, e))) {
        refuse('The pattern changed, select it again');
        return false;
      }
      // the element was picked in the drawing before the typing
      const n = named(e);
      if (n.edge && (!prev || !drawn || !stillNames(prev, drawn, n.edge))) {
        refuse('The pattern changed, select the edge again');
        return false;
      }
      if (n.node && !drawn?.nodes.some((x) => x.id === n.node)) {
        refuse('The pattern changed, select the node again');
        return false;
      }
    }
    const before = answer;
    let changes: Change[];
    try {
      changes = editAny(drawnText, before, e, parseNow);
    } catch (err) {
      if (err instanceof EditError) {
        refuse(err.message);
        return false;
      }
      throw err;
    }
    // a canvas edit never lands in text the server would refuse, and the parse of the
    // result must be the pattern the edit describes (R3)
    const next = applyChanges(drawnText, changes);
    const check = patternNow(next, true);
    if (!check) {
      refuse('The browser module is not ready, so the edit was not made');
      return false;
    }
    if (!check.ok) {
      refuse(`That edit would not parse: ${check.error}`);
      return false;
    }
    if (!agreesWith(before, e, check)) {
      refuse('That edit would change more of the pattern than it names, so it was not made');
      return false;
    }
    // an empty editor gets the whole query at once
    if (drawnText !== text) changes = [{ from: 0, to: text.length, insert: next }];
    onedit!(changes);
    const after = drawingOf(check);
    live =
      e.kind === 'add' || e.kind === 'delete' || e.kind === 'optional'
        ? announcement(e, before, after)
        : `${describeEdit(e as Phase2Edit)}, ${counts(after)}`;
    selected = null;
    intent = null;
    return true;
  }

  // ------------------------------------------------------------- terms and classes ----

  /** The term an edge writes for a node. */
  function termOf(id: string): string | null {
    const w = waiting.find((x) => x.id === id);
    if (w) return w.text;
    if (id.startsWith('v:')) return `?${id.slice(2)}`;
    if (!answer) return null;
    for (const { triple: t } of triplesOf(answer))
      for (const term of [t.s, t.o])
        if (term.kind !== 'literal' && termKey(term) === id) return term.text;
    return null;
  }

  /** The nodes an edge may start or end at, as their terms. */
  const ends = $derived(
    (shown?.nodes ?? [])
      .filter((n) => n.kind === 'var' || n.kind === 'iri' || n.kind === 'bnode')
      .map((n) => ({ id: n.id, term: termOf(n.id) ?? n.label, waiting: !!n.waiting })),
  );

  /** The full IRI of a typed term, or null when it is not one. */
  function iriOf(t: string): string | null {
    if (!answer) return null;
    try {
      const term = resolveTerm(t.trim(), answer, datasetPrefixes);
      return term.kind === 'iri' ? term.value : null;
    } catch {
      return null;
    }
  }

  /** The classes of the node a term names: its `?x a C` triples, or its waiting class. */
  function classesOf(term: string): string[] {
    const t = term.trim();
    const w = waiting.find((x) => x.text === t);
    if (w?.cls) return [iriOf(w.cls)].filter((x): x is string => !!x);
    if (!answer) return [];
    const out: string[] = [];
    for (const { triple } of triplesOf(answer))
      if (
        triple.s.text === t &&
        triple.p.kind === 'iri' &&
        triple.p.value === RDF_TYPE &&
        triple.o.kind === 'iri'
      )
        out.push(triple.o.value);
    return out;
  }

  /** The profile of a class from the cache, fetched when missing. */
  function profileOf(cls: string | undefined): ClassProfile | null {
    if (!cls || !schema) return null;
    const mine = profiles;
    if (!mine) return null;
    // an answer for a cache that a new source replaced since redraws nothing
    return mine.get(cls, () => mine === profiles && schemaRev++);
  }

  /** How a picker writes an IRI: a prefixed name of the query or the dataset, else `<iri>`. */
  const short = (iri: string) =>
    compact(iri, [...(answer?.prologue.prefixes ?? []), ...datasetPrefixes]);

  /** The predicates for an edge between two terms, ranked by their classes (§2.2). */
  function predicatesFor(from: string, to: string, typed: string): Choice[] {
    void schemaRev;
    if (!schema) return [];
    const src = profileOf(classesOf(from)[0]);
    const tgt = profileOf(classesOf(to)[0]);
    return narrow(rankPredicates(schema.predicates, src, tgt), typed, short);
  }

  /** The datatypes a variable's values were seen with, from the profiles of its subjects. */
  function datatypes(v: string): string[] | null {
    void schemaRev;
    if (!answer) return null;
    const out = new Set<string>();
    const read = new Set<string>();
    for (const { triple } of triplesOf(answer)) {
      if (triple.o.kind !== 'var' || triple.o.value !== v || triple.p.kind !== 'iri') continue;
      for (const cls of classesOf(triple.s.text)) {
        if (!read.has(cls) && read.size >= DATATYPE_CLASSES) continue;
        read.add(cls);
        for (const dt of datatypesOf(profileOf(cls), triple.p.value) ?? []) out.add(dt);
      }
    }
    return out.size ? [...out] : null;
  }

  const classChoices = (typed: string): Choice[] => {
    void schemaRev;
    return schema
      ? narrow(rankClasses(schema.classes), typed, short).map((c) => ({ ...c, hint: 'class' }))
      : [];
  };
  const endChoices = (typed: string): Choice[] =>
    narrow(
      ends.map((x) => ({ iri: x.term, count: null, hint: x.waiting ? 'waiting' : undefined })),
      typed,
      (t) => t,
    );

  // ------------------------------------------------------------------- the forms ----

  function closeForms() {
    edgeOpen = nodeOpen = false;
    edgeError = nodeError = null;
    tapEdge = null;
  }

  async function openNode() {
    closeForms();
    nodeOpen = true;
    nodeText = '';
    nodeClass = false;
    await tick();
    root?.querySelector<HTMLInputElement>('input[name="pattern-node"]')?.focus();
  }

  /** Open the edge form, from a node when one is given, to one when given too. */
  async function openEdge(from?: string, to?: string) {
    closeForms();
    edgeOpen = true;
    edgeS = from ? (termOf(from) ?? '') : '';
    edgeO = to ? (termOf(to) ?? '') : '';
    edgeP = '';
    await tick();
    root
      ?.querySelector<HTMLInputElement>(`input[name="${from ? 'pattern-p' : 'pattern-s'}"]`)
      ?.focus();
  }

  /** Add a waiting node: a variable, an IRI, or a class that becomes `?x a C` when linked. */
  function addNode(ev: SubmitEvent) {
    ev.preventDefault();
    if (!answer) return;
    const t = nodeText.trim();
    const fail = (why: string) => {
      nodeError = why;
      live = why;
    };
    if (!t) return fail('Type a variable, an IRI or a class');
    let w: Waiting;
    try {
      if (/^[?$]/.test(t)) {
        const name = checkVarName(t);
        w = { id: `v:${name}`, kind: 'var', text: `?${name}` };
      } else {
        checkTerm(t, 's');
        const iri = iriOf(t);
        if (!iri)
          return fail(`${t} is not an IRI or a prefixed name the query or dataset declares`);
        const isClass = nodeClass || (schema?.classes.some((c) => c.iri === iri) ?? false);
        if (isClass) {
          const name = varName(iri, [
            ...patternVars(answer),
            ...waiting.map((x) => x.text.slice(1)),
          ]);
          w = {
            id: `v:${name}`,
            kind: 'var',
            text: `?${name}`,
            cls: t.startsWith('<') ? short(iri) : t,
          };
        } else w = { id: `i:${iri}`, kind: 'iri', text: t };
      }
    } catch (err) {
      if (err instanceof EditError) return fail(err.message);
      throw err;
    }
    if (shown?.nodes.some((n) => n.id === w.id)) return fail(`${w.text} is already in the pattern`);
    waiting = [...waiting, w];
    live = w.cls
      ? `Added ${w.text}, a ${w.cls}, waiting for an edge`
      : `Added ${w.text}, waiting for an edge`;
    nodeOpen = false;
    nodeError = null;
    void tick().then(() => canvas?.focus());
  }

  /** Add a class node from the empty state. */
  function addClass(iri: string) {
    nodeText = short(iri);
    nodeClass = true;
    addNode(new SubmitEvent('submit'));
  }

  function addEdge(ev: SubmitEvent) {
    ev.preventDefault();
    const s = edgeS.trim();
    const p = edgeP.trim();
    const o = edgeO.trim();
    const fail = (why: string) => {
      edgeError = why;
      live = why;
    };
    if (!s || !p || !o) return fail('An edge needs a source, a predicate and a target');
    // a waiting class node is written as `?x a C` after the edge, so the edge's source
    // decides the group they go in
    const triples: Words[] = [{ s, p, o }];
    for (const end of [s, o]) {
      const w = waiting.find((x) => x.text === end && x.cls);
      if (w && !triples.some((x) => x.s === w.text && x.p === 'a' && x.o === w.cls))
        triples.push({ s: w.text, p: 'a', o: w.cls! });
    }
    const words = triples.flatMap((x) => [x.s, x.p, x.o]);
    const declare = answer ? missingPrefixes(words, answer, prefixes) : [];
    const e: AnyEdit =
      triples.length === 1
        ? { kind: 'add', s, p, o, ...(declare.length ? { declare } : {}) }
        : { kind: 'add-many', triples, ...(declare.length ? { declare } : {}) };
    if (apply(e, fail)) {
      // the ends are written now, so they wait no longer, even before the redraw
      waiting = waiting.filter((w) => w.text !== s && w.text !== o);
      edgeOpen = false;
      edgeError = null;
      edgeS = edgeP = edgeO = '';
      void tick().then(() => canvas?.focus());
    }
  }

  // ------------------------------------------------------------- panel callbacks ----

  /** A panel's edit, with PREFIX lines for the dataset's prefixes its terms use. */
  function withDeclare(e: AnyEdit): AnyEdit {
    if (!answer) return e;
    const terms =
      e.kind === 'predicate'
        ? pathPrefixes(e.p)
        : e.kind === 'filter-add' || e.kind === 'filter-edit'
          ? pathPrefixes(e.expr)
          : [];
    const declared = new Set(answer.prologue.prefixes.map((x) => x.prefix));
    const declare = [...new Set(terms)]
      .filter((x) => !declared.has(x) && prefixes[x])
      .map((prefix) => ({ prefix, iri: prefixes[prefix] }));
    return declare.length ? ({ ...e, declare } as AnyEdit) : e;
  }

  function rename(from: string, to: string) {
    if (!answer) return;
    let name: string;
    try {
      name = checkVarName(to);
    } catch (err) {
      if (err instanceof EditError) return void (live = err.message);
      throw err;
    }
    const reach = renameReach(answer, from);
    if (!reach.locked.length) return void apply({ kind: 'rename', from, to: name, locked: false });
    confirm = {
      title: `Rename ?${from} to ?${name}`,
      lead: `?${from} is used in ${reach.locked.length === 1 ? 'a locked block' : `${reach.locked.length} locked blocks`}, which the canvas changes only when you say so.`,
      blocks: reach.locked.map((b) => b.text),
      actions: [
        {
          label: 'Rename in the locked blocks too',
          run: () => apply({ kind: 'rename', from, to: name, locked: true }),
        },
        {
          label: 'Leave the locked blocks as they are',
          run: () => apply({ kind: 'rename', from, to: name, locked: false }),
        },
      ],
    };
  }

  /** Delete a locked block or a filter drawn as a box, after a confirmation that shows it. */
  function deleteLocked(key: string, blockText: string, filter: boolean) {
    confirm = {
      title: filter ? 'Delete this filter' : 'Delete this locked block',
      lead: 'The canvas deletes exactly this text from the query.',
      blocks: [blockText],
      actions: [
        {
          label: 'Delete',
          danger: true,
          run: () =>
            apply(
              filter
                ? { kind: 'filter-delete', filter: key }
                : { kind: 'delete-locked', block: key },
            ),
        },
      ],
    };
  }

  function removeWaiting(id: string) {
    const w = waiting.find((x) => x.id === id);
    waiting = waiting.filter((x) => x.id !== id);
    selected = null;
    if (w) live = `Removed the waiting node ${w.text}`;
  }

  /** The canvas picked an element, or a tap of the phone's two-tap edge did. */
  function onselect(el: Focusable | null) {
    if (tapEdge) {
      if (!el || !('node' in el) || !ends.some((x) => x.id === el.node.id)) return;
      if (tapEdge.stage === 'source') {
        tapEdge = { stage: 'target', source: el.node.id };
        live = `From ${el.node.label}. Tap the target node, or New node.`;
      } else void openEdge(tapEdge.source, el.node.id);
      return;
    }
    selected = el;
    intent = null;
    const m = planMatch;
    if (m && el) {
      const id = opOf(m, el);
      if (id) onplanselect?.(id);
    }
  }

  function startTapEdge() {
    closeForms();
    selected = null;
    tapEdge = { stage: 'source' };
    live = 'Tap the source node';
  }

  // -------------------------------------------------------------------- the strip ----

  const vars = $derived(answer ? starVars(answer) : []);

  /** Set LIMIT or OFFSET from its field, which shows the query's value again when the
   * edit is refused. */
  function setNumber(kind: 'limit' | 'offset', el: HTMLInputElement) {
    const v = el.value.trim();
    const value = v === '' ? null : Number(v);
    const cur = answer?.[kind]?.value ?? null;
    const reset = () => (el.value = cur === null ? '' : String(cur));
    if (value !== null && (!Number.isInteger(value) || value < 0)) {
      live = `${kind.toUpperCase()} takes a whole number`;
      return reset();
    }
    if (value === (cur === null ? null : Number(cur))) return;
    if (!apply({ kind, value })) reset();
  }

  async function runCount() {
    if (!answer || !count) return;
    // typing since the last redraw: count the text as it is now
    const now = source(text);
    if (now !== drawnText) {
      const a = patternNow(now);
      if (!a?.ok) return void (countText = 'The text does not parse, so there is nothing to count');
      show(now, a, 0);
      await tick();
    }
    if (!answer) return;
    let sparql: string;
    try {
      sparql = countQuery(drawnText, answer);
    } catch (err) {
      if (err instanceof EditError) return void (countText = err.message);
      throw err;
    }
    countAbort?.abort();
    const ctl = new AbortController();
    countAbort = ctl;
    counting = true;
    countText = null;
    try {
      const n = await count(sparql, ctl.signal);
      if (ctl.signal.aborted) return;
      countText =
        n === 'timeout'
          ? 'More than the count could finish'
          : `${n.toLocaleString()} ${n === 1 ? 'match' : 'matches'}`;
    } catch (err) {
      if (ctl.signal.aborted) return;
      countText = `The count failed: ${err instanceof Error ? err.message : String(err)}`;
    } finally {
      if (countAbort === ctl) counting = false;
    }
    live = countText ?? '';
  }

  // ------------------------------------------------------------------------ keys ----

  /** Undo, redo and Escape anywhere in the pane but its text inputs, the canvas, the list
   * and the panel alike. */
  function paneKey(e: KeyboardEvent) {
    const t = e.target as HTMLElement | null;
    const inField = !!t?.closest('input, textarea, select, [contenteditable="true"]');
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === 'Escape' && !mod && inField && (edgeOpen || nodeOpen)) {
      // Escape in a form closes it and returns to the canvas
      e.preventDefault();
      closeForms();
      canvas?.focus();
      return;
    }
    if (inField) return;
    if (mod && !e.altKey && (e.key === 'z' || e.key === 'Z')) {
      e.preventDefault();
      if (e.shiftKey) onredo?.();
      else onundo?.();
    } else if (mod && !e.altKey && (e.key === 'y' || e.key === 'Y')) {
      e.preventDefault();
      onredo?.();
    } else if (e.key === 'Escape' && !mod) {
      e.preventDefault();
      if (tapEdge || edgeOpen || nodeOpen || optionsOpen) {
        closeForms();
        optionsOpen = false;
        canvas?.focus();
      } else if (selected) {
        selected = null;
        canvas?.focus();
      } else onescape?.();
    }
  }

  /** The canvas's own keys on the focused element; the pane's keys bubble to `paneKey`. */
  function key(e: KeyboardEvent, el: Focusable | null) {
    if (planMatch && !e.ctrlKey && !e.metaKey && !e.altKey && (e.key === 't' || e.key === 'T')) {
      e.preventDefault();
      planTime = !planTime;
      live = planTime ? 'Showing each operator’s own time' : 'Showing rows';
      return;
    }
    if (e.ctrlKey || e.metaKey || e.altKey || !canEdit) return;
    const edge = el && 'edge' in el && el.edge.triple ? el.edge.id : undefined;
    const node = el && 'node' in el ? el.node : undefined;
    const k = e.key.toLowerCase();
    if (e.key === 'Delete' || e.key === 'Backspace') {
      if (edge) {
        e.preventDefault();
        apply({ kind: 'delete', edge });
      } else if (node?.waiting) {
        e.preventDefault();
        removeWaiting(node.id);
      } else if (
        node?.locked &&
        (node.kind === 'chip' || lockedBlocks(answer!).some((b) => b.key === node.id))
      ) {
        e.preventDefault();
        deleteLocked(
          node.kind === 'chip' ? node.filters![0] : node.id,
          node.locked.text,
          node.kind === 'chip',
        );
      } else if (
        node &&
        (node.kind === 'var' || node.kind === 'iri' || node.kind === 'bnode') &&
        !node.fromLocked
      ) {
        e.preventDefault();
        apply({ kind: 'delete-node', node: node.id });
      }
    } else if (k === 'o' && edge) {
      e.preventDefault();
      apply({ kind: 'optional', edge });
    } else if (k === 'n') {
      e.preventDefault();
      void openNode();
    } else if (k === 'e') {
      e.preventDefault();
      void openEdge(node && ends.some((x) => x.id === node.id) ? node.id : undefined);
    } else if (k === 'f' && node?.kind === 'var' && !node.fromLocked && !node.waiting) {
      e.preventDefault();
      selected = { node };
      intent = 'filter';
    }
  }

  const lineCol = (e: PatternError) => `line ${e.line}, column ${e.column}`;
  const modifiers = $derived(answer?.modifiers ?? []);

  /** Focus wanted before the first pattern arrived: given when the canvas or list appears. */
  let pendingFocus = $state(false);

  /** Focus the canvas, or the list. */
  export function focus() {
    if (!tryFocus()) pendingFocus = true;
  }

  function tryFocus(): boolean {
    const list = root?.querySelector<HTMLElement>('.plist button');
    if (mode === 'canvas' && canvas) canvas.focus();
    else if (mode === 'list' && list) list.focus();
    else return false;
    return true;
  }

  $effect(() => {
    if (!pendingFocus || !drawn) return;
    void tick().then(() => {
      pendingFocus = false;
      tryFocus();
    });
  });

  onMount(() => {
    root.dataset.ready = 'true';
  });
</script>

{#snippet options(a: PatternOk)}
  {#if a.projection}
    <span class="lbl">SELECT</span>
    {#if canEdit && a.form === 'select'}
      <label class="check"
        ><input
          type="checkbox"
          checked={a.projection.distinct}
          onchange={(e) => {
            const el = e.currentTarget;
            if (!apply({ kind: 'distinct', on: el.checked })) el.checked = !el.checked;
          }}
        /> DISTINCT</label
      >
      {#if a.projection.star}<span class="chip mono" title="Every variable is projected">*</span
        >{/if}
      <span class="vars" role="group" aria-label="Projected variables">
        {#each vars as v (v)}
          <label class="check mono"
            ><input
              type="checkbox"
              checked={a.projection.star || a.projection.vars.includes(v)}
              onchange={(e) => {
                const el = e.currentTarget;
                if (!apply({ kind: 'projection', var: v, on: el.checked }))
                  el.checked = !el.checked;
              }}
            />?{v}</label
          >
        {/each}
      </span>
    {:else}
      {#if a.projection.distinct}<span class="chip">DISTINCT</span>{/if}
      {#if a.projection.star}<span class="chip mono">*</span>{/if}
      {#each a.projection.vars as v (v)}<span class="chip mono">?{v}</span>{/each}
    {/if}
    {#if a.projection.locked}<span
        class="chip locked"
        title="Expressions in the projection are edited in the editor">locked</span
      >{/if}
  {:else}
    <span class="lbl">{a.form.toUpperCase()}</span>
  {/if}
  {#if canEdit && a.form === 'select'}
    <label class="num"
      >LIMIT <input
        type="number"
        min="0"
        step="1"
        inputmode="numeric"
        value={a.limit?.value ?? ''}
        onchange={(e) => setNumber('limit', e.currentTarget)}
      /></label
    >
    <label class="num"
      >OFFSET <input
        type="number"
        min="0"
        step="1"
        inputmode="numeric"
        value={a.offset?.value ?? ''}
        onchange={(e) => setNumber('offset', e.currentTarget)}
      /></label
    >
  {:else}
    {#if a.limit}<span class="chip mono">LIMIT {a.limit.value}</span>{/if}
    {#if a.offset}<span class="chip mono">OFFSET {a.offset.value}</span>{/if}
  {/if}
  {#each modifiers as m, i (i)}<span class="chip mono locked" title={m.text}>{m.kind}</span>{/each}
  {#if count && !readonly && a.root}
    <button class="btn sm" onclick={runCount} disabled={counting || !!error}
      >{counting ? 'Counting…' : 'Count matches'}</button
    >
    {#if countText}<span class="count" data-testid="pattern-count">{countText}</span>{/if}
  {/if}
{/snippet}

<!-- the pane's shortcuts reach it from every control inside; see paneKey -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section
  class="ppane"
  class:phone
  bind:this={root}
  onkeydown={paneKey}
  aria-label={readonly ? 'Pattern of the query that ran' : 'Pattern'}
  data-parse-ms={timing.parseMs.toFixed(2)}
  data-draw-ms={timing.drawMs.toFixed(2)}
  data-state={unavailable
    ? 'unavailable'
    : error
      ? error.code === 'update'
        ? 'update'
        : 'error'
      : answer
        ? 'ok'
        : 'loading'}
>
  <header class="phead">
    {#if !phone}<strong>Pattern</strong>{/if}
    {#if readonly}<span class="badge">read-only</span>{/if}
    {#if planSwitch}
      <label class="switch" title="Draw the plan's rows on the pattern">
        <input
          type="checkbox"
          checked={planSwitch.on}
          onchange={(e) => planSwitch?.set(e.currentTarget.checked)}
        />
        Plan on pattern
      </label>
    {/if}
    <span class="spacer"></span>
    {#if onopen}
      <button
        class="btn sm"
        title="Put this query in the editor and open the Pattern pane"
        onclick={onopen}>Edit</button
      >
    {/if}
    {#if canEdit}
      <button class="btn sm" onclick={openNode} title="Add a node (N)">
        <Icon name="plus" size={13} /> Node
      </button>
      <button
        class="btn sm"
        aria-pressed={!!tapEdge}
        onclick={() =>
          phone
            ? startTapEdge()
            : openEdge(selected && 'node' in selected ? selected.node.id : undefined)}
        title="Add an edge (E)"
      >
        <Icon name="plus" size={13} /> Edge
      </button>
    {/if}
    <div class="seg" role="group" aria-label="Show the pattern as">
      <button class="btn sm" aria-pressed={mode === 'canvas'} onclick={() => (mode = 'canvas')}
        >Canvas</button
      >
      <button class="btn sm" aria-pressed={mode === 'list'} onclick={() => (mode = 'list')}
        >List</button
      >
    </div>
    {#if mode === 'canvas' && drawn}
      <button
        class="btn sm icon"
        title="Fit to view"
        aria-label="Fit to view"
        onclick={() => canvas?.fit()}><Icon name="fit" size={14} /></button
      >
      <button class="btn sm" title="Lay the whole pattern out again" onclick={() => canvas?.tidy()}
        >Tidy</button
      >
    {/if}
    {#if onclose}
      <button
        class="btn ghost icon sm"
        aria-label="Close the Pattern pane"
        title="Close (Alt+P)"
        onclick={onclose}><Icon name="x" size={12} /></button
      >
    {/if}
  </header>

  {#if tapEdge && canEdit}
    <div class="tapbar" role="status">
      <span>{tapEdge.stage === 'source' ? 'Tap the source node' : 'Tap the target node'}</span>
      {#if tapEdge.stage === 'target'}
        <button
          class="btn sm"
          onclick={() => tapEdge?.stage === 'target' && openEdge(tapEdge.source)}>New node</button
        >
      {/if}
      <button class="btn sm ghost" onclick={() => (tapEdge = null)}>Cancel</button>
    </div>
  {/if}

  {#if nodeOpen && canEdit}
    <form class="add" class:full={phone} onsubmit={addNode} aria-label="Add a node">
      {#if phone}<strong class="formtitle">New node</strong>{/if}
      <PatternPicker
        label="Node"
        name="pattern-node"
        bind:value={nodeText}
        placeholder="?person, ex:lyon or a class"
        choices={classChoices}
        {short}
        note={schemaNote}
        more={!!schema?.next.classes}
        onmore={() => schema?.more('classes').then(() => schemaRev++)}
        onpick={() => (nodeClass = true)}
      />
      <label class="check"
        ><input type="checkbox" bind:checked={nodeClass} /> a class (?x a C)</label
      >
      <button class="btn sm primary" type="submit">Add</button>
      <button class="btn sm ghost" type="button" onclick={closeForms}>Cancel</button>
      {#if nodeError}<p class="err" role="alert">{nodeError}</p>{/if}
    </form>
  {/if}

  {#if edgeOpen && canEdit}
    <form class="add" class:full={phone} onsubmit={addEdge} aria-label="Add an edge">
      {#if phone}<strong class="formtitle">New edge</strong>{/if}
      <PatternPicker
        label="From"
        name="pattern-s"
        bind:value={edgeS}
        placeholder="?person"
        choices={endChoices}
        short={(t) => t}
      />
      <PatternPicker
        label="Predicate"
        name="pattern-p"
        bind:value={edgeP}
        placeholder="ex:worksFor"
        choices={(typed) => predicatesFor(edgeS, edgeO, typed)}
        {short}
        note={schemaNote}
        more={!!schema?.next.predicates}
        onmore={() => schema?.more('predicates').then(() => schemaRev++)}
      />
      <PatternPicker
        label="To"
        name="pattern-o"
        bind:value={edgeO}
        placeholder="?org"
        choices={endChoices}
        short={(t) => t}
      />
      <button class="btn sm primary" type="submit">Add</button>
      <button class="btn sm ghost" type="button" onclick={closeForms}>Cancel</button>
      {#if edgeError}<p class="err" role="alert">{edgeError}</p>{/if}
    </form>
  {/if}

  {#if planMatch && mode === 'canvas'}
    <div class="planbar" role="group" aria-label="Plan on pattern" data-testid="plan-bar">
      <span class="ptitle"
        ><Icon name="plan" size={12} /> Plan{planExecuted ? '' : ' estimate'}{planHeader
          ? ` · ${planHeader}`
          : ''}</span
      >
      <div class="seg" role="group" aria-label="Numbers on the pattern">
        <button class="btn sm" aria-pressed={!planTime} onclick={() => (planTime = false)}
          >Rows</button
        >
        <button
          class="btn sm"
          aria-pressed={planTime}
          disabled={!planExecuted}
          title="Each operator's own time (T)"
          onclick={() => (planTime = true)}>Time</button
        >
      </div>
      {#if planMatch.hiddenEstimates}
        <span class="faint small">Estimates are hidden from your view.</span>
      {:else if !phone}
        <span class="faint small legend" title={LEGEND}>⚠ over ten times off</span>
      {/if}
      {#if planMatch.unmatched.length}
        <button
          class="btn sm ghost"
          aria-expanded={notDrawnOpen}
          onclick={() => (notDrawnOpen = !notDrawnOpen)}
          data-testid="plan-not-drawn"
          >{planMatch.unmatched.length} operator{planMatch.unmatched.length === 1 ? '' : 's'} not drawn</button
        >
      {/if}
    </div>
    {#if notDrawnOpen && planMatch.unmatched.length}
      <ul class="notdrawn" aria-label="Operators not drawn">
        {#each planMatch.unmatched as u, i (i)}
          <li>
            <button
              class="linkish"
              onclick={() => {
                onplanselect?.(u.op.id);
                onshowplan?.(u.op.id);
              }}
              ><strong>{u.op.node.operator}</strong>
              <span class="mono">{u.pattern.s} {u.pattern.p} {u.pattern.o}</span>
              <span class="faint"
                >{planLabel(
                  {
                    op: u.op,
                    est: u.op.node.estimatedRows,
                    rows: planExecuted ? u.op.node.actualRows : -1,
                    timeMs: u.op.node.timeMs,
                    self: u.op.self,
                    ratio: null,
                  },
                  { time: planTime, phone },
                )}</span
              ></button
            >
          </li>
        {/each}
      </ul>
    {/if}
  {:else if planStale && !readonly}
    <p class="notice" role="status" data-testid="plan-stale">
      The plan is of the query that ran, and the text has changed since, so it is not drawn here.
    </p>
  {/if}

  <div class="pbody">
    {#if unavailable}
      <div class="state">
        <Icon name="alert" size={18} />
        <p>
          The visual query builder needs the browser module of the formatter, which did not load.
        </p>
      </div>
    {:else if tooLarge}
      <div class="state"><p>This query is longer than 256 KiB, more than the pane parses.</p></div>
    {:else if error?.code === 'update' && !answer}
      <div class="state"><p>Updates are not drawn. The builder edits queries.</p></div>
    {:else if error && !answer}
      <div class="state" role="status">
        <p>The query does not parse ({lineCol(error)}).</p>
        <p class="faint small">{error.error}</p>
      </div>
    {:else if answer && drawn && shown}
      {#if error}
        <p class="notice warn" role="status">
          {error.code === 'update'
            ? 'Updates are not drawn. The builder edits queries.'
            : `The text does not parse at ${lineCol(error)}, so this is the last pattern and edits wait until it parses.`}
        </p>
      {:else if over}
        <p class="notice" role="status">
          This pattern has {drawn.triples} triple patterns, more than the canvas draws.
        </p>
      {:else if shown.nodes.length === 0}
        <div class="notice empty" role="status">
          <span>Add a node to start.</span>
          {#if canEdit}
            <button class="btn sm" onclick={openNode}><Icon name="plus" size={13} /> Node</button>
            {#each schema && schemaRev >= 0 ? schema.topClasses(5) : [] as c (c.iri)}
              <button
                class="btn sm ghost mono"
                title="Add a {short(c.iri)} node"
                onclick={() => addClass(c.iri)}>{short(c.iri)}</button
              >
            {/each}
          {/if}
        </div>
      {:else if drawn.parts > 1}
        <p class="notice warn" role="status">
          These parts are not connected, so the query matches every combination.
        </p>
      {/if}
      {#if mode === 'canvas' && !over}
        <div class="canvas-host">
          <PatternCanvas
            bind:this={canvas}
            drawing={shown}
            {layout}
            dim={!!error}
            {phone}
            selected={selected ? ('node' in selected ? selected.node.id : selected.edge.id) : null}
            {onselect}
            {onlayout}
            onkey={key}
            overlay={canvasOverlay}
            lit={planLit}
            onhover={(id) => (hovered = id)}
          />
          {#if selected}
            <PatternPanel
              el={selected}
              drawing={shown}
              {answer}
              text={drawnText}
              readonly={!canEdit}
              sheet={phone}
              {intent}
              predicates={(typed) =>
                selected && 'edge' in selected
                  ? predicatesFor(
                      termOf(selected.edge.source) ?? '',
                      termOf(selected.edge.target) ?? '',
                      typed,
                    )
                  : []}
              {short}
              note={schemaNote}
              {datatypes}
              {planInfo}
              {onshowplan}
              onedit={(e) => apply(withDeclare(e))}
              onrename={rename}
              onedge={(id) => openEdge(id)}
              ondeletelocked={deleteLocked}
              onremovewaiting={removeWaiting}
              onclose={() => {
                selected = null;
                intent = null;
              }}
            />
          {/if}
        </div>
      {:else}
        <ol class="plist" aria-label="Pattern elements in text order">
          {#each rows as row, i (i)}
            {@const triple = 'edge' in row ? row.edge.triple : undefined}
            {@const edge = 'edge' in row ? row.edge.id : ''}
            {@const said =
              'edge' in row
                ? canvasOverlay?.edges[row.edge.id]?.spoken
                : canvasOverlay?.nodes[row.node.id]?.spoken}
            <li class:locked={'node' in row && !!row.node.locked}>
              <span class="desc">{describe(row, shown)}{said ? `, ${said}` : ''}</span>
              {#if triple && canEdit}
                <button class="btn sm" onclick={() => apply({ kind: 'optional', edge })}
                  >{triple.path.length > 1 ? 'Required' : 'Optional'}</button
                >
                <button
                  class="btn sm danger"
                  aria-label="Delete {triple.triple.s.text} {triple.triple.p.text} {triple.triple.o
                    .text}"
                  onclick={() => apply({ kind: 'delete', edge })}
                  ><Icon name="trash" size={12} /></button
                >
              {/if}
            </li>
          {/each}
        </ol>
      {/if}
      {#if phone && !readonly}
        <div class="strip">
          <button class="btn sm" aria-expanded={optionsOpen} onclick={() => (optionsOpen = true)}
            >Query options</button
          >
          {#if countText}<span class="count">{countText}</span>{/if}
          <span class="spacer"></span>
          <span class="faint small" data-testid="pattern-counts"
            >{drawn.triples} triples · {lockedOf(answer).length} locked</span
          >
        </div>
        {#if optionsOpen}
          <div class="sheet" role="dialog" aria-label="Query options">
            <div class="sheethead">
              <strong>Query options</strong>
              <button
                class="btn ghost icon sm"
                aria-label="Close"
                onclick={() => (optionsOpen = false)}><Icon name="x" size={12} /></button
              >
            </div>
            <div class="sheetbody">{@render options(answer)}</div>
          </div>
        {/if}
      {:else}
        <div class="strip" role="group" aria-label="Projection and modifiers">
          {@render options(answer)}
          <span class="spacer"></span>
          <span class="faint small" data-testid="pattern-counts"
            >{drawn.triples} triples · {lockedOf(answer).length} locked</span
          >
        </div>
      {/if}
    {:else}
      <div class="state"><p class="faint">Reading the pattern…</p></div>
    {/if}
  </div>
  <p class="sr-only" aria-live="polite" data-testid="pattern-live">{live}</p>
  <Modal open={!!confirm} title={confirm?.title ?? ''} width={480} onclose={() => (confirm = null)}>
    {#if confirm}
      <p>{confirm.lead}</p>
      {#each confirm.blocks as b, i (i)}<pre class="mono locked-text">{b}</pre>{/each}
    {/if}
    {#snippet actions()}
      {#if confirm}
        <button class="btn" onclick={() => (confirm = null)}>Cancel</button>
        {#each confirm.actions as a (a.label)}
          <button
            class="btn"
            class:danger={a.danger}
            class:primary={!a.danger}
            onclick={() => {
              const run = a.run;
              confirm = null;
              run();
            }}>{a.label}</button
          >
        {/each}
      {/if}
    {/snippet}
  </Modal>
</section>

<style>
  .ppane {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    height: 100%;
    background: var(--surface);
  }
  .phead {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
    font-size: var(--fs-sm);
  }
  .spacer {
    flex: 1;
  }
  .seg {
    display: inline-flex;
  }
  .seg .btn[aria-pressed='true'] {
    background: var(--spark-soft);
    border-color: var(--spark);
  }
  .add {
    /* above the canvas and its panel, so a picker's list opens over the drawing */
    position: relative;
    z-index: 4;
    display: flex;
    gap: 6px;
    padding: 6px 10px;
    flex-wrap: wrap;
    border-bottom: 1px solid var(--border);
  }
  .add input {
    flex: 1 1 110px;
    min-width: 0;
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .add .err {
    flex-basis: 100%;
    margin: 0;
    color: var(--danger);
    font-size: 12px;
  }
  .pbody {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .canvas-host {
    position: relative;
    flex: 1;
    min-height: 0;
  }
  .state {
    flex: 1;
    display: grid;
    place-items: center;
    align-content: center;
    gap: 6px;
    padding: 16px;
    text-align: center;
    color: var(--text-2);
  }
  .state p {
    margin: 0;
  }
  .notice {
    margin: 0;
    padding: 6px 10px;
    font-size: 12px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
  }
  .notice.warn {
    background: var(--warn-soft, var(--surface-2));
  }
  .plist {
    flex: 1;
    min-height: 0;
    overflow: auto;
    margin: 0;
    padding: 6px 10px 6px 34px;
    font-size: 12px;
  }
  .plist li {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 0;
  }
  .plist li.locked .desc {
    font-family: var(--font-mono);
    color: var(--text-2);
  }
  .plist .desc {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .strip {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-wrap: wrap;
    padding: 6px 10px;
    border-top: 1px solid var(--border);
    font-size: 12px;
  }
  .lbl {
    font-weight: 600;
    margin-right: 2px;
  }
  .chip {
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .chip.locked {
    border-style: dashed;
    color: var(--text-2);
  }
  .small {
    font-size: 12px;
  }
  .phone .phead :global(.btn),
  .phone .plist :global(.btn) {
    min-height: 44px;
    min-width: 44px;
  }
  .add.full {
    position: fixed;
    inset: 0;
    z-index: 20;
    align-content: flex-start;
    background: var(--surface);
    padding: 16px;
    overflow: auto;
  }
  .add.full :global(.picker) {
    flex-basis: 100%;
  }
  /* the full-screen picker's list pushes the buttons down instead of covering them */
  .add.full :global(.list) {
    position: static;
    width: 100%;
    max-width: none;
  }
  .add.full :global(.btn),
  .add.full :global(input) {
    min-height: 44px;
  }
  .formtitle {
    flex-basis: 100%;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 12px;
  }
  .vars {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .num {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-family: var(--font-mono);
  }
  .num input {
    width: 64px;
    font-size: 12px;
  }
  .count {
    font-weight: 600;
  }
  .tapbar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
    background: var(--spark-soft);
    font-size: 12px;
  }
  .tapbar :global(.btn) {
    min-height: 44px;
  }
  .notice.empty {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .sheet {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 10;
    max-height: 70vh;
    overflow: auto;
    background: var(--surface);
    border-top: 1px solid var(--border);
    border-radius: 12px 12px 0 0;
    box-shadow: var(--shadow-pop);
    padding: 10px 16px 16px;
  }
  .sheethead {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 8px;
  }
  .sheetbody {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: center;
    font-size: 13px;
  }
  .sheetbody :global(.btn),
  .sheetbody input,
  .sheetbody .check {
    min-height: 44px;
  }
  .phone .strip :global(.btn) {
    min-height: 44px;
  }
  .locked-text {
    margin: 6px 0 0;
    max-height: 200px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-word;
    font-size: 12px;
    background: var(--surface-2);
    padding: 6px 8px;
    border-radius: 4px;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  .switch {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text-2);
    cursor: pointer;
    white-space: nowrap;
    font-size: 12px;
  }
  .planbar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    padding: 4px 10px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
    font-size: 12px;
  }
  .planbar .ptitle {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-weight: 600;
  }
  .notdrawn {
    margin: 0;
    padding: 4px 10px 6px 26px;
    border-bottom: 1px solid var(--border);
    font-size: 12px;
    max-height: 140px;
    overflow: auto;
  }
  .notdrawn .linkish {
    background: none;
    border: 0;
    padding: 2px 0;
    color: inherit;
    text-align: left;
    cursor: pointer;
    display: inline-flex;
    gap: 6px;
    flex-wrap: wrap;
  }
</style>

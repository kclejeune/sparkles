// The lens state of one graph canvas (docs/specs/U10-canvas-lenses.md): the lens chosen on
// each channel, their results for the drawn elements, and the encoding the views draw. A
// page gives it what it draws with `setInput`, and the lens menu and legend read it.
import { untrack } from 'svelte';
import { errorMessage } from './api';
import { isAbort } from './supersede';
import {
  announcement,
  badgeResult,
  CHANNELS,
  colourMemory,
  colourResult,
  encode,
  inferredResult,
  sizeResult,
  stableIndex,
  validationResult,
  type Channel,
  type ColourMemory,
  type LensResult,
  type Results,
  type ValidationBadge,
} from './lens';
import {
  ALGOS,
  cancelRun,
  drawnPredicates,
  isAlgo,
  MAX_DRAWN,
  NoViewRuns,
  projections as readProjections,
  readMemberships,
  runDrawn,
  runParams,
  runProjection,
  summaryText,
  TOP,
  type AlgoId,
  type AlgoRows,
  type ProjectionContext,
  type ProjectionRef,
  type DrawnContext,
} from './algo';
import { status as graphragStatus } from './graphrag';
import {
  BATCH,
  categoryOf,
  iriNodes,
  limiter,
  classQuery,
  DEFAULT_GRAPH,
  drawnDegrees,
  graphQuery,
  inferredPerNode,
  MAX_LENS_NODES,
  pickCommon,
  pickGraph,
  pickNumber,
  propertyQuery,
  readDegrees,
  readInferred,
  readProperties,
  readValues,
  SEVERAL_GRAPHS,
  type LensEdge,
  type LensNode,
  type PropertyInfo,
  type ReadContext,
  type SelectFn,
  type ValueCache,
} from './lens-sources';

export type LensId =
  | 'none'
  | 'degree'
  | 'degree-dataset'
  | 'property'
  | 'class'
  | 'graph'
  | 'inferred'
  | 'validation'
  | 'graphrag'
  | AlgoId;

/** Where an algorithm lens runs (§2.3): over the drawn graph, or over a projection. */
export type Scope = 'drawn' | 'projection';

export type LensChoice = {
  lens: LensId;
  property?: string;
  /** Algorithm lenses: the scope, `drawn` unless given. */
  scope?: Scope;
  /** Algorithm lenses: the parameters a person changed. */
  params?: Record<string, number>;
  /**
   * Projection scope: `name:<name>` for a stored projection, `pred:<iri>` for one predicate,
   * and `drawn` (the default) for every predicate of the drawn edges.
   */
  projection?: string;
  /** BFS: the node id it starts from. */
  source?: string;
  /** Hierarchical communities and GraphRAG: the level, the last one unless given. */
  level?: number;
};

/** The lenses of each channel in the menu, in order. */
export const LENS_OPTIONS: Record<
  Channel,
  { id: LensId; name: string; reasoning?: boolean; validation?: boolean; graphrag?: boolean }[]
> = {
  size: [
    { id: 'none', name: 'none' },
    { id: 'degree', name: 'degree in the drawn graph' },
    { id: 'degree-dataset', name: 'degree in the dataset' },
    { id: 'property', name: 'a numeric property' },
    { id: 'pageRank', name: ALGOS.pageRank.name },
    { id: 'betweenness', name: ALGOS.betweenness.name },
    { id: 'bfs', name: ALGOS.bfs.name },
    { id: 'kCore', name: ALGOS.kCore.name },
    { id: 'triangleCount', name: ALGOS.triangleCount.name },
  ],
  colour: [
    { id: 'none', name: 'none' },
    { id: 'class', name: 'class' },
    { id: 'graph', name: 'graph' },
    { id: 'property', name: "a property's values" },
    { id: 'wcc', name: ALGOS.wcc.name },
    { id: 'labelPropagation', name: ALGOS.labelPropagation.name },
    { id: 'louvain', name: ALGOS.louvain.name },
    { id: 'leiden', name: ALGOS.leiden.name },
    { id: 'graphrag', name: 'GraphRAG community', graphrag: true },
  ],
  badge: [
    { id: 'none', name: 'none' },
    { id: 'inferred', name: 'has inferred edges', reasoning: true },
    { id: 'validation', name: 'validation results', validation: true },
  ],
  edge: [
    { id: 'none', name: 'none' },
    { id: 'inferred', name: 'inferred dashed', reasoning: true },
  ],
};

/** What the canvas draws, for the lens sources. */
export type LensInput = {
  /** The dataset, with its branch as `name@branch`. */
  ds: string;
  /** The state the canvas shows, empty for the head. */
  at?: string;
  /**
   * Whether `at` is a past state, rather than the head pinned to its commit as a query
   * result's state is. A set `at` is past unless this says otherwise.
   */
  past?: boolean;
  /**
   * Changes whenever the page draws its graph from new reads, such as a refocus or a re-read
   * at the head. Lens values read for another epoch are read again, since the head may have
   * moved since.
   */
  epoch?: number | string;
  branch?: string | null;
  nodes: LensNode[];
  edges: LensEdge[];
  /** Whether the dataset has reasoning, materialized or on read: the inferred lenses exist. */
  reasoning: boolean;
  /** Read the edges with reasoning too, for edges that do not come from a read of the data. */
  verify?: boolean;
  /** A node's caption, for announcements. */
  label: (id: string) => string;
  /** A short form of an IRI, for categories. */
  short: (iri: string) => string;
  /**
   * The left handle's state in compare mode (§4.3): numeric lenses read there too and show the
   * change since it.
   */
  compareAt?: string;
  /** The time slider plays: projection-scope runs wait until it stops (§4.3). */
  playing?: boolean;
};

/**
 * The state a query result was read at, for its lenses: the commit an At selector resolved
 * to, or the head's commit the result's metadata names, so that the lenses read the same
 * state even after the head moves.
 */
export function resultState(r: {
  at?: { commit: number | null } | null;
  meta?: { commit?: number | null } | null;
}): string | undefined {
  const c = r.at?.commit ?? r.meta?.commit;
  return c != null ? `commit:${c}` : undefined;
}

export type LensColumn = { channel: Channel; title: string; cells: Map<string, string> };

export type ChannelStatus = {
  busy: boolean;
  ms?: number;
  error?: string;
  note?: string;
  /** A background run: its task, its progress from 0 to 1, and what it does. */
  task?: string;
  progress?: number;
  phase?: string;
};

/** What the algorithm lenses can offer on this canvas. */
export type AlgoOptions = {
  /** The stored projections, read on main only. */
  projections: string[];
  /** Why projection scope is off, such as a server that predates view-only runs. */
  projectionOff: string | null;
  /** GraphRAG communities, offered on main at the head when the caller may read them. */
  graphrag: { levels: number[] } | null;
  /** Why the GraphRAG lens is absent, when the dataset has reports. */
  graphragOff: string | null;
};

const idle = (): Record<Channel, ChannelStatus> => ({
  size: { busy: false },
  colour: { busy: false },
  badge: { busy: false },
  edge: { busy: false },
});

const none = (): Record<Channel, LensChoice> => ({
  size: { lens: 'none' },
  colour: { lens: 'none' },
  badge: { lens: 'none' },
  edge: { lens: 'none' },
});

const choiceKey = (c: LensChoice) =>
  JSON.stringify([c.lens, c.property, c.scope, c.params, c.projection, c.source, c.level]);
const sameChoice = (a: LensChoice, b: LensChoice) => choiceKey(a) === choiceKey(b);

/** Lenses whose numbers come from the dataset, which compare mode reads at both handles. */
const DATASET_NUMERIC = new Set<LensId>(['degree-dataset', 'property']);

const fmtDelta = (n: number) => {
  const t = Number.isInteger(n)
    ? Math.abs(n).toLocaleString('en-US')
    : Math.abs(n).toLocaleString('en-US', { maximumSignificantDigits: 3 });
  return n > 0 ? `+${t}` : n < 0 ? `−${t}` : '±0';
};

export class Lenses {
  choice = $state<Record<Channel, LensChoice>>(none());
  results = $state<Results>({});
  status = $state<Record<Channel, ChannelStatus>>(idle());
  /** The inferred lenses exist: the dataset has reasoning. */
  reasoning = $state(false);
  properties = $state<PropertyInfo[]>([]);
  algo = $state<AlgoOptions>({
    projections: [],
    projectionOff: null,
    graphrag: null,
    graphragOff: null,
  });
  announcement = $state('');
  /** Why the lenses are off, such as a graph over the node limit. */
  blocked = $state<string | null>(null);
  /**
   * The validation lens exists: the page has a validation overlay the caller may use
   * (U07 §11.2). `validationNote` says why it does not, such as a caller whose access
   * covers part of the dataset.
   */
  validation = $state(false);
  validationNote = $state<string | null>(null);
  /**
   * A lens the person chose, with the channel and the lens `toggle` would turn on next:
   * the chosen one, or the outgoing one when the channel is turned off, which may be a
   * default the person never chose.
   */
  onchoose: ((channel: Channel, choice: LensChoice, remember: LensChoice) => void) | undefined;
  encoding = $derived(encode(this.results));
  /** The list view's column per active lens: its title and each node's value as text. */
  columns = $derived(
    CHANNELS.flatMap((ch): LensColumn[] => {
      const r = this.results[ch];
      if (!r) return [];
      if (ch !== 'edge') return [{ channel: ch, title: r.name, cells: r.text }];
      // the edge lens shows each node's inferred edges
      const per = inferredPerNode(this.#input?.edges ?? [], new Set(r.codes.keys()));
      return [
        {
          channel: ch,
          title: 'inferred edges',
          cells: new Map([...per].map(([id, n]) => [id, String(n)])),
        },
      ];
    }),
  );
  active = $derived(CHANNELS.some((c) => this.choice[c].lens !== 'none'));

  /** The query function, for tests. */
  select: SelectFn | undefined;
  /** The run route and task reads of projection runs, for tests. */
  projectionHooks: Pick<ProjectionContext, 'post' | 'getTask' | 'poll'> = {};
  /** How long a drawn-graph algorithm waits for the canvas to be still, in ms (§4.1). */
  stillDelay = 1000;
  /** How long a dataset read waits for the drawn graph to settle, in ms. */
  delay = 250;

  #input: LensInput | null = null;
  /** The last lens of each channel that was on, which Shift+1 to Shift+4 turn on again. */
  #last: Record<Channel, LensChoice> = {
    size: { lens: 'degree' },
    colour: { lens: 'class' },
    badge: { lens: 'inferred' },
    edge: { lens: 'inferred' },
  };
  /** Whether the person chose the edge lens, so the default no longer applies. */
  #edgeChosen = false;
  /** The channels whose lens the person chose, so that `suggest` leaves them alone. */
  #chosen = new Set<Channel>();
  /** The channels whose lens `suggest` set, which a change of dataset undoes. */
  #suggested = new Set<Channel>();
  /** The validation overlay's badges, which the validation lens shows. */
  #vBadges: ReadonlyMap<string, ValidationBadge> = new Map();
  #caches = new Map<string, ValueCache | Map<string, boolean>>();
  #seq: Record<Channel, number> = { size: 0, colour: 0, badge: 0, edge: 0 };
  #ctl: Partial<Record<Channel, AbortController>> = {};
  #timer: ReturnType<typeof setTimeout> | undefined;
  #propsKey = '';
  /** The slots all of this canvas's lens reads share (§5: at most four in flight). */
  #limiter = limiter();
  /** The inferred-edge read of an input, which the badge and edge lenses share. */
  #inferredRead: {
    input: LensInput;
    promise: Promise<{ inferred: Set<string>; unchecked: Set<string> }>;
    ctl: AbortController;
  } | null = null;

  /** Stop the shared inferred-edge read, such as when it is superseded or nothing needs it. */
  #dropInferred() {
    this.#inferredRead?.ctl.abort();
    this.#inferredRead = null;
  }
  #dsKey = '';
  /** The communities of the last colour run, for stable colours (§2.3). */
  #colours: ColourMemory | null = null;
  #algoKey = '';

  /** What the canvas draws now. Lenses whose data it changes run again. */
  setInput(input: LensInput) {
    untrack(() => {
      const prev = this.#input;
      this.#input = input;
      const dsKey = `${input.ds}|${input.branch ?? ''}`;
      if (dsKey !== this.#dsKey) {
        this.#dsKey = dsKey;
        this.#caches.clear();
        this.#dropInferred();
        this.#edgeChosen = false;
        this.properties = [];
        this.#propsKey = '';
        this.#colours = null;
        this.#algoKey = '';
        this.algo = { projections: [], projectionOff: null, graphrag: null, graphragOff: null };
        // a default of the last dataset is not this one's
        for (const ch of this.#suggested) this.#clear(ch);
        this.#suggested.clear();
      }
      if (input.reasoning !== this.reasoning) this.reasoning = input.reasoning;
      // the inferred-edge lens is on by default where the dataset has reasoning (§2.2), and
      // absent where it has none (§2.7)
      if (input.reasoning && !this.#edgeChosen && this.choice.edge.lens === 'none')
        this.choice.edge = { lens: 'inferred' };
      if (!input.reasoning)
        for (const ch of ['badge', 'edge'] as const)
          if (this.choice[ch].lens === 'inferred') {
            this.choice[ch] = { lens: 'none' };
            this.#ctl[ch]?.abort();
            this.#seq[ch]++;
            this.results[ch] = null;
            this.status[ch] = { busy: false };
          }
      if (this.choice.badge.lens !== 'inferred' && this.choice.edge.lens !== 'inferred')
        this.#dropInferred();
      this.blocked =
        input.nodes.length > MAX_LENS_NODES
          ? `Lenses apply to at most ${MAX_LENS_NODES.toLocaleString('en-US')} drawn nodes, and this graph has ${input.nodes.length.toLocaleString('en-US')}. A numeric lens on a projection shows the top ${TOP.toLocaleString('en-US')}.`
          : null;
      const stateChanged =
        !prev ||
        prev.at !== input.at ||
        prev.ds !== input.ds ||
        prev.epoch !== input.epoch ||
        prev.compareAt !== input.compareAt ||
        !!prev.playing !== !!input.playing;
      if (prev && prev.epoch !== input.epoch) {
        // values read before the redraw may be of an older head
        this.#caches.clear();
        this.#dropInferred();
      }
      // what the algorithm lenses offer depends on the state shown: read it again once read
      if (this.#algoKey && stateChanged) void this.loadAlgoOptions();
      this.#schedule(stateChanged);
    });
  }

  /** Choose the lens of a channel. `byPerson` is false for a default the page sets. */
  set(channel: Channel, choice: LensChoice, byPerson = true) {
    if (byPerson) {
      if (channel === 'edge') this.#edgeChosen = true;
      this.#chosen.add(channel);
      this.#suggested.delete(channel);
      this.onchoose?.(channel, choice, choice.lens !== 'none' ? choice : this.#last[channel]);
    }
    if (sameChoice(this.choice[channel], choice)) return;
    this.choice[channel] = choice;
    if (choice.lens !== 'none') this.#last[channel] = choice;
    this.results[channel] = null;
    this.status[channel] = { busy: false };
    this.#ctl[channel]?.abort();
    if (this.choice.badge.lens !== 'inferred' && this.choice.edge.lens !== 'inferred')
      this.#dropInferred();
    this.#runSoon(channel, 0);
  }

  /**
   * Turn a lens on by default, such as the validation lens on a dataset with a write-time
   * guard (U07 §11.2), unless the person chose the channel's lens or it has one.
   */
  suggest(channel: Channel, choice: LensChoice) {
    if (this.#chosen.has(channel) || this.choice[channel].lens !== 'none') return;
    this.#suggested.add(channel);
    this.set(channel, choice, false);
  }

  /**
   * Turn on a lens the person chose before, such as the validation lens remembered in the
   * browser once the dataset offers it again, unless the channel has a lens now.
   */
  restore(channel: Channel, choice: LensChoice) {
    if (this.choice[channel].lens !== 'none') return;
    this.#suggested.delete(channel);
    this.set(channel, choice, false);
  }

  /** The lens `toggle` turns on again, such as one remembered from an earlier visit. */
  setLast(channel: Channel, choice: LensChoice) {
    if (choice.lens !== 'none') this.#last[channel] = choice;
  }

  /** Whether the validation lens exists, and why not. Turning it off ends the lens. */
  setValidation(available: boolean, note: string | null = null) {
    untrack(() => {
      this.validation = available;
      this.validationNote = available ? null : note;
      if (!available && this.choice.badge.lens === 'validation') this.#clear('badge');
    });
  }

  /** The validation overlay's badges for the drawn nodes. */
  setValidationBadges(badges: ReadonlyMap<string, ValidationBadge>) {
    untrack(() => {
      this.#vBadges = badges;
      if (this.choice.badge.lens === 'validation') void this.#run('badge');
    });
  }

  #clear(channel: Channel) {
    this.choice[channel] = { lens: 'none' };
    this.#ctl[channel]?.abort();
    this.#seq[channel]++;
    this.results[channel] = null;
    this.status[channel] = { busy: false };
  }

  /** Turn a channel's lens off, or on again with the last lens it had (§2.5). */
  toggle(channel: Channel) {
    const cur = this.choice[channel];
    if (cur.lens !== 'none') {
      this.set(channel, { lens: 'none' });
      return;
    }
    let last = this.#last[channel];
    // without reasoning the badge channel's other lens is validation, where it exists
    if (last.lens === 'inferred' && !this.reasoning) {
      if (channel !== 'badge' || !this.validation) return;
      last = { lens: 'validation' };
    }
    if (last.lens === 'validation' && !this.validation) return;
    if (last.lens === 'property' && !last.property) return;
    this.set(channel, last);
  }

  /** Read the properties of the drawn nodes, for the property lens's picker. */
  async loadProperties() {
    const input = this.#input;
    if (!input) return;
    const sample = iriNodes(input.nodes)
      .slice(0, BATCH)
      .map((n) => n.iri)
      .join(' ');
    const key = `${input.ds}|${input.at ?? ''}|${input.epoch ?? ''}|${sample}`;
    if (key === this.#propsKey) return;
    this.#propsKey = key;
    try {
      const props = await readProperties(this.#ctx(input), input.nodes);
      // a read for another drawing that finished later than this one's is dropped
      if (this.#propsKey === key) this.properties = props;
    } catch (e) {
      if (!isAbort(e) && this.#propsKey === key) this.properties = [];
    }
  }

  /**
   * Read what the algorithm lenses can offer: the stored projections and the GraphRAG
   * communities, on main, and the GraphRAG lens at the head only (§6).
   */
  async loadAlgoOptions() {
    const input = this.#input;
    if (!input) return;
    const main = !input.branch || input.branch === 'main';
    const past = input.past ?? !!input.at;
    const key = `${input.ds}|${input.branch ?? ''}|${past}`;
    if (key === this.#algoKey) return;
    this.#algoKey = key;
    const [projections, gr] = await Promise.all([
      main ? readProjections(input.ds) : Promise.resolve([]),
      main ? graphragStatus(input.ds).catch(() => null) : Promise.resolve(null),
    ]);
    if (this.#algoKey !== key) return;
    const levels = (gr?.levels ?? []).map((l) => l.level).sort((a, b) => a - b);
    const has = !!gr?.enabled && levels.length > 0;
    this.algo = {
      ...this.algo,
      projections,
      graphrag: has && gr!.available && !past ? { levels } : null,
      graphragOff: !main
        ? 'GraphRAG communities are built on main, and this graph shows a branch.'
        : has && !gr!.available
          ? 'GraphRAG communities are not shown, since your access does not cover everything their build read.'
          : has && past
            ? 'GraphRAG communities describe the head, and this graph shows a past state.'
            : null,
    };
  }

  /** The predicates of the drawn edges, for an inline projection. */
  drawnPredicates(): string[] {
    return drawnPredicates(this.#input?.edges ?? []);
  }

  /** The drawn IRI nodes, for the BFS start node. */
  sourceNodes(): { id: string; label: string }[] {
    const input = this.#input;
    if (!input) return [];
    return iriNodes(input.nodes)
      .slice(0, 500)
      .map((n) => ({ id: n.id, label: input.label(n.id) }));
  }

  /**
   * Stop a channel's background run (§2.3). The task is cancelled when this caller started
   * it; otherwise the lens only stops following it.
   */
  async cancel(channel: Channel) {
    const task = this.status[channel].task;
    this.#ctl[channel]?.abort();
    const seq = ++this.#seq[channel];
    const ok = task ? await cancelRun(task) : true;
    // a newer run of the channel started while the cancel was on its way: its status stays
    if (this.#seq[channel] !== seq) return;
    this.status[channel] = {
      busy: false,
      note: ok
        ? 'Cancelled.'
        : 'Stopped following the run. Another caller started it, so it goes on for them.',
    };
  }

  destroy() {
    clearTimeout(this.#timer);
    clearTimeout(this.#stillTimer);
    this.#dropInferred();
    for (const c of Object.values(this.#ctl)) c?.abort();
  }

  #stillTimer: ReturnType<typeof setTimeout> | undefined;

  #schedule(stateChanged: boolean) {
    clearTimeout(this.#timer);
    clearTimeout(this.#stillTimer);
    const drawnAlgo = (c: LensChoice) => isAlgo(c.lens) && (c.scope ?? 'drawn') === 'drawn';
    // the drawn degree is counted at once, the reads wait for the graph to settle
    for (const ch of CHANNELS) if (this.choice[ch].lens === 'degree') void this.#run(ch);
    this.#timer = setTimeout(
      () => {
        for (const ch of CHANNELS) {
          const c = this.choice[ch];
          if (c.lens !== 'none' && c.lens !== 'degree' && !drawnAlgo(c)) void this.#run(ch);
        }
      },
      stateChanged ? 0 : this.delay,
    );
    // a drawn-graph run depends on the whole drawing, so it waits until the canvas is still
    this.#stillTimer = setTimeout(
      () => {
        for (const ch of CHANNELS) if (drawnAlgo(this.choice[ch])) void this.#run(ch);
      },
      stateChanged ? 0 : this.stillDelay,
    );
  }

  #runSoon(channel: Channel, ms: number) {
    setTimeout(() => void this.#run(channel), ms);
  }

  #ctx(input: LensInput, signal?: AbortSignal): ReadContext {
    return {
      ds: input.ds,
      at: input.at || undefined,
      signal,
      select: this.select,
      limiter: this.#limiter,
    };
  }

  #cache<T extends ValueCache | Map<string, boolean>>(key: string, make: () => T): T {
    let c = this.#caches.get(key) as T | undefined;
    if (!c) this.#caches.set(key, (c = make()));
    return c;
  }

  async #run(channel: Channel) {
    const input = this.#input;
    const choice = this.choice[channel];
    if (!input || choice.lens === 'none') return;
    const projection = isAlgo(choice.lens) && choice.scope === 'projection';
    // above the node limit, only a numeric lens on a projection runs, for the top nodes (§5)
    if (this.blocked && !(projection && channel === 'size')) {
      this.results[channel] = null;
      this.status[channel] = { busy: false, note: this.blocked };
      return;
    }
    if (projection && input.playing) {
      // a background run per commit of Play would be wasted (§4.3)
      this.#ctl[channel]?.abort();
      this.#seq[channel]++;
      this.status[channel] = { busy: false, note: 'paused while playing' };
      return;
    }
    const seq = ++this.#seq[channel];
    this.#ctl[channel]?.abort();
    const ctl = new AbortController();
    this.#ctl[channel] = ctl;
    const owns = () => this.#seq[channel] === seq && sameChoice(this.choice[channel], choice);
    const t0 = performance.now();
    const quick = choice.lens === 'degree';
    if (!quick) this.status[channel] = { ...this.status[channel], busy: true, error: undefined };
    try {
      const r = await this.#compute(channel, choice, input, ctl.signal, owns);
      if (!owns() || this.#input !== input) return;
      this.results[channel] = r;
      this.status[channel] = { busy: false, ms: performance.now() - t0 };
      if (r && channel === 'colour' && (isAlgo(choice.lens) || choice.lens === 'graphrag'))
        this.#colours = colourMemory(r);
      if (r) this.announcement = announcement(r, this.#name(channel, choice), input.label);
    } catch (e) {
      if (!owns() || isAbort(e)) return;
      if (e instanceof NoViewRuns) this.algo = { ...this.algo, projectionOff: e.message };
      // the values of an earlier state must not pass for this one's
      this.results[channel] = null;
      this.status[channel] = { busy: false, error: errorMessage(e) };
    }
  }

  #name(channel: Channel, choice: LensChoice): string {
    if (choice.lens === 'property' && choice.property)
      return this.#input?.short(choice.property) ?? choice.property;
    if (channel === 'badge')
      return choice.lens === 'validation' ? 'validation results' : 'inferred edges';
    return LENS_OPTIONS[channel].find((o) => o.id === choice.lens)?.name ?? choice.lens;
  }

  /** The projection a choice names. */
  #projection(choice: LensChoice, input: LensInput): ProjectionRef {
    const p = choice.projection ?? 'drawn';
    if (p.startsWith('name:')) return { name: p.slice(5) };
    if (p.startsWith('pred:')) return { predicates: [p.slice(5)] };
    const preds = drawnPredicates(input.edges);
    if (!preds.length) throw new Error('The drawn edges have no predicate to project on.');
    return { predicates: preds };
  }

  /** An algorithm's values per drawn node, over the drawn graph or a projection. */
  async #algoRows(
    channel: Channel,
    choice: LensChoice,
    input: LensInput,
    signal: AbortSignal,
    owns: () => boolean,
    at: string | undefined,
  ): Promise<AlgoRows> {
    const spec = ALGOS[choice.lens as AlgoId];
    const nodes = iriNodes(input.nodes);
    const iris = [...new Set(nodes.map((n) => n.iri))];
    let source: string | undefined;
    if (spec.needsSource) {
      const id = choice.source ?? this.sourceNodes()[0]?.id;
      source = nodes.find((n) => n.id === id)?.iri;
      if (!source) throw new Error('Choose the node the search starts from.');
    }
    const params = runParams(spec, choice.params, source);
    if ((choice.scope ?? 'drawn') === 'drawn') {
      if (iris.length > MAX_DRAWN)
        throw new Error(
          `Drawn-graph runs take at most ${MAX_DRAWN.toLocaleString('en-US')} nodes, and ${iris.length.toLocaleString('en-US')} are drawn. Use projection scope.`,
        );
      const ctx: DrawnContext = {
        ds: input.ds,
        at,
        signal,
        select: this.select,
        limiter: this.#limiter,
      };
      return runDrawn(ctx, spec, iris, params, choice.level);
    }
    if (this.algo.projectionOff) throw new Error(this.algo.projectionOff);
    const top = iris.length > MAX_LENS_NODES ? TOP : undefined;
    return runProjection(
      {
        ds: input.ds,
        at,
        signal,
        ...this.projectionHooks,
        onTask: (t) => {
          if (!owns()) return;
          this.status[channel] = {
            ...this.status[channel],
            busy: true,
            task: t.id,
            progress: t.progress,
            phase: t.state === 'queued' ? 'queued' : (t.message ?? 'running'),
          };
        },
      },
      spec,
      this.#projection(choice, input),
      iris,
      params,
      { level: choice.level, top },
    );
  }

  /** Values per node id from values per IRI. */
  #byId<T>(input: LensInput, byIri: Map<string, T>): Map<string, T | null> {
    const out = new Map<string, T | null>();
    for (const n of input.nodes) out.set(n.id, null);
    for (const n of iriNodes(input.nodes)) out.set(n.id, byIri.get(n.iri) ?? null);
    return out;
  }

  /** In compare mode, the size lens's text gains the change since the left handle (§4.3). */
  #withDelta(r: LensResult, before: Map<string, number | null>, label: string): LensResult {
    for (const [id, v] of r.values) {
      if (typeof v !== 'number') continue;
      const b = before.get(id);
      const t = r.text.get(id) ?? String(v);
      r.text.set(
        id,
        b == null ? `${t}, new since ${label}` : `${t}, ${fmtDelta(v - b)} since ${label}`,
      );
    }
    r.summary = `${r.summary ? `${r.summary}, ` : ''}changes since ${label} in the tooltip and list`;
    return r;
  }

  async #compute(
    channel: Channel,
    choice: LensChoice,
    input: LensInput,
    signal: AbortSignal,
    owns: () => boolean = () => true,
  ): Promise<LensResult | null> {
    const r = await this.#computeAt(channel, choice, input, signal, owns, input.at || undefined);
    const dataset =
      DATASET_NUMERIC.has(choice.lens) || (isAlgo(choice.lens) && choice.scope === 'projection');
    if (!r || channel !== 'size' || !input.compareAt || !dataset) return r;
    const left = await this.#computeAt(channel, choice, input, signal, owns, input.compareAt);
    if (!left) return r;
    const before = new Map<string, number | null>();
    for (const [id, v] of left.values) before.set(id, typeof v === 'number' ? v : null);
    return this.#withDelta(r, before, 'the left handle');
  }

  async #computeAt(
    channel: Channel,
    choice: LensChoice,
    input: LensInput,
    signal: AbortSignal,
    owns: () => boolean,
    atState: string | undefined,
  ): Promise<LensResult | null> {
    if (atState !== (input.at || undefined)) input = { ...input, at: atState };
    const ctx = this.#ctx(input, signal);
    const at = input.at ?? '';
    const computedAt = { at: at || 'head', branch: input.branch ?? 'main' };
    const name = this.#name(channel, choice);
    const key = (k: string) => `${k}|${at}`;
    const nodes = input.nodes;
    switch (choice.lens) {
      case 'validation':
        // the overlay validates, and the lens shows what it found
        return validationResult({ lens: 'validation', name, values: this.#vBadges, computedAt });
      case 'degree':
        return sizeResult({
          lens: 'degree',
          name,
          values: drawnDegrees(nodes, input.edges),
          computedAt,
        });
      case 'degree-dataset': {
        const cache = this.#cache<ValueCache>(key('degree'), () => new Map());
        const values = await readDegrees(ctx, nodes, cache);
        return sizeResult({ lens: 'degree-dataset', name, values, computedAt });
      }
      case 'property': {
        const p = choice.property;
        if (!p) return null;
        const cache = this.#cache<ValueCache>(key(`p:${p}`), () => new Map());
        await readValues(ctx, nodes, (b) => propertyQuery(b, p), cache);
        if (channel === 'size')
          return sizeResult({
            lens: 'property',
            name,
            values: pickNumber(nodes, cache),
            computedAt,
          });
        const lists = new Map(
          nodes.map((n) => [
            n.id,
            (cache.get(n.id) ?? []).map(categoryOf).filter((v): v is string => !!v),
          ]),
        );
        return colourResult({
          lens: 'property',
          name,
          values: pickCommon(nodes, lists),
          label: (v) => (/^[a-z][a-z0-9+.-]*:/i.test(v) ? input.short(v) : v),
          computedAt,
        });
      }
      case 'class': {
        const cache = this.#cache<ValueCache>(key('class'), () => new Map());
        // the asserted classes: with reasoning, every node would also be in each superclass
        await readValues(ctx, nodes, classQuery, cache, { reasoning: false });
        const lists = new Map(
          nodes.map((n) => [
            n.id,
            (cache.get(n.id) ?? []).map(categoryOf).filter((v): v is string => !!v),
          ]),
        );
        return colourResult({
          lens: 'class',
          name,
          values: pickCommon(nodes, lists),
          label: input.short,
          computedAt,
        });
      }
      case 'graph': {
        const cache = this.#cache<ValueCache>(key('graph'), () => new Map());
        await readValues(ctx, nodes, graphQuery, cache);
        return colourResult({
          lens: 'graph',
          name,
          values: pickGraph(nodes, cache),
          label: (v) =>
            v === SEVERAL_GRAPHS
              ? 'several graphs'
              : v === DEFAULT_GRAPH
                ? 'the default graph'
                : input.short(v),
          computedAt,
        });
      }
      case 'graphrag': {
        // checked against the graph as it is now, not when the options were last read (§6)
        if (input.branch && input.branch !== 'main')
          throw new Error('GraphRAG communities are built on main, and this graph shows a branch.');
        if (input.past ?? !!input.at)
          throw new Error(
            'GraphRAG communities describe the head, and this graph shows a past state.',
          );
        const gr = this.algo.graphrag;
        if (!gr)
          throw new Error(this.algo.graphragOff ?? 'The dataset has no GraphRAG communities.');
        const level =
          choice.level != null && gr.levels.includes(choice.level) ? choice.level : gr.levels[0];
        const iris = [...new Set(iriNodes(nodes).map((n) => n.iri))];
        const m = await readMemberships(input.ds, iris, level, signal);
        if (!m.available) throw new Error(m.reason ?? 'GraphRAG communities are not available.');
        const byIri = new Map<string, string>();
        for (const x of m.memberships) if (!byIri.has(x.node)) byIri.set(x.node, x.community);
        const info = new Map(m.communities.map((c) => [c.iri, c]));
        const values = this.#byId(input, byIri);
        const label = (v: string) => info.get(v)?.title || input.short(v);
        const r = colourResult({
          lens: 'graphrag',
          name: `GraphRAG community at level ${level}`,
          values,
          label,
          index: stableIndex(values, this.#colours, label),
          entry: (v, drawn) => {
            const all = info.get(v)?.memberCount;
            return `${label(v)}, ${drawn.toLocaleString('en-US')} drawn${all != null ? ` of ${all.toLocaleString('en-US')}` : ''}`;
          },
          noun: ['community', 'communities'],
          computedAt,
        });
        for (const e of r.legend) if (e.key.startsWith('cat-')) e.community = e.key.slice(4);
        return r;
      }
      case 'inferred': {
        const cache = this.#cache<Map<string, boolean>>(
          key(`inferred:${input.verify ? 'v' : ''}`),
          () => new Map(),
        );
        // one read serves both the badge and the edge lens
        let read = this.#inferredRead;
        if (!read || read.input !== input || read.ctl.signal.aborted) {
          // its own lifetime: either lens may stop waiting while the other still needs it
          this.#dropInferred();
          const ctl = new AbortController();
          const p = readInferred({ ...ctx, signal: ctl.signal }, input.edges, cache, input.verify);
          const r = { input, promise: p, ctl };
          read = this.#inferredRead = r;
          p.catch(() => {
            if (this.#inferredRead === r) this.#inferredRead = null;
          });
        }
        const { inferred, unchecked } = await read.promise;
        signal.throwIfAborted();
        if (channel === 'edge')
          return inferredResult({
            lens: 'inferred',
            name: 'inferred',
            edges: input.edges,
            inferred,
            unchecked,
            computedAt,
          });
        return badgeResult({
          lens: 'inferred',
          name: 'inferred edges',
          values: inferredPerNode(input.edges, inferred),
          computedAt,
        });
      }
    }
    if (isAlgo(choice.lens)) {
      const spec = ALGOS[choice.lens];
      const rows = await this.#algoRows(
        channel,
        choice,
        input,
        signal,
        owns,
        input.at || undefined,
      );
      const scope =
        (choice.scope ?? 'drawn') === 'drawn' ? 'in the drawn graph' : 'in the projection';
      const level =
        spec.hierarchical && rows.levels.length > 1
          ? ` at level ${choice.level != null && rows.levels.includes(choice.level) ? choice.level : rows.levels.at(-1)}`
          : '';
      const lensName = `${spec.name}${level} ${scope}`;
      // over the drawn graph the legend already counts the communities
      const said = summaryText(rows.summary, choice.scope === 'projection');
      let r: LensResult;
      if (spec.channel === 'size') {
        const values = this.#byId(input, rows.values as Map<string, number>);
        r = sizeResult({ lens: spec.id, name: lensName, values, computedAt });
        const total = input.nodes.length;
        if (total > MAX_LENS_NODES)
          r.summary = `the top ${rows.values.size.toLocaleString('en-US')} of ${total.toLocaleString('en-US')} drawn nodes by value, the rest at the smallest size`;
      } else {
        const values = this.#byId(input, rows.values as Map<string, string>);
        const label = (v: string) => `${spec.output} ${v}`;
        r = colourResult({
          lens: spec.id,
          name: lensName,
          values,
          label,
          index: stableIndex(values, this.#colours, label),
          entry: (v, drawn) => {
            const all = rows.sizes.get(v);
            return all != null && all !== drawn
              ? `${label(v)}, ${drawn.toLocaleString('en-US')} drawn of ${all.toLocaleString('en-US')}`
              : `${label(v)}, ${drawn.toLocaleString('en-US')} ${drawn === 1 ? 'node' : 'nodes'}`;
          },
          noun:
            spec.output === 'component'
              ? ['component', 'components']
              : ['community', 'communities'],
          computedAt,
        });
      }
      if (said) r.summary = r.summary ? `${r.summary}; ${said}` : said;
      if (rows.cached) r.summary = `${r.summary ? `${r.summary}; ` : ''}from the server's kept run`;
      r.levels = rows.levels;
      return r;
    }
    return null;
  }
}

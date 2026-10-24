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
  colourResult,
  encode,
  inferredResult,
  sizeResult,
  validationResult,
  type Channel,
  type LensResult,
  type Results,
  type ValidationBadge,
} from './lens';
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
  | 'validation';
export type LensChoice = { lens: LensId; property?: string };

/** The lenses of each channel in the menu, in order. */
export const LENS_OPTIONS: Record<
  Channel,
  { id: LensId; name: string; reasoning?: boolean; validation?: boolean }[]
> = {
  size: [
    { id: 'none', name: 'none' },
    { id: 'degree', name: 'degree in the drawn graph' },
    { id: 'degree-dataset', name: 'degree in the dataset' },
    { id: 'property', name: 'a numeric property' },
  ],
  colour: [
    { id: 'none', name: 'none' },
    { id: 'class', name: 'class' },
    { id: 'graph', name: 'graph' },
    { id: 'property', name: "a property's values" },
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

export type ChannelStatus = { busy: boolean; ms?: number; error?: string; note?: string };

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

const sameChoice = (a: LensChoice, b: LensChoice) => a.lens === b.lens && a.property === b.property;

export class Lenses {
  choice = $state<Record<Channel, LensChoice>>(none());
  results = $state<Results>({});
  status = $state<Record<Channel, ChannelStatus>>(idle());
  /** The inferred lenses exist: the dataset has reasoning. */
  reasoning = $state(false);
  properties = $state<PropertyInfo[]>([]);
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
  /** A lens the person chose, with the channel, such as for remembering it. */
  onchoose: ((channel: Channel, choice: LensChoice) => void) | undefined;
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
          ? `Lenses apply to at most ${MAX_LENS_NODES.toLocaleString('en-US')} drawn nodes, and this graph has ${input.nodes.length.toLocaleString('en-US')}.`
          : null;
      const stateChanged =
        !prev || prev.at !== input.at || prev.ds !== input.ds || prev.epoch !== input.epoch;
      if (prev && prev.epoch !== input.epoch) {
        // values read before the redraw may be of an older head
        this.#caches.clear();
        this.#dropInferred();
      }
      this.#schedule(stateChanged);
    });
  }

  /** Choose the lens of a channel. `byPerson` is false for a default the page sets. */
  set(channel: Channel, choice: LensChoice, byPerson = true) {
    if (byPerson) {
      if (channel === 'edge') this.#edgeChosen = true;
      this.#chosen.add(channel);
      this.#suggested.delete(channel);
      this.onchoose?.(channel, choice);
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

  destroy() {
    clearTimeout(this.#timer);
    this.#dropInferred();
    for (const c of Object.values(this.#ctl)) c?.abort();
  }

  #schedule(stateChanged: boolean) {
    clearTimeout(this.#timer);
    // the drawn degree is counted at once, the reads wait for the graph to settle
    for (const ch of CHANNELS) if (this.choice[ch].lens === 'degree') void this.#run(ch);
    this.#timer = setTimeout(
      () => {
        for (const ch of CHANNELS) {
          const l = this.choice[ch].lens;
          if (l !== 'none' && l !== 'degree') void this.#run(ch);
        }
      },
      stateChanged ? 0 : this.delay,
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
    if (this.blocked) {
      this.results[channel] = null;
      this.status[channel] = { busy: false, note: this.blocked };
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
      const r = await this.#compute(channel, choice, input, ctl.signal);
      if (!owns() || this.#input !== input) return;
      this.results[channel] = r;
      this.status[channel] = { busy: false, ms: performance.now() - t0 };
      if (r) this.announcement = announcement(r, this.#name(channel, choice), input.label);
    } catch (e) {
      if (!owns() || isAbort(e)) return;
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

  async #compute(
    channel: Channel,
    choice: LensChoice,
    input: LensInput,
    signal: AbortSignal,
  ): Promise<LensResult | null> {
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
    return null;
  }
}

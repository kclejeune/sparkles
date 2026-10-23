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
  type Channel,
  type LensResult,
  type Results,
} from './lens';
import {
  categoryOf,
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
  | 'inferred';
export type LensChoice = { lens: LensId; property?: string };

/** The lenses of each channel in the menu, in order. */
export const LENS_OPTIONS: Record<Channel, { id: LensId; name: string; reasoning?: boolean }[]> = {
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
  #caches = new Map<string, ValueCache | Map<string, boolean>>();
  #seq: Record<Channel, number> = { size: 0, colour: 0, badge: 0, edge: 0 };
  #ctl: Partial<Record<Channel, AbortController>> = {};
  #timer: ReturnType<typeof setTimeout> | undefined;
  #propsKey = '';
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
        this.#edgeChosen = false;
        this.properties = [];
        this.#propsKey = '';
      }
      if (input.reasoning !== this.reasoning) this.reasoning = input.reasoning;
      // the inferred-edge lens is on by default where the dataset has reasoning (§2.2), and
      // absent where it has none (§2.7)
      if (input.reasoning && !this.#edgeChosen && this.choice.edge.lens === 'none')
        this.choice.edge = { lens: 'inferred' };
      if (!input.reasoning)
        for (const ch of ['badge', 'edge'] as const)
          if (this.choice[ch].lens === 'inferred') this.choice[ch] = { lens: 'none' };
      this.blocked =
        input.nodes.length > MAX_LENS_NODES
          ? `Lenses apply to at most ${MAX_LENS_NODES.toLocaleString('en-US')} drawn nodes, and this graph has ${input.nodes.length.toLocaleString('en-US')}.`
          : null;
      const stateChanged = !prev || prev.at !== input.at || prev.ds !== input.ds;
      this.#schedule(stateChanged);
    });
  }

  /** Choose the lens of a channel. */
  set(channel: Channel, choice: LensChoice) {
    if (channel === 'edge') this.#edgeChosen = true;
    if (sameChoice(this.choice[channel], choice)) return;
    this.choice[channel] = choice;
    if (choice.lens !== 'none') this.#last[channel] = choice;
    this.results[channel] = null;
    this.status[channel] = { busy: false };
    this.#ctl[channel]?.abort();
    this.#runSoon(channel, 0);
  }

  /** Turn a channel's lens off, or on again with the last lens it had (§2.5). */
  toggle(channel: Channel) {
    const cur = this.choice[channel];
    if (cur.lens !== 'none') {
      this.set(channel, { lens: 'none' });
      return;
    }
    const last = this.#last[channel];
    if (last.lens === 'inferred' && !this.reasoning) return;
    if (last.lens === 'property' && !last.property) return;
    this.set(channel, last);
  }

  /** Read the properties of the drawn nodes, for the property lens's picker. */
  async loadProperties() {
    const input = this.#input;
    if (!input) return;
    const key = `${input.ds}|${input.at ?? ''}|${input.nodes.length}`;
    if (key === this.#propsKey) return;
    this.#propsKey = key;
    try {
      this.properties = await readProperties(this.#ctx(input), input.nodes);
    } catch (e) {
      if (!isAbort(e)) this.properties = [];
    }
  }

  destroy() {
    clearTimeout(this.#timer);
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
    return { ds: input.ds, at: input.at || undefined, signal, select: this.select };
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
      this.status[channel] = { busy: false, error: errorMessage(e) };
    }
  }

  #name(channel: Channel, choice: LensChoice): string {
    if (choice.lens === 'property' && choice.property)
      return this.#input?.short(choice.property) ?? choice.property;
    if (channel === 'badge') return 'inferred edges';
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
        const { inferred, unchecked } = await readInferred(ctx, input.edges, cache, input.verify);
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

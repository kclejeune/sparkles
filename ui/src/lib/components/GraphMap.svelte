<script lang="ts">
  // Explore's Map: the drawn graph on a map (U11 section 2.2). Each drawn entity with a
  // geometry becomes a marker at its point or centroid, each drawn edge between two located
  // entities an arc. Positions come from ordinary queries under the caller's view, at the
  // state the graph slider reads, so an entity the caller cannot see a geometry for is
  // listed as not located and its edges are not drawn. Only the edges the graph already
  // shows are drawn: the map never asks for relations of its own.
  import * as api from '$lib/api';
  import { MAX_POINTS, type Link, type Place } from '$lib/arcs';
  import { fmtInt } from '$lib/format';
  import { locateEntities } from '$lib/locate';
  import ArcMap from './ArcMap.svelte';
  import type { GEdge, GNode } from './GraphView.svelte';

  let {
    ds,
    nodes,
    edges,
    at = '',
    fromAt = '',
    fitKey = '',
    onopen,
  }: {
    ds: string;
    nodes: GNode[];
    edges: GEdge[];
    /** The state the graph is read at (the slider), empty for the head. */
    at?: string;
    /** Compare mode: the earlier state, where the entities the later state removed are read. */
    fromAt?: string;
    fitKey?: string;
    onopen?: (iri: string) => void;
  } = $props();

  /** Where each entity is, by the state it was read at and its IRI; null for none. */
  let cache = new Map<string, [number, number] | null>();
  let cacheOwner = '';
  let version = $state(0);
  let pending = $state(0);
  let failure = $state('');

  const stateOf = (n: GNode) => (n.change === 'removed' && fromAt ? fromAt : at) || '';
  const keyOf = (n: GNode) => `${stateOf(n)}\n${n.title}`;

  const entities = $derived(
    nodes
      .filter((n) => n.kind === 'iri' && n.title)
      .slice(0, MAX_POINTS)
      .map((n) => ({ n, key: keyOf(n) })),
  );

  let runs = 0;
  $effect(() => {
    const owner = `${ds}\n${at}\n${fromAt}`;
    const wanted = entities;
    const mine = ++runs;
    if (owner !== cacheOwner) {
      cache = new Map();
      cacheOwner = owner;
      failure = '';
      version++;
    }
    const fresh = wanted.filter((e) => !cache.has(e.key));
    if (!fresh.length) {
      pending = 0;
      return;
    }
    const ctl = new AbortController();
    // one lookup per state: entities the later state removed are read at the earlier one
    const groups = new Map<string, string[]>();
    for (const e of fresh) {
      const s = stateOf(e.n);
      groups.set(s, [...(groups.get(s) ?? []), e.n.title!]);
    }
    pending = fresh.length;
    void (async () => {
      for (const [state, iris] of groups) {
        const res = await locateEntities(iris, {
          run: async (sparql, limit) =>
            api.select(ds, sparql, { at: state || undefined, send: limit, signal: ctl.signal }),
          convert: async (batch) => (await api.geoConvert(batch, ctl.signal)).results,
          signal: ctl.signal,
        });
        if (ctl.signal.aborted || mine !== runs) return;
        for (const iri of iris) cache.set(`${state}\n${iri}`, res.found.get(iri) ?? null);
        if (res.failed) failure = res.firstError ?? 'a lookup failed';
        version++;
      }
      if (mine === runs) pending = 0;
    })();
    return () => ctl.abort();
  });

  const places = $derived.by((): Place[] => {
    void version;
    const out: Place[] = [];
    for (const { n, key } of entities) {
      const p = cache.get(key);
      if (p) out.push({ id: n.id, label: n.label, lon: p[0], lat: p[1] });
    }
    return out;
  });

  const links = $derived.by((): Link[] => {
    const have = new Set(places.map((p) => p.id));
    const out: Link[] = [];
    for (const e of edges) {
      if (!have.has(e.source) || !have.has(e.target)) continue;
      out.push({
        from: e.source,
        to: e.target,
        predicate: e.title ?? e.label,
        predicateLabel: e.label,
        change: e.change === 'added' || e.change === 'removed' ? e.change : undefined,
      });
    }
    return out;
  });

  const notLocated = $derived.by(() => {
    void version;
    if (pending) return [];
    const have = new Set(places.map((p) => p.id));
    return nodes
      .filter((n) => (n.kind === 'iri' || n.kind === 'bnode') && !have.has(n.id))
      .map((n) => ({ id: n.id, label: n.label, iri: n.kind === 'iri' ? n.title : undefined }));
  });

  const loading = $derived(
    pending ? `Locating ${fmtInt(pending)} ${pending === 1 ? 'entity' : 'entities'}…` : '',
  );
</script>

<div class="graphmap" data-state={pending ? 'loading' : 'ready'}>
  <ArcMap
    {places}
    {links}
    {notLocated}
    {loading}
    {fitKey}
    {onopen}
    label="Graph map"
    emptyText="None of these entities has a geometry"
  />
  {#if failure}
    <p class="warn small">Could not load some positions: {failure}</p>
  {/if}
</div>

<style>
  .graphmap {
    height: 100%;
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    overflow: auto;
  }
  .warn {
    margin: 0;
    color: var(--warn);
    font-size: var(--fs-xs);
  }
  @media (max-width: 980px) {
    .graphmap {
      height: auto;
      overflow: visible;
    }
  }
</style>

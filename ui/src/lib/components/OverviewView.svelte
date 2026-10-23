<script lang="ts">
  /**
   * The graph overview (spec U06 §2.3): the dataset's classes and the predicates between
   * them, from the class profiles, and semantic zoom into one class or group at a time
   * through `POST /$/graph/{ds}/groups`. A click or tap opens a class or a group in place,
   * a member opens in the resource view, and the breadcrumb (or Backspace) closes levels.
   * The list view (L) is the accessible companion of the canvas, and the view falls back
   * to Cytoscape without WebGL.
   */
  import { onDestroy, onMount } from 'svelte';
  import * as api from '$lib/api';
  import { ApiError, type ClassProfile, type GroupsBy } from '$lib/api';
  import { fmtCompact, fmtInt } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import type { PrefixMap } from '$lib/rdf';
  import {
    classLevel,
    describe,
    hasWebGL2,
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

  let {
    ds,
    prefixes,
    onresource,
    onschema,
  }: {
    ds: string;
    prefixes: PrefixMap;
    /** Open a member in the resource view. */
    onresource: (iri: string, label?: string) => void;
    /** Show the schema tab. */
    onschema?: () => void;
  } = $props();

  let profiles = $state<ClassProfile[] | null>(null);
  let loadError = $state<string | null>(null);
  let loading = $state(false);
  let loadMs = $state(0);
  let builtins = $state(true);
  let path = $state<OvLevel[]>([]);
  let opening = $state<string | null>(null);
  let openError = $state<string | null>(null);
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

  const label = (iri: string) => shortLabel(iri, prefixes);

  const base = $derived.by(() =>
    profiles ? classLevel(profiles, { builtins, label }) : { nodes: [], edges: [], hidden: 0 },
  );
  const picture: OvGraph = $derived(visible(base, path, label));
  const announce = $derived(profiles ? describe(picture, path, fmtInt) : '');
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

  function nodeLabel(n: OvNode) {
    return n.kind === 'member' ? n.label : `${n.label} (${fmtCompact(n.size)})`;
  }

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
        color: classColor.get(n.cls) ?? palette[0],
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
    }));
  });
  const rows: ListRow[] = $derived(
    picture.nodes.map((n) => ({
      key: n.key,
      label: n.label,
      kind: n.kind,
      size: n.size,
      cls: label(n.cls),
      degree: degree.get(n.key) ?? 0,
      color: classColor.get(n.cls) ?? palette[0],
      openable: n.openable,
    })),
  );
  // the Cytoscape fallback
  const gnodes: GNode[] = $derived(
    picture.nodes.map((n) => ({
      id: n.key,
      label: nodeLabel(n),
      kind: 'iri',
      title: n.key.slice(2),
    })),
  );
  const gedges: GEdge[] = $derived(
    picture.edges.map((e) => ({
      id: e.key,
      source: e.source,
      target: e.target,
      label: `${e.predicates[0]?.iri ? label(e.predicates[0].iri) : ''}${e.predicates.length > 1 ? ` +${e.predicates.length - 1}` : ''} · ${fmtCompact(e.count)}`,
    })),
  );

  function readPalette() {
    if (!wrap) return;
    const css = getComputedStyle(wrap);
    const names = ['--iri', '--literal', '--bnode', '--num', '--var', '--ok', '--spark', '--kw'];
    palette = names.map((n) => css.getPropertyValue(n).trim()).filter(Boolean);
    if (!palette.length) palette = ['#2459c7'];
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

  async function load() {
    controller?.abort();
    controller = new AbortController();
    loading = true;
    loadError = null;
    const t = performance.now();
    try {
      const p = await api.schemaProfiles(ds, { signal: controller.signal });
      profiles = p.classes;
      loadMs = performance.now() - t;
      // a dataset of built-in classes only keeps them, others start without them
      builtins = !p.classes.some((c) => !c.builtin && c.instances > 0);
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') return;
      loadError = errorText(e);
    } finally {
      loading = false;
    }
  }

  function byOf(value: string): GroupsBy {
    if (value === 'auto' || value === 'members') return value;
    if (value.startsWith('community:')) return { community: value.slice(10) };
    return { property: value.slice(9) };
  }

  /** Open a class or a group: the path keeps the levels above it. */
  async function open(key: string, how: GroupsBy = 'auto') {
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
      if (key.startsWith('c:')) keep = 0;
      if (current?.key === key) keep = path.length - 1;
      const lbl = target?.label ?? current?.label ?? key;
      path = [...path.slice(0, keep), { key, label: lbl, answer }];
      by =
        typeof how === 'string'
          ? how
          : 'community' in how
            ? `community:${how.community}`
            : `property:${how.property}`;
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
    if (!current) return;
    void open(current.key, byOf(value));
  }

  /** The class profile of the open class, for the grouping choices. */
  const openClass = $derived(current ? current.answer.within.class : null);
  const groupChoices = $derived.by(() => {
    if (!openClass || !profiles) return [];
    const p = profiles.find((c) => c.class === openClass);
    if (!p) return [];
    return p.properties
      .filter((x) => !x.predicate.endsWith('#type'))
      .slice(0, 40)
      .map((x) => ({ value: `property:${x.predicate}`, label: label(x.predicate) }));
  });

  function onKey(e: KeyboardEvent) {
    if ((e.target as HTMLElement)?.closest?.('input, textarea, select, [contenteditable]')) return;
    if (e.metaKey || e.ctrlKey || e.altKey) return;
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
    void load();
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
    if (!current) return base.hidden ? `The ${fmtInt(MAX_CLASSES)} largest classes are drawn.` : '';
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
</script>

<svelte:window onkeydown={onKey} />

<div class="ov" bind:this={wrap}>
  <div class="ov-bar">
    <nav class="crumbs" aria-label="Overview levels">
      <button
        class="crumb"
        class:cur={!path.length}
        onclick={() => closeTo(0)}
        disabled={!path.length}><Icon name="graph" size={13} /> Overview</button
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
    {#if current}
      <label class="by faint small">
        <span class="hide-xs">Group by</span>
        <select
          value={by}
          onchange={(e) => regroup((e.currentTarget as HTMLSelectElement).value)}
          aria-label="Group by"
        >
          <option value="auto">Automatic</option>
          <option value="members">Members</option>
          {#if current?.answer.communities && current.answer.grouping.predicate}
            <option value={`community:${current.answer.grouping.predicate}`}>Communities</option>
          {/if}
          {#each groupChoices as g (g.value)}
            <option value={g.value}>{g.label}</option>
          {/each}
        </select>
      </label>
    {/if}
    {#if !path.length && profiles?.some((c) => c.builtin && c.instances > 0)}
      <label class="row check small"
        ><input type="checkbox" bind:checked={builtins} /> Built-in</label
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
  {#if note || openError || failReason}
    <div class="ov-notes small">
      {#if openError}<p class="err" role="alert">{openError}</p>{/if}
      {#if note}<p class="faint">{note}</p>{/if}
      {#if failReason && !listView}<p class="faint">
          {failReason}; the overview is drawn with Cytoscape.
        </p>{/if}
    </div>
  {/if}
  <p class="sr-only" aria-live="polite">{announce}</p>

  <div class="ov-body">
    {#if loading && !profiles}
      <div class="ov-empty"><span class="spinner"></span> Reading the classes…</div>
    {:else if loadError}
      <div class="ov-empty">
        <Icon name="alert" size={20} />
        <p>The overview could not be read: {loadError}</p>
        <button class="btn sm" onclick={load}><Icon name="refresh" size={13} /> Retry</button>
      </div>
    {:else if profiles && !base.nodes.length && !path.length}
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
    {:else if profiles}
      {#if listView}
        <GraphListView {rows} {selected} onselect={(k) => (selected = k)} onopen={(k) => open(k)} />
      {:else if webgl && !failReason}
        <LargeGraphView
          bind:this={large}
          nodes={lnodes}
          edges={ledges}
          {selected}
          label="Graph overview"
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
      <span class="dot" style:background={classColor.get(sel.cls) ?? palette[0]} aria-hidden="true"
      ></span>
      <div class="d-main">
        <div class="d-title" title={sel.iri ?? sel.label}>{sel.label}</div>
        <div class="faint small">
          {#if sel.kind === 'class'}Class · {fmtInt(sel.size)} instances
          {:else if sel.kind === 'group'}Group of {label(sel.cls)} · {fmtInt(sel.size)} instances
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
  {#if !path.length && profiles && base.nodes.length}
    <div class="ov-foot faint small">
      {fmtInt(base.nodes.length)} classes · read in {fmtInt(Math.round(loadMs))} ms · tap a class to open
      it
      {#if onschema}· <button class="link" onclick={onschema}>Browse the schema</button>{/if}
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
    max-width: 16ch;
  }
  .ov-notes {
    padding: 4px 12px;
    border-bottom: 1px solid var(--border);
  }
  .ov-notes p {
    margin: 2px 0;
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
  }
</style>

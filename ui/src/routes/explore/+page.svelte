<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
  import type { Term } from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import * as ex from '$lib/explore';
  import { fmtCompact, fmtInt } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import { displayIri, RDF_TYPE, termKey } from '$lib/rdf';
  import ClassTree from '$components/ClassTree.svelte';
  import GraphView, { type GEdge, type GNode } from '$components/GraphView.svelte';
  import Icon from '$components/Icon.svelte';
  import TermView from '$components/TermView.svelte';

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

  const ds = $derived(app.current);
  const prefixes = $derived(app.prefixes(ds));

  let tab = $state<'graph' | 'schema'>(
    page.url.searchParams.get('tab') === 'schema' ? 'schema' : 'graph',
  );
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
  let graphGen = 1;

  function resetGraph() {
    graphGen++;
    nodes = {};
    edges = {};
    expanding = {};
    detailCache = {};
  }
  let graphView: GraphView | undefined = $state();

  // --- graph model -----------------------------------------------------------

  const gNodes = $derived.by((): GNode[] =>
    Object.values(nodes)
      .filter((n) => showLiterals || !n.lit)
      .map((n) => ({
        id: n.id,
        label: n.label,
        kind: n.term.type === 'uri' ? 'iri' : n.term.type,
        title: n.term.type === 'uri' ? n.term.value : undefined,
        focus: n.id === focusId,
        expanded: n.expanded,
      })),
  );
  const gEdges = $derived(
    Object.values(edges).filter((e) => (showLiterals || !e.lit) && (!hideTypes || !e.isType)),
  );
  const nodeCount = $derived(Object.keys(nodes).length);

  function labelFor(t: Term, label?: string) {
    if (label) return label;
    if (t.type === 'uri') return shortLabel(t.value, prefixes);
    if (t.type === 'bnode') return `_:${t.value.slice(0, 8)}`;
    if (t.type === 'literal') return t.value.length > 48 ? t.value.slice(0, 45) + '…' : t.value;
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

  async function expand(id: string) {
    const n = nodes[id];
    if (!n || n.term.type !== 'uri' || !ds || expanding[id]) return;
    const gen = graphGen;
    const stale = () => gen !== graphGen || nodes[id] !== n;
    if (nodeCount > MAX_NODES) {
      toasts.push(
        'error',
        'Graph is getting large',
        `Over ${MAX_NODES} nodes. Remove some or refocus before expanding.`,
      );
      return;
    }
    expanding[id] = gen;
    try {
      const { out, inc, label } = await ex.neighbours(ds, n.iri);
      if (stale()) return;
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
      n.expanded = true;
      if (out.length >= 100 || inc.length >= 100) {
        toasts.push(
          'info',
          'Showing the first 100 edges each way',
          'Open the resource in Query to see everything.',
        );
      }
    } catch (e) {
      if (!stale()) toasts.error(`Could not expand ${n.label}`, e);
    } finally {
      if (expanding[id] === gen) delete expanding[id];
    }
  }

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
    if (tab === 'schema') p.set('tab', 'schema');
    goto(`${resolve('/explore')}?${p}`, { replaceState: true, keepFocus: true, noScroll: true });
  }

  function select(id: string | null) {
    selectedId = id;
    if (id) void loadDetails(id);
  }

  async function loadDetails(id: string) {
    const n = nodes[id];
    if (!n || n.term.type !== 'uri' || !ds || detailCache[id]) return;
    const gen = graphGen;
    let d: ex.Details | { error: string };
    try {
      d = await ex.details(ds, n.iri);
    } catch (e) {
      d = { error: api.errorMessage(e) };
    }
    if (gen === graphGen && nodes[id] === n) detailCache[id] = d;
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
  const detail = $derived(selectedId ? detailCache[selectedId] : undefined);
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

  let starters = $state<{ iri: string; instances: number }[]>([]);
  async function loadStarters(name: string) {
    try {
      const s = await api.datasetStats(name);
      starters = s.classes.slice(0, 10);
    } catch {
      starters = [];
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

  async function loadSchema(name: string) {
    schemaLoading = true;
    schemaErr = null;
    try {
      schema = await ex.loadSchema(name);
      schemaFor = name;
      // Open the first two levels by default.
      const open = new Set<string>();
      for (const r of schema.roots) {
        open.add(r);
        for (const s of schema.classes.get(r)?.subs ?? []) open.add(s);
      }
      openClasses = open;
    } catch (e) {
      schemaErr = api.errorMessage(e);
    } finally {
      schemaLoading = false;
    }
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
    return f
      ? schema.properties.filter(
          (p) => (p.label ?? '').toLowerCase().includes(f) || p.iri.toLowerCase().includes(f),
        )
      : schema.properties;
  });
  const cls = $derived(selectedClass && schema ? schema.classes.get(selectedClass) : undefined);
  const clsProps = $derived.by(() => {
    if (!cls || !schema) return { domain: [], range: [] };
    return {
      domain: schema.properties.filter((p) => p.domains.includes(cls.iri)),
      range: schema.properties.filter((p) => p.ranges.includes(cls.iri)),
    };
  });
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

  function setTab(t: 'graph' | 'schema') {
    tab = t;
    syncUrl(focusId && nodes[focusId] ? nodes[focusId].iri : null);
  }

  onMount(() => {
    const onDoc = (e: MouseEvent) => {
      if (searchOpen && !(e.target as HTMLElement).closest('.search')) searchOpen = false;
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  });
</script>

<svelte:head><title>Explore | Sparkles</title></svelte:head>

<div class="page">
  <header class="bar">
    <div class="search">
      <Icon name="search" size={15} />
      <input
        class="search-input"
        placeholder={ds ? `Find a resource in ${ds} by label or IRI` : 'Choose a dataset first'}
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
    </div>
  </header>

  {#if !ds}
    <div class="empty">
      <Icon name="database" size={22} />
      <p>No dataset selected.</p>
      <a class="btn" href={resolve('/datasets')}>Go to Datasets</a>
    </div>
  {:else if tab === 'graph'}
    <div class="split">
      <div class="canvas">
        <div class="gtools">
          <label class="row check"
            ><input type="checkbox" bind:checked={showLiterals} /> Literals</label
          >
          <label class="row check"
            ><input type="checkbox" bind:checked={hideTypes} /> Hide rdf:type</label
          >
          <span class="spacer"></span>
          <span class="faint">{gNodes.length} nodes, {gEdges.length} edges</span>
          {#if nodeCount}<button class="btn sm ghost" onclick={clearGraph}
              ><Icon name="trash" size={13} /> Clear</button
            >{/if}
        </div>
        {#if nodeCount === 0}
          <div class="start">
            <Icon name="explore" size={26} />
            <h2>Start from a resource</h2>
            <p class="muted">
              Search above, paste an IRI, or pick a class to start from one of its instances.
            </p>
            {#if starters.length}
              <div class="chips">
                {#each starters as s (s.iri)}
                  <button class="chip" onclick={() => sampleOf(s.iri)} title={s.iri}>
                    <span class="t-iri mono">{displayIri(s.iri, prefixes)}</span>
                    <span class="faint">{fmtCompact(s.instances)}</span>
                  </button>
                {/each}
              </div>
            {/if}
            <button class="btn sm" onclick={() => setTab('schema')}
              ><Icon name="tree" size={13} /> Browse the schema</button
            >
          </div>
        {:else}
          <div class="cy-host">
            <GraphView
              bind:this={graphView}
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
                <div class="mono faint">{n.term.type === 'literal' ? 'literal' : n.term.type}</div>
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
                  class="btn sm ghost icon"
                  title="Remove from graph"
                  aria-label="Remove from graph"
                  onclick={() => removeNode(n.id)}
                >
                  <Icon name="x" size={13} />
                </button>
              </div>
            {/if}
          </div>

          {#if n.term.type === 'literal'}
            <div class="side-body"><pre class="literal">{n.term.value}</pre></div>
          {:else if n.term.type !== 'uri'}
            <div class="side-body faint">
              Blank nodes can only be explored through their neighbours.
            </div>
          {:else if !detail}
            <div class="side-body row faint"><span class="spinner"></span> Loading properties…</div>
          {:else if 'error' in detail}
            <div class="side-body"><div class="error-box">{detail.error}</div></div>
          {:else}
            <div class="side-body scroll">
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
                        <th title={r.p}><span class="t-iri">{displayIri(r.p, prefixes)}</span></th>
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
    </div>
  {:else}
    <!-- schema -->
    <div class="split">
      <div class="schema-main">
        {#if schema?.ontology}
          <div class="ontology">
            <strong>{schema.ontology.label ?? shortLabel(schema.ontology.iri, prefixes)}</strong>
            {#if schema.ontology.version}<span class="badge">v{schema.ontology.version}</span>{/if}
            <span class="mono faint">{schema.ontology.iri}</span>
          </div>
        {/if}
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
              Properties <span class="count">{schema ? schema.properties.length : ''}</span>
            </button>
          </div>
          <span class="spacer"></span>
          {#if schemaView === 'classes'}
            <input class="input sm" placeholder="Filter classes" bind:value={classFilter} />
          {:else}
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
                />
              {:else}
                <div class="legend faint"><span>Class</span><span>subclasses, instances</span></div>
                <ClassTree
                  {schema}
                  iris={schema.roots}
                  {prefixes}
                  open={openClasses}
                  selected={selectedClass}
                  onselect={(i) => (selectedClass = i)}
                  ontoggle={toggleClass}
                />
              {/if}
            {:else}
              <table class="data plist">
                <thead><tr><th>Property</th><th>Kind</th><th>Domain</th><th>Range</th></tr></thead>
                <tbody>
                  {#each propMatches as p (p.iri)}
                    <tr>
                      <td title={p.iri}>
                        <div class="pname">{p.label ?? shortLabel(p.iri, prefixes)}</div>
                        <div class="mono faint small">{displayIri(p.iri, prefixes)}</div>
                      </td>
                      <td
                        >{#each p.kinds as k (k)}<span class="badge">{kindName(k)}</span>
                        {/each}</td
                      >
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
                    <tr><td colspan="4" class="faint">No properties match.</td></tr>
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
            <h3 class="sub">Superclasses</h3>
            <div class="chips left">
              {#each cls.supers as s (s)}<button class="cls-link" onclick={() => revealClass(s)}
                  >{classLabel(s)}</button
                >{:else}<span class="faint">None (root class)</span>{/each}
            </div>
            <h3 class="sub">Subclasses</h3>
            <div class="chips left">
              {#each cls.subs as s (s)}<button class="cls-link" onclick={() => revealClass(s)}
                  >{classLabel(s)}</button
                >{:else}<span class="faint">None</span>{/each}
            </div>
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
  .hint {
    padding: 4px 12px;
    font-size: var(--fs-xs);
    border-top: 1px solid var(--border);
    background: var(--surface);
  }
  .start {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    padding: 24px;
    text-align: center;
    color: var(--text-2);
  }
  .start h2 {
    color: var(--text);
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
</style>

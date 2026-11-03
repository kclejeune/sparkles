<script lang="ts">
  // The ontology browser (docs/specs/U12-ontology-workbench/SPEC.md, "Ontology browser"):
  // a read-only page for each entity of a dataset's ontologies and thesauri, and a landing
  // page for the ontology of a graph. An entity's page reads the term route and one level
  // of the hierarchy route with its ancestors, which read the store's indexes around the
  // entity and never the whole ontology. The landing page reads the header and counts by
  // SPARQL and the roots from the hierarchy route. The components come from
  // lib/components/browse, and this page only lays them out.
  import { goto } from '$app/navigation';
  import { base } from '$app/paths';
  import { page } from '$app/state';
  import * as api from '$lib/api';
  import { app } from '$lib/app.svelte';
  import { branchParam, branchPath, onBranch } from '$lib/branches';
  import {
    axiomGroups,
    browseHref,
    builtIn,
    countsQuery,
    definitionsQuery,
    displayName,
    downloadHref,
    editHref,
    fetchHierarchy,
    fetchTerm,
    headGraphsQuery,
    incomingQuery,
    usageQuery,
    kindWord,
    mentionGroups,
    mergePieces,
    neighbourhood,
    oboIri,
    ontologyHead,
    parseCounts,
    parseDefinitions,
    parseReuse,
    reuseQuery,
    termSections,
    treeKindOf,
    treeWords,
    isLiteral,
    valueText,
    type BrowseSelection,
    type GraphPiece,
    type Hierarchy,
    type InfoRow,
    type Linker,
    type OntologyHead,
    type Reuse,
    type TermPage,
    type TreeKind,
    type TreeNode,
  } from '$lib/browse';
  import type { EntitySearchKind } from '$lib/entity-search';
  import { iriName } from '$lib/ontology';
  import { WELL_KNOWN } from '$lib/rdf';
  import Icon from '$components/Icon.svelte';
  import DownloadButtons from '$components/browse/DownloadButtons.svelte';
  import EntitySearchList from '$components/browse/EntitySearchList.svelte';
  import LineageTree from '$components/browse/LineageTree.svelte';
  import TermGraph from '$components/browse/TermGraph.svelte';
  import TermHeader from '$components/browse/TermHeader.svelte';
  import TermInfobox from '$components/browse/TermInfobox.svelte';
  import TermLabels from '$components/browse/TermLabels.svelte';
  import TermLogic from '$components/browse/TermLogic.svelte';
  import TermNotes from '$components/browse/TermNotes.svelte';
  import TermReuse from '$components/browse/TermReuse.svelte';
  import TermSynonyms from '$components/browse/TermSynonyms.svelte';
  import TermUsage from '$components/browse/TermUsage.svelte';
  import TermXrefs from '$components/browse/TermXrefs.svelte';
  import TopTerms from '$components/browse/TopTerms.svelte';

  const RDFS = WELL_KNOWN.rdfs;
  const SKOS = WELL_KNOWN.skos;
  const OWL = WELL_KNOWN.owl;
  const OBO = 'http://purl.obolibrary.org/obo/';
  const OIO = 'http://www.geneontology.org/formats/oboInOwl#';

  // --- the URL's state -----------------------------------------------------------------
  const q = $derived(page.url.searchParams);
  const name = $derived(q.get('ds') ?? app.current ?? '');
  const branch = $derived(branchParam(q.get('branch')));
  const target = $derived(onBranch(name, branch));
  const graph = $derived(q.get('graph') || null);
  const at = $derived(q.get('at') || null);
  const iri = $derived(q.get('iri') || null);
  const view = $derived(q.get('view') === 'graph' ? 'graph' : 'tree');
  const sel = $derived<BrowseSelection>({ graph, at });
  const prefixes = $derived(app.prefixes(name));
  const langs: readonly string[] = typeof navigator === 'undefined' ? [] : [...navigator.languages];

  $effect(() => {
    if (name && name !== app.current && app.datasets.some((d) => d.name === name))
      app.setDataset(name);
    if (name) void app.loadPrefixes(name);
  });

  const keep = $derived({ graph, branch, at });
  const hrefOf = (i: string | null, extra: Record<string, string | null> = {}) =>
    browseHref(base, name, i, { ...keep, ...extra });

  // --- an entity's page ----------------------------------------------------------------
  let term = $state<TermPage | null>(null);
  let termError = $state<{ status: number | null; text: string } | null>(null);
  let tree = $state<Hierarchy | null>(null);
  let treeKind = $state<TreeKind | null>(null);
  let piece = $state<GraphPiece | null>(null);
  /** The header of the graph's ontology or scheme, for the breadcrumbs and the infobox. */
  let termHead = $state<{ head: OntologyHead; scheme: boolean } | null>(null);
  let graphEpoch = $state(0);
  let reuse = $state<{ checked: string[]; found: Reuse[]; loading: boolean }>({
    checked: [],
    found: [],
    loading: false,
  });

  $effect(() => {
    const t = target;
    const s = sel;
    const i = iri;
    if (!t || !i) return;
    const ctl = new AbortController();
    void loadTerm(t, s, i, ctl.signal);
    return () => ctl.abort();
  });

  async function loadTerm(t: string, s: BrowseSelection, i: string, signal: AbortSignal) {
    term = null;
    termError = null;
    tree = null;
    piece = null;
    reuse = { checked: [], found: [], loading: false };
    let got: TermPage;
    try {
      got = await fetchTerm(t, s, i, {}, signal);
    } catch (e) {
      if (signal.aborted) return;
      termError = {
        status: e instanceof api.ApiError ? e.status : null,
        text: api.errorMessage(e),
      };
      return;
    }
    if (signal.aborted) return;
    term = got;
    piece = neighbourhood(got, prefixes);
    graphEpoch++;
    const kind = treeKindOf(got.kinds);
    treeKind = kind;
    if (kind)
      fetchHierarchy(t, s, { kind, iri: i, ancestors: true, limit: 50 }, signal).then(
        (h) => {
          if (signal.aborted) return;
          tree = h;
          if (piece) piece = mergePieces(piece, lineagePiece(h));
        },
        () => {},
      );
    ontologyHead(t, s, langs, signal).then(
      (h) => {
        if (!signal.aborted) termHead = h;
      },
      () => {},
    );
    void loadReuse(i, signal);
  }

  /** The entity's direct parents for the graph view, from the hierarchy's ancestors. */
  function lineagePiece(h: Hierarchy): GraphPiece {
    const nodes = new Map<string, { id: string; label: string }>();
    const edges: GraphPiece['edges'] = [];
    const word =
      h.kind === 'concept' ? 'broader' : h.kind === 'class' ? 'subclass of' : 'subproperty of';
    for (const [c, p] of h.ancestors?.edges ?? []) {
      if (c !== iri) continue;
      const n = h.ancestors?.nodes.find((x) => x.iri === p);
      nodes.set(p, { id: p, label: n?.label ?? n?.shortForm ?? iriName(p, prefixes) });
      edges.push({ id: `${c} ${word} ${p}`, source: c, target: p, label: word });
    }
    return { nodes: [...nodes.values()], edges };
  }

  async function loadReuse(i: string, signal: AbortSignal) {
    const others = app.datasets
      .map((d) => d.name)
      .filter((d) => d !== name)
      .slice(0, 20);
    reuse = { checked: others, found: [], loading: others.length > 0 };
    if (!others.length) return;
    const answers = await Promise.allSettled(
      others.map(async (d) =>
        parseReuse(d, (await api.select(d, reuseQuery(i), { signal })) as never),
      ),
    );
    if (signal.aborted) return;
    reuse = {
      checked: others,
      found: answers.flatMap((a) => (a.status === 'fulfilled' && a.value ? [a.value] : [])),
      loading: false,
    };
  }

  async function expand(id: string) {
    if (!piece || builtIn(id)) return;
    try {
      const t = await fetchTerm(target, sel, id, { mentions: 50, incoming: 10 });
      if (piece) piece = mergePieces(piece, neighbourhood(t, prefixes, 10));
    } catch {
      // a node the graphs do not describe has no neighbourhood to add
    }
  }

  async function moreIncoming(
    kind: 'incoming' | 'usage',
    property: string,
    offset: number,
  ): Promise<string[]> {
    if (!term) return [];
    const q =
      kind === 'usage'
        ? usageQuery(term.iri, term.graphs, offset)
        : incomingQuery(term.iri, property, term.graphs, offset);
    const rows = await api.select(target, q, {
      at: at ?? undefined,
    });
    return rows.flatMap((r) => (r.s && r.s.type === 'uri' ? [r.s.value] : []));
  }

  const linker = $derived<Linker>({
    href: (i) => (builtIn(i) ? null : hrefOf(i)),
    name: (i) => (term && i === term.iri ? title : displayName(i, term?.names, prefixes)),
    short: (i) => term?.names[i]?.shortForm ?? (term?.iri === i ? term.shortForm : null),
  });

  /** CURIEs such as GO:0032047 link when they are in the entity's own OBO namespace. */
  const curieIri = $derived.by(() => {
    const sf = term?.shortForm ?? '';
    const own = /^([A-Za-z][A-Za-z0-9_]*):\d+$/.exec(sf)?.[1];
    return (c: string) => (own && c.startsWith(own + ':') ? oboIri(c) : null);
  });

  const sections = $derived(term ? termSections(term, langs) : null);
  const groups = $derived(term ? axiomGroups(term.iri, term.kinds, term.axioms) : []);
  const mentions = $derived(term ? mentionGroups(term.iri, term.mentions.axioms) : []);
  const title = $derived(term ? (term.label ?? term.shortForm ?? iriName(term.iri, prefixes)) : '');
  const kind = $derived(term ? kindWord(term.kinds) : '');
  const isSkos = $derived(
    !!term?.kinds.some((k) => k === 'concept' || k === 'conceptScheme' || k === 'collection'),
  );
  const values = (p: string) =>
    term?.statements.filter((s) => s.property === p).map((s) => s.value) ?? [];
  const iris = (p: string) => values(p).flatMap((v) => (isLiteral(v) ? [] : [v as string]));
  const ontologyIri = $derived(term?.ontologies.find((o) => o.ontology)?.ontology ?? null);
  const ontologyName = $derived(
    termHead?.head.title ?? (ontologyIri ? displayName(ontologyIri, term?.names, prefixes) : name),
  );
  const plural: Record<string, string> = {
    class: 'Classes',
    'object property': 'Object properties',
    'data property': 'Data properties',
    'annotation property': 'Annotation properties',
    individual: 'Individuals',
    concept: 'Concepts',
    'concept scheme': 'Concept schemes',
    collection: 'Collections',
    datatype: 'Datatypes',
  };
  const skipIncoming = $derived(
    treeKind === 'class'
      ? [RDFS + 'subClassOf']
      : treeKind === 'concept'
        ? [SKOS + 'broader', SKOS + 'narrower']
        : treeKind
          ? [RDFS + 'subPropertyOf']
          : [],
  );

  const infoRows = $derived.by<InfoRow[]>(() => {
    if (!term || !sections) return [];
    const rows: InfoRow[] = [
      { label: 'Kind', text: kind.charAt(0).toUpperCase() + kind.slice(1) },
      term.deprecated
        ? { label: 'Status', text: 'deprecated', badge: 'danger' }
        : { label: 'Status', text: 'current', badge: 'ok' },
    ];
    for (const g of sections.info)
      rows.push({
        label: g.kind,
        values: g.values,
        mono: g.kind === 'ID' || g.kind === 'Notation',
      });
    if (ontologyIri || term.ontologies.length) {
      const o = term.ontologies[0];
      rows.push({
        label: 'Ontology',
        text: ontologyName,
        note: [
          termHead?.head.version ? `version ${termHead.head.version}` : null,
          o ? (o.graph === 'default' ? 'default graph' : o.graph) : null,
        ]
          .filter(Boolean)
          .join(', '),
      });
    }
    if (tree?.node) {
      const w = treeWords(tree.kind);
      rows.push({
        label: 'Hierarchy',
        text: `${tree.node.parents.toLocaleString('en-US')} ${tree.node.parents === 1 ? 'parent' : 'parents'}, ${tree.node.children.toLocaleString('en-US')} ${tree.node.children === 1 ? w.child : w.children}`,
      });
    }
    const inst = term.incoming.find((i) => i.property === WELL_KNOWN.rdf + 'type');
    if (inst) rows.push({ label: 'Instances', text: inst.count.toLocaleString('en-US') });
    if (isSkos) {
      const ls = [...new Set(sections.labels.map((l) => l.lang).filter(Boolean))].sort();
      if (ls.length) rows.push({ label: 'Languages', text: ls.join(', ') });
      const cols = term.incoming.find((i) => i.property === SKOS + 'member');
      if (cols) rows.push({ label: 'Collections', values: cols.subjects });
    }
    return rows;
  });

  const exploreHref = $derived(
    iri
      ? `${base}/explore?${new URLSearchParams({ ds: name, iri, ...(branch ? { branch } : {}) })}`
      : '',
  );
  const editLink = $derived.by(() => {
    if (!term) return '';
    const h = editHref(base, name, term.iri, term.kinds, sel);
    return branch ? `${h}&branch=${encodeURIComponent(branch)}` : h;
  });

  // --- the landing page ----------------------------------------------------------------
  let head = $state<{ head: OntologyHead; scheme: boolean } | null>(null);
  let counts = $state<{ kind: string; label: string; count: number }[]>([]);
  let roots = $state<Hierarchy | null>(null);
  let rootItems = $state<TreeNode[]>([]);
  let rootDefs = $state<Map<string, string>>(new Map());
  let rootBusy = $state(false);
  let headGraphs = $state<string[]>([]);
  let landingError = $state<string | null>(null);
  let landingLoading = $state(false);

  $effect(() => {
    const t = target;
    const s = sel;
    if (!t || iri) return;
    const ctl = new AbortController();
    void loadLanding(t, s, ctl.signal);
    return () => ctl.abort();
  });

  async function loadLanding(t: string, s: BrowseSelection, signal: AbortSignal) {
    head = null;
    counts = [];
    roots = null;
    rootItems = [];
    rootDefs = new Map();
    landingError = null;
    landingLoading = true;
    const opts = { at: s.at ?? undefined, signal };
    try {
      const [h, c, g] = await Promise.all([
        ontologyHead(t, s, langs, signal),
        api.select(t, countsQuery(s.graph), opts).then((rows) => parseCounts(rows as never)),
        api
          .select(t, headGraphsQuery(), opts)
          .then((rows) => rows.flatMap((r) => (r.g?.type === 'uri' ? [r.g.value] : [])))
          .catch(() => [] as string[]),
      ]);
      if (signal.aborted) return;
      headGraphs = g;
      head = h;
      counts = c;
      const has = (k: string) => c.some((x) => x.kind === k);
      const kind: TreeKind = !has(OWL + 'Class') && has(SKOS + 'Concept') ? 'concept' : 'class';
      const r = await fetchHierarchy(t, s, { kind, limit: 50 }, signal);
      if (signal.aborted) return;
      roots = r;
      rootItems = r.children;
      void loadDefinitions(t, s, r.children, signal);
    } catch (e) {
      if (!signal.aborted) landingError = api.errorMessage(e);
    } finally {
      if (!signal.aborted) landingLoading = false;
    }
  }

  async function loadDefinitions(
    t: string,
    s: BrowseSelection,
    nodes: TreeNode[],
    signal?: AbortSignal,
  ) {
    if (!nodes.length) return;
    try {
      const rows = await api.select(
        t,
        definitionsQuery(
          nodes.map((n) => n.iri),
          s.graph,
        ),
        { at: s.at ?? undefined, signal },
      );
      if (signal?.aborted) return;
      rootDefs = new Map([...rootDefs, ...parseDefinitions(rows as never, langs)]);
    } catch {
      // the roots stand without their definitions
    }
  }

  async function moreRoots() {
    if (!roots || roots.next === null) return;
    rootBusy = true;
    try {
      const r = await fetchHierarchy(target, sel, {
        kind: roots.kind,
        limit: 50,
        offset: rootItems.length,
      });
      roots = r;
      rootItems = [...rootItems, ...r.children];
      void loadDefinitions(target, sel, r.children);
    } finally {
      rootBusy = false;
    }
  }

  const chips = $derived.by(() => {
    const has = (k: string) => counts.some((c) => c.kind === k);
    const out: { label: string; kind: EntitySearchKind }[] = [];
    if (has(OWL + 'Class')) out.push({ label: 'Classes', kind: 'class' });
    if (
      has(OWL + 'ObjectProperty') ||
      has(OWL + 'DatatypeProperty') ||
      has(OWL + 'AnnotationProperty')
    )
      out.push({ label: 'Properties', kind: 'property' });
    if (has(OWL + 'NamedIndividual')) out.push({ label: 'Individuals', kind: 'individual' });
    if (has(SKOS + 'Concept')) out.push({ label: 'Concepts', kind: 'concept' });
    return out;
  });
  const entityTotal = $derived(
    counts.filter((c) => c.kind !== 'deprecated').reduce((n, c) => n + c.count, 0),
  );
  const landingTitle = $derived(head?.head.title ?? head?.head.iri ?? name);
  const pageTitle = $derived(iri ? (term ? title : 'Browse') : landingTitle);
</script>

<svelte:head><title>{pageTitle || 'Browse'} | Sparkles</title></svelte:head>

<div class="wrap">
  {#if !name}
    <div class="page-container">
      <div class="empty">
        <p>No dataset selected.</p>
        <a class="btn" href="{base}/datasets">Go to Datasets</a>
      </div>
    </div>
  {:else if iri}
    {#if termError}
      <div class="page-container">
        <div class="empty" role="alert">
          {#if termError.status === 404}
            <p>The graphs of <span class="mono">{name}</span> do not mention this IRI.</p>
            <p class="mono faint iri-line">{iri}</p>
          {:else}
            <p>{termError.text}</p>
          {/if}
          <a class="btn" href={hrefOf(null)}>Back to {name}</a>
        </div>
      </div>
    {:else if !term || !sections}
      <div class="page-container"><p class="faint" aria-live="polite">Loading…</p></div>
    {:else}
      <div class="browse" class:graph={view === 'graph'} data-kind={treeKind ?? 'none'}>
        <div class="b-head">
          <TermHeader
            name={term}
            {title}
            {kind}
            crumbs={[
              { label: ontologyName, href: hrefOf(null) },
              { label: plural[kind] ?? 'Entities', href: null },
            ]}
            definition={sections.definition}
            replacedBy={iris(OBO + 'IAO_0100001')}
            consider={values(OIO + 'consider')
              .map((v) => valueText(v))
              .map((v) => oboIri(v) ?? v)}
            {linker}
            {curieIri}
          >
            {#snippet actions()}
              {#if !treeKind}
                {#if view === 'graph'}
                  <a class="btn sm" href={hrefOf(iri)} data-sveltekit-noscroll
                    ><Icon name="book" size={14} />Page</a
                  >
                {:else}
                  <a class="btn sm" href={hrefOf(iri, { view: 'graph' })} data-sveltekit-noscroll
                    ><Icon name="graph" size={14} />Graph</a
                  >
                {/if}
              {/if}
              <a class="btn sm" href={exploreHref}
                ><Icon name="explore" size={14} />Open in Explore</a
              >
              <a class="btn sm" href={editLink}><Icon name="pencil" size={14} />Edit</a>
            {/snippet}
          </TermHeader>
        </div>

        {#if treeKind}
          <aside class="b-tree" aria-label="Hierarchy">
            <div class="tree-head">
              <h2>Hierarchy</h2>
              <span class="spacer"></span>
              <span class="seg" role="group" aria-label="View">
                <a
                  href={hrefOf(term.iri)}
                  aria-current={view === 'tree' ? 'true' : undefined}
                  data-sveltekit-noscroll>Tree</a
                ><a
                  href={hrefOf(term.iri, { view: 'graph' })}
                  aria-current={view === 'graph' ? 'true' : undefined}
                  data-sveltekit-noscroll>Graph</a
                >
              </span>
            </div>
            {#if tree}
              <LineageTree
                {target}
                {sel}
                kind={treeKind}
                iri={term.iri}
                {title}
                answer={tree}
                href={(i) => hrefOf(i)}
              />
            {:else}
              <p class="faint">Loading…</p>
            {/if}
          </aside>
        {/if}

        {#if view === 'graph' && piece}
          <div class="b-graph">
            <TermGraph
              ds={target}
              {piece}
              {linker}
              {exploreHref}
              onexpand={expand}
              epoch={graphEpoch}
            />
          </div>
        {:else}
          <div class="b-article">
            <TermLogic {groups} {linker} {prefixes} />
            <TermSynonyms synonyms={sections.synonyms} {linker} />
            <TermNotes
              notes={sections.otherDefinitions.length
                ? [
                    { kind: 'Definition in other languages', values: sections.otherDefinitions },
                    ...sections.notes,
                  ]
                : sections.notes}
              {linker}
              {langs}
              {curieIri}
            />
            <TermLabels labels={sections.labels} />
            {#if sections.related.length}
              <section class="term-sec" aria-labelledby="rel-h">
                <h2 id="rel-h">
                  Related concepts <span class="count-badge">{sections.related.length}</span>
                </h2>
                <div class="term-flow">
                  {#each sections.related as r (r)}<a href={linker.href(r)}>{linker.name(r)}</a
                    >{/each}
                </div>
              </section>
            {/if}
            {#if sections.members.length}
              <section class="term-sec" aria-labelledby="mem-h">
                <h2 id="mem-h">
                  Members <span class="count-badge">{sections.members.length}</span>
                </h2>
                <div class="term-flow">
                  {#each sections.members as r (r)}<a href={linker.href(r)}>{linker.name(r)}</a
                    >{/each}
                </div>
              </section>
            {/if}
            <TermXrefs
              xrefs={sections.xrefs}
              seeAlso={sections.seeAlso}
              mappings={sections.mappings}
              {linker}
            />
            <TermUsage
              iri={term.iri}
              groups={mentions}
              total={term.mentions.total}
              truncated={term.mentions.truncated}
              incoming={term.incoming}
              usage={term.usage}
              {linker}
              {prefixes}
              more={moreIncoming}
              skip={skipIncoming}
            />
            <TermReuse
              found={reuse.found}
              checked={reuse.checked}
              loading={reuse.loading}
              href={(d) => browseHref(base, d, term?.iri ?? null)}
            />
            {#if sections.other.length}
              <section class="term-sec">
                <details class="ann">
                  <summary
                    >All other annotations <span class="count-badge"
                      >{sections.other.reduce((n, o) => n + o.values.length, 0)}</span
                    ></summary
                  >
                  <div class="table-scroll">
                    <table class="data">
                      <tbody>
                        {#each sections.other as o (o.property)}
                          <tr>
                            <th scope="row" class="mono">{iriName(o.property, prefixes)}</th>
                            <td>
                              {#each o.values as v, i (i)}{#if i > 0}<br
                                  />{/if}{#if isLiteral(v)}{v['@value']}{#if v['@language']}<span
                                      class="lang-tag">{v['@language']}</span
                                    >{/if}{:else if linker.href(v)}<a href={linker.href(v)}
                                    >{linker.name(v)}</a
                                  >{:else}<span class="mono">{v}</span>{/if}{/each}
                            </td>
                          </tr>
                        {/each}
                      </tbody>
                    </table>
                  </div>
                </details>
              </section>
            {/if}
            {#if term.statementsTruncated}
              <p class="faint">
                This entity has more statements than the page reads. The download has them all.
              </p>
            {/if}
          </div>
        {/if}

        <aside class="b-info" aria-label="About this entity">
          <TermInfobox rows={infoRows} {linker}>
            {#snippet footer()}
              <DownloadButtons
                noun="this {kind}"
                href={(f) => branchPath(downloadHref(target, sel, term!.iri, f))}
              />
            {/snippet}
          </TermInfobox>
        </aside>
      </div>
    {/if}
  {:else}
    <div class="landing">
      <header class="term-head">
        <nav class="crumbs" aria-label="Breadcrumbs">
          <span>{name}</span><span aria-hidden="true">›</span><span>{graph ?? 'default graph'}</span
          >
        </nav>
        <div class="title-row">
          <h1 class="term-title">{landingTitle}</h1>
          <div class="actions">
            {#if headGraphs.length}
              <label class="form-row">
                Graph
                <select
                  class="select"
                  value={graph ?? ''}
                  onchange={(e) =>
                    goto(
                      browseHref(base, name, null, {
                        ...keep,
                        graph: e.currentTarget.value || null,
                      }),
                    )}
                >
                  <option value="">default graph</option>
                  {#each headGraphs as g (g)}<option value={g}>{iriName(g, prefixes)}</option
                    >{/each}
                  {#if graph && !headGraphs.includes(graph)}<option value={graph}>{graph}</option
                    >{/if}
                </select>
              </label>
            {/if}
            <a
              class="btn sm"
              href="{base}/{head?.scheme ? 'thesaurus' : 'ontology'}?{new URLSearchParams({
                ds: name,
                ...(graph ? { graph } : {}),
              })}"
              ><Icon name="pencil" size={14} />Open in {head?.scheme ? 'Thesaurus' : 'Ontology'} editor</a
            >
          </div>
        </div>
        {#if head}
          <div class="ids">
            {#if head.head.iri}<span class="iri-text">{head.head.iri}</span>{/if}
            {#if head.head.version}<span class="faint">version {head.head.version}</span>{/if}
            {#if head.head.license}
              {#if /^https?:/.test(head.head.license)}<a
                  href={head.head.license}
                  rel="noopener noreferrer"
                  target="_blank">License</a
                >{:else}<span>{head.head.license}</span>{/if}
            {/if}
          </div>
          {#if head.head.description}<p class="lede">{head.head.description}</p>{/if}
        {/if}
      </header>
      {#if landingError}
        <div class="error-box" role="alert">
          <strong>Could not read the ontology.</strong>
          {landingError}
        </div>
      {/if}
      <!-- the entity search reads a branch head, so a past state has no search box (item 658) -->
      {#if !at}
        <EntitySearchList
          {target}
          {graph}
          href={(i) => hrefOf(i)}
          {chips}
          placeholder={entityTotal
            ? `Search ${entityTotal.toLocaleString('en-US')} entities by label, synonym, ID or definition`
            : 'Search by label, synonym, ID or definition'}
        />
      {/if}
      <div class="cols">
        {#if roots}
          <TopTerms
            title={roots.kind === 'concept' ? 'Top concepts' : 'Top classes'}
            roots={rootItems}
            kind={roots.kind}
            definitions={rootDefs}
            href={(i) => hrefOf(i)}
            from={roots.roots}
            deprecated={roots.deprecatedRoots ?? 0}
            more={roots.next !== null ? moreRoots : undefined}
            busy={rootBusy}
          />
        {:else if landingLoading}
          <p class="faint" aria-live="polite">Loading…</p>
        {:else}
          <div></div>
        {/if}
        <aside class="side">
          {#if counts.length}
            <section class="panel" aria-labelledby="contents-h">
              <div class="panel-head"><h3 id="contents-h">Contents</h3></div>
              <div class="panel-body">
                <dl class="counts">
                  {#each counts as c (c.kind)}
                    <dt class:sub={c.kind === 'deprecated'}>{c.label}</dt>
                    <dd class:sub={c.kind === 'deprecated'}>{c.count.toLocaleString('en-US')}</dd>
                  {/each}
                </dl>
              </div>
            </section>
          {/if}
          {#if head?.head.annotations.length}
            <section class="panel" aria-labelledby="about-h">
              <div class="panel-head">
                <h3 id="about-h">About this {head.scheme ? 'thesaurus' : 'ontology'}</h3>
              </div>
              <div class="panel-body">
                <dl class="about">
                  {#each head.head.annotations.slice(0, 12) as a, i (i)}
                    <dt>{displayName(a.property, undefined, prefixes)}</dt>
                    <dd>
                      {#if isLiteral(a.value)}{a.value[
                          '@value'
                        ]}{:else if /^https?:/.test(a.value)}<a
                          href={a.value}
                          rel="noopener noreferrer"
                          target="_blank">{iriName(a.value, prefixes)}</a
                        >{:else}{a.value}{/if}
                    </dd>
                  {/each}
                </dl>
              </div>
            </section>
          {/if}
        </aside>
      </div>
    </div>
  {/if}
</div>

<style>
  .wrap {
    container: browse / inline-size;
    min-width: 0;
  }
  /* one column on a phone: head, facts, hierarchy, article */
  .browse {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    grid-template-areas: 'head' 'info' 'tree' 'article';
    gap: var(--sp-6);
    padding: var(--gutter) var(--gutter) var(--sp-10);
    max-width: 1360px;
    min-width: 0;
  }
  .browse.graph {
    grid-template-areas: 'head' 'graph' 'tree' 'info';
  }
  .b-head {
    grid-area: head;
    min-width: 0;
  }
  .b-article {
    grid-area: article;
    min-width: 0;
    display: grid;
    gap: var(--sp-8);
    align-content: start;
  }
  .b-graph {
    grid-area: graph;
    min-width: 0;
  }
  .b-info {
    grid-area: info;
    min-width: 0;
    align-self: start;
  }
  .b-tree {
    grid-area: tree;
    min-width: 0;
    align-self: start;
  }
  .browse[data-kind='none'] {
    grid-template-areas: 'head' 'info' 'article';
  }
  .browse[data-kind='none'].graph {
    grid-template-areas: 'head' 'graph' 'info';
  }
  /* two columns: the article, and a side column with the hierarchy above the facts */
  @container browse (min-width: 640px) {
    .browse {
      grid-template-columns: minmax(0, 1fr) 272px;
      grid-template-areas: 'head head' 'article tree' 'article info';
      grid-template-rows: auto auto 1fr;
    }
    .browse.graph {
      grid-template-areas: 'head head' 'graph graph' 'tree info';
      grid-template-rows: auto auto 1fr;
    }
    .browse[data-kind='none'] {
      grid-template-areas: 'head head' 'article info';
      grid-template-rows: auto 1fr;
    }
    .browse[data-kind='none'].graph {
      grid-template-areas: 'head head' 'graph graph' '. info';
    }
  }
  /* three columns: the hierarchy, the article and the facts */
  @container browse (min-width: 1040px) {
    .browse {
      grid-template-columns: 264px minmax(0, 1fr) 288px;
      grid-template-areas: 'tree head head' 'tree article info';
      grid-template-rows: auto 1fr;
      gap: var(--sp-6) var(--sp-8);
    }
    .browse.graph {
      grid-template-areas: 'tree head head' 'tree graph graph';
      grid-template-rows: auto 1fr;
    }
    .browse.graph .b-info {
      display: none;
    }
    .browse[data-kind='none'] {
      grid-template-columns: minmax(0, 1fr) 288px;
      grid-template-areas: 'head head' 'article info';
    }
    .browse[data-kind='none'].graph {
      grid-template-areas: 'head head' 'graph graph';
    }
    .b-tree {
      position: sticky;
      top: var(--gutter);
      max-height: calc(100vh - var(--topbar-h) - 2 * var(--gutter));
      overflow: auto;
    }
  }
  .tree-head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-bottom: var(--sp-3);
  }
  .tree-head h2 {
    font-size: var(--fs-md);
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--r);
    overflow: hidden;
    background: var(--surface);
  }
  .seg a {
    display: inline-flex;
    align-items: center;
    height: var(--ctl-h-sm);
    padding: 0 10px;
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--text-2);
    text-decoration: none;
  }
  .seg a[aria-current='true'] {
    background: var(--surface-3);
    color: var(--text);
  }
  details.ann summary {
    cursor: pointer;
    color: var(--text-2);
    font-weight: 500;
  }
  details.ann .table-scroll {
    margin-top: var(--sp-3);
  }
  details.ann th {
    font-weight: 500;
    color: var(--text-2);
    white-space: nowrap;
    vertical-align: top;
  }
  details.ann td a {
    color: var(--iri);
  }
  .iri-line {
    overflow-wrap: anywhere;
  }

  /* the landing page */
  .landing {
    padding: var(--gutter) var(--gutter) var(--sp-10);
    display: grid;
    gap: var(--sp-8);
    max-width: 1180px;
    min-width: 0;
  }
  .crumbs {
    font-size: var(--fs-sm);
    color: var(--text-3);
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    margin-bottom: var(--sp-2);
  }
  .title-row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    flex-wrap: wrap;
  }
  .term-title {
    font-size: var(--fs-display);
    line-height: 1.12;
    letter-spacing: -0.025em;
    font-weight: 600;
    margin: 0;
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .actions {
    margin-left: auto;
    display: flex;
    gap: var(--sp-2);
    flex-wrap: wrap;
  }
  .ids {
    display: flex;
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
    flex-wrap: wrap;
    margin-top: var(--sp-2);
  }
  .iri-text {
    font: 12px var(--font-mono);
    color: var(--iri);
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .lede {
    margin-top: var(--sp-5);
    font-size: 15px;
    line-height: 1.6;
    max-width: 68ch;
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--sp-6);
  }
  @container browse (min-width: 640px) {
    .cols {
      grid-template-columns: minmax(0, 1fr) 280px;
    }
  }
  @container browse (min-width: 1040px) {
    .cols {
      grid-template-columns: minmax(0, 1fr) 320px;
      gap: var(--sp-8);
    }
  }
  @container browse (max-width: 639px) {
    .actions {
      margin-left: 0;
      width: 100%;
    }
    .lede {
      font-size: 14px;
    }
  }
  .side {
    display: grid;
    gap: var(--gap);
    align-content: start;
    min-width: 0;
  }
  .counts,
  .about {
    display: grid;
    gap: 6px var(--sp-4);
    margin: 0;
  }
  .counts {
    grid-template-columns: minmax(0, 1fr) auto;
  }
  .counts dt {
    color: var(--text-2);
  }
  .counts dd {
    margin: 0;
    text-align: right;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
  .counts .sub {
    color: var(--text-3);
    font-weight: 400;
    font-size: var(--fs-sm);
  }
  .counts dt.sub {
    padding-left: var(--sp-3);
  }
  .about {
    grid-template-columns: 96px minmax(0, 1fr);
  }
  .about dt {
    color: var(--text-3);
    font-size: var(--fs-sm);
  }
  .about dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
</style>

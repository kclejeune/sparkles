<script lang="ts" module>
  import type { Term } from '$lib/api';
  /** A drawn node the overlay may validate. */
  export type OverlayNode = { id: string; term: Term; label: string };
</script>

<script lang="ts">
  /**
   * The validation overlay of Explore (docs/specs/U07-data-quality-and-schema-diagram.md
   * §11): validates the drawn nodes against the write-time guard, a shapes graph of the
   * dataset or the Validate panel's shapes, badges the nodes through GraphView, lists the
   * nodes with results, and opens a node's results with the triples involved and a
   * suggested fix that is previewed as a dry run before it can run.
   */
  import { untrack } from 'svelte';
  import * as api from '$lib/api';
  import { fmtInt } from '$lib/format';
  import { displayIri, displayTerm, termKey, type PrefixMap } from '$lib/rdf';
  import { load } from '$lib/storage';
  import { readShaclSyntax, shaclSyntaxKey } from '$lib/shacl';
  import { isAbort } from '$lib/supersede';
  import { hasPlaceholder, suggestFix, type FixGraph } from '$lib/fixes';
  import {
    badges as toBadges,
    countsText,
    fixGraph,
    focusable,
    groupResults,
    MARK,
    MAX_DRAWN,
    PANEL_LIMIT,
    previewUpdate,
    probeGuard,
    resolution,
    resultsByNode,
    validateNodes,
    type Badge,
    type GuardProbe,
    type OverlaySource,
    type VResult,
    type WritePreview,
  } from '$lib/validation-overlay';
  import Icon from './Icon.svelte';

  let {
    ds,
    items,
    prefixes,
    at = '',
    branch = '',
    paused = false,
    head = null,
    selectedId = null,
    onselect,
    onshowtriples,
    onopenquery,
    auto = false,
    live = $bindable(false),
    badges = $bindable(new Map()),
    marked = $bindable(null),
  }: {
    ds: string;
    /** The drawn nodes. */
    items: OverlayNode[];
    prefixes: PrefixMap;
    /** The state the canvas shows (`at=`); the head when empty. */
    at?: string;
    /** The branch the page is on, for the header. */
    branch?: string;
    /** The time slider is playing: no validation until it stops (U10 §4.3). */
    paused?: boolean;
    /** The head commit, so that a write validates the drawn nodes again. */
    head?: number | null;
    selectedId?: string | null;
    onselect?: (id: string) => void;
    /** Draw the triples of a result and return the elements to mark. */
    onshowtriples?: (r: VResult) => { nodes: string[]; edges: string[] };
    /** Open an update in the query page's editor. */
    onopenquery?: (text: string) => void;
    /** On by default: show and validate only when the dataset has a guard (§11.2). */
    auto?: boolean;
    /** Whether the overlay shows and validates. */
    live?: boolean;
    badges?: Map<string, Badge>;
    marked?: { nodes: string[]; edges: string[] } | null;
  } = $props();

  // --- the source -----------------------------------------------------------------
  let probe = $state<GuardProbe | null>(null);
  let probeError = $state<string | null>(null);
  type SourceKind = 'guard' | 'graph' | 'editor';
  let sourceKind = $state<SourceKind>('guard');
  let graphIri = $state('');
  let graphDraft = $state('');
  const editorShapes = $derived.by(() => {
    const text = load<string | null>(`sparkles.shacl.${ds}`, null);
    return text
      ? { shapes: text, syntax: readShaclSyntax(load<unknown>(shaclSyntaxKey(ds), null)) }
      : null;
  });

  /** Where the guard's results live, for suggested fixes (`choose` until known). */
  let guardGraph = $state<FixGraph>('choose');
  // the shapes-graph and editor sources validate the default graph
  const graphOfFix = $derived<FixGraph>(sourceKind === 'guard' ? guardGraph : null);

  $effect(() => {
    const name = ds;
    probe = null;
    probeError = null;
    guardGraph = 'choose';
    const ctl = new AbortController();
    probeGuard(name, ctl.signal)
      .then((p) => {
        if (ds !== name) return;
        probe = p;
        // without a guard the editor's shapes are the source (§11.2)
        if (p.state === 'none') sourceKind = editorShapes ? 'editor' : 'graph';
        else sourceKind = 'guard';
        // the guard's data graph, which a suggested fix writes (unknown: the person chooses)
        if (p.state === 'shacl' || p.state === 'shex')
          api
            .writeValidation(name, ctl.signal)
            .then((w) => {
              if (ds === name) guardGraph = fixGraph(w?.config.dataGraph, w?.unionDefaultGraph);
            })
            .catch(() => {
              if (ds === name) guardGraph = 'choose';
            });
      })
      .catch((e) => {
        if (!isAbort(e) && ds === name) probeError = api.errorMessage(e);
      });
    return () => ctl.abort();
  });

  const lang = $derived<'shacl' | 'shex'>(
    sourceKind === 'guard' && probe?.state === 'shex' ? 'shex' : 'shacl',
  );
  const source = $derived.by((): OverlaySource | null => {
    if (sourceKind === 'guard')
      return probe?.state === 'shacl' || probe?.state === 'shex' ? { kind: 'guard' } : null;
    if (sourceKind === 'graph') return graphIri ? { kind: 'graph', iri: graphIri } : null;
    return editorShapes ? { kind: 'editor', ...editorShapes } : null;
  });
  const available = $derived(
    probe != null && probe.state !== 'restricted' && probe.state !== 'unavailable',
  );
  const isLive = $derived(!auto || probe?.state === 'shacl' || probe?.state === 'shex');
  $effect(() => {
    live = isLive;
  });

  // --- validating ---------------------------------------------------------------------
  let results = $state<VResult[]>([]);
  let validating = $state(false);
  let progress = $state<{ done: number; total: number } | null>(null);
  let error = $state<string | null>(null);
  let validatedAt = $state<{ at: string; nodes: number } | null>(null);
  let ctl: AbortController | null = null;

  const drawn = $derived(new Set(items.map((n) => n.id)));
  const targets = $derived(focusable(items.map((n) => n.term)));
  const targetKey = $derived(targets.map(termKey).sort().join('\n'));

  async function run(src: OverlaySource, nodes: api.Term[], state: string) {
    ctl?.abort();
    ctl = new AbortController();
    const signal = ctl.signal;
    validating = true;
    error = null;
    progress = { done: 0, total: nodes.length };
    try {
      const rs = await validateNodes(ds, src, lang, nodes, {
        at: state || undefined,
        signal,
        onbatch: (so, done, total) => {
          if (signal.aborted) return;
          results = [...so];
          progress = { done, total };
        },
      });
      if (signal.aborted) return;
      results = rs;
      validatedAt = { at: state, nodes: nodes.length };
    } catch (e) {
      if (isAbort(e) || signal.aborted) return;
      results = [];
      error = api.errorMessage(e);
      if (e instanceof api.ApiError && e.status === 507)
        error = `The validation is over the server's budget, so the badges are off. ${error}`;
    } finally {
      if (!signal.aborted) {
        validating = false;
        progress = null;
      }
    }
  }

  // validate when the drawn nodes, the source, the state or the head change
  $effect(() => {
    void targetKey;
    void head;
    const src = source;
    const state = at;
    if (paused || !src || !available || !isLive) return;
    const nodes = untrack(() => targets);
    if (!nodes.length) {
      results = [];
      return;
    }
    // a short pause lets an expansion or a slider settle first
    const t = setTimeout(() => void run(src, nodes, state), 150);
    return () => clearTimeout(t);
  });
  $effect(() => () => ctl?.abort());

  const byNode = $derived(resultsByNode(results, drawn));
  $effect(() => {
    badges = toBadges(byNode);
  });
  const withResults = $derived(
    items.filter((n) => byNode.has(n.id)).sort((a, b) => a.label.localeCompare(b.label)),
  );

  // --- the results panel ------------------------------------------------------------
  let openId = $state<string | null>(null);
  const openNode = $derived(openId ? items.find((n) => n.id === openId) : undefined);
  const openResults = $derived(openId ? (byNode.get(openId) ?? []) : []);
  let panel: HTMLElement | undefined = $state();

  export function open(id: string) {
    openId = id;
    fix = null;
    queueMicrotask(() => panel?.querySelector<HTMLElement>('h3')?.focus());
  }
  function close() {
    openId = null;
    fix = null;
    marked = null;
  }
  $effect(() => {
    // the drawn node went away
    if (openId && !drawn.has(openId)) close();
  });

  function showTriples(r: VResult) {
    marked = onshowtriples?.(r) ?? null;
  }

  // --- a suggested fix ----------------------------------------------------------------
  let fix = $state<{
    r: VResult;
    text: string;
    preview: WritePreview | null;
    previewed: string;
    busy: boolean;
    error: string | null;
    ran: boolean;
  } | null>(null);

  async function suggest(r: VResult) {
    let values: api.Term[] | undefined;
    const tooMany =
      r.kind === 'maxCount' ||
      (r.kind === 'cardinality' && r.max != null && (r.count ?? 0) > r.max);
    if (tooMany && r.path && r.focus.type === 'uri') {
      try {
        const rows = await api.select(
          ds,
          `SELECT ?v WHERE { <${r.focus.value}> <${r.path}> ?v } LIMIT 50`,
          { at: at || undefined },
        );
        values = rows.flatMap((x) => (x.v ? [x.v] : []));
      } catch {
        values = undefined;
      }
    }
    const text = suggestFix(r, { prefixes, values, graph: graphOfFix });
    if (!text) return;
    fix = { r, text, preview: null, previewed: '', busy: false, error: null, ran: false };
  }

  const placeholder = $derived(fix ? hasPlaceholder(fix.text) : false);
  const fixResolution = $derived(fix?.preview ? resolution(fix.r, fix.preview) : null);
  const canRun = $derived(
    !!fix && !!fix.preview && fix.previewed === fix.text && fix.preview.outcome === 'commit' && !at,
  );

  async function preview() {
    if (!fix || placeholder) return;
    const f = fix;
    // the text sent is the text previewed, whatever the editor holds when the answer comes
    const sent = f.text;
    f.busy = true;
    f.error = null;
    try {
      f.preview = await previewUpdate(ds, sent);
      f.previewed = sent;
    } catch (e) {
      f.preview = null;
      f.error = api.errorMessage(e);
    } finally {
      f.busy = false;
    }
  }

  async function runFix() {
    if (!fix || !canRun) return;
    const f = fix;
    f.busy = true;
    f.error = null;
    try {
      await api.update(ds, f.text);
      f.ran = true;
      // validate the drawn nodes again, so a fixed badge goes away
      if (source && available) void run(source, targets, at);
    } catch (e) {
      f.error = api.errorMessage(e);
    } finally {
      f.busy = false;
    }
  }

  // --- keys: V opens the selected node's results, Shift+V selects the next node with one
  function onkeydown(e: KeyboardEvent) {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if ((e.target as HTMLElement)?.closest?.('input, textarea, select, [contenteditable]')) return;
    if (e.key === 'V' && e.shiftKey) {
      if (!withResults.length) return;
      e.preventDefault();
      const i = withResults.findIndex((n) => n.id === selectedId);
      const next = withResults[(i + 1) % withResults.length];
      onselect?.(next.id);
      return;
    }
    if (e.key === 'v' && !e.shiftKey && selectedId && byNode.has(selectedId)) {
      e.preventDefault();
      open(selectedId);
    }
  }

  // --- words ----------------------------------------------------------------------
  const short = (iri: string) => displayIri(iri, prefixes);
  const shapeName = (s: string) => (s.startsWith('_:') || s === 'START' ? s : short(s));
  const kindName = (r: VResult) => (r.lang === 'shacl' ? `sh:${r.kind}` : r.kind);
  const stateName = $derived(
    `${at ? `at ${at.replace(/^commit:/, 'commit ')}` : 'at the head'}${branch ? ` on ${branch}` : ''}`,
  );
  const hasSparql = $derived(lang === 'shacl' && results.some((r) => r.kind === 'sparql'));
  function useGraph(e: SubmitEvent) {
    e.preventDefault();
    graphIri = graphDraft.trim();
  }
  const OUTCOME: Record<string, string> = {
    commit: 'The update would commit.',
    'no-change': 'The update would change nothing.',
    rejected: 'The write-time guard would reject the update.',
    'storage-refused': 'The storage limit would refuse the update.',
    'precondition-failed': 'A precondition would fail.',
  };
</script>

<svelte:window {onkeydown} />

{#if isLive}
  <section class="vo" aria-label="Validation overlay">
    {#if probe?.state === 'restricted' || probe?.state === 'unavailable'}
      <p class="faint small" role="status">{probe.message}.</p>
    {:else if probeError}
      <p class="danger small" role="alert">{probeError}</p>
    {:else if !probe}
      <p class="faint small"><span class="spinner"></span> Looking for the dataset's shapes…</p>
    {:else}
      <div class="vo-head">
        <label class="row small">
          Shapes
          <select bind:value={sourceKind} aria-label="Validation source">
            {#if probe.state === 'shacl' || probe.state === 'shex'}
              <option value="guard"
                >Write-time guard ({probe.state === 'shex' ? 'ShEx' : 'SHACL'})</option
              >
            {/if}
            <option value="graph">A shapes graph</option>
            {#if editorShapes}<option value="editor">The Validate panel's shapes</option>{/if}
          </select>
        </label>
        {#if sourceKind === 'graph'}
          <form class="row small" onsubmit={useGraph}>
            <input
              class="mono"
              placeholder="http://example.org/shapes"
              aria-label="Shapes graph IRI"
              bind:value={graphDraft}
            />
            <button class="btn sm" type="submit">Use</button>
          </form>
        {/if}
        <span class="spacer"></span>
        <span class="small faint" role="status" aria-live="polite">
          {#if paused}
            Paused while the history plays.
          {:else if validating && progress}
            <span class="spinner"></span> Validating {fmtInt(progress.done)} of {fmtInt(
              progress.total,
            )} nodes…
          {:else if !source}
            {sourceKind === 'graph'
              ? 'Name a shapes graph of the dataset.'
              : 'No shapes to validate with.'}
          {:else if validatedAt}
            {fmtInt(withResults.length)} of {fmtInt(validatedAt.nodes)} nodes have results, {stateName}.
          {/if}
        </span>
      </div>
      {#if error}<p class="danger small" role="alert">{error}</p>{/if}
      {#if targets.length >= MAX_DRAWN}
        <p class="faint small">The overlay validates the first {fmtInt(MAX_DRAWN)} drawn nodes.</p>
      {/if}
      {#if hasSparql}
        <p class="faint small">
          The overlay validates the drawn nodes as focus nodes. A SHACL-SPARQL constraint that is
          not anchored at its focus node can give other results in a full validation.
        </p>
      {/if}
      {#if withResults.length}
        <details class="vo-list">
          <summary class="small">Nodes with results ({fmtInt(withResults.length)})</summary>
          <table class="small">
            <thead><tr><th scope="col">Node</th><th scope="col">Validation</th></tr></thead>
            <tbody>
              {#each withResults as n (n.id)}
                {@const b = badges.get(n.id)}
                <tr class:sel={n.id === selectedId}>
                  <td>
                    <button class="linkish" onclick={() => (onselect?.(n.id), open(n.id))}
                      >{n.label}</button
                    >
                  </td>
                  <td class={b?.worst}>{b ? `${MARK[b.worst]} ${b.words}` : ''}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </details>
      {/if}
    {/if}
  </section>
{/if}

{#if openNode && isLive}
  <div
    class="vo-panel"
    bind:this={panel}
    role="dialog"
    aria-label="Validation results of {openNode.label}"
  >
    <div class="vo-panel-head">
      <h3 tabindex="-1">{openNode.label} · {countsText(openResults)}</h3>
      <button class="btn sm ghost icon" aria-label="Close the results" onclick={close}
        ><Icon name="x" size={13} /></button
      >
    </div>
    <p class="faint small">{lang === 'shex' ? 'ShEx' : 'SHACL'}, {stateName}.</p>
    {#each groupResults(openResults.slice(0, PANEL_LIMIT)) as g (g.shape + g.kind)}
      <section class="vo-group">
        <h4 class="small">{shapeName(g.shape)} · {kindName(g.results[0])}</h4>
        {#each g.results as r, i (i)}
          <div class="vo-result {r.severity}">
            <div class="small">
              <span class="sev">{MARK[r.severity]} {r.severity}</span>
              {#if r.path}· path <span class="mono">{short(r.path)}</span>{:else if r.pathText}·
                path
                <span class="mono">{r.pathText}</span>{/if}
              {#if r.value}· value <span class="mono">{displayTerm(r.value, prefixes)}</span
                >{:else if r.kind === 'minCount' || (r.kind === 'cardinality' && (r.count ?? 0) < (r.min ?? 0))}·
                no value{/if}
              {#if r.constraint}· <span class="mono">{r.constraint}</span>{/if}
            </div>
            {#each r.messages as m, mi (mi)}<div class="small faint msg">{m}</div>{/each}
            <div class="row vo-actions">
              <button class="btn sm" onclick={() => showTriples(r)}>Show triples</button>
              {#if suggestFix(r, { prefixes })}
                <button class="btn sm" onclick={() => void suggest(r)}>Suggest fix</button>
              {/if}
            </div>
          </div>
        {/each}
      </section>
    {/each}
    {#if openResults.length > PANEL_LIMIT}
      <p class="faint small">And {fmtInt(openResults.length - PANEL_LIMIT)} more results.</p>
    {/if}

    {#if fix}
      <section class="vo-fix" aria-label="Suggested fix">
        <h4 class="small">Suggested fix</h4>
        <textarea
          class="mono"
          rows={Math.min(14, fix.text.split('\n').length + 1)}
          aria-label="Suggested update"
          readonly={fix.busy}
          bind:value={fix.text}></textarea>
        {#if placeholder}
          <p class="faint small">Fill in the placeholders, such as ?value, before the preview.</p>
        {/if}
        <div class="row vo-actions">
          <button class="btn sm primary" disabled={placeholder || fix.busy} onclick={preview}>
            {#if fix.busy}<span class="spinner"></span>{/if} Preview
          </button>
          <button
            class="btn sm"
            disabled={!canRun || fix.busy}
            title={at
              ? 'Return the canvas to the head to run an update'
              : 'Preview the update first'}
            onclick={runFix}>Run</button
          >
          {#if onopenquery}
            <button class="btn sm ghost" onclick={() => onopenquery?.(fix!.text)}
              >Open in Query</button
            >
          {/if}
        </div>
        {#if fix.error}<p class="danger small" role="alert">{fix.error}</p>{/if}
        {#if fix.preview}
          {@const p = fix.preview}
          <div class="small vo-preview" role="status">
            <p>{OUTCOME[p.outcome] ?? p.outcome}{p.error ? ` ${p.error}` : ''}</p>
            {#if p.commit}
              <p>{fmtInt(p.commit.inserted)} added, {fmtInt(p.commit.deleted)} removed.</p>
            {/if}
            {#if p.changes?.quads.length}
              <ul class="mono">
                {#each p.changes.quads as q, qi (qi)}
                  <li class={q.op === '+' ? 'added' : 'removed'}>
                    {q.op}
                    {q.subject}
                    {q.predicate}
                    {q.object}
                  </li>
                {/each}
              </ul>
            {/if}
            <p class="resolution">
              {#if fixResolution === 'resolved'}
                The guard would resolve this result.
              {:else if fixResolution === 'remains'}
                The guard would still report this result.
              {:else}
                The preview does not say whether this result would be resolved. The overlay
                validates again after the update runs.
              {/if}
            </p>
            {#if fix.previewed !== fix.text}<p class="faint">
                The text changed since the preview.
              </p>{/if}
          </div>
        {/if}
        {#if fix.ran}<p class="ok small" role="status">
            The update ran. The overlay validated the drawn nodes again.
          </p>{/if}
      </section>
    {/if}
  </div>
{/if}

<style>
  .vo {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
  }
  .vo-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .vo-head input {
    width: 22ch;
  }
  .spacer {
    flex: 1;
  }
  .vo-list table {
    border-collapse: collapse;
    width: 100%;
    max-width: 520px;
  }
  .vo-list th,
  .vo-list td {
    text-align: left;
    padding: 2px 6px;
    border-bottom: 1px solid var(--border);
  }
  .vo-list tr.sel td {
    background: var(--spark-soft);
  }
  .vo-list td.violation {
    color: var(--danger);
  }
  .vo-list td.warning {
    color: var(--warn);
  }
  .linkish {
    background: none;
    border: 0;
    padding: 0;
    color: var(--iri);
    cursor: pointer;
    font: inherit;
    text-align: left;
  }
  .vo-panel {
    position: absolute;
    right: 10px;
    top: 48px;
    max-height: calc(100% - 58px);
    width: min(340px, calc(100% - 20px));
    overflow: auto;
    z-index: 5;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-pop);
    padding: 10px 12px;
  }
  .vo-panel-head {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .vo-panel-head h3 {
    flex: 1;
    margin: 0;
    font-size: var(--fs-md);
    overflow-wrap: anywhere;
  }
  .vo-group h4 {
    margin: 10px 0 4px;
  }
  .vo-result {
    border-left: 3px solid var(--danger);
    padding: 4px 8px;
    margin-bottom: 6px;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    overflow-wrap: anywhere;
  }
  .vo-result.warning {
    border-left-color: var(--warn);
  }
  .vo-result.info {
    border-left-color: var(--text-3);
  }
  .sev {
    font-weight: 600;
  }
  .msg {
    margin-top: 2px;
  }
  .vo-actions {
    gap: 6px;
    margin-top: 4px;
    flex-wrap: wrap;
  }
  .vo-fix textarea {
    width: 100%;
    box-sizing: border-box;
    font-size: var(--fs-xs);
  }
  .vo-preview ul {
    padding-left: 0;
    list-style: none;
    font-size: var(--fs-xs);
    overflow-wrap: anywhere;
  }
  .vo-preview li.added {
    color: var(--added);
  }
  .vo-preview li.removed {
    color: var(--removed);
  }
  .danger {
    color: var(--danger);
  }
  .ok {
    color: var(--ok);
  }
  @media (max-width: 640px) {
    .vo-panel {
      position: fixed;
      left: 0;
      right: 0;
      top: auto;
      bottom: 0;
      width: auto;
      max-height: 70vh;
      border-radius: var(--r-lg) var(--r-lg) 0 0;
    }
  }
</style>

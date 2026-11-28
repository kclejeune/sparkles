<script lang="ts">
  import { untrack } from 'svelte';
  import { ClassificationFrame } from '$lib/classification-frame.svelte';
  import {
    CLASSIFICATION_DISPLAY_CAP,
    keptAvailability,
    type ClassificationName,
    type ClassificationFramePart as ClassificationPart,
    type ClassificationExcluded,
    type ClassificationSourceRow,
  } from '$lib/classification-read';
  import { fmtCount } from '$lib/format';
  import { iriForm } from '$lib/iri-form.svelte';
  import { consistencyInfo, OWL_THING, type AxiomItem, type ClassifyOptions } from '$lib/ontology';
  import { ClassifyOptionsState } from '$lib/explicit-classification.svelte';
  import type { InferredPins } from '$lib/inferred-read';
  import MaterializedFrame from './MaterializedFrame.svelte';

  let {
    target,
    graph,
    imports = true,
    mode = 'lenient',
    iri,
    commit,
    items,
    onopen,
    href,
    pins,
    classifyOptions,
  }: {
    target: string;
    graph: string | null;
    imports?: boolean;
    mode?: 'lenient' | 'strict';
    iri: string;
    commit: number | null;
    items: AxiomItem[];
    onopen?: (iri: string) => void;
    href?: (iri: string) => string | null;
    pins?: InferredPins;
    classifyOptions?: ClassifyOptions;
  } = $props();
  const model = new ClassificationFrame(
    () => pins?.classificationId,
    (result) => pins?.acceptClassification(result.resultId) ?? true,
  );
  let materialized = $state(false);
  const opts = new ClassifyOptionsState(
    untrack(() => target),
    untrack(() => graph),
    untrack(() => classifyOptions),
  );
  $effect(() => {
    opts.sync(target, graph, classifyOptions);
  });
  function runClassify() {
    if (opts.parsedDeadline.error) return;
    materialized = false;
    void model.classify(opts.requestOptions);
  }
  // The read has pinned labels but no proven historical prefix map. Rendering
  // preferences therefore fall back to the complete IRI outside label mode.
  const display = (row: ClassificationName) =>
    iriForm.mode === 'label' ? (row.label ?? row.iri) : row.iri;
  $effect(() => {
    const view =
      commit == null
        ? null
        : {
            target,
            selection: { graph, imports },
            mode,
            iri,
            commit,
            languages: [...iriForm.languages],
          };
    materialized = false;
    untrack(() => void model.reset(view));
  });
  $effect(() => () => model.drop());
  const asserted = $derived.by(() => {
    const parents = new Set<string>();
    const equivalents = new Set<string>();
    for (const { axiom } of items) {
      if (
        axiom.type === 'SubClassOf' &&
        axiom.subClass === iri &&
        typeof axiom.superClass === 'string'
      )
        parents.add(axiom.superClass);
      if (
        axiom.type === 'EquivalentClasses' &&
        Array.isArray(axiom.classes) &&
        axiom.classes.includes(iri)
      )
        for (const value of axiom.classes) if (typeof value === 'string') equivalents.add(value);
    }
    return { parents, equivalents };
  });
  const parents = $derived(
    (model.parts.parents.items as ClassificationName[]).filter(
      (n) => n.iri !== iri && n.iri !== OWL_THING && !asserted.parents.has(n.iri),
    ),
  );
  const equivalents = $derived(
    (model.parts.equivalents.items as ClassificationName[]).filter(
      (n) => n.iri !== iri && !asserted.equivalents.has(n.iri),
    ),
  );
  const clauses = $derived([
    { part: 'parents' as const, title: 'SubClassOf', rows: parents },
    { part: 'equivalents' as const, title: 'EquivalentTo', rows: equivalents },
  ]);
  const axiomPage = $derived(model.parts.axioms);
  const result = $derived(model.reply?.result);
  const outcome = $derived(model.reply?.outcome);
  const consistency = $derived(
    outcome ? consistencyInfo(outcome.consistency, undefined, outcome.partial) : null,
  );
  const finished = $derived(
    ['parents', 'equivalents'].every((p) => {
      const page = model.parts[p as ClassificationPart];
      return page.loaded && !page.loading && !page.error && page.total === page.items.length;
    }),
  );
</script>

{#snippet classifyForm(buttonLabel: string)}
  <div class="classify-controls">
    <label
      class="check"
      title="Return proved subsumptions if the deadline stops a disjunctive classification"
    >
      <input type="checkbox" bind:checked={opts.partial} />
      Allow partial
    </label>
    <label class="field">
      <span>Deadline (s)</span>
      <input
        class="input sm"
        type="text"
        inputmode="decimal"
        size="12"
        placeholder="Server default"
        aria-label="Frame classification deadline in seconds"
        aria-invalid={!!opts.parsedDeadline.error}
        bind:value={opts.deadline}
      />
    </label>
    <label class="field">
      <span>Expansion</span>
      <select class="select sm" aria-label="Frame expansion strategy" bind:value={opts.expansion}>
        <option value="cautious">Cautious</option>
        <option value="eager">Eager</option>
      </select>
    </label>
    <button
      class="btn sm ghost"
      disabled={model.computing || model.loading || !!opts.parsedDeadline.error}
      onclick={runClassify}>{buttonLabel}</button
    >
  </div>
  {#if opts.parsedDeadline.error}<p class="note error" role="alert">
      {opts.parsedDeadline.error}
    </p>{/if}
{/snippet}

<section class="classified-frame" aria-label="Inferred class axioms" aria-live="polite">
  <div class="heading">
    <h3>Inferred class axioms</h3>
    <button
      class="btn sm ghost"
      disabled={commit == null || model.loading || model.computing}
      onclick={() => {
        materialized = false;
        void model.refresh();
      }}>Read again</button
    >
  </div>
  {#if commit == null}
    <p class="note">Waiting for the displayed source commit.</p>
  {:else if model.loading}<p class="note">
      <span class="spinner"></span> Reading kept classification…
    </p>
  {:else if model.computing}<p class="note">
      <span class="spinner"></span> Classifying the displayed ontology…
    </p>{/if}
  {#if model.error}<p class="note error" role="alert">
      Could not read classification: {model.error}
    </p>{/if}
  {#if model.reply?.status === 'running'}
    <p class="note">Classification is running for this source. Read again when it finishes.</p>
  {:else if model.reply?.status === 'unavailable' || model.reply?.status === 'mismatch'}
    <p class="note">
      {keptAvailability(model.reply)}
    </p>
    {@render classifyForm('Classify')}
    {#if materialized}
      <MaterializedFrame
        {target}
        {graph}
        {imports}
        {mode}
        {iri}
        {commit}
        {items}
        {pins}
        {onopen}
        {href}
        kind="Class"
      />
    {:else}
      <button class="btn sm ghost" onclick={() => (materialized = true)}>
        Read fresh materialized axioms
      </button>
    {/if}
  {:else if result && outcome}
    <div class="status-row" role="status">
      <span class="status-badge">{outcome.tier.toUpperCase()}</span>
      {#if outcome.partial}
        <span class="status-badge warn"
          >Partial · {fmtCount(outcome.incompleteCount, 'open class', 'open classes')}</span
        >
      {/if}
      {#if consistency}
        <span
          class="status-badge"
          class:ok={consistency.tone === 'ok'}
          class:warn={consistency.tone === 'warn'}
          class:danger={consistency.tone === 'danger'}>{consistency.label}</span
        >
      {/if}
      {#if model.reply?.entity?.incomplete}
        <span class="status-badge warn">Open class</span>
      {/if}
    </div>
    <p class="provenance">
      {model.isolated ? 'Scoped classification' : 'Kept classification'} · Classified commit {result.classifiedCommit}
      · {outcome.tier.toUpperCase()} · {outcome.consistency}
    </p>
    {#if model.reply?.activity}<p class="note">
        A reasoning run is active for this commit. These clauses come from the completed pinned
        result.
      </p>{/if}
    {#if result.sourceCounts.warnings > 0}<p class="note">
        Source resolution has {result.sourceCounts.warnings} warnings. This result covers the resolved
        inputs; read Sources and warnings for details.
      </p>{/if}
    {#if outcome.consistency === 'inconsistent'}
      <p class="note error" role="alert">
        The ontology is inconsistent. Normal superclass and equivalence clauses are not displayed.
      </p>
    {:else}
      {#if model.reply?.entity?.unsatisfiable}<p class="note error">
          This class is unsatisfiable.
        </p>{/if}
      {#if outcome.consistency === 'unknown'}<p class="note warn" role="status">
          {outcome.partial
            ? 'Consistency is unknown because the classification stopped before deciding it.'
            : 'Consistency is unknown.'}
        </p>{/if}
      {#if model.reply?.entity?.incomplete}<p class="note warn" role="status">
          {outcome.partial
            ? 'This class is left open in this partial classification and may miss inferred clauses.'
            : outcome.excludedCount > 0 || outcome.unmappedCount > 0
              ? 'This class is incomplete because the classification excluded or could not map part of the input, and may miss inferred clauses.'
              : 'This class is incomplete and may miss inferred clauses.'}
        </p>{/if}
      {#if model.reply?.entity?.present === false}<p class="note">
          The classification has no named-class node for this entity.
        </p>{/if}
      {#each clauses as { part: key, title, rows }}
        {@const page = model.parts[key]}
        {#if rows.length}
          <div
            class="clause"
            data-section={key === 'parents' ? 'inferred-sub' : 'inferred-equivalent'}
          >
            <strong>{title}:</strong>
            <ul>
              {#each rows as row (row.iri)}
                <li>
                  {#if href?.(row.iri)}<a class="iri-link" href={href(row.iri)} title={row.iri}
                      >{display(row)}</a
                    >{:else if onopen}<button
                      class="iri-link"
                      title={row.iri}
                      onclick={() => onopen?.(row.iri)}>{display(row)}</button
                    >{:else}<span title={row.iri}>{display(row)}</span>{/if}
                  <span class="badge">Inferred</span>
                  {#if (row.equivalenceCount ?? 0) > 1}<span class="note"
                      >parent group of {row.equivalenceCount} equivalent classes</span
                    >{/if}
                </li>
              {/each}
            </ul>
          </div>
        {/if}
        {#if page.error}<p class="note error" role="alert">
            Could not read {key}: {page.error}
          </p>{/if}
        {#if page.loading}<p class="note">Reading {key}…</p>{/if}
        {#if page.total > page.items.length}<p class="note">
            Read {page.items.length} of {page.total}
            {key}. {#if model.isolated}Further pages are unavailable for this isolated result.{:else if page.items.length >= CLASSIFICATION_DISPLAY_CAP}The
              display limit is {CLASSIFICATION_DISPLAY_CAP} rows.{/if}
          </p>{/if}
        {#if !model.isolated && (page.error || page.next != null) && page.items.length < CLASSIFICATION_DISPLAY_CAP}
          <button class="btn sm ghost" disabled={page.loading} onclick={() => void model.more(key)}
            >{page.error ? 'Retry' : 'Show more'} {key}</button
          >
        {/if}
      {/each}
      {#if model.reply?.entity?.present && outcome.complete && !outcome.partial && outcome.consistency === 'consistent' && finished && !parents.length && !equivalents.length}
        <p class="note">No additional direct named-class clauses beyond the asserted frame.</p>
      {/if}
    {/if}
    {#if !model.isolated && model.reply?.entity?.present && outcome.consistency !== 'inconsistent'}
      <details>
        <summary>Kept taxonomy axioms</summary>
        <p class="note">
          These axioms mention the selected IRI. Subclass edges name the representatives of their
          equivalence groups.
        </p>
        {#each ['SubClassOf', 'EquivalentClasses'] as type}
          {@const rows = (axiomPage.items as AxiomItem[]).filter((item) => item.type === type)}
          {#if rows.length}
            <h4>{type}</h4>
            <ul>
              {#each rows as row (row.ofn)}
                <li><code>{row.ofn}</code> <span class="badge">Inferred</span></li>
              {/each}
            </ul>
          {/if}
        {/each}
        {#if axiomPage.error}
          <p class="note error" role="alert">Could not read taxonomy axioms. {axiomPage.error}</p>
        {/if}
        {#if axiomPage.loaded}
          <p class="note">
            Read {axiomPage.items.length} of {axiomPage.total} taxonomy axioms.
            {#if axiomPage.next != null && axiomPage.items.length >= CLASSIFICATION_DISPLAY_CAP}
              The display limit is {CLASSIFICATION_DISPLAY_CAP} rows.
            {/if}
          </p>
        {/if}
        {#if (!axiomPage.loaded || axiomPage.next != null || axiomPage.error) && axiomPage.items.length < CLASSIFICATION_DISPLAY_CAP}
          <button
            class="btn sm ghost"
            disabled={axiomPage.loading}
            onclick={() => void model.more('axioms')}
          >
            {axiomPage.error ? 'Retry' : axiomPage.loaded ? 'Show more' : 'Read'} taxonomy axioms
          </button>
        {/if}
      </details>
    {/if}
    {#if outcome.partial}<p class="note warn" role="status">
        This is a partial classification ({outcome.tier.toUpperCase()}). It leaves
        {fmtCount(outcome.incompleteCount, 'class', 'classes')} open and can miss clauses for them.
      </p>{/if}
    {#if !outcome.complete && outcome.excludedCount > 0}<p class="note">
        The {outcome.tier.toUpperCase()} classification omitted or partly used {outcome.excludedCount}
        axioms and can miss clauses.
      </p>{/if}
    {#if !outcome.complete && outcome.unmappedCount > 0}<p class="note">
        {outcome.unmappedCount}
        {outcome.unmappedCount === 1 ? 'triple' : 'triples'} of the input map to no axiom, so the classification
        can miss clauses.
      </p>{/if}
    {#if model.isolated}<p class="note">
        This explicit result belongs to this view and is not retained for shared reads.
      </p>{/if}
    <details class="reclassify">
      <summary>Classify with deadline or partial options</summary>
      {@render classifyForm('Classify')}
    </details>
    <details>
      <summary>Classification source and coverage</summary>
      <p class="note">
        Direct named-class parents, equivalences and unsatisfiability only. Classification
        justifications are unavailable.
      </p>
      <dl class="source">
        <dt>Input fingerprint</dt>
        <dd><code>{result.inputFingerprint}</code></dd>
        <dt>Content fingerprint</dt>
        <dd><code>{result.contentFingerprint}</code></dd>
        <dt>Source fingerprint</dt>
        <dd><code>{result.sourceFingerprint}</code></dd>
        <dt>Producer</dt>
        <dd>
          {result.assertions ? 'With assertions' : 'Terminology'} · {result.elOnly
            ? 'EL only'
            : 'Automatic tier'} · {result.expansion} expansion · {result.partialRequested
            ? 'Partial answers requested'
            : 'Complete run requested'}
        </dd>
      </dl>
      {#each ['sources', 'excluded'] as part}
        {@const key = part as 'sources' | 'excluded'}
        {@const page = model.parts[key]}
        <h4>{key === 'sources' ? 'Sources and warnings' : 'Excluded axioms'}</h4>
        {#if key === 'sources'}<p class="note">
            {result.sourceCounts.requestedGraphs} requested graphs, {result.sourceCounts
              .resolvedGraphs} resolved graphs, {result.sourceCounts.imports} imports, {result
              .sourceCounts.warnings} warnings.
          </p>{/if}
        {#if page.loaded}
          <ul>
            {#each page.items as row, index (index)}<li>
                {#if key === 'sources'}
                  {@const source = row as ClassificationSourceRow}
                  {#if source.kind === 'warning'}Warning: {source.message}{:else if source.kind === 'import'}Import
                    {source.iri} → {source.graph ?? 'unresolved'}{source.location
                      ? ` (${source.location})`
                      : ''}{:else}{source.kind === 'requestedGraph' ? 'Requested' : 'Resolved'} graph
                    {source.graph}{/if}
                {:else}
                  {@const exclusion = row as ClassificationExcluded}<code>{exclusion.ofn}</code>
                  <p class="note">
                    {exclusion.gap.kind === 'omitted'
                      ? `${exclusion.gap.rule}: ${exclusion.gap.message}`
                      : exclusion.gap.reason}
                  </p>
                {/if}
              </li>{/each}
          </ul>
          <p class="note">
            Read {page.items.length} of {page.total} rows.{#if model.isolated && page.total > page.items.length}
              Further isolated pages are unavailable.{:else if page.items.length >= CLASSIFICATION_DISPLAY_CAP && page.total > page.items.length}
              Display limit reached.{/if}
          </p>
        {/if}
        {#if page.error}<p class="note error" role="alert">
            Could not read {key}: {page.error}
          </p>{/if}
        {#if !model.isolated && (!page.loaded || page.error || page.next != null) && page.items.length < CLASSIFICATION_DISPLAY_CAP}
          <button class="btn sm ghost" disabled={page.loading} onclick={() => void model.more(key)}
            >{page.error ? 'Retry' : page.loaded ? 'Show more' : 'Read'} {key}</button
          >
        {/if}
      {/each}
    </details>
  {:else if commit != null && !model.loading && !model.computing && model.error}
    {@render classifyForm('Classify')}
  {/if}
</section>

<style>
  .classified-frame {
    padding-top: 1rem;
    border-top: 1px solid var(--border);
    margin-top: 1rem;
  }
  .heading {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
  }
  h3,
  h4 {
    margin: 0.5rem 0;
  }
  .note,
  .provenance {
    color: var(--text-2);
    font-size: 0.85rem;
  }
  .error {
    color: var(--danger);
  }
  .warn {
    color: var(--warn);
  }
  .status-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin: 0.4rem 0 0.25rem;
  }
  .status-badge {
    font-size: 0.72rem;
    padding: 1px 7px;
    border-radius: 999px;
    background: var(--surface-2, var(--hover));
    color: var(--text-2);
    font-weight: 500;
  }
  .status-badge.ok {
    color: var(--ok);
    background: var(--ok-soft);
  }
  .status-badge.warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
  }
  .status-badge.danger {
    color: var(--danger);
    background: var(--danger-soft);
  }
  .classify-controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem 0.75rem;
    margin: 0.4rem 0;
    font-size: 0.8rem;
  }
  .classify-controls .check,
  .classify-controls .field {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    color: var(--text-2);
  }
  .reclassify {
    margin: 0.5rem 0;
    font-size: 0.85rem;
  }
  .clause {
    margin-block: 0.75rem;
  }
  ul {
    padding-left: 1.25rem;
  }
  li {
    margin-block: 0.3rem;
    overflow-wrap: anywhere;
  }
  .iri-link {
    all: unset;
    color: var(--iri);
    cursor: pointer;
  }
  .iri-link:hover {
    text-decoration: underline;
  }
  .iri-link:focus-visible {
    box-shadow: var(--focus);
  }
  .badge {
    color: var(--text-2);
    font-size: 0.7rem;
    margin-left: 0.4rem;
  }
  .source {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 0.4rem 1rem;
    font-size: 0.8rem;
  }
  .source dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
</style>

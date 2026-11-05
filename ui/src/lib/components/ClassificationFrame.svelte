<script lang="ts">
  import { untrack } from 'svelte';
  import { ClassificationFrame } from '$lib/classification-frame.svelte';
  import {
    CLASSIFICATION_DISPLAY_CAP,
    type ClassificationName,
    type ClassificationPart,
    type ClassificationExcluded,
    type ClassificationSourceRow,
  } from '$lib/classification-read';
  import { iriForm } from '$lib/iri-form.svelte';
  import { OWL_THING, type AxiomItem } from '$lib/ontology';

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
  } = $props();
  const model = new ClassificationFrame();
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
  const result = $derived(model.reply?.result);
  const outcome = $derived(model.reply?.outcome);
  const finished = $derived(
    ['parents', 'equivalents'].every((p) => {
      const page = model.parts[p as ClassificationPart];
      return page.loaded && !page.loading && !page.error && page.total === page.items.length;
    }),
  );
</script>

<section class="classified-frame" aria-label="Inferred class axioms" aria-live="polite">
  <div class="heading">
    <h3>Inferred class axioms</h3>
    <button
      class="btn sm ghost"
      disabled={commit == null || model.loading || model.computing}
      onclick={() => void model.refresh()}>Read again</button
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
      {model.reply.status === 'mismatch'
        ? 'The kept classification does not match this source or result pin.'
        : model.reply.reason === 'unsupportedScope'
          ? 'This view does not share a kept classification.'
          : 'No kept classification is available for this source.'}
      <button class="btn sm ghost" disabled={model.computing} onclick={() => void model.classify()}
        >Classify</button
      >
    </p>
  {:else if result && outcome}
    <p class="provenance">
      Classified commit {result.classifiedCommit} · {outcome.tier.toUpperCase()} · {outcome.consistency}
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
    {#if outcome.partial}<p class="note">
        This is a partial classification. It can miss clauses for {outcome.incompleteCount} incomplete
        classes.
      </p>{/if}
    {#if !outcome.complete && !outcome.partial}<p class="note">
        The {outcome.tier.toUpperCase()} classification omitted or partly used {outcome.excludedCount}
        axioms and can miss clauses.
      </p>{/if}
    {#if model.isolated}<p class="note">
        This explicit result belongs to this view and is not retained for shared reads.
      </p>{/if}
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

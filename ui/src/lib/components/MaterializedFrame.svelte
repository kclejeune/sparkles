<script lang="ts">
  import { untrack } from 'svelte';
  import { MaterializedFrame } from '$lib/materialized-frame.svelte';
  import { INFERRED_GRAPH, type InferredPins } from '$lib/inferred-read';
  import { iriForm } from '$lib/iri-form.svelte';
  import type { AxiomItem, EntityKind, ManchesterToken } from '$lib/ontology';
  import { displayName } from '$lib/browse';
  import { materializedClauses } from '$lib/materialized-clauses';
  import type { PrefixMap } from '$lib/rdf';
  import MaterializationNotice from './MaterializationNotice.svelte';
  let {
    target,
    graph,
    imports = true,
    kind,
    iri,
    commit,
    items,
    pins,
    prefixes = {},
    href,
    onopen,
  }: {
    target: string;
    graph: string | null;
    imports?: boolean;
    kind: EntityKind;
    iri: string;
    commit: number | null;
    items: AxiomItem[];
    pins?: InferredPins;
    /** The page's prefixes. Without them names fall back to labels and complete IRIs. */
    prefixes?: PrefixMap;
    href?: (iri: string) => string | null;
    onopen?: (iri: string) => void;
  } = $props();
  const model = new MaterializedFrame();
  $effect(() => {
    const view =
      commit == null
        ? null
        : { target, selection: { graph, imports }, iri, commit, languages: [...iriForm.languages] };
    const capturedPins = pins;
    untrack(() => model.reset(view, capturedPins));
  });
  $effect(() => () => model.drop());
  const asserted = $derived(
    new Set(items.filter((item) => item.graph !== INFERRED_GRAPH).map((item) => item.ofn)),
  );
  const inferred = $derived(
    model.page?.axioms.filter((item) => item.graph === INFERRED_GRAPH) ?? [],
  );
  // Labels come from the same pinned read as the rows, so a name never mixes sources.
  const name = (i: string) => displayName(i, model.page?.names, prefixes);
  const frame = $derived(materializedClauses(kind, iri, inferred, asserted, name, prefixes));
  const listed = $derived(
    frame.clauses.reduce((n, c) => n + c.rows.length, 0) + frame.other.length,
  );
</script>

{#snippet tokens(
  ts: ManchesterToken[],
)}{#each ts as t, i (i)}{#if t.role === 'name' && t.iri}{@const to =
        t.iri}{#if to !== iri && href?.(to)}<a class="iri-link" href={href(to)} title={to}
          >{name(to)}</a
        >{:else if to !== iri && onopen}<button
          class="iri-link"
          title={to}
          onclick={() => onopen?.(to)}>{name(to)}</button
        >{:else}<span title={to}>{name(to)}</span>{/if}{:else if t.role === 'kw'}<span class="kw"
        >{t.text}</span
      >{:else}{t.text}{/if}{/each}{/snippet}

<section class="materialized-frame" aria-label="Materialized inferred axioms" aria-live="polite">
  <div class="heading">
    <h3>Materialized inferred axioms</h3>
    <button
      class="btn sm ghost"
      disabled={commit == null || model.loading}
      onclick={() => void model.refresh()}>Read again</button
    >
  </div>
  {#if commit == null}<p>Waiting for the displayed source commit.</p>
  {:else if model.loading}<p>Reading materialized facts…</p>{/if}
  {#if model.error}<p class="error-box" role="alert">
      Could not read materialized facts: {model.error}
    </p>
    <p>A matching fresh materialization must already exist. This read does not run reasoning.</p>
  {/if}
  {#if model.page?.inferenceSource}
    <MaterializationNotice source={model.page.inferenceSource} />
    <p>These rows come from <code>{INFERRED_GRAPH}</code> and are read only.</p>
    {#each frame.clauses as clause (clause.id)}
      <div class="clause" data-section={`materialized-${clause.id}`}>
        <strong>{clause.word}:</strong>
        <ul>
          {#each clause.rows as row, i (row.ofn + i)}
            <li title={row.ofn}>
              {@render tokens(row.tokens)}<span class="badge">Inferred</span>{#if row.alsoAsserted}
                <span class="note">Also asserted.</span>{/if}
            </li>
          {/each}
        </ul>
      </div>
    {/each}
    {#if frame.other.length}
      <div class="clause" data-section="materialized-other">
        <strong>Other axioms:</strong>
        <ul>
          {#each frame.other as row, i (row.ofn + i)}
            <li>
              <code>{row.ofn}</code><span class="badge">Inferred</span>{#if row.alsoAsserted}
                <span class="note">Also asserted.</span>{/if}
            </li>
          {/each}
        </ul>
      </div>
    {/if}
    {#if !listed}<p class="note">
        No materialized axioms beyond trivial ones were returned for this entity. This does not
        establish that no other entailments exist.
      </p>{/if}
    {#if frame.trivial}<p class="note">
        {frame.trivial === 1 ? 'One trivial entailment' : `${frame.trivial} trivial entailments`},
        such as a property's subproperty relation to itself or a type of owl:Thing, {frame.trivial ===
        1
          ? 'is'
          : 'are'} not listed.
      </p>{/if}
    {#if model.page.statementsTruncated || model.page.warnings?.length}
      <p class="note">
        The frame may omit facts because its statement limit or supported axiom mapping was reached.
      </p>
      {#each model.page.warnings ?? [] as warning, i (i)}<p class="note">{warning}</p>{/each}
    {/if}
  {/if}
</section>

<style>
  .materialized-frame {
    margin-top: var(--sp-4);
    overflow-wrap: anywhere;
  }
  .heading {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  h3 {
    margin: 0;
  }
  .clause {
    margin-block: var(--sp-3);
  }
  ul {
    margin: var(--sp-1) 0 0;
    padding-left: 1.25rem;
  }
  li {
    margin: var(--sp-1) 0;
    overflow-wrap: anywhere;
  }
  .kw {
    color: var(--kw);
    font-weight: 600;
    font-style: italic;
  }
  .iri-link {
    all: unset;
    color: var(--iri);
    cursor: pointer;
  }
  a.iri-link:hover,
  button.iri-link:hover {
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
  .note {
    color: var(--text-2);
    font-size: 0.85rem;
  }
</style>

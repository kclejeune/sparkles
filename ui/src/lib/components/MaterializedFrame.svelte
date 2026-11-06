<script lang="ts">
  import { untrack } from 'svelte';
  import { MaterializedFrame } from '$lib/materialized-frame.svelte';
  import { INFERRED_GRAPH, type InferredPins } from '$lib/inferred-read';
  import { iriForm } from '$lib/iri-form.svelte';
  import type { AxiomItem } from '$lib/ontology';
  import MaterializationNotice from './MaterializationNotice.svelte';
  let {
    target,
    graph,
    imports = true,
    iri,
    commit,
    items,
    pins,
  }: {
    target: string;
    graph: string | null;
    imports?: boolean;
    iri: string;
    commit: number | null;
    items: AxiomItem[];
    pins?: InferredPins;
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
</script>

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
    {#if inferred.length}
      <ul>
        {#each inferred as item, i (item.ofn + i)}
          <li>
            <code>{item.ofn}</code>{#if asserted.has(item.ofn)}
              <span class="faint">Also asserted.</span>{/if}
          </li>
        {/each}
      </ul>
    {:else}<p>
        No materialized axioms were returned for this entity. This does not establish that no other
        entailments exist.
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
  li {
    margin: var(--sp-2) 0;
  }
</style>

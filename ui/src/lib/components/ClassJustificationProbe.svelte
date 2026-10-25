<script lang="ts">
  import { onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import type { PrefixMap } from '$lib/rdf';
  import JustificationPanel from './JustificationPanel.svelte';
  let {
    ds,
    prefixes,
    onremove,
    onopen,
  }: {
    ds: string;
    prefixes: PrefixMap;
    onopen?: (iri: string) => void;
    onremove?: (fact: api.SupportFact, commit: number, profile?: string) => void;
  } = $props();
  let classIri = $state('');
  let profile = $state('owl-rl');
  let report = $state<api.JustificationReport | null>(null);
  let busy = $state(false);
  let error = $state('');
  let controller: AbortController | null = null;
  let generation = 0;
  onDestroy(() => controller?.abort());
  $effect(() => {
    void ds;
    void classIri;
    void profile;
    controller?.abort();
    generation++;
    report = null;
    busy = false;
    error = '';
  });
  async function probe(all: boolean) {
    controller?.abort();
    controller = new AbortController();
    const current = ++generation;
    busy = true;
    error = '';
    try {
      const result = await api.justify(ds, all ? { classes: 'all' } : { class: classIri.trim() }, {
        profile: profile || undefined,
        signal: controller.signal,
      });
      if (generation === current) report = result;
    } catch (failure) {
      if (
        generation === current &&
        !(failure instanceof DOMException && failure.name === 'AbortError')
      )
        error = api.errorMessage(failure);
    } finally {
      if (generation === current) busy = false;
    }
  }
</script>

<details class="class-probes">
  <summary>Class satisfiability</summary>
  <p class="faint">Test whether a fresh instance of a class would cause an inconsistency.</p>
  <div class="row">
    <label
      >Class IRI <input
        class="input"
        bind:value={classIri}
        placeholder="https://example.org/Class"
      /></label
    >
    <label
      >Program <select class="select" bind:value={profile}
        ><option value="owl-rl">OWL 2 RL</option><option value="rdfs">RDFS</option><option value=""
          >Recorded program</option
        ></select
      ></label
    >
    <button class="btn sm" disabled={busy || !classIri.trim()} onclick={() => probe(false)}
      >Test class</button
    >
    <button class="btn ghost sm" disabled={busy} onclick={() => probe(true)}
      >Find relevant unsatisfiable classes</button
    >
    {#if busy}<button class="btn ghost sm" onclick={() => controller?.abort()}>Cancel</button>{/if}
  </div>
  {#if error}<p class="error-box">{error}</p>{/if}
  {#if report}
    {#each report.results as result, index (index)}
      <section>
        <h4>{result.class}</h4>
        {#if result.holds && result.finding}
          <p>A fresh instance causes an inconsistency.</p>
          <JustificationPanel
            {ds}
            finding={result.finding}
            commit={report.commit}
            {prefixes}
            classIri={result.class ?? undefined}
            profile={profile || undefined}
            initials={result.justifications}
            {onopen}
            onremove={onremove
              ? (fact) => onremove?.(fact, report!.commit, profile || undefined)
              : undefined}
            initialComplete={result.complete}
          />
        {:else}<p class="faint">
            {result.complete
              ? 'No inconsistency was found for a fresh instance.'
              : 'The search did not establish a result within its budget.'}
          </p>{/if}
      </section>
    {/each}
    {#if !report.complete}<p class="faint">
        The search is incomplete. More classes or supports may exist.
      </p>{/if}
    {#if report.complete && !report.results.length}<p class="faint">
        No relevant unsatisfiable classes were found.
      </p>{/if}
  {/if}
</details>

<style>
  .class-probes {
    border-top: 1px solid var(--border);
    padding-top: 10px;
  }
  .row {
    flex-wrap: wrap;
    gap: 8px;
    margin: 10px 0;
  }
  label {
    display: flex;
    align-items: center;
    gap: 6px;
  }
</style>

<script lang="ts">
  import { onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import type { PrefixMap } from '$lib/rdf';
  import TermView from './TermView.svelte';
  let {
    ds,
    finding,
    commit,
    prefixes,
    initial,
    initials,
    initialComplete,
    onremove,
    classIri,
    profile,
    onopen,
  }: {
    ds: string;
    finding: api.DiagnosticFinding;
    commit: number;
    prefixes: PrefixMap;
    initial?: api.Justification | null;
    initials?: api.Justification[];
    initialComplete?: boolean;
    onremove?: (fact: api.SupportFact) => void;
    classIri?: string;
    profile?: string;
    onopen?: (iri: string) => void;
  } = $props();
  let results = $state<api.Justification[]>([]);
  let complete = $state<boolean | null>(null);
  let busy = $state(false);
  let error = $state('');
  let controller: AbortController | null = null;
  let generation = 0;
  onDestroy(() => controller?.abort());
  $effect(() => {
    void ds;
    void finding;
    void commit;
    void classIri;
    void profile;
    controller?.abort();
    generation++;
    results = initials ?? (initial ? [initial] : []);
    complete = initialComplete ?? null;
    error = '';
    busy = false;
  });
  async function justify() {
    controller?.abort();
    controller = new AbortController();
    const current = ++generation;
    busy = true;
    error = '';
    try {
      const report = await api.justify(ds, classIri ? { class: classIri } : { finding }, {
        at: String(commit),
        profile,
        signal: controller.signal,
      });
      if (generation === current) {
        results = report.results[0]?.justifications ?? [];
        complete = report.complete;
      }
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

<div class="supports">
  <div class="row">
    <strong>Minimal asserted supports</strong><button
      class="btn ghost sm"
      onclick={justify}
      disabled={busy}
      >{busy ? 'Searching…' : results.length ? 'Find alternatives' : 'Find a justification'}</button
    >
  </div>
  {#if error}<p class="error-box">{error}</p>{/if}
  {#each results as support, i (i)}
    <section aria-label={`Justification ${i + 1}`}>
      <h4>Justification {i + 1}</h4>
      {#if !support.facts.length}<p class="faint">
          The rule program entails this without asserted facts.
        </p>{/if}
      {#each ['schema', 'assertion'] as role (role)}
        {@const facts = support.facts.filter((fact) => fact.role === role)}
        {#if facts.length}<h5>{role === 'schema' ? 'Schema axioms' : 'Data assertions'}</h5>
          <ul>
            {#each facts as fact, j (j)}<li>
                <span class="mono"
                  >{#each fact.triple as term, k (k)}<TermView
                      {term}
                      {prefixes}
                      {onopen}
                    />{' '}{/each}</span
                ><span class="faint"
                  >in {#if fact.graph === 'default'}default graph{:else}<TermView
                      term={fact.graph}
                      {prefixes}
                      {onopen}
                    />{/if}</span
                >{#if onremove}<button class="btn ghost sm" onclick={() => onremove?.(fact)}
                    >Remove on a branch</button
                  >{/if}
              </li>{/each}
          </ul>{/if}
      {/each}
    </section>
  {/each}
  {#if complete === false}<p class="faint">
      The search is incomplete. Other justifications may exist.
    </p>{:else if complete && !results.length}<p class="faint">
      No asserted support for this finding was found under the current program.
    </p>{/if}
</div>

<style>
  .supports {
    display: grid;
    gap: 10px;
    padding: 10px;
  }
  ul {
    list-style: none;
    padding: 0;
  }
  li {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
    margin: 6px 0;
  }
  h5 {
    color: var(--muted);
  }
</style>

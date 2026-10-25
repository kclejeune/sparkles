<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import type { Term } from '$lib/api';
  import * as api from '$lib/api';
  import { parseTermText } from '$lib/forms';
  import type { PrefixMap } from '$lib/rdf';
  import {
    explainMissing,
    type WhyNotExplanation,
    type WhyTriple,
    type Derivation,
  } from '$lib/why';
  import TermView from './TermView.svelte';

  let {
    ds,
    subject,
    prefixes,
    onopen,
  }: { ds: string; subject: Term; prefixes: PrefixMap; onopen?: (iri: string) => void } = $props();
  let predicate = $state('a');
  let object = $state('');
  let result = $state<WhyNotExplanation | null>(null);
  let busy = $state(false);
  let error = $state('');
  let ctl: AbortController | null = null;
  onDestroy(() => ctl?.abort());
  $effect(() => {
    void ds;
    void subject;
    untrack(() => {
      ctl?.abort();
      result = null;
      error = '';
      busy = false;
    });
  });

  async function search(e: SubmitEvent) {
    e.preventDefault();
    const p =
      predicate.trim() === 'a'
        ? { type: 'uri' as const, value: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type' }
        : parseTermText(predicate, prefixes);
    const o = parseTermText(object, prefixes);
    if ('error' in p || 'error' in o || p.type !== 'uri') {
      error = 'Enter a property IRI and an RDF value, using prefixes or <IRIs>.';
      return;
    }
    ctl?.abort();
    const c = (ctl = new AbortController());
    busy = true;
    error = '';
    result = null;
    try {
      const r = await explainMissing(ds, { s: subject, p, o }, c.signal);
      if (!c.signal.aborted) result = r;
    } catch (e) {
      if (!c.signal.aborted) error = api.errorMessage(e);
    } finally {
      if (!c.signal.aborted) busy = false;
    }
  }
</script>

{#snippet triple(t: WhyTriple)}
  <span class="triple mono"
    ><TermView term={t.s} {prefixes} {onopen} />
    <TermView term={t.p} {prefixes} />
    <TermView term={t.o} {prefixes} {onopen} /></span
  >
{/snippet}
{#snippet node(d: Derivation)}
  <li>
    {@render triple(d.triple)}
    <span class="faint">{d.missing ? 'missing' : (d.rule?.name ?? d.kind)}</span>
    {#if d.premises?.length}<ul>
        {#each d.premises as p}{@render node(p)}{/each}
      </ul>{/if}
  </li>
{/snippet}
<section aria-label="Why not?">
  <details>
    <summary>Why not…?</summary>
    <form onsubmit={search}>
      <label
        >Property<input
          class="input sm"
          aria-label="Why not property"
          bind:value={predicate}
          placeholder="a or ex:property"
          required
        /></label
      >
      <label
        >Value<input
          class="input sm"
          aria-label="Why not value"
          bind:value={object}
          placeholder="ex:Class or an RDF value"
          required
        /></label
      >
      <button class="btn sm" disabled={busy}>{busy ? 'Searching…' : 'Find missing facts'}</button>
    </form>
    {#if error}<p class="error-box" role="status">{error}</p>{/if}
    {#if result}
      {#if result.entailed}<p>This triple already follows from the data.</p>
      {:else}
        {#each result.hypotheses ?? [] as h, i}
          <details>
            <summary
              >Possibility {i + 1}: add {h.missing.length} fact{h.missing.length === 1
                ? ''
                : 's'}</summary
            >
            {#if h.inconsistent === true}<p class="error-box">
                Adding these facts introduces a new inconsistency.
              </p>
            {:else if h.consistencyComplete === false}<p class="faint small">
                The consistency check is incomplete.
              </p>{/if}
            <ul>
              {#each h.missing as t}<li>{@render triple(t)}</li>{/each}
            </ul>
            {#if h.placeholders.length}<p class="faint small">
                Blank nodes stand for a resource that satisfies these facts.
              </p>{/if}
            <details>
              <summary>Verified derivation</summary>
              <ul>{@render node(h.tree)}</ul>
            </details>
          </details>
        {/each}
        {#if !result.hypotheses?.length}<p>No missing facts found within the search limits.</p>{/if}
        {#each result.rules ?? [] as r}{#each r.blockedBy ?? [] as b}<p class="small">
              {r.name} is blocked by <code>{b.call}</code>{#if b.fact}: {@render triple(
                  b.fact,
                )}{/if}
            </p>{/each}{/each}
        {#if !result.complete}<p class="faint small">
            The search reached its limits. Other possibilities may exist.
          </p>{/if}
      {/if}
    {/if}
  </details>
</section>

<style>
  section {
    margin: 12px 0;
    border-top: 1px solid var(--border);
    padding-top: 12px;
  }
  summary {
    cursor: pointer;
    padding: 4px 0;
  }
  form {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 8px 0;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  ul {
    padding-left: 16px;
  }
  li,
  .triple {
    overflow-wrap: anywhere;
  }
</style>

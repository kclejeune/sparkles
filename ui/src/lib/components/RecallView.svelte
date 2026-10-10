<script lang="ts">
  // A recall result (C17 §5.5) for people: each entity's facts grouped by predicate with
  // citation markers, status badges and conflicts, and the citations a marker expands.
  import type { RecallEntity, RecallResult, RecallSuperseded } from '$lib/ask-api';
  import { parseCompact } from '$lib/compact';
  import {
    conflictsOf,
    filterFacts,
    groupByPredicate,
    principalName,
    type MemoryFilter,
  } from '$lib/memory';
  import type { PrefixMap } from '$lib/rdf';
  import TermView from './TermView.svelte';

  let {
    result,
    prefixes,
    filter = { graphs: new Set(), agents: new Set() },
    entities,
    superseded = [],
    onopen,
    showHeader = true,
  }: {
    result: RecallResult;
    prefixes: PrefixMap;
    filter?: MemoryFilter;
    /** The entities to show (all of the result's by default). */
    entities?: RecallEntity[];
    /** Superseded facts to show inline, struck through. */
    superseded?: RecallSuperseded[];
    onopen?: (iri: string) => void;
    showHeader?: boolean;
  } = $props();

  const all = $derived({ ...prefixes, ...result.prefixes });
  const term = (s: string) => parseCompact(s, all);
  const byId = $derived(new Map(result.citations.map((c) => [String(c.id), c])));
  /** Citations expanded under the facts, in the order they were opened. */
  let open = $state<string[]>([]);
  const toggle = (id: number | string) => {
    const k = String(id);
    open = open.includes(k) ? open.filter((x) => x !== k) : [...open, k];
  };
  const shown = $derived(entities ?? result.entities);
</script>

{#snippet marker(id: number | string)}
  <button
    class="cite"
    class:on={open.includes(String(id))}
    aria-expanded={open.includes(String(id))}
    title="Show citation {id}"
    onclick={() => toggle(id)}>[{id}]</button
  >
{/snippet}

<div class="recall">
  {#each shown as e (e.iri)}
    {@const facts = filterFacts(e.facts, result.citations, filter)}
    {@const groups = groupByPredicate(facts)}
    {@const gone = superseded.filter((h) => h.s === e.iri)}
    <section class="entity" aria-label="Facts about {e.label ?? e.iri}">
      {#if showHeader}
        <h4 class="ehead">
          <span class="mono"><TermView term={term(e.iri)} prefixes={all} {onopen} /></span>
          {#if e.label}<span>"{e.label}"</span>{/if}
          {#each e.types as t (t)}<span class="badge iri">{t}</span>{/each}
          <span class="faint small"
            >{e.seed != null ? `seed ${e.seed}` : ''}{e.hop ? ` · hop ${e.hop}` : ''}</span
          >
        </h4>
      {/if}
      {#if groups.length === 0 && gone.length === 0}
        <p class="faint small">No facts you can read.</p>
      {/if}
      <table class="facts">
        <tbody>
          {#each groups as g (g.p)}
            {@const hist = gone.filter((h) => h.p === g.p)}
            <tr>
              <th><span class="mono"><TermView term={term(g.p)} prefixes={all} /></span></th>
              <td>
                {#each g.values as v (v.o)}
                  <div class="val" data-status={v.status ?? ''}>
                    <span class="mono"><TermView term={term(v.o)} prefixes={all} {onopen} /></span>
                    <span class="cites"
                      >{#each v.citations as c (c)}{@render marker(c)}{/each}</span
                    >
                    {#if v.status}
                      <span class="badge status {v.status}">{v.status}</span>
                    {/if}
                  </div>
                {/each}
                {#each hist as h (h.reifier)}
                  <div class="val gone">
                    <span class="mono"><TermView term={term(h.o)} prefixes={all} /></span>
                    <span class="badge">superseded</span>
                  </div>
                {/each}
              </td>
            </tr>
          {/each}
          {#each gone.filter((h) => !groups.some((g) => g.p === h.p)) as h (h.reifier)}
            <tr>
              <th><span class="mono"><TermView term={term(h.p)} prefixes={all} /></span></th>
              <td>
                <div class="val gone">
                  <span class="mono"><TermView term={term(h.o)} prefixes={all} /></span>
                  <span class="badge">{h.replacedBy ? 'superseded' : 'retracted'}</span>
                </div>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
      {#each conflictsOf(result.conflicts, e.iri) as c (c.p)}
        <p class="conflict small" role="note">
          <strong>Conflict</strong>
          <span class="mono"><TermView term={term(c.p)} prefixes={all} /></span>
          {#each c.values as v, i (v.o)}{#if i}<span class="faint"> · </span>{/if}<span class="mono"
              ><TermView term={term(v.o)} prefixes={all} /></span
            >
            {@render marker(v.citation)}{/each}
        </p>
      {/each}
    </section>
  {/each}

  {#if open.length}
    <div class="citations" aria-label="Citations">
      {#each open as id (id)}
        {@const c = byId.get(id)}
        {#if c}
          <dl class="citation" aria-label="Citation {id}">
            <dt class="cid">
              <button class="cite on" onclick={() => toggle(id)} aria-label="Hide citation {id}"
                >[{id}]</button
              >
            </dt>
            <dd class="cbody">
              <div class="kv">
                <span class="faint">graph</span><span class="mono">{c.graph}</span>
              </div>
              {#if c.source}
                <div class="kv">
                  <span class="faint">source</span><span class="mono"
                    ><TermView term={term(c.source)} prefixes={all} {onopen} /></span
                  >
                </div>
              {/if}
              {#if c.quote}
                <div class="kv">
                  <span class="faint">quote</span><q>{c.quote}</q>
                </div>
              {/if}
              {#if c.at}
                <div class="kv"><span class="faint">time</span><span>{c.at}</span></div>
              {/if}
              {#if c.by}
                <div class="kv">
                  <span class="faint">by</span><span title={c.by}>{principalName(c.by)}</span>
                </div>
              {/if}
              {#if c.confidence != null}
                <div class="kv">
                  <span class="faint">confidence</span><span>{c.confidence.toFixed(2)}</span>
                </div>
              {/if}
              {#if c.status}
                <div class="kv">
                  <span class="faint">status</span><span class="badge status {c.status}"
                    >{c.status}</span
                  >
                </div>
              {/if}
              {#if !c.reifier}
                <div class="faint small">No reifier: the fact is cited by its graph alone.</div>
              {/if}
            </dd>
          </dl>
        {/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .recall {
    display: grid;
    gap: 10px;
    min-width: 0;
  }
  .entity {
    min-width: 0;
  }
  .ehead {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
    margin: 0 0 4px;
    font-size: var(--fs-sm);
    font-weight: 600;
  }
  .facts {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--fs-sm);
  }
  .facts th {
    text-align: left;
    vertical-align: top;
    font-weight: 500;
    padding: 3px 8px 3px 0;
    width: 34%;
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
  .facts td {
    padding: 3px 0;
    vertical-align: top;
    min-width: 0;
  }
  .val {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px;
    overflow-wrap: anywhere;
  }
  .val.gone {
    text-decoration: line-through;
    color: var(--text-3);
  }
  .cites {
    display: inline-flex;
    gap: 1px;
  }
  .cite {
    all: unset;
    cursor: pointer;
    color: var(--iri);
    font-size: var(--fs-xs);
    font-family: var(--font-mono);
    border-radius: 3px;
    padding: 0 1px;
  }
  .cite.on {
    background: color-mix(in srgb, var(--iri) 16%, transparent);
  }
  .cite:focus-visible {
    box-shadow: var(--focus);
  }
  .badge.status.reviewed {
    background: color-mix(in srgb, var(--ok) 14%, transparent);
    color: var(--ok);
  }
  .badge.status.unreviewed {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .conflict {
    margin: 4px 0 0;
    color: var(--warn);
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: baseline;
  }
  .citations {
    display: grid;
    gap: 6px;
    border-top: 1px solid var(--border);
    padding-top: 8px;
  }
  .citation {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 6px;
    margin: 0;
    font-size: var(--fs-sm);
  }
  .cid,
  .cbody {
    margin: 0;
  }
  .cbody {
    display: grid;
    gap: 2px;
  }
  .kv {
    display: grid;
    grid-template-columns: 76px minmax(0, 1fr);
    gap: 6px;
    overflow-wrap: anywhere;
  }
</style>

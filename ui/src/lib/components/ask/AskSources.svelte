<script lang="ts">
  // The Sources fold of an Ask turn: the rows the answer cites, the memory facts about
  // the entities they name or the question linked (with each fact's quote, document,
  // status and confidence, from `POST /{ds}/recall`), and the community reports the
  // answer read. Recall runs once, when the fold first opens.
  import { resolve } from '$app/paths';
  import type { Term } from '$lib/api';
  import type { PrefixMap } from '$lib/rdf';
  import * as askApi from '$lib/ask-api';
  import { citedIris, type ThreadTurn } from '$lib/ask-thread';
  import { shortLabel } from '$lib/graph';
  import { onMount } from 'svelte';

  let {
    turn,
    ds,
    prefixes,
    chosen,
    oncite,
  }: {
    turn: ThreadTurn;
    ds: string;
    prefixes: PrefixMap;
    chosen: number | null;
    oncite: (row: number) => void;
  } = $props();

  type Fact = {
    s: string;
    p: string;
    o: string;
    sLabel?: string;
    citation?: askApi.RecallCitation;
  };
  let facts = $state<Fact[] | null>(null);
  let recallError = $state<string | null>(null);

  const rows = $derived(turn.result?.results?.rows ?? []);
  const vars = $derived(turn.result?.results?.vars ?? []);
  const cited = $derived([...new Set(turn.summary?.citations ?? [])].filter((n) => rows[n - 1]));
  const seeds = $derived(
    [...new Set([...citedIris(turn, 8), ...(turn.entities ?? [])])].slice(0, 10),
  );
  const reports = $derived(
    turn.global?.reports.length
      ? turn.global.reports.map((r) => ({ iri: r.iri, title: r.title }))
      : (turn.communities ?? []).map((iri) => ({ iri, title: shortLabel(iri, prefixes) })),
  );

  const explore = (iri: string, memory = false) =>
    `${resolve('/explore')}?ds=${encodeURIComponent(ds)}&iri=${encodeURIComponent(iri)}${memory ? '&view=memory' : ''}`;
  const communities = $derived(`${resolve('/datasets/[name]', { name: ds })}?tab=communities`);
  /** A source as N-Triples gives it, `<…>` or a literal, in plain form. */
  const plain = (s: string | undefined) =>
    (s ?? '').replace(/^<(.*)>$/, '$1').replace(/^"(.*)"(\^\^.*|@.*)?$/, '$1');
  const isLink = (s: string | undefined) => /^https?:\/\//.test(plain(s));
  const agent = (s: string) => plain(s).replace(/^urn:x-sparkles:principal:/, '');
  const termText = (t: Term | null) =>
    !t ? '' : t.type === 'uri' ? shortLabel(t.value, prefixes) : String(t.value);

  onMount(() => {
    if (!seeds.length) {
      facts = [];
      return;
    }
    const ctl = new AbortController();
    askApi
      .recall(ds, { seeds, hops: 0, maxTriples: 60 }, ctl.signal)
      .then((r) => {
        const byId = new Map(r.citations.map((c) => [String(c.id), c]));
        const out: Fact[] = [];
        for (const e of r.entities)
          for (const f of e.facts) {
            const c = byId.get(String(f.citation));
            // a fact with provenance came from memory or a document
            if (c && (c.quote || c.source || c.by))
              out.push({ s: f.s, p: f.p, o: f.o, sLabel: e.label, citation: c });
          }
        facts = out.slice(0, 20);
      })
      .catch((e) => {
        if (!ctl.signal.aborted) {
          facts = [];
          recallError = (e as Error).message;
        }
      });
    return () => ctl.abort();
  });
</script>

<div class="sources">
  {#if cited.length}
    <h4>Cited rows</h4>
    <ol class="list">
      {#each cited as n (n)}
        <li class:hot={chosen === n}>
          <button type="button" class="cite" class:hot={chosen === n} onclick={() => oncite(n)}
            >{n}</button
          >
          <span class="row">
            {#each rows[n - 1] as cell, i (i)}
              {#if cell}
                <span class="cell"
                  ><span class="faint">{vars[i]}</span>
                  {#if cell.type === 'uri'}<a class="link" href={explore(cell.value)}
                      >{termText(cell)}</a
                    >{:else}{termText(cell)}{/if}</span
                >
              {/if}
            {/each}
          </span>
        </li>
      {/each}
    </ol>
  {:else if turn.summary?.citations.length && !rows.length}
    <p class="faint small">
      The answer cites {turn.summary.citations.length === 1 ? 'a row' : 'rows'} of its result. Rows are
      not kept, so open Rows and choose Show rows to read them.
    </p>
  {/if}

  {#if seeds.length}
    <h4>Memory and documents</h4>
    {#if facts == null}
      <p class="faint small" role="status">Reading memory…</p>
    {:else if facts.length}
      <ul class="list facts">
        {#each facts as f, i (i)}
          <li>
            <div>
              <a class="link" href={explore(f.s, true)}>{f.sLabel ?? shortLabel(f.s, prefixes)}</a>
              <span class="faint">{shortLabel(f.p, prefixes)}</span>
              {f.o.startsWith('http')
                ? shortLabel(f.o, prefixes)
                : f.o.replace(/^"|"(@.*|\^\^.*)?$/g, '')}
            </div>
            {#if f.citation?.quote}<blockquote>{f.citation.quote}</blockquote>{/if}
            <div class="prov small faint">
              {#if f.citation?.source}<span
                  >from {#if isLink(f.citation.source)}<a
                      class="link"
                      href={plain(f.citation.source)}
                      rel="noopener noreferrer"
                      target="_blank">{plain(f.citation.source)}</a
                    >{:else}{plain(f.citation.source)}{/if}</span
                >{/if}
              {#if f.citation?.status}<span>{f.citation.status}</span>{/if}
              {#if f.citation?.by}<span>by {agent(f.citation.by)}</span>{/if}
              {#if f.citation?.confidence != null}<span
                  >confidence {f.citation.confidence.toFixed(2)}</span
                >{/if}
            </div>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="faint small">
        {recallError
          ? `Memory could not be read: ${recallError}`
          : 'Memory holds no facts with a source about these entities.'}
      </p>
    {/if}
  {/if}

  {#if reports.length}
    <h4>Community reports</h4>
    <ul class="list">
      {#each reports as r (r.iri)}
        <li><a class="link" href={communities}>{r.title}</a></li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .sources {
    display: grid;
    gap: var(--sp-2);
    min-width: 0;
  }
  h4 {
    margin: var(--sp-1) 0 0;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--sp-2);
  }
  .list > li {
    display: flex;
    gap: var(--sp-2);
    align-items: baseline;
    min-width: 0;
    border-radius: var(--r-sm);
  }
  .list > li.hot {
    background: var(--spark-soft);
  }
  .facts > li {
    display: grid;
    gap: 2px;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 2px var(--sp-3);
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .cell .faint {
    font-size: var(--fs-xs);
  }
  .link {
    color: var(--iri);
    text-decoration: none;
    overflow-wrap: anywhere;
  }
  .link:hover {
    text-decoration: underline;
  }
  blockquote {
    margin: 2px 0 0;
    padding-left: var(--sp-2);
    border-left: 2px solid var(--border-strong);
    color: var(--text-2);
    font-style: italic;
  }
  .prov {
    display: flex;
    flex-wrap: wrap;
    gap: 2px var(--sp-2);
  }
  .cite {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 18px;
    height: 16px;
    padding: 0 4px;
    border: 0;
    border-radius: var(--r-sm);
    background: var(--surface-3);
    color: var(--text-2);
    font: inherit;
    font-size: var(--fs-xs);
    font-weight: 600;
    cursor: pointer;
    flex: none;
  }
  .cite.hot {
    background: var(--spark-soft);
    color: var(--spark-ink);
  }
</style>

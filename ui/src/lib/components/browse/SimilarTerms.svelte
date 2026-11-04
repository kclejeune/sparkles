<script lang="ts">
  // A term page's "Similar terms" (ROADMAP item 633): the term's nearest neighbours in the
  // dataset's first ready vector index, read through the same search as Similar's Search
  // page, with a link to that page for more. A dataset without an index, or a term without
  // a vector in it, shows nothing.
  import * as api from '$lib/api';
  import * as ex from '$lib/explore';
  import { MAIN, splitTarget } from '$lib/branches';
  import { routes } from '$lib/routes';
  import { localName } from '$lib/rdf';
  import { LatestRun } from '$lib/supersede';
  import { fmtScore, similar, type SimilarHit } from '$lib/vectors';

  let {
    ds,
    iri,
    href,
    k = 8,
  }: {
    ds: string;
    iri: string;
    /** The page of a neighbour. */
    href: (iri: string) => string;
    k?: number;
  } = $props();

  let index = $state<api.VectorIndexStatus | null>(null);
  type Hit = { iri: string; score: number };
  let hits = $state<Hit[]>([]);
  let labels = $state<Map<string, string>>(new Map());
  const runs = new LatestRun();
  // `ds` may name a branch, `name@branch`, which Similar takes as a dataset and a `branch`
  const family = $derived(splitTarget(ds));

  $effect(() => {
    const name = ds;
    const entity = iri;
    const owns = runs.claim('similar');
    const ctl = new AbortController();
    void (async () => {
      try {
        const s = await api.vectorStatus(name, ctl.signal);
        const ix = s.indexes.find((i) => i.state === 'ready') ?? null;
        if (!owns()) return;
        index = ix;
        if (!ix) {
          hits = [];
          return;
        }
        const found = await similar(
          name,
          entity,
          { predicate: ix.predicate, query: { entity }, k, metric: ix.metric },
          ctl.signal,
        );
        if (!owns()) return;
        hits = found.flatMap((h: SimilarHit) =>
          h.term.type === 'uri' ? [{ iri: h.term.value, score: h.score }] : [],
        );
        const l = await ex
          .labels(
            name,
            hits.map((h) => h.iri),
          )
          .catch(() => new Map<string, string>());
        if (owns()) labels = l;
      } catch {
        // no index, or no vector for this term: the section stays out
        if (owns()) hits = [];
      }
    })();
    return () => ctl.abort();
  });
</script>

{#if index && hits.length}
  <section class="term-sec" aria-labelledby="similar-terms-h">
    <h2 id="similar-terms-h">Similar terms</h2>
    <ol>
      {#each hits as h (h.iri)}
        <li>
          <a href={href(h.iri)} title={h.iri}>{labels.get(h.iri) ?? localName(h.iri)}</a>
          <span class="faint small mono" title="{index.metric} score">{fmtScore(h.score)}</span>
        </li>
      {/each}
    </ol>
    <a
      class="more small"
      href={routes.similar(family.name, {
        iri,
        index: index.name,
        branch: family.branch === MAIN ? null : family.branch,
      })}>More in Similar, by the {index.name} index</a
    >
  </section>
{/if}

<style>
  ol {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 2px;
  }
  li {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    justify-content: space-between;
  }
  li a {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .more {
    display: inline-block;
    margin-top: var(--sp-2);
  }
</style>

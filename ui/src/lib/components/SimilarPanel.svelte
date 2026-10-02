<script lang="ts">
  import { resolve } from '$app/paths';
  import type { Term } from '$lib/api';
  import * as ex from '$lib/explore';
  import { shortLabel } from '$lib/graph';
  import { displayIri, type PrefixMap } from '$lib/rdf';
  import { LatestRun } from '$lib/supersede';
  import {
    fmtScore,
    metricInfo,
    METRICS,
    scoreBars,
    similar,
    vectorErrorHint,
    vectorPredicates,
    type Metric,
    type SimilarHit,
  } from '$lib/vectors';
  import Icon from './Icon.svelte';
  import TermView from './TermView.svelte';

  let {
    ds,
    iri,
    props,
    prefixes,
  }: {
    ds: string;
    /** The entity whose neighbours are listed. */
    iri: string;
    /** The entity's outgoing properties (its vector literals among them). */
    props: { p: string; o: Term }[];
    prefixes: PrefixMap;
  } = $props();

  const KS = [5, 10, 20, 50];

  const preds = $derived(vectorPredicates(props));
  let predicate = $state('');
  let vectorIndex = $state(0);
  let k = $state(10);
  let metric = $state<Metric>('cosine');

  // a different entity (or one without the chosen predicate): start from its first one
  $effect(() => {
    if (!preds.some((p) => p.iri === predicate)) {
      predicate = preds[0]?.iri ?? '';
      vectorIndex = 0;
    }
  });

  const current = $derived(preds.find((p) => p.iri === predicate));

  let hits = $state<SimilarHit[] | null>(null);
  let labels = $state<Map<string, string>>(new Map());
  let error = $state<{ title: string; hint?: string } | null>(null);
  let loading = $state(false);
  const runs = new LatestRun();

  async function run(
    entity: string,
    pred: string,
    vectors: number,
    index: number,
    kk: number,
    m: Metric,
  ) {
    const owns = runs.claim('similar');
    loading = true;
    error = null;
    try {
      // an entity with several vectors under the predicate is searched by one of them
      const query = vectors > 1 && current ? { vector: current.vectors[index] } : { entity };
      const found = await similar(ds, entity, { predicate: pred, query, k: kk, metric: m });
      if (!owns()) return;
      hits = found;
      const iris = found.flatMap((h) => (h.term.type === 'uri' ? [h.term.value] : []));
      const l = await ex.labels(ds, iris).catch(() => new Map<string, string>());
      if (owns()) labels = l;
    } catch (e) {
      if (!owns()) return;
      hits = null;
      error = vectorErrorHint(e);
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    if (!current) return;
    void run(iri, current.iri, current.vectors.length, vectorIndex, k, metric);
  });

  const info = $derived(metricInfo(metric));
  const bars = $derived(
    hits
      ? scoreBars(
          hits.map((h) => h.score),
          metric,
        )
      : [],
  );
  const href = (target: string) =>
    `${resolve('/explore')}?ds=${encodeURIComponent(ds)}&iri=${encodeURIComponent(target)}`;
  const similarHref = $derived(
    `${resolve('/similar')}?${new URLSearchParams({
      ds,
      ...(predicate ? { predicate } : {}),
      iri,
      ...(metric !== 'cosine' ? { metric } : {}),
      ...(k !== 10 ? { k: String(k) } : {}),
    })}`,
  );
</script>

{#if preds.length}
  <h3 class="sub head">
    Similar
    {#if loading}<span class="spinner"></span>{/if}
    <a
      class="more"
      href={similarHref}
      title="Search with ef, exact and other vectors on the Similar page"
      >Open in Similar <Icon name="external" size={12} /></a
    >
  </h3>
  <div class="controls">
    {#if preds.length > 1}
      <label class="ctl" title="The embedding predicate to compare">
        <span class="faint">by</span>
        <select class="select sm" bind:value={predicate}>
          {#each preds as p (p.iri)}
            <option value={p.iri}
              >{displayIri(p.iri, prefixes)} ({p.dimensions.join(', ') || '?'})</option
            >
          {/each}
        </select>
      </label>
    {:else if current}
      <span class="faint ctl" title={current.iri}
        >by <span class="mono">{displayIri(current.iri, prefixes)}</span
        >{#if current.dimensions.length}
          · {current.dimensions.join(', ')} dims{/if}</span
      >
    {/if}
    {#if current && current.vectors.length > 1}
      <label class="ctl" title="This entity has several vectors under the predicate">
        <span class="faint">vector</span>
        <select class="select sm" bind:value={vectorIndex}>
          {#each current.vectors as _, i (i)}<option value={i}
              >{i + 1} of {current.vectors.length}</option
            >{/each}
        </select>
      </label>
    {/if}
    <div class="seg" role="radiogroup" aria-label="Metric">
      {#each METRICS as m (m)}
        <button
          class:on={metric === m}
          role="radio"
          aria-checked={metric === m}
          title="{metricInfo(m).label}: {metricInfo(m).better} is closer"
          onclick={() => (metric = m)}>{m}</button
        >
      {/each}
    </div>
    <label class="ctl">
      <span class="faint">top</span>
      <select class="select sm" bind:value={k}>
        {#each KS as n (n)}<option value={n}>{n}</option>{/each}
      </select>
    </label>
  </div>
  {#if error}
    <div class="error-box small">
      <strong>{error.title}</strong>
      {#if error.hint}<div class="muted">{error.hint}</div>{/if}
    </div>
  {:else if hits}
    {#if hits.length === 0}
      <p class="faint small">No other entity has a vector of this dimension.</p>
    {:else}
      <table class="sim">
        <thead>
          <tr>
            <th>Entity</th>
            <th class="num" title="{info.label}: {info.better} is closer"
              >{info.score} {info.better === 'lower' ? '↓' : '↑'}</th
            >
          </tr>
        </thead>
        <tbody>
          {#each hits as h, i (i)}
            <tr>
              <td class="ent">
                {#if h.term.type === 'uri'}
                  {@const target = h.term.value}
                  <a class="t-iri" href={href(target)} title={target}
                    >{labels.get(target) ?? shortLabel(target, prefixes)}</a
                  >
                {:else}
                  <TermView term={h.term} {prefixes} />
                {/if}
              </td>
              <td class="num score">
                <span class="bar" style:width="{bars[i] * 100}%"></span>
                <span class="val mono">{fmtScore(h.score)}</span>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
      <p class="faint small note">
        {info.label}{info.better === 'lower' ? ' distance: lower is closer' : ': higher is closer'}.
        This entity is left out.
      </p>
    {/if}
  {/if}
{/if}

<style>
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .more {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-weight: 400;
    font-size: var(--fs-xs);
    color: var(--iri);
    text-decoration: none;
  }
  .more:hover {
    text-decoration: underline;
  }
  .sub {
    margin: 14px 0 6px;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
    margin-bottom: 8px;
    font-size: var(--fs-sm);
  }
  .ctl {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
  }
  .select.sm {
    height: 24px;
    font-size: var(--fs-sm);
    max-width: 190px;
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--r);
    overflow: hidden;
  }
  .seg button {
    all: unset;
    padding: 2px 8px;
    font-size: var(--fs-xs);
    cursor: pointer;
    color: var(--text-2);
  }
  .seg button + button {
    border-left: 1px solid var(--border);
  }
  .seg button.on {
    background: color-mix(in srgb, var(--iri) 12%, transparent);
    color: var(--iri);
    font-weight: 600;
  }
  .seg button:focus-visible {
    box-shadow: var(--focus);
  }
  .sim {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
    table-layout: fixed;
  }
  .sim th {
    text-align: left;
    font-weight: 500;
    color: var(--text-3);
    padding: 3px 0;
    border-bottom: 1px solid var(--border);
  }
  .sim th.num {
    text-align: right;
    width: 44%;
  }
  .sim td {
    padding: 4px 0;
    border-bottom: 1px solid var(--border);
  }
  .ent {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding-right: 8px !important;
  }
  .ent a {
    text-decoration: none;
  }
  .ent a:hover {
    text-decoration: underline;
  }
  .score {
    position: relative;
    text-align: right;
  }
  .bar {
    position: absolute;
    right: 0;
    top: 5px;
    bottom: 5px;
    border-radius: 3px;
    background: color-mix(in srgb, var(--literal) 16%, transparent);
    pointer-events: none;
  }
  .val {
    position: relative;
    padding-right: 4px;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .note {
    margin-top: 6px;
  }
</style>

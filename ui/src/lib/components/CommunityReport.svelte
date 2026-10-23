<script lang="ts">
  // One GraphRAG community with its report (spec C22 §2.3): the title, the level and
  // counts, the report's status, summary and findings with their citations, the
  // sub-communities and the members. The Communities tab shows it with the review
  // actions, and Explore's side panel shows it beside the overview, where choosing a
  // finding marks the entities it is about on the canvas.
  import type { Snippet } from 'svelte';
  import { resolve } from '$app/paths';
  import { fmtInt } from '$lib/format';
  import { displayIri } from '$lib/rdf';
  import * as gr from '$lib/graphrag';
  import Icon from './Icon.svelte';

  let {
    ds,
    detail,
    prefixes = {},
    review = null,
    onopen,
    finding = null,
    onfinding,
    actions,
    heading = 'h3',
  }: {
    ds: string;
    detail: gr.Community;
    prefixes?: Record<string, string>;
    /** The review actions, shown when given. */
    review?: {
      busy: boolean;
      onstatus: (s: 'reviewed' | 'rejected') => void;
      onedit: () => void;
    } | null;
    /** Show another community of the hierarchy. */
    onopen?: (iri: string) => void;
    /** The chosen finding, whose entities the caller marks. */
    finding?: number | null;
    /** Choose a finding, or none; findings are buttons only with this. */
    onfinding?: (i: number | null) => void;
    /** More actions beside the title. */
    actions?: Snippet;
    heading?: 'h2' | 'h3';
  } = $props();

  const short = (iri: string) => displayIri(iri, prefixes);
  const r = $derived(detail.report);
</script>

<div class="community-report">
  <header>
    <svelte:element this={heading} class="title">{r?.title ?? short(detail.iri)}</svelte:element>
    <p class="faint mono small iri">{detail.iri}</p>
    <p class="small">
      Level {detail.level} · {fmtInt(detail.memberCount)} members · {fmtInt(detail.factCount)} facts
      {#if detail.parent}
        · part of
        {#if onopen}<button class="link" onclick={() => onopen(detail.parent!)}
            >{short(detail.parent)}</button
          >{:else}{short(detail.parent)}{/if}
      {/if}
    </p>
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  </header>
  {#if r}
    <div class="row status-line">
      <span class="badge {gr.statusClass(r.status)}">{r.status}</span>
      {#if r.rating != null}<span class="faint">importance {r.rating}/10</span>{/if}
      {#if r.revisionOf}<span class="faint">edited revision</span>{/if}
      <span class="spacer"></span>
      {#if review && r.status !== 'failed'}
        <button
          class="btn sm"
          disabled={review.busy || r.status === 'reviewed'}
          onclick={() => review.onstatus('reviewed')}
          ><Icon name="check" size={12} /> Reviewed</button
        >
        <button
          class="btn sm"
          disabled={review.busy || r.status === 'rejected'}
          onclick={() => review.onstatus('rejected')}><Icon name="x" size={12} /> Reject</button
        >
        <button class="btn sm" disabled={review.busy} onclick={review.onedit}>Edit</button>
      {/if}
    </div>
    {#if r.failure}
      <div class="error-box"><strong>No report.</strong> {r.failure}</div>
    {/if}
    {#if r.summary}<p class="summary-text">{r.summary}</p>{/if}
    {#if r.findings.length}
      {#if onfinding}<p class="small faint">Choose a finding to mark its entities.</p>{/if}
      <ol class="findings">
        {#each r.findings as f, i (i)}
          <li class:chosen={finding === i}>
            {#if onfinding}
              <button
                class="finding"
                aria-pressed={finding === i}
                onclick={() => onfinding(finding === i ? null : i)}>{f.text}</button
              >
            {:else}
              <p>{f.text}</p>
            {/if}
            {#if f.unmatchedNames.length}
              <p class="small warn-text">
                <Icon name="alert" size={11} /> Not found among the members: {f.unmatchedNames.join(
                  ', ',
                )}
              </p>
            {/if}
            {#if f.cites.length}
              <details>
                <summary class="small faint"
                  >{f.cites.length} citation{f.cites.length === 1 ? '' : 's'}</summary
                >
                <ul class="cites">
                  {#each f.cites as c (c)}<li class="mono small">
                      {gr.citeText(c)}
                    </li>{/each}
                </ul>
              </details>
            {/if}
          </li>
        {/each}
      </ol>
    {/if}
    <p class="small faint">
      {#if r.activity}Written by {r.activity.model} from commit {r.sourceCommit}, {fmtInt(
          r.activity.inputTokens,
        )}
        + {fmtInt(r.activity.outputTokens)} tokens.{/if}
      {#if r.droppedCitations || r.droppedUnsupported}
        The checks dropped {r.droppedCitations ?? 0} invalid citation{r.droppedCitations === 1
          ? ''
          : 's'} and
        {r.droppedUnsupported ?? 0} unsupported finding{r.droppedUnsupported === 1 ? '' : 's'}.
      {/if}
    </p>
  {:else}
    <p class="faint">
      This community has no report. It is smaller than the minimum size or still pending.
    </p>
  {/if}
  {#if detail.children.length}
    <h4>Sub-communities</h4>
    <ul class="plain">
      {#each detail.children as c (c.iri)}
        <li>
          {#if onopen}
            <button class="link" onclick={() => onopen(c.iri)}>{c.title ?? short(c.iri)}</button>
          {:else}{c.title ?? short(c.iri)}{/if}
        </li>
      {/each}
    </ul>
  {/if}
  <h4>Members</h4>
  <ul class="members">
    {#each detail.members as m (m.iri)}
      <li>
        <a
          href="{resolve('/explore')}?ds={encodeURIComponent(ds)}&iri={encodeURIComponent(m.iri)}"
          title={m.iri}>{m.label ?? short(m.iri)}</a
        >
      </li>
    {/each}
  </ul>
  {#if detail.members.length < detail.memberCount}
    <p class="small faint">
      The first {detail.members.length} of {fmtInt(detail.memberCount)} members.
    </p>
  {/if}
</div>

<style>
  .community-report {
    min-width: 0;
  }
  .title {
    margin: 0;
    overflow-wrap: anywhere;
  }
  h2.title {
    font-size: var(--fs-lg);
  }
  h4 {
    margin: 14px 0 4px;
    font-size: var(--fs-sm);
  }
  .iri {
    margin: 2px 0;
    overflow-wrap: anywhere;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 6px;
  }
  .status-line {
    flex-wrap: wrap;
    gap: 6px;
    margin: 8px 0;
  }
  .summary-text {
    line-height: 1.5;
  }
  .findings {
    padding-left: 20px;
  }
  .findings li {
    margin-bottom: 8px;
  }
  .findings p {
    margin: 0 0 2px;
  }
  .finding {
    all: unset;
    display: block;
    cursor: pointer;
    border-radius: var(--r-sm);
    padding: 2px 4px;
    margin: 0 -4px 2px;
    overflow-wrap: anywhere;
  }
  .finding:hover {
    background: var(--hover, var(--surface-2));
  }
  .finding:focus-visible {
    outline: 2px solid var(--spark);
  }
  .chosen .finding {
    background: var(--surface-3);
    box-shadow: inset 3px 0 0 var(--spark);
  }
  .cites {
    margin: 4px 0;
    padding-left: 16px;
  }
  .cites li {
    overflow-wrap: anywhere;
  }
  .warn-text {
    color: var(--warn, inherit);
  }
  .members {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    list-style: none;
    padding: 0;
    margin: 0;
    font-size: var(--fs-sm);
  }
  .members li {
    overflow-wrap: anywhere;
  }
  .plain {
    list-style: none;
    padding: 0;
    margin: 0;
    font-size: var(--fs-sm);
  }
  .link {
    border: 0;
    padding: 0;
    background: none;
    color: var(--link, var(--spark));
    font: inherit;
    cursor: pointer;
    text-align: left;
    overflow-wrap: anywhere;
  }
</style>

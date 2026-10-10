<script lang="ts">
  // The question header of a query tab (C18 §6.1): the question, the agent's explanation
  // and assumptions, the terms the query uses as the server's check sees them, and the
  // check's verdict. It sits between the tab strip and the editor.
  import type { TabCheck, TabQuestion } from '$lib/ask';
  import { checkedLine, termDetail } from '$lib/ask';
  import { parseCompact } from '$lib/compact';
  import type { PrefixMap } from '$lib/rdf';
  import Icon from './Icon.svelte';
  import TermView from './TermView.svelte';

  let {
    q,
    check,
    edited,
    checking,
    prefixes,
    canAdmin,
    onrecheck,
    onopen,
    onsave,
    onsuggest,
    onclose,
  }: {
    q: TabQuestion;
    check: TabCheck | undefined;
    /** The query differs from the one the question came with. */
    edited: boolean;
    checking: boolean;
    prefixes: PrefixMap;
    canAdmin: boolean;
    onrecheck: () => void;
    onopen: (iri: string) => void;
    onsave: () => void;
    onsuggest: () => void;
    onclose: () => void;
  } = $props();

  const r = $derived(check?.result);
  const all = $derived({ ...prefixes, ...(r?.prefixes ?? {}) });
  const stale = $derived(!!check && !checking && check.text !== undefined && edited && !r);
  let termsOpen = $state(true);
</script>

<section class="qhead" aria-label="Question">
  <div class="row1">
    <span class="tag">Q</span>
    <div class="body">
      {#if q.question}<p class="question">{q.question}</p>{/if}
      {#if q.explanation}<p class="explanation muted">{q.explanation}</p>{/if}
    </div>
    {#if edited}<span class="badge edited" title="The query differs from the one asked">edited</span
      >{/if}
    <div class="actions">
      {#if canAdmin}
        <button class="btn sm" onclick={onsave} title="Store this query with the question"
          ><Icon name="archive" size={12} /> Save as example</button
        >
      {:else}
        <button
          class="btn sm"
          onclick={onsuggest}
          disabled={!q.question}
          title="Ask an admin to store this query with the question"
          ><Icon name="plus" size={12} /> Suggest as example</button
        >
      {/if}
      <button
        class="btn ghost icon sm"
        aria-label="Remove the question from this tab"
        title="Remove the question from this tab"
        onclick={onclose}><Icon name="x" size={12} /></button
      >
    </div>
  </div>
  {#if q.assumptions?.length}
    <div class="line">
      <span class="key faint">Assumes</span>
      <ul class="plain">
        {#each q.assumptions as a, i (i)}<li>{a}</li>{/each}
      </ul>
    </div>
  {/if}
  {#if r?.terms?.length}
    <div class="line">
      <button
        class="key faint linkish"
        aria-expanded={termsOpen}
        onclick={() => (termsOpen = !termsOpen)}>Terms</button
      >
      {#if termsOpen}
        <ul class="plain terms" aria-label="Terms used">
          {#each r.terms as t (t.term)}
            <li class:missing={!t.occurs}>
              <span class="mono"
                ><TermView term={parseCompact(t.term, all)} prefixes={all} {onopen} /></span
              >
              {#if t.label}<span class="label">"{t.label}"</span>{/if}
              <span class="faint">· {termDetail(t)}</span>
            </li>
          {/each}
        </ul>
      {:else}
        <span class="faint small">{r.terms.length} terms</span>
      {/if}
    </div>
  {/if}
  <div class="line">
    <span class="key faint">Checked</span>
    {#if checking}
      <span class="faint"><span class="spinner"></span> Checking…</span>
    {:else if check?.error}
      <span class="err">{check.error}</span>
    {:else if r}
      <span class:ok={r.issues.length === 0} class:warn={r.issues.length > 0}
        >{#if r.issues.length === 0}<Icon name="check" size={12} />{:else}<Icon
            name="alert"
            size={12}
          />{/if}
        {checkedLine(r)}</span
      >
      {#if r.commit != null}<span class="faint small">at commit {r.commit}</span>{/if}
    {:else}
      <span class="faint">not yet</span>
    {/if}
    {#if edited || stale || check?.error}
      <button class="btn ghost sm" onclick={onrecheck} disabled={checking}>
        <Icon name="refresh" size={12} /> Check again
      </button>
    {/if}
  </div>
  {#if r?.issues.length}
    <ul class="plain issues" aria-label="Issues">
      {#each r.issues as issue, i (i)}
        <li class={issue.severity}>
          <span class="mono small">{issue.code}</span>
          {issue.message}{#if issue.line}<span class="faint">
              (line {issue.line}{#if issue.column}, column {issue.column}{/if})</span
            >{/if}
          {#if issue.suggestions?.length}
            <span class="faint"
              >Try {issue.suggestions
                .map((s) => `${s.term}${s.label ? ` "${s.label}"` : ''}`)
                .join(', ')}.</span
            >
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .qhead {
    display: grid;
    gap: 4px;
    padding: 8px 12px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    font-size: var(--fs-sm);
    max-height: 38vh;
    overflow: auto;
    min-width: 0;
  }
  .row1 {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    min-width: 0;
  }
  .tag {
    flex: none;
    font-weight: 700;
    font-size: var(--fs-xs);
    padding: 1px 6px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--iri) 14%, transparent);
    color: var(--iri);
  }
  .body {
    flex: 1;
    min-width: 0;
  }
  .question {
    margin: 0;
    font-weight: 600;
    font-size: var(--fs-md, 14px);
    color: var(--text);
    overflow-wrap: anywhere;
  }
  .explanation {
    margin: 2px 0 0;
    overflow-wrap: anywhere;
  }
  .actions {
    display: flex;
    gap: 4px;
    flex: none;
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  .badge.edited {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .line {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding-left: 30px;
    flex-wrap: wrap;
    min-width: 0;
  }
  .key {
    flex: none;
    width: 64px;
  }
  .linkish {
    all: unset;
    cursor: pointer;
    width: 64px;
  }
  .linkish:focus-visible {
    box-shadow: var(--focus);
  }
  .plain {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }
  .terms li {
    overflow-wrap: anywhere;
  }
  .terms li.missing {
    color: var(--warn);
  }
  .label {
    color: var(--literal);
  }
  .ok {
    color: var(--ok);
  }
  .warn {
    color: var(--warn);
  }
  .err {
    color: var(--danger);
  }
  .issues {
    padding-left: 102px;
  }
  .issues li.error {
    color: var(--danger);
  }
  .issues li.warning {
    color: var(--warn);
  }
  @media (max-width: 600px) {
    .line,
    .issues {
      padding-left: 0;
    }
  }
</style>

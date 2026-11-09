<script lang="ts">
  // Discussions on entities (ROADMAP item 618), in the manner of WebProtégé's discussions:
  // comment threads on an entity or one of its axioms, with replies, @mentions and
  // resolve and reopen. With `iri` the view is the entity frame's Discussion view and can
  // start a thread; without it the view lists every thread of the ontology, each linking to
  // its entity. Threads are Web Annotation RDF in the ontology graph's discussion graph, and
  // each comment, resolve or reopen is one ordinary commit (lib/discussions.ts).
  import { onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import {
    discussionGraph,
    MAX_TEXT,
    newId,
    newThreadUpdate,
    parseThreads,
    replyUpdate,
    resolveUpdate,
    textParts,
    threadsQuery,
    type Thread,
  } from '$lib/discussions';
  import { fmtCount, fmtRelative } from '$lib/format';
  import { committedSeq, runUpdate, type Selection } from '$lib/ontology';
  import { LatestRun } from '$lib/supersede';

  let {
    target,
    sel,
    iri = null,
    readonly = true,
    user,
    axioms = [],
    name = (i: string) => i,
    refreshKey = 0,
    oncommitted,
    onopen,
  }: {
    /** The dataset on its branch (`ds@branch`). */
    target: string;
    /** The ontology graph whose discussion graph holds the threads. */
    sel: Selection;
    /** The entity, or null for every thread of the ontology. */
    iri?: string | null;
    /** No comment, reply, resolve or reopen is offered. */
    readonly?: boolean;
    /** The caller, named as each comment's creator. */
    user: string;
    /** The entity's axioms in functional-style syntax, which a new thread may be about. */
    axioms?: readonly string[];
    /** The display name of an entity. */
    name?: (iri: string) => string;
    /** The head the editor shows. A new head reads the threads again. */
    refreshKey?: number;
    /** A discussion write committed `seq`. */
    oncommitted?: (seq: number | null) => void;
    /** Opens an entity in the editor. */
    onopen?: (iri: string) => void;
  } = $props();

  let threads = $state<Thread[]>([]);
  let truncated = $state(false);
  let loading = $state(false);
  let error = $state<Error | null>(null);
  let filter = $state<'open' | 'resolved' | 'all'>('open');
  let draft = $state('');
  let about = $state('');
  let replies = $state<Record<string, string>>({});
  let busy = $state<string | null>(null);
  let writeError = $state<string | null>(null);
  let now = $state(Date.now());
  const runs = new LatestRun();
  onDestroy(() => runs.abortAll());

  async function load() {
    const owns = runs.claim('read');
    loading = true;
    try {
      const rows = await api.select(target, threadsQuery(sel.graph, iri), {
        nocache: true,
        signal: runs.signal('read'),
      });
      if (!owns()) return;
      const r = parseThreads(rows as never);
      threads = r.threads;
      truncated = r.truncated;
      error = null;
      now = Date.now();
    } catch (e) {
      if (owns()) error = e as Error;
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    void refreshKey;
    void target;
    void iri;
    void sel.graph;
    writeError = null;
    void load();
  });

  const shown = $derived(
    threads.filter((t) => filter === 'all' || (filter === 'open') === !t.resolved),
  );
  const openCount = $derived(threads.filter((t) => !t.resolved).length);

  async function write(key: string, sparql: () => string, message: string, after?: () => void) {
    busy = key;
    writeError = null;
    try {
      const a = await runUpdate(target, sparql(), { message });
      if ('dryRun' in a) throw new Error('The write was not committed.');
      after?.();
      oncommitted?.(committedSeq(a));
      await load();
    } catch (e) {
      writeError = api.errorMessage(e as Error);
    } finally {
      busy = null;
    }
  }

  const stamp = () => new Date().toISOString();

  function start() {
    if (!iri) return;
    const entity = iri;
    void write(
      'new',
      () =>
        newThreadUpdate(sel.graph, {
          id: newId(),
          entity,
          axiom: about || null,
          text: draft,
          user,
          now: stamp(),
        }),
      `Comment on ${name(entity)}`,
      () => {
        draft = '';
        about = '';
      },
    );
  }

  function reply(t: Thread) {
    void write(
      `reply:${t.id}`,
      () =>
        replyUpdate(sel.graph, {
          id: newId(),
          thread: t.id,
          text: replies[t.id] ?? '',
          user,
          now: stamp(),
        }),
      `Reply on ${name(t.entity)}`,
      () => (replies = { ...replies, [t.id]: '' }),
    );
  }

  function toggle(t: Thread) {
    void write(
      `resolve:${t.id}`,
      () => resolveUpdate(sel.graph, t.id, !t.resolved, user, stamp()),
      `${t.resolved ? 'Reopen' : 'Resolve'} a discussion on ${name(t.entity)}`,
    );
  }
</script>

{#snippet body(text: string)}
  <p class="text">
    {#each textParts(text) as p, i (i)}{#if p.mention}<span class="mention">{p.text}</span
        >{:else}{p.text}{/if}{/each}
  </p>
{/snippet}

<div class="discussions">
  <div class="bar">
    <div class="segmented sm" role="group" aria-label="Show threads">
      <button aria-pressed={filter === 'open'} onclick={() => (filter = 'open')}
        >Open {openCount}</button
      >
      <button aria-pressed={filter === 'resolved'} onclick={() => (filter = 'resolved')}
        >Resolved {threads.length - openCount}</button
      >
      <button aria-pressed={filter === 'all'} onclick={() => (filter = 'all')}>All</button>
    </div>
    <span class="faint small mono graph" title="The graph that holds these threads"
      >{discussionGraph(sel.graph)}</span
    >
  </div>
  {#if error}<p class="error-box" role="alert">{api.errorMessage(error)}</p>{/if}
  {#if writeError}<p class="error-box" role="alert">{writeError}</p>{/if}
  {#if iri && !readonly}
    <form
      class="compose"
      aria-label="New thread"
      onsubmit={(e) => {
        e.preventDefault();
        start();
      }}
    >
      <textarea
        class="input"
        aria-label="Comment"
        placeholder="Start a thread. Mention someone with @name."
        rows="3"
        maxlength={MAX_TEXT}
        bind:value={draft}></textarea>
      <div class="row">
        {#if axioms.length}
          <select class="select sm" aria-label="About" bind:value={about}>
            <option value="">About the entity</option>
            {#each axioms.slice(0, 200) as a (a)}<option value={a}>{a}</option>{/each}
          </select>
        {/if}
        <span class="spacer"></span>
        <button class="btn sm primary" disabled={!draft.trim() || busy != null}
          >{#if busy === 'new'}<span class="spinner"></span>{/if} Comment</button
        >
      </div>
    </form>
  {/if}
  {#if !loading && !error && !shown.length}
    <p class="empty">
      {filter === 'resolved'
        ? 'No resolved thread.'
        : filter === 'open'
          ? iri
            ? 'No open thread on this entity.'
            : 'No open thread in this ontology.'
          : iri
            ? 'No thread on this entity.'
            : 'No thread in this ontology.'}
    </p>
  {/if}
  <ol class="threads" aria-label={iri ? 'Threads on the entity' : 'Threads in the ontology'}>
    {#each shown as t (t.id)}
      <li class:resolved={t.resolved}>
        <article aria-label="Thread by {t.creator}">
          <header>
            {#if !iri}
              <button class="link entity" onclick={() => onopen?.(t.entity)}
                >{name(t.entity)}</button
              >
            {/if}
            <strong>{t.creator}</strong>
            <span class="faint small" title={t.created}>{fmtRelative(t.created, now)}</span>
            {#if t.resolved}<span class="badge"
                >Resolved{t.resolvedBy ? ` by ${t.resolvedBy}` : ''}</span
              >{/if}
          </header>
          {#if t.axiom}<p class="axiom mono small" title="The axiom the thread is about">
              {t.axiom}
            </p>{/if}
          {@render body(t.text)}
          {#if t.replies.length}
            <ul class="replies" aria-label="Replies">
              {#each t.replies as r (r.id)}
                <li>
                  <header>
                    <strong>{r.creator}</strong>
                    <span class="faint small" title={r.created}>{fmtRelative(r.created, now)}</span>
                  </header>
                  {@render body(r.text)}
                </li>
              {/each}
            </ul>
          {/if}
          {#if !readonly}
            <div class="actions">
              {#if !t.resolved}
                <input
                  class="input sm"
                  aria-label="Reply to {t.creator}"
                  placeholder="Reply"
                  maxlength={MAX_TEXT}
                  value={replies[t.id] ?? ''}
                  oninput={(e) => (replies = { ...replies, [t.id]: e.currentTarget.value })}
                />
                <button
                  class="btn sm"
                  disabled={!(replies[t.id] ?? '').trim() || busy != null}
                  onclick={() => reply(t)}>Reply</button
                >
              {/if}
              <button class="btn sm ghost" disabled={busy != null} onclick={() => toggle(t)}
                >{t.resolved ? 'Reopen' : 'Resolve'}</button
              >
            </div>
          {/if}
        </article>
      </li>
    {/each}
  </ol>
  {#if loading}<p class="faint small"><span class="spinner"></span> Reading the threads…</p>{/if}
  {#if truncated}<p class="faint small">
      Showing the first {fmtCount(threads.length, 'thread')} and their replies.
    </p>{/if}
</div>

<style>
  .discussions {
    display: grid;
    /* a long axiom in the picker must not widen the view past its panel */
    grid-template-columns: minmax(0, 1fr);
    gap: var(--sp-2);
    align-content: start;
    min-width: 0;
  }
  .bar,
  .row,
  .actions,
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  .graph {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .compose,
  .threads,
  article {
    min-width: 0;
  }
  .compose {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--sp-2);
  }
  .compose textarea {
    width: 100%;
    box-sizing: border-box;
    resize: vertical;
  }
  .compose select {
    width: 0;
    min-width: 0;
    flex: 1 1 16rem;
    text-overflow: ellipsis;
  }
  .spacer {
    flex: 1;
  }
  .threads {
    grid-template-columns: minmax(0, 1fr);
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: var(--sp-2);
  }
  .threads > li {
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: var(--sp-2) var(--sp-3);
    background: var(--surface);
  }
  .threads > li.resolved {
    opacity: 0.75;
  }
  article {
    display: grid;
    gap: 6px;
  }
  .text {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .mention {
    color: var(--accent);
    font-weight: 600;
  }
  .axiom {
    margin: 0;
    padding: 2px 6px;
    border-left: 2px solid var(--border-strong, var(--border));
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
  .replies {
    margin: 0;
    padding: 0 0 0 var(--sp-3);
    list-style: none;
    display: grid;
    gap: 6px;
    border-left: 2px solid var(--border);
  }
  .actions input {
    flex: 1 1 12rem;
    min-width: 0;
  }
  .entity {
    font-weight: 600;
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>

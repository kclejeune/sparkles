<script lang="ts">
  // The review page of a proposal, ingest or review branch (C18 §7.10): the source text
  // with the cited spans highlighted, the facts the branch proposes with the signals the
  // server checked, the facts of main it retracts, and the entities it would create with
  // their possible duplicates. **Use existing** makes the branch name an existing entity
  // instead of a new one, **Edit value** replaces a fact's object, **Reject** retracts a
  // fact on the branch, and **Merge** opens the merge page, where the person merges. Each
  // action is a commit on the branch as the person, so the grants decide.
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { principalName } from '$lib/memory';
  import * as review from '$lib/review-api';
  import {
    bare,
    factKey,
    KIND_LABELS,
    SIGNALS,
    segments,
    signalClass,
    signalText,
  } from '$lib/review';
  import { LatestRun } from '$lib/supersede';
  import Icon from '$components/Icon.svelte';

  const name = $derived(page.params.name ?? '');
  const branch = $derived(page.params.branch ?? '');
  const mergeHref = $derived(
    `${resolve('/datasets/[name]/merge', { name })}?source=${encodeURIComponent(branch)}`,
  );
  const canWrite = $derived(auth.can(name, 'write'));

  let data = $state<review.BranchReview | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let busy = $state<string | null>(null);
  /** The fact whose span is shown, by key. */
  let focus = $state<string | null>(null);
  /** The fact being edited, by key, and its new object. */
  let editing = $state<string | null>(null);
  let newValue = $state('');
  const runs = new LatestRun();

  async function load() {
    const [ds, b] = [name, branch];
    if (!ds || !b) return;
    const owns = runs.claim('review');
    loading = true;
    error = null;
    try {
      const r = await review.branchReview(ds, b);
      if (owns()) data = r;
    } catch (e) {
      if (owns()) {
        data = null;
        error = api.errorMessage(e);
      }
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    void name;
    void branch;
    void load();
  });

  async function act(key: string, run: () => Promise<unknown>, done: string) {
    busy = key;
    try {
      await run();
      toasts.push('success', done);
      editing = null;
      await load();
    } catch (e) {
      toasts.error('The change was not made', e);
    } finally {
      busy = null;
    }
  }

  const rejectFact = (f: review.ReviewFact) =>
    act(
      `reject ${factKey(f)}`,
      () => review.reject(name, [f], { branch }),
      'Rejected on the branch',
    );

  const saveEdit = (f: review.ReviewFact) =>
    act(
      `edit ${factKey(f)}`,
      () => review.editFact(name, f, newValue.trim(), branch),
      'Edited on the branch',
    );

  const useExisting = (e: review.ReviewEntity, to: string) =>
    act(
      `relink ${e.iri}`,
      () => review.relink(name, branch, e.iri, bare(to)),
      `${e.label ?? e.shown} now names ${to}`,
    );

  /** Each source's text in pieces, with the indexes of the facts citing each piece. */
  const texts = $derived.by(() => {
    if (!data) return [];
    const facts = data.facts;
    return data.sources.map((src) => {
      const spans = facts
        .map((f, i) => ({ f, i }))
        .filter(({ f }) => f.span && bare(f.span.rendition) === bare(src.rendition))
        .map(({ f, i }) => ({ start: f.span!.start, end: f.span!.end, fact: i }));
      return { src, pieces: src.text != null ? segments(src.text, spans) : null };
    });
  });

  const focusIndex = $derived(data?.facts.findIndex((f) => factKey(f) === focus) ?? -1);
  const term = (f: review.ReviewFact, which: 's' | 'o') =>
    (which === 's' ? f.sLabel : f.oLabel) ?? f.shown[which];
</script>

<svelte:head><title>Review {branch} | Sparkles</title></svelte:head>

<div class="page">
  <header class="head">
    <div>
      <h1>
        <Icon name="branch" size={18} />
        Review <span class="mono">{branch}</span>
        {#if data?.kind}<span class="badge spark">{KIND_LABELS[data.kind]}</span>{/if}
      </h1>
      <p class="muted small">
        <a href={resolve('/datasets/[name]', { name })}>{name}</a>
        {#if data?.creator}· by {principalName(data.creator)}{/if}
        {#if data?.ahead != null}· {data.ahead} commit{data.ahead === 1 ? '' : 's'} ahead of main{/if}
        {#if data?.behind}· {data.behind} behind{/if}
        {#if data?.note}· {data.note}{/if}
      </p>
    </div>
    <span class="spacer"></span>
    <button class="btn sm" onclick={load} disabled={loading}
      ><Icon name="refresh" size={13} /> Refresh</button
    >
    <a class="btn primary sm" href={mergeHref}><Icon name="merge" size={13} /> Merge</a>
  </header>

  {#if loading && !data}
    <p class="faint"><span class="spinner"></span></p>
  {:else if error}
    <div class="error-box">{error}</div>
  {:else if data}
    {#if data.facts.length === 0 && data.retracts.length === 0}
      <p class="faint">
        The branch proposes no fact with a reifier. The merge page shows every change.
      </p>
    {/if}

    <div class="cols">
      <div class="col">
        <section class="panel" aria-label="Proposed facts">
          <div class="panel-head">
            <h2>Proposed facts</h2>
            <span class="faint small">{data.facts.length}</span>
          </div>
          <ul class="facts panel-body">
            {#each data.facts as f, i (factKey(f))}
              <li class:focus={focusIndex === i}>
                <button
                  class="triple linkish"
                  onclick={() => (focus = focus === factKey(f) ? null : factKey(f))}
                  aria-label="Show the passage of {term(f, 's')} {f.shown.p} {term(f, 'o')}"
                >
                  <span title={f.s}>{term(f, 's')}</span>
                  <span class="mono small faint">{f.shown.p}</span>
                  <span title={f.o}>{term(f, 'o')}</span>
                </button>
                <div class="signals">
                  {#if f.signals}
                    {#each SIGNALS as sig (sig.key)}
                      {@const text = signalText(
                        sig.key,
                        f.signals[sig.key],
                        f.candidates?.length ?? 0,
                      )}
                      {#if text}<span class="badge {signalClass(f.signals[sig.key])}">{text}</span
                        >{/if}
                    {/each}
                  {/if}
                  {#if f.confidence}<span class="faint small">confidence {f.confidence}</span>{/if}
                  <span class="mono small faint">{f.shown.graph}</span>
                </div>
                {#if f.quote}<blockquote class="small">{f.quote}</blockquote>{/if}
                {#if f.notes?.length}<div class="small error-text">{f.notes.join(' ')}</div>{/if}
                {#if editing === factKey(f)}
                  <form
                    class="row edit"
                    onsubmit={(e) => {
                      e.preventDefault();
                      void saveEdit(f);
                    }}
                  >
                    <input class="input mono" bind:value={newValue} aria-label="New value" />
                    <button
                      class="btn primary sm"
                      type="submit"
                      disabled={!newValue.trim() || busy != null}>Save</button
                    >
                    <button class="btn sm" type="button" onclick={() => (editing = null)}
                      >Cancel</button
                    >
                  </form>
                {:else}
                  <div class="row">
                    <button
                      class="btn sm"
                      disabled={!canWrite || busy != null}
                      onclick={() => {
                        editing = factKey(f);
                        newValue = f.o;
                      }}>Edit value</button
                    >
                    <button
                      class="btn danger sm"
                      disabled={!canWrite || busy != null}
                      onclick={() => rejectFact(f)}
                    >
                      {#if busy === `reject ${factKey(f)}`}<span class="spinner"></span>{/if}
                      Reject
                    </button>
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        </section>

        {#if data.retracts.length}
          <section class="panel" aria-label="Retracted facts">
            <div class="panel-head"><h2>Retracts from main</h2></div>
            <ul class="facts panel-body">
              {#each data.retracts as f (factKey(f))}
                <li>
                  <span class="triple">
                    <span>−</span>
                    <span title={f.s}>{term(f, 's')}</span>
                    <span class="mono small faint">{f.shown.p}</span>
                    <span title={f.o}>{term(f, 'o')}</span>
                  </span>
                  <span class="mono small faint">{f.shown.graph}</span>
                </li>
              {/each}
            </ul>
          </section>
        {/if}

        {#if data.entities.length}
          <section class="panel" aria-label="New entities">
            <div class="panel-head"><h2>New entities</h2></div>
            <ul class="facts panel-body">
              {#each data.entities as e (e.iri)}
                <li>
                  <div class="triple">
                    <strong>{e.label ?? e.shown}</strong>
                    <span class="mono small faint">{e.types.join(', ')}</span>
                  </div>
                  {#if e.candidates.length}
                    <div class="small">Possible duplicates:</div>
                    {#each e.candidates as c (c.iri)}
                      <div class="row">
                        <span>{c.label ?? c.shown}</span>
                        <span class="mono small faint">{c.shown}</span>
                        <button
                          class="btn sm"
                          disabled={!canWrite || busy != null}
                          onclick={() => useExisting(e, c.iri)}
                          aria-label="Use existing {c.label ?? c.shown} for {e.label ?? e.shown}"
                        >
                          {#if busy === `relink ${e.iri}`}<span class="spinner"></span>{/if}
                          Use existing
                        </button>
                      </div>
                    {/each}
                  {:else}
                    <div class="faint small">No other entity has this label.</div>
                  {/if}
                </li>
              {/each}
            </ul>
          </section>
        {/if}
        {#if data.rejected}
          <p class="faint small">
            {data.rejected} fact{data.rejected === 1 ? ' was' : 's were'} proposed and rejected on this
            branch.
          </p>
        {/if}
      </div>

      <div class="col">
        {#each texts as t (t.src.rendition)}
          <section
            class="panel"
            aria-label="Source {t.src.title ?? t.src.source ?? t.src.rendition}"
          >
            <div class="panel-head">
              <h2>{t.src.title ?? 'Source'}</h2>
              <span class="mono small faint">{t.src.source ?? t.src.rendition}</span>
            </div>
            <div class="panel-body">
              {#if t.pieces}
                <pre class="source">{#each t.pieces as p (p.start)}{#if p.facts.length}<mark
                        class:focus={p.facts.includes(focusIndex)}
                        title="{p.facts.length} fact{p.facts.length === 1 ? '' : 's'}"
                        >{p.text}</mark
                      >{:else}{p.text}{/if}{/each}</pre>
              {:else if t.src.textOmitted}
                <p class="faint small">The text is too long to show here.</p>
              {:else}
                <p class="faint small">The dataset keeps no text of this source.</p>
              {/if}
            </div>
          </section>
        {:else}
          <p class="faint small">The facts cite no registered source.</p>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .page {
    padding: 16px;
    display: grid;
    gap: 14px;
    align-content: start;
    min-width: 0;
    overflow: auto;
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }
  .head h1 {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin: 0;
    font-size: var(--fs-lg);
    overflow-wrap: anywhere;
  }
  .head p {
    margin: 4px 0 0;
  }
  .cols {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 420px), 1fr));
    gap: 14px;
    align-items: start;
  }
  .col {
    display: grid;
    gap: 14px;
    min-width: 0;
  }
  .facts {
    list-style: none;
    margin: 0;
    display: grid;
    gap: 8px;
  }
  .facts li {
    display: grid;
    gap: 4px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--border);
    min-width: 0;
  }
  .facts li:last-child {
    border-bottom: 0;
  }
  .facts li.focus {
    background: var(--spark-soft);
    border-radius: var(--r);
  }
  .triple {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    overflow-wrap: anywhere;
    text-align: left;
  }
  .linkish {
    border: 0;
    background: none;
    padding: 0;
    font: inherit;
    color: inherit;
    cursor: pointer;
  }
  .signals {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
  }
  blockquote {
    margin: 0;
    padding-left: 8px;
    border-left: 2px solid var(--border);
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
  .edit .input {
    flex: 1;
    min-width: 0;
  }
  .row {
    flex-wrap: wrap;
  }
  .source {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: inherit;
    font-size: var(--fs-sm);
    max-height: 70vh;
    overflow: auto;
  }
  mark {
    background: color-mix(in srgb, var(--spark) 22%, transparent);
    color: inherit;
    border-radius: 2px;
  }
  mark.focus {
    background: color-mix(in srgb, var(--spark) 55%, transparent);
  }
  .error-text {
    color: var(--danger);
  }
</style>

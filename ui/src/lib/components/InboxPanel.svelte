<script lang="ts">
  // The review inbox of C18 §8.9: the unreviewed session facts by session, each with the
  // span, link, guard and corroboration signals the server checked, and the open review
  // branches. **Accept all that pass** selects without promoting, so the person sees the
  // selection first. **Promote selected** writes the facts into the target graph on a
  // review branch and opens its merge page, where the person merges. **Reject selected**
  // retracts them with a message that names the reviewer.
  import { goto } from '$app/navigation';
  import { base } from '$app/paths';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { principalName } from '$lib/memory';
  import * as review from '$lib/review-api';
  import {
    factKey,
    KIND_LABELS,
    passingKeys,
    reviewHref,
    SIGNALS,
    signalClass,
    signalText,
  } from '$lib/review';
  import { LatestRun } from '$lib/supersede';
  import Icon from './Icon.svelte';

  let {
    ds,
    canWrite = true,
    oncount,
  }: {
    ds: string;
    /** The caller may write somewhere in the dataset; the server checks each action. */
    canWrite?: boolean;
    /** The number of open items, for the tab. */
    oncount?: (n: number) => void;
  } = $props();

  let data = $state<review.Inbox | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let selected = $state<Set<string>>(new Set());
  let corroborated = $state(false);
  let target = $state('');
  let reason = $state('');
  let acting = $state<'promote' | 'reject' | null>(null);
  const runs = new LatestRun();

  async function load() {
    const name = ds;
    const owns = runs.claim('inbox');
    loading = true;
    error = null;
    try {
      const r = await review.inbox(name);
      if (!owns()) return;
      data = r;
      target = target || r.target || '';
      selected = new Set([...selected].filter((k) => allFacts.some((f) => factKey(f) === k)));
      oncount?.(r.open);
    } catch (e) {
      if (owns()) {
        data = null;
        error =
          e instanceof api.ApiError && e.status === 404
            ? 'This server has no review inbox.'
            : api.errorMessage(e);
      }
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    if (ds) void load();
  });

  const allFacts = $derived(data?.sessions.flatMap((s) => s.facts) ?? []);
  const chosen = $derived(allFacts.filter((f) => selected.has(factKey(f))));

  function toggle(f: review.ReviewFact, on: boolean) {
    const next = new Set(selected);
    if (on) next.add(factKey(f));
    else next.delete(factKey(f));
    selected = next;
  }

  function acceptPassing() {
    selected = passingKeys(allFacts, corroborated);
    toasts.push(
      'info',
      `${selected.size} fact${selected.size === 1 ? '' : 's'} pass${selected.size === 1 ? 'es' : ''} every check`,
    );
  }

  async function promote() {
    if (!chosen.length) return;
    acting = 'promote';
    try {
      const r = await review.promote(ds, chosen, target.trim() ? { target: target.trim() } : {});
      toasts.push(
        'success',
        `Promoted ${r.promoted} fact${r.promoted === 1 ? '' : 's'} on ${r.branch}`,
      );
      selected = new Set();
      goto(
        `${base}/datasets/${encodeURIComponent(ds)}/merge?source=${encodeURIComponent(r.branch)}`,
      );
    } catch (e) {
      toasts.error('The action failed', e);
    } finally {
      acting = null;
    }
  }

  async function reject() {
    if (!chosen.length) return;
    acting = 'reject';
    try {
      const r = await review.reject(ds, chosen, reason.trim() ? { reason: reason.trim() } : {});
      toasts.push('success', `Rejected ${r.rejected} fact${r.rejected === 1 ? '' : 's'}`);
      selected = new Set();
      reason = '';
      await load();
    } catch (e) {
      toasts.error('The action failed', e);
    } finally {
      acting = null;
    }
  }

  const when = (t?: string) => (t ? t.slice(0, 16).replace('T', ' ') : '');
  const term = (f: review.ReviewFact, which: 's' | 'o') =>
    (which === 's' ? f.sLabel : f.oLabel) ?? f.shown[which];
</script>

<section class="inbox" aria-label="Inbox">
  {#if loading && !data}
    <p class="faint"><span class="spinner"></span></p>
  {:else if error}
    <div class="error-box">{error}</div>
  {:else if data}
    {#if data.sessions.length === 0 && data.branches.length === 0}
      <p class="faint">Nothing waits for review.</p>
    {/if}

    {#each data.sessions as s (s.graph)}
      <section class="group" aria-label="Session {s.shown}">
        <h3>
          <span class="badge">Session</span>
          <span>{s.by.map(principalName).join(', ') || 'unknown agent'}</span>
          {#if s.first}<span class="faint small">{when(s.first)}</span>{/if}
          <span class="mono small faint">{s.shown}</span>
        </h3>
        <ul class="facts">
          {#each s.facts as f (factKey(f))}
            <li>
              <label class="fact">
                <input
                  type="checkbox"
                  checked={selected.has(factKey(f))}
                  onchange={(e) => toggle(f, e.currentTarget.checked)}
                  aria-label="Select {term(f, 's')} {f.shown.p} {term(f, 'o')}"
                />
                <span class="triple">
                  <span title={f.s}>{term(f, 's')}</span>
                  <span class="mono small faint">{f.shown.p}</span>
                  <span title={f.o}>{term(f, 'o')}</span>
                </span>
              </label>
              <span class="signals">
                {#if f.signals}
                  {#each SIGNALS as sig (sig.key)}
                    {@const text = signalText(
                      sig.key,
                      f.signals[sig.key],
                      f.candidates?.length ?? 0,
                    )}
                    {#if text}
                      <span class="badge {signalClass(f.signals[sig.key])}">{text}</span>
                    {/if}
                  {/each}
                {/if}
                {#if f.confidence}<span class="faint small">confidence {f.confidence}</span>{/if}
              </span>
              {#if f.candidates?.length}
                <div class="small faint">
                  Also matches {f.candidates.map((c) => c.label ?? c.shown).join(', ')}
                </div>
              {/if}
              {#if f.quote}<blockquote class="small">{f.quote}</blockquote>{/if}
            </li>
          {/each}
        </ul>
      </section>
    {/each}

    {#each data.branches as b (b.name)}
      <section class="group" aria-label="{KIND_LABELS[b.kind]} {b.name}">
        <h3>
          <span class="badge spark">{KIND_LABELS[b.kind]}</span>
          <span class="mono">{b.name}</span>
          {#if b.facts != null}<span class="small"
              >{b.facts} fact{b.facts === 1 ? '' : 's'}{#if b.retracts}
                · retracts {b.retracts}{/if}</span
            >{/if}
          {#if b.note}<span class="faint small">{b.note}</span>{/if}
          <a class="btn sm" href={reviewHref(base, ds, b.name)}
            >Open <Icon name="chevron" size={12} /></a
          >
        </h3>
      </section>
    {/each}

    {#if data.truncated}
      <p class="faint small">More facts wait than the inbox lists. Review these first.</p>
    {/if}

    {#if allFacts.length}
      <div class="actions" role="group" aria-label="Inbox actions">
        <span class="small">{chosen.length} selected</span>
        <button class="btn sm" onclick={acceptPassing}>Accept all that pass</button>
        <label class="small row"
          ><input type="checkbox" bind:checked={corroborated} /> require corroboration</label
        >
        <span class="spacer"></span>
        <label class="small row target">
          Promote into
          <input
            class="input mono"
            bind:value={target}
            placeholder="https://example.org/memory/consolidated"
            aria-label="Promote into"
          />
        </label>
        <button
          class="btn primary sm"
          disabled={!canWrite || !chosen.length || acting != null || !target.trim()}
          onclick={promote}
        >
          {#if acting === 'promote'}<span class="spinner"></span>{/if}
          Promote selected
        </button>
        <input
          class="input reason"
          bind:value={reason}
          placeholder="reason (optional)"
          aria-label="Reason"
        />
        <button
          class="btn danger sm"
          disabled={!canWrite || !chosen.length || acting != null}
          onclick={reject}
        >
          {#if acting === 'reject'}<span class="spinner"></span>{/if}
          Reject selected
        </button>
      </div>
    {/if}
  {/if}
</section>

<style>
  .inbox {
    display: grid;
    gap: 12px;
    min-width: 0;
  }
  .group {
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
    padding: 10px 12px;
    display: grid;
    gap: 8px;
    min-width: 0;
  }
  .group h3 {
    margin: 0;
    font-size: var(--fs-sm);
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    overflow-wrap: anywhere;
  }
  .group h3 .btn {
    margin-left: auto;
  }
  .facts {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }
  .facts li {
    display: grid;
    gap: 3px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--border);
    min-width: 0;
  }
  .facts li:last-child {
    border-bottom: 0;
    padding-bottom: 0;
  }
  .fact {
    display: flex;
    gap: 8px;
    align-items: baseline;
    min-width: 0;
  }
  .triple {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .signals {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
    padding-left: 24px;
  }
  blockquote {
    margin: 0 0 0 24px;
    padding-left: 8px;
    border-left: 2px solid var(--border);
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
  .actions {
    position: sticky;
    bottom: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2, var(--surface));
  }
  .target {
    flex: 1 1 260px;
    min-width: 0;
  }
  .target .input {
    flex: 1;
    min-width: 0;
  }
  .reason {
    flex: 0 1 180px;
    min-width: 0;
  }
</style>

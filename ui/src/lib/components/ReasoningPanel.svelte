<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { fmtInt, fmtRelative } from '$lib/format';
  import type { PrefixMap } from '$lib/rdf';
  import DiagnosticsPanel from './DiagnosticsPanel.svelte';
  import Icon from './Icon.svelte';

  let {
    name,
    info,
    busy = false,
    readOnly = false,
    prefixes,
    explore,
    onstart,
    onchanged,
  }: {
    name: string;
    info: api.DatasetInfo | undefined;
    /** Another action is starting; disables the task buttons. */
    busy?: boolean;
    readOnly?: boolean;
    prefixes: PrefixMap;
    /** Explorer URL of an IRI. */
    explore: (iri: string) => string;
    /** Start a task with a toast (the page's task runner). */
    onstart: (label: string, fn: () => Promise<api.Task>) => Promise<void>;
    /** Something changed on the server: refresh the dataset. */
    onchanged: () => void;
  } = $props();

  // --- status ----------------------------------------------------------------
  let status = $state<api.ReasoningStatus | null>(null);
  let now = $state(Date.now());

  async function loadStatus() {
    try {
      status = await api.reasonStatus(name);
    } catch {
      // older servers have no status route: fall back to the dataset info
      status = null;
    }
  }

  $effect(() => {
    // reload when the dataset or its recorded reasoning changes
    void name;
    void JSON.stringify(info?.reasoning ?? null);
    void loadStatus();
  });

  onMount(() => {
    const t = setInterval(() => {
      now = Date.now();
      // follow a planned automatic run
      if (status?.auto.scheduledAt || (status?.auto.enabled && status.stale)) void loadStatus();
    }, 2000);
    return () => clearInterval(t);
  });

  const r = $derived(info?.reasoning ?? null);
  const stale = $derived(status ? status.stale : (r?.stale ?? null));
  const since = $derived(status ? status.commitsSince : (r?.commitsSince ?? null));
  const commit = $derived(status ? status.commit : (r?.commit ?? null));
  const freshness = $derived.by(() => {
    if (!r) return null;
    if (stale === false) return { cls: 'ok', label: 'Up to date' };
    if (stale === true)
      return {
        cls: 'warn',
        label:
          since != null
            ? `Stale · ${fmtInt(since)} commit${since === 1 ? '' : 's'} since`
            : 'Stale',
      };
    return { cls: '', label: 'Freshness unknown' };
  });
  const nextRunIn = $derived.by(() => {
    const at = status?.auto.scheduledAt;
    if (!at) return null;
    return Math.max(0, Math.round((new Date(at).getTime() - now) / 1000));
  });

  // --- materialization -------------------------------------------------------
  let profile = $state<api.ReasonProfile>('rdfs');
  let rules = $state(`# Jena rule syntax. rdf:, rdfs:, owl: and xsd: are predefined.
# Add a built-in rule set with:  @include <rdfs> .   (or <owl-rl>)
@prefix ex: <http://example.org/> .

# Everyone who authored something is a Researcher.
[author: (?p ex:authorOf ?d) -> (?p rdf:type ex:Researcher)]`);
  let dropping = $state(false);

  async function dropInf() {
    dropping = true;
    try {
      await api.dropInferences(name);
      toasts.push('success', 'Dropped inferred triples');
      onchanged();
    } catch (e) {
      toasts.error('Could not drop inferences', e);
    } finally {
      dropping = false;
    }
  }
</script>

<section class="panel">
  <div class="panel-head">
    <h2>Reasoning</h2>
    <span class="spacer"></span>
    {#if freshness}
      <span
        class="badge {freshness.cls}"
        title={status?.staleReason ?? (stale === null ? 'No recorded store position' : '')}
      >
        <Icon name={stale === false ? 'check' : 'alert'} size={12} />
        {freshness.label}
      </span>
    {/if}
  </div>
  <div class="panel-body reason">
    {#if r}
      <div class="status" class:stale={stale !== false}>
        <span class="line">
          <strong>{r.profile}</strong>
          {[
            '',
            `${fmtInt(r.inferred)} inferred`,
            ...(commit != null ? [`at commit ${commit}`] : []),
            fmtRelative(r.at, now),
          ].join(' · ')}
        </span>
        {#if stale !== false}
          <span class="faint why">
            {#if stale === true}
              {status?.staleReason
                ? `${status.staleReason[0].toUpperCase()}${status.staleReason.slice(1)}.`
                : 'The data changed since the inferences were materialized.'}
              Queries may miss new entailments or keep ones of deleted data.
            {:else}
              These inferences were recorded without a store position, so it is unknown whether they
              are up to date.
            {/if}
          </span>
          <button
            class="btn primary sm rerun"
            disabled={busy || readOnly}
            onclick={() => onstart('Reasoning', () => api.rerunReasoning(name))}
            title="Re-run {r.profile} on the current data"
          >
            <Icon name="refresh" size={13} /> Re-run reasoning
          </button>
        {/if}
        {#if status?.auto.enabled}
          <span class="faint auto">
            Auto re-run: on ({status.auto.debounceSeconds ?? 0} s after the last update){#if nextRunIn != null},
              next run in {nextRunIn} s{/if}
          </span>
        {/if}
        {#if status?.warnings.length}
          <span class="faint">Warnings: {status.warnings.join('; ')}</span>
        {/if}
      </div>
    {/if}
    <div class="profiles" role="radiogroup" aria-label="Reasoning profile">
      {#each [{ v: 'rdfs', l: 'RDFS', d: 'subClassOf, subPropertyOf, domain, range' }, { v: 'owl-rl', l: 'OWL 2 RL', d: 'RDFS plus inverse, symmetric, transitive, sameAs…' }, { v: 'rules', l: 'Custom rules', d: 'Your own rules, Jena rule syntax' }] as const as p (p.v)}
        <label class="opt" class:sel={profile === p.v}>
          <input type="radio" bind:group={profile} value={p.v} />
          <span><strong>{p.l}</strong><span class="faint">{p.d}</span></span>
        </label>
      {/each}
    </div>
    {#if profile === 'rules'}
      <textarea
        class="textarea"
        rows="7"
        bind:value={rules}
        spellcheck="false"
        aria-label="Custom rules"></textarea>
    {/if}
    <div class="row">
      <button class="btn" onclick={dropInf} disabled={dropping || !r || readOnly}>
        {#if dropping}<span class="spinner"></span>{:else}<Icon name="trash" size={13} />{/if} Drop inferences
      </button>
      <span class="spacer"></span>
      <button
        class="btn primary"
        disabled={busy || readOnly || (profile === 'rules' && !rules.trim())}
        onclick={() => onstart('Reasoning', () => api.reason(name, profile, rules))}
      >
        <Icon name="wand" size={14} /> Materialize inferences
      </button>
    </div>
  </div>
  <DiagnosticsPanel {name} hasInferences={!!r} {stale} {prefixes} {explore} />
</section>

<style>
  .reason {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
  }
  .status {
    display: grid;
    gap: 6px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    font-size: var(--fs-sm);
  }
  .status.stale {
    border-color: color-mix(in srgb, var(--warn) 45%, transparent);
    background: color-mix(in srgb, var(--warn) 6%, transparent);
  }
  .rerun {
    justify-self: start;
  }
  .reason > .row {
    flex-wrap: wrap;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .profiles {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
  }
  .opt {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    cursor: pointer;
    font-size: var(--fs-sm);
  }
  .opt > span {
    display: grid;
    gap: 2px;
  }
  .opt.sel {
    border-color: var(--iri);
    background: color-mix(in srgb, var(--iri) 6%, transparent);
  }
  .opt input {
    margin-top: 2px;
    accent-color: var(--iri);
  }
  @media (max-width: 760px) {
    .profiles {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>

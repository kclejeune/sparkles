<script lang="ts">
  // Server-side ingestion status line and admin controls (spec C19 item 77), shared by
  // IngestPanel (dataset page) and IngestSettings (memory page).
  import { base } from '$app/paths';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import * as askApi from '$lib/ask-api';
  import {
    datasetKindUrl,
    describeIngestState,
    disableIngest,
    enableIngest,
    readKind,
    sourceReason,
    valueText,
    type IngestAvailability,
    type SettingsKind,
  } from '$lib/settings';
  import Modal from './Modal.svelte';

  let {
    ds,
    canAdmin = false,
    onstate,
  }: {
    ds: string;
    canAdmin?: boolean;
    onstate?: (state: { on: boolean; loading: boolean }) => void;
  } = $props();

  let kind = $state<SettingsKind | null>(null);
  let assistant = $state<askApi.AssistantSettings | null>(null);
  let loading = $state(true);
  let unsupported = $state(false);
  let busy = $state(false);
  let confirmOpen = $state(false);

  const enc = encodeURIComponent;

  const availability: IngestAvailability = $derived(
    describeIngestState(kind, assistant?.status ?? null),
  );

  const settingsHref = (field?: string) =>
    `${base}/datasets/${enc(ds)}?tab=settings${field ? `#setting-assistant-${field}` : ''}`;

  async function load() {
    const name = ds;
    loading = true;
    unsupported = false;
    try {
      const [kRes, aRes] = await Promise.allSettled([
        readKind(datasetKindUrl(name, 'assistant')),
        askApi.assistantSettings(name),
      ]);
      if (name !== ds) return;
      if (kRes.status === 'fulfilled') {
        kind = kRes.value;
      } else {
        kind = null;
        const e = kRes.reason;
        if (e instanceof api.ApiError && (e.status === 404 || e.status === 405 || e.status === 501))
          unsupported = true;
      }
      assistant = aRes.status === 'fulfilled' ? aRes.value : null;
      if (!kind && !assistant) unsupported = true;
    } finally {
      if (name === ds) {
        loading = false;
      }
    }
  }

  $effect(() => {
    if (ds) void load();
  });

  $effect(() => {
    onstate?.({ on: unsupported ? true : availability.on, loading });
  });

  async function turnOn() {
    busy = true;
    try {
      const res = await enableIngest(ds);
      kind = res.kind;
      confirmOpen = false;
      toasts.push(
        'success',
        res.changed ? 'Server ingestion turned on' : 'Server ingestion settings were unchanged',
      );
      await load();
    } catch (e) {
      toasts.error('Could not turn on server ingestion', e);
    } finally {
      busy = false;
    }
  }

  async function turnOff() {
    busy = true;
    try {
      kind = await disableIngest(ds);
      toasts.push('success', 'Server ingestion turned off');
      await load();
    } catch (e) {
      toasts.error('Could not turn off server ingestion', e);
    } finally {
      busy = false;
    }
  }
</script>

{#if !unsupported && !loading}
  <div
    class="ingest-state"
    class:off={!availability.on}
    role="status"
    data-testid="server-ingest-status"
  >
    <div class="state-line">
      {#if availability.on}
        <span class="small">
          <strong>Server ingestion is on:</strong> documents go to
          {#each availability.activeProviders as p, i (`${p.provider}/${p.model}`)}
            {#if i > 0}{', '}{/if}<code class="mono">{p.provider}/{p.model}</code>
          {/each}.
        </span>
        {#if canAdmin && !availability.ingestLocked}
          <button class="btn sm" disabled={busy} onclick={turnOff}>Turn off server ingestion</button
          >
        {/if}
      {:else}
        <span class="small">
          <strong>Server ingestion is off:</strong> uploaded documents are stored as sources without
          facts{#if availability.reason}
            {' '}({availability.reason}){/if}.
        </span>
        {#if canAdmin}
          {#if availability.canEnableDirectly}
            <button class="btn primary sm" disabled={busy} onclick={() => (confirmOpen = true)}
              >Turn on server ingestion</button
            >
          {:else if availability.blockingField}
            <a class="btn sm" href={settingsHref(availability.blockingField.field)}
              >Open {availability.blockingField.field} in Settings</a
            >
          {/if}
        {/if}
      {/if}
    </div>
  </div>
{/if}

<Modal bind:open={confirmOpen} title="Turn on server ingestion" width={500} closeDisabled={busy}>
  {#if availability.plan}
    <p class="small">
      Turning on server ingestion changes only default assistant fields and leaves configured or
      locked settings untouched.
    </p>
    {#if availability.plan.changed.length}
      <div class="dialog-section">
        <h3>Fields that will change</h3>
        <ul>
          {#each availability.plan.changed as c (c.field)}
            <li>
              <code class="mono">{c.field}</code>:
              <code class="mono">{valueText(c.current)}</code> &rarr;
              <code class="mono">{valueText(c.wanted)}</code>
            </li>
          {/each}
        </ul>
      </div>
    {/if}
    {#if availability.plan.unchanged.length}
      <div class="dialog-section">
        <h3>Fields that stay as configured</h3>
        <ul>
          {#each availability.plan.unchanged as u (u.field)}
            <li>
              <code class="mono">{u.field}</code> stays
              <code class="mono">{valueText(u.current)}</code> ({sourceReason(u.source)})
            </li>
          {/each}
        </ul>
      </div>
    {/if}
    <div class="dialog-section">
      <h3>Extract providers</h3>
      {#if availability.providers.length}
        <ul>
          {#each availability.providers as p (`${p.provider}/${p.model}`)}
            <li>
              <code class="mono">{p.provider}/{p.model}</code>
              {#if !p.receivesDocuments && p.send !== 'documents' && kind?.effective?.sendByProvider}
                <span class="faint">
                  (per-provider limit: <code class="mono">{p.send}</code>)
                </span>
              {/if}
            </li>
          {/each}
        </ul>
      {:else}
        <p class="faint small">No provider and model are configured in the extract role.</p>
      {/if}
    </div>
    <p class="faint small">
      Setting <code class="mono">send</code> to <code class="mono">documents</code> also lets query result
      rows go to the summarize providers for summaries.
    </p>
  {/if}
  {#snippet actions()}
    <button class="btn sm" disabled={busy} onclick={() => (confirmOpen = false)}>Cancel</button>
    <button class="btn primary sm" disabled={busy} onclick={turnOn}>Turn on server ingestion</button
    >
  {/snippet}
</Modal>

<style>
  .ingest-state {
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
    width: 100%;
    box-sizing: border-box;
  }
  .ingest-state.off {
    border-color: color-mix(in srgb, var(--warn, var(--border)) 45%, var(--border));
  }
  .state-line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .small {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .dialog-section {
    display: grid;
    gap: 4px;
  }
  .dialog-section h3 {
    margin: 0;
    font-size: var(--fs-sm);
  }
  .dialog-section ul {
    margin: 0;
    padding-left: 18px;
    font-size: var(--fs-sm);
  }
</style>

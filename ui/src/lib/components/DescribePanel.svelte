<script lang="ts">
  // The dataset page's DESCRIBE setting (`/$/describe/{ds}`): how DESCRIBE describes a
  // resource in this dataset. Everyone who can read the dataset sees it; an admin of the
  // dataset changes it or goes back to the server's defaults.
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import {
    describeBody,
    describeChanged,
    describeForm,
    describeSummary,
    MODE_TEXT,
    type DescribeForm,
  } from '$lib/describe';

  let {
    name,
    canEdit = false,
  }: {
    name: string;
    /** Admin on the dataset and a writable server. */
    canEdit?: boolean;
  } = $props();

  let setting = $state<api.DescribeSetting | null>(null);
  let form = $state<DescribeForm | null>(null);
  let error = $state<string | null>(null);
  let unsupported = $state(false);
  let saving = $state(false);

  async function load() {
    try {
      const s = await api.describeSetting(name);
      setting = s;
      form = describeForm(s);
      error = null;
    } catch (e) {
      if (e instanceof api.ApiError && (e.status === 404 || e.status === 501)) unsupported = true;
      else error = api.errorMessage(e);
    }
  }

  $effect(() => {
    void name;
    setting = null;
    form = null;
    unsupported = false;
    void load();
  });

  const body = $derived(form ? describeBody(form) : null);
  const invalid = $derived(body && 'error' in body ? body.error : null);
  const changed = $derived(!!(form && setting && describeChanged(form, setting)));

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (!body || 'error' in body) return;
    saving = true;
    try {
      const s = await api.setDescribeSetting(name, body.ok);
      setting = s;
      form = describeForm(s);
      toasts.push('info', 'DESCRIBE setting saved', describeSummary(s));
    } catch (err) {
      toasts.error('Saving the DESCRIBE setting failed', err);
    } finally {
      saving = false;
    }
  }

  async function reset() {
    saving = true;
    try {
      const s = await api.clearDescribeSetting(name);
      setting = s;
      form = describeForm(s);
      toasts.push('info', 'DESCRIBE uses the defaults', describeSummary(s));
    } catch (err) {
      toasts.error('Resetting the DESCRIBE setting failed', err);
    } finally {
      saving = false;
    }
  }
</script>

<section class="panel" data-testid="describe-setting">
  <div class="panel-head">
    <h2>DESCRIBE</h2>
    <span class="spacer"></span>
    {#if setting}
      <span class="badge" title="Where the setting comes from">
        {setting.source === 'dataset' ? 'dataset setting' : 'defaults'}
      </span>
    {/if}
  </div>
  <div class="panel-body">
    {#if error}
      <p class="faint">Could not load the DESCRIBE setting: {error}</p>
    {:else if unsupported}
      <p class="faint">This server has no DESCRIBE setting per dataset.</p>
    {:else if !setting || !form}
      <p class="faint"><span class="spinner"></span> Loading…</p>
    {:else}
      <form class="describe" onsubmit={save}>
        <label class="field">
          Mode
          <select class="select" bind:value={form.mode} disabled={!canEdit}>
            {#each setting.modes as m (m)}
              <option value={m}>{m}</option>
            {/each}
          </select>
          <span class="faint small">{MODE_TEXT[form.mode] ?? ''}</span>
        </label>
        <div class="checks">
          <label class="check">
            <input type="checkbox" bind:checked={form.labels} disabled={!canEdit} />
            <span
              >Labels <span class="faint">(rdfs:label and skos:prefLabel of linked IRIs)</span
              ></span
            >
          </label>
          <label class="check">
            <input type="checkbox" bind:checked={form.reifiers} disabled={!canEdit} />
            <span>Reifiers <span class="faint">(the reifiers of described triples)</span></span>
          </label>
        </div>
        <div class="limits">
          <label class="field">
            Max triples
            <input
              class="input"
              inputmode="numeric"
              placeholder="no limit"
              bind:value={form.maxTriples}
              disabled={!canEdit}
              aria-invalid={invalid?.startsWith('Max triples') ?? false}
            />
          </label>
          <label class="field">
            Max depth
            <input
              class="input"
              inputmode="numeric"
              placeholder="no limit"
              bind:value={form.maxDepth}
              disabled={!canEdit}
              aria-invalid={invalid?.startsWith('Max depth') ?? false}
            />
          </label>
        </div>
        {#if invalid}<p class="bad">{invalid}</p>{/if}
        {#if canEdit}
          <div class="row actions">
            <button class="btn primary sm" type="submit" disabled={saving || !changed || !!invalid}>
              Save
            </button>
            <button
              class="btn sm"
              type="button"
              onclick={reset}
              disabled={saving || setting.source === 'default'}
            >
              Use the defaults
            </button>
          </div>
        {/if}
        <p class="faint small">
          A query can lower the limits with <code>describe-max-triples</code> and
          <code>describe-max-depth</code>, and choose another mode with <code>describe</code>.
        </p>
      </form>
    {/if}
  </div>
</section>

<style>
  .describe {
    display: grid;
    gap: 10px;
    font-size: var(--fs-sm);
  }
  .checks {
    display: grid;
    gap: 4px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .limits {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 12rem));
    gap: 10px;
  }
  .actions {
    gap: 6px;
  }
  .bad {
    color: var(--danger);
    margin: 0;
  }
  .small {
    font-size: var(--fs-xs, 11px);
    font-weight: 400;
  }
  p {
    margin: 0;
  }
  @media (max-width: 600px) {
    .limits {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>

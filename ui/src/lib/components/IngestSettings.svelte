<script lang="ts">
  // The ingest settings of C18 §7.3 and §7.4: whether the dataset keeps the text of its
  // sources, and the named profiles that narrow what an agent extracts. An admin edits
  // them, and everyone else reads them.
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import * as review from '$lib/review-api';
  import { LatestRun } from '$lib/supersede';

  let { ds, canAdmin = false }: { ds: string; canAdmin?: boolean } = $props();

  let data = $state<review.IngestProfiles | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  /** The profile being edited, and its fields as text. */
  let editing = $state<string | null>(null);
  let form = $state({ name: '', classes: '', predicates: '', labelPredicate: '', language: '' });
  const runs = new LatestRun();

  async function load() {
    const name = ds;
    const owns = runs.claim('ingest');
    error = null;
    try {
      const r = await review.ingestProfiles(name);
      if (owns()) data = r;
    } catch (e) {
      if (owns()) {
        data = null;
        error =
          e instanceof api.ApiError && e.status === 404
            ? 'This server has no ingest settings.'
            : api.errorMessage(e);
      }
    }
  }

  $effect(() => {
    if (ds) void load();
  });

  const lines = (t: string) =>
    t
      .split(/[\s,]+/)
      .map((x) => x.trim())
      .filter(Boolean);

  async function setKeepText(on: boolean) {
    busy = true;
    try {
      await review.putKeepText(ds, on);
      toasts.push('success', on ? 'Sources keep their text' : 'Sources keep no text');
      await load();
    } catch (e) {
      toasts.error('The setting was not saved', e);
    } finally {
      busy = false;
    }
  }

  function edit(name: string | null) {
    const p = name ? (data?.profiles[name] ?? {}) : {};
    editing = name ?? '';
    form = {
      name: name ?? '',
      classes: (p.classes ?? []).join('\n'),
      predicates: (p.predicates ?? []).join('\n'),
      labelPredicate: p.labelPredicate ?? '',
      language: p.language ?? '',
    };
  }

  async function save() {
    const name = form.name.trim();
    if (!name) return;
    const p: review.IngestProfile = {};
    if (lines(form.classes).length) p.classes = lines(form.classes);
    if (lines(form.predicates).length) p.predicates = lines(form.predicates);
    if (form.labelPredicate.trim()) p.labelPredicate = form.labelPredicate.trim();
    if (form.language.trim()) p.language = form.language.trim();
    busy = true;
    try {
      await review.putProfile(ds, name, p);
      toasts.push('success', `Saved the profile ${name}`);
      editing = null;
      await load();
    } catch (e) {
      toasts.error('The profile was not saved', e);
    } finally {
      busy = false;
    }
  }

  async function remove(name: string) {
    busy = true;
    try {
      await review.deleteProfile(ds, name);
      toasts.push('success', `Removed the profile ${name}`);
      await load();
    } catch (e) {
      toasts.error('The profile was not removed', e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="card" aria-label="Ingest settings">
  <h2>Ingest</h2>
  {#if error}
    <p class="faint">{error}</p>
  {:else if !data}
    <p class="faint"><span class="spinner"></span></p>
  {:else}
    <label class="row small">
      <input
        type="checkbox"
        checked={data.keepText}
        disabled={!canAdmin || busy}
        onchange={(e) => setKeepText(e.currentTarget.checked)}
      />
      Keep the text of sources
    </label>
    {#if !data.keepText}
      <p class="faint small">
        Sources keep only their digest and length, so every fact from them needs a quote.
      </p>
    {/if}
    <h3>Profiles</h3>
    <ul class="profiles">
      {#each Object.entries(data.profiles) as [name, p] (name)}
        <li>
          <span class="mono">{name}</span>
          <span class="faint small">
            {p.classes?.length ?? 'all'} classes · {p.predicates?.length ?? 'all'} predicates
            {#if p.language}· {p.language}{/if}
          </span>
          {#if canAdmin}
            <span class="spacer"></span>
            <button class="btn sm" disabled={busy} onclick={() => edit(name)}>Edit</button>
            <button
              class="btn danger sm"
              disabled={busy}
              onclick={() => remove(name)}
              aria-label="Remove the profile {name}">Remove</button
            >
          {/if}
        </li>
      {:else}
        <li class="faint small">No profile, so agents extract against the whole schema.</li>
      {/each}
    </ul>
    {#if canAdmin && editing == null}
      <button class="btn sm" onclick={() => edit(null)}>New profile</button>
    {/if}
    {#if editing != null}
      <form
        class="edit"
        aria-label="Ingest profile"
        onsubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <label
          >Name <input
            class="input mono"
            bind:value={form.name}
            readonly={editing !== ''}
            placeholder="people"
          /></label
        >
        <label
          >Classes <textarea class="input mono" rows="3" bind:value={form.classes}
          ></textarea></label
        >
        <label
          >Predicates <textarea class="input mono" rows="3" bind:value={form.predicates}
          ></textarea></label
        >
        <label>Label predicate <input class="input mono" bind:value={form.labelPredicate} /></label>
        <label>Language <input class="input" bind:value={form.language} placeholder="en" /></label>
        <div class="row">
          <button class="btn primary sm" type="submit" disabled={busy || !form.name.trim()}
            >Save profile</button
          >
          <button class="btn sm" type="button" onclick={() => (editing = null)}>Cancel</button>
        </div>
      </form>
    {/if}
  {/if}
</section>

<style>
  h3 {
    margin: 10px 0 4px;
    font-size: var(--fs-sm);
  }
  .profiles {
    list-style: none;
    margin: 0 0 8px;
    padding: 0;
    display: grid;
    gap: 4px;
  }
  .profiles li {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }
  .edit {
    display: grid;
    gap: 6px;
  }
  .edit label {
    display: grid;
    gap: 2px;
    font-size: var(--fs-sm);
  }
</style>

<script lang="ts">
  // One layered settings kind (C19 §10): a form for its common fields, each with its
  // source, lock and reset, and an editor of the whole runtime layer for the members the
  // form leaves out. Writes send `If-Match`. A 412 reloads the kind, a 409 highlights the
  // locked fields, and a 400 shows the server's message. The kind is named by its URL, so
  // a server-wide kind uses the panel as a dataset's kind does.
  import type { Snippet } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import {
    conflicts,
    dirtyFields,
    fieldInfo,
    formOf,
    formPatch,
    patchKind,
    readKind,
    resetKind,
    runtimePatch,
    runtimeText,
    writeFailure,
    type FieldDef,
    type FormValues,
    type SettingsKind,
  } from '$lib/settings';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';
  import SettingRow from './SettingRow.svelte';

  let {
    title,
    text,
    url,
    fields,
    canEdit = false,
    forbiddenText = 'Changing these settings needs admin on the dataset.',
    groupExtra,
    onchange,
  }: {
    title: string;
    /** What the kind covers, in a line. */
    text?: string;
    /** The kind's route, such as `/$/settings/{ds}/assistant`. */
    url: string;
    fields: readonly FieldDef[];
    /** The caller may change the settings. */
    canEdit?: boolean;
    /** What a 403 on a write says. */
    forbiddenText?: string;
    /** Shown after the fields of a group, by the group's name. */
    groupExtra?: Snippet<[string]>;
    /** The kind after a load or a write. */
    onchange?: (k: SettingsKind) => void;
  } = $props();

  const slug = $derived(title.toLowerCase().replace(/[^a-z0-9]+/g, '-'));
  const lower = $derived(title.toLowerCase());

  let k = $state<SettingsKind | null>(null);
  let loadError = $state<string | null>(null);
  let unsupported = $state(false);
  let initial = $state<FormValues>({});
  let form = $state<FormValues>({});
  let busy = $state(false);
  /** A write was refused with 403: the tab stays read-only. */
  let forbidden = $state(false);
  let writeError = $state<string | null>(null);
  let conflictFields = $state<string[]>([]);
  let fieldErrors = $state<Record<string, string>>({});
  let jsonText = $state('');
  let jsonError = $state<string | null>(null);
  let resetAllOpen = $state(false);

  const editable = $derived(canEdit && !forbidden);
  const dirty = $derived(dirtyFields(fields, initial, form));
  const jsonDirty = $derived(k != null && jsonText !== runtimeText(k));
  const runtimeCount = $derived(k ? Object.keys(k.runtime ?? {}).length : 0);
  const groups = $derived.by(() => {
    const out: { name: string; defs: FieldDef[] }[] = [];
    for (const d of fields) {
      const name = d.group ?? '';
      const g = out.find((x) => x.name === name);
      if (g) g.defs.push(d);
      else out.push({ name, defs: [d] });
    }
    return out;
  });

  /** Show a kind as the server answered it, dropping unsaved edits. */
  function show(next: SettingsKind) {
    k = next;
    initial = formOf(fields, next.effective);
    form = { ...initial };
    jsonText = runtimeText(next);
    jsonError = null;
    fieldErrors = {};
    onchange?.(next);
  }

  async function load() {
    const at = url;
    try {
      const next = await readKind(at);
      if (at !== url) return;
      show(next);
      loadError = null;
    } catch (e) {
      if (at !== url) return;
      if (e instanceof api.ApiError && (e.status === 404 || e.status === 501) && !e.code)
        unsupported = true;
      else loadError = api.errorMessage(e);
    }
  }

  $effect(() => {
    void url;
    k = null;
    unsupported = false;
    loadError = null;
    forbidden = false;
    writeError = null;
    conflictFields = [];
    void load();
  });

  /** Run a write; the answer is the kind after it. */
  async function write(what: string, fn: (etag: string | undefined) => Promise<SettingsKind>) {
    busy = true;
    writeError = null;
    try {
      const next = await fn(k?.etag);
      conflictFields = [];
      show(next);
      toasts.push('success', what);
      return true;
    } catch (e) {
      const f = writeFailure(e);
      switch (f.kind) {
        case 'stale':
          conflictFields = [];
          await load();
          toasts.push(
            'info',
            `Someone else changed the ${lower} settings`,
            'The section shows their version now. Make your change again.',
          );
          break;
        case 'locked':
          conflictFields = f.fields;
          writeError = f.message;
          break;
        case 'forbidden':
          forbidden = true;
          writeError = forbiddenText;
          break;
        case 'invalid':
          writeError = f.message;
          break;
        default:
          toasts.error(`The ${lower} settings were not saved`, e);
      }
      return false;
    } finally {
      busy = false;
    }
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (!k) return;
    const effective = k.effective;
    const r = formPatch(fields, initial, form, effective);
    fieldErrors = r.errors;
    if (Object.keys(r.errors).length) return;
    if (r.changed.length === 0) {
      form = { ...initial };
      return;
    }
    await write(`Saved the ${lower} settings`, (etag) => patchKind(url, r.patch, etag));
  }

  function resetField(def: FieldDef) {
    void write(`Reset ${def.label}`, (etag) => resetKind(url, def.path, etag));
  }

  async function resetAll() {
    if (await write(`Reset the ${lower} settings`, (etag) => resetKind(url, null, etag)))
      resetAllOpen = false;
  }

  async function saveJson() {
    if (!k) return;
    const r = runtimePatch(k, jsonText);
    if ('error' in r) {
      jsonError = r.error;
      return;
    }
    jsonError = null;
    if (!r.patch) return;
    const patch = r.patch;
    await write(`Saved the ${lower} settings`, (etag) => patchKind(url, patch, etag));
  }

  const setValue = (path: string, v: string | boolean) => (form = { ...form, [path]: v });
  const inputId = (d: FieldDef) => `setting-${slug}-${d.path.replace(/[^A-Za-z0-9]+/g, '-')}`;
  const placeholderOf = (d: FieldDef) =>
    d.placeholder ?? (d.fallback !== undefined ? String(d.fallback) : '');
</script>

<section class="panel kind" aria-label="{title} settings" data-testid="settings-{slug}">
  <div class="panel-head">
    <h2>{title}</h2>
    {#if k && !k.status.valid}
      <span class="badge danger" title={k.status.error}
        ><Icon name="alert" size={11} /> not valid</span
      >
    {/if}
    {#if k && runtimeCount}
      <span class="badge spark" title="Fields changed here or through the API"
        >{runtimeCount} changed</span
      >
    {/if}
    <span class="spacer"></span>
    {#if editable && k && runtimeCount}
      <button
        class="btn sm"
        disabled={busy}
        onclick={() => (resetAllOpen = true)}
        title="Remove every runtime change of this kind">Reset all</button
      >
    {/if}
    <button class="btn ghost icon sm" aria-label="Reload {title}" onclick={load} disabled={busy}
      ><Icon name="refresh" size={13} /></button
    >
  </div>
  <div class="panel-body">
    {#if text}<p class="faint intro">{text}</p>{/if}
    {#if unsupported}
      <p class="faint">This server has no layered settings.</p>
    {:else if loadError}
      <div class="error-box"><strong>Could not load the settings.</strong> {loadError}</div>
    {:else if !k}
      <p class="faint"><span class="spinner"></span> Loading…</p>
    {:else}
      {#if !k.status.valid}
        <div class="error-box" role="alert">
          <strong>The effective {lower} settings are not valid.</strong>
          {k.status.error ?? ''} The features that read them treat the problem as a missing provider or
          setting until it is fixed.
        </div>
      {/if}
      <form onsubmit={save} aria-label="{title} settings form">
        {#each groups as g (g.name)}
          <fieldset class="group">
            {#if g.name}<legend>{g.name}</legend>{/if}
            {#each g.defs as d (d.path)}
              {@const info = fieldInfo(k, d.path)}
              {@const id = inputId(d)}
              {@const disabled = !editable || info.locked || busy}
              <SettingRow
                {id}
                label={d.label}
                help={d.help}
                {info}
                conflict={conflicts(d.path, conflictFields)}
                dirty={dirty.includes(d.path)}
                error={fieldErrors[d.path]}
                canEdit={editable}
                {busy}
                onreset={() => resetField(d)}
              >
                {#if d.type === 'bool'}
                  <label class="check">
                    <input
                      {id}
                      type="checkbox"
                      checked={form[d.path] === true}
                      {disabled}
                      onchange={(e) => setValue(d.path, e.currentTarget.checked)}
                    />
                    <span class="faint">{form[d.path] === true ? 'on' : 'off'}</span>
                  </label>
                {:else if d.type === 'enum'}
                  <select
                    {id}
                    class="select"
                    value={form[d.path]}
                    {disabled}
                    onchange={(e) => setValue(d.path, e.currentTarget.value)}
                  >
                    {#if d.optional}<option value="">not set</option>{/if}
                    {#each d.options ?? [] as o (o)}<option value={o}>{o}</option>{/each}
                  </select>
                {:else if d.type === 'lines' || d.type === 'json'}
                  <textarea
                    {id}
                    class="textarea"
                    rows={d.type === 'json' ? 3 : 2}
                    spellcheck="false"
                    placeholder={placeholderOf(d)}
                    value={String(form[d.path])}
                    {disabled}
                    oninput={(e) => setValue(d.path, e.currentTarget.value)}></textarea>
                {:else}
                  <input
                    {id}
                    class="input"
                    class:mono={d.type === 'text'}
                    inputmode={d.type === 'int'
                      ? 'numeric'
                      : d.type === 'number'
                        ? 'decimal'
                        : undefined}
                    placeholder={placeholderOf(d)}
                    value={String(form[d.path])}
                    {disabled}
                    oninput={(e) => setValue(d.path, e.currentTarget.value)}
                  />
                {/if}
              </SettingRow>
            {/each}
            {#if groupExtra && g.name}{@render groupExtra(g.name)}{/if}
          </fieldset>
        {/each}
        {#if writeError}
          <div class="error-box" role="alert">
            <strong>The {lower} settings were not saved.</strong>
            {writeError}
          </div>
        {/if}
        {#if editable}
          <div class="row actions">
            {#if dirty.length}<span class="faint small"
                >{dirty.length} unsaved change{dirty.length === 1 ? '' : 's'}</span
              >{/if}
            <span class="spacer"></span>
            <button
              type="button"
              class="btn sm"
              disabled={busy || dirty.length === 0}
              onclick={() => {
                form = { ...initial };
                fieldErrors = {};
              }}>Discard</button
            >
            <button class="btn primary sm" type="submit" disabled={busy || dirty.length === 0}>
              {#if busy}<span class="spinner"></span>{/if} Save
            </button>
          </div>
        {/if}
      </form>

      <details class="advanced">
        <summary>Advanced: the runtime layer as JSON</summary>
        <div class="layers">
          <label class="field">
            Runtime layer
            <span class="faint small"
              >The fields changed at runtime. Saving makes the runtime layer equal to this object,
              so removing a member resets it. Role lists, routing and other members the form leaves
              out are edited here.</span
            >
            <textarea
              class="textarea"
              rows="8"
              spellcheck="false"
              aria-label="{title} runtime layer"
              bind:value={jsonText}
              readonly={!editable}></textarea>
          </label>
          <div class="field">
            <span>From the server's settings file</span>
            <span class="faint small">
              {k.locked.length
                ? `Locked: ${k.locked.join(', ')}.`
                : 'The settings file locks no field of this kind.'}
            </span>
            <pre class="declared">{JSON.stringify(k.declared ?? {}, null, 2)}</pre>
          </div>
        </div>
        {#if jsonError}<p class="bad small">{jsonError}</p>{/if}
        {#if editable}
          <div class="row actions">
            <span class="spacer"></span>
            <button
              class="btn sm"
              disabled={busy || !jsonDirty}
              onclick={() => {
                if (k) jsonText = runtimeText(k);
                jsonError = null;
              }}>Discard</button
            >
            <button class="btn primary sm" disabled={busy || !jsonDirty} onclick={saveJson}
              >Save JSON</button
            >
          </div>
        {/if}
      </details>
    {/if}
  </div>
</section>

<Modal bind:open={resetAllOpen} title="Reset the {lower} settings?">
  <p>
    This removes every change made at runtime to the {lower} settings, so each field takes the value of
    the server's settings file or the default.
  </p>
  {#if k}<pre class="declared">{runtimeText(k)}</pre>{/if}
  {#snippet actions()}
    <button class="btn" onclick={() => (resetAllOpen = false)}>Cancel</button>
    <button class="btn danger solid" disabled={busy} onclick={resetAll}>
      {#if busy}<span class="spinner"></span>{/if} Reset all
    </button>
  {/snippet}
</Modal>

<style>
  .kind {
    min-width: 0;
  }
  .intro {
    max-width: 72ch;
    margin: 0 0 8px;
    font-size: var(--fs-sm);
  }
  form {
    display: grid;
    gap: 10px;
  }
  .group {
    margin: 0;
    padding: 0;
    border: 0;
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .group + .group {
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
  .group legend {
    padding: 0 10px 4px;
    font-size: var(--fs-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-3);
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-height: 28px;
    font-size: var(--fs-sm);
  }
  .check input {
    accent-color: var(--iri);
    margin: 0;
  }
  .input,
  .select {
    max-width: 420px;
    width: 100%;
  }
  .textarea {
    width: 100%;
    max-width: 640px;
  }
  .actions {
    flex-wrap: wrap;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .bad {
    color: var(--danger);
  }
  .advanced {
    margin-top: 12px;
    border-top: 1px solid var(--border);
    padding-top: 10px;
  }
  .advanced summary {
    cursor: pointer;
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 500;
  }
  .layers {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 12px;
    margin: 10px 0;
  }
  .layers .field {
    display: grid;
    gap: 4px;
    align-content: start;
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 500;
  }
  .declared {
    margin: 0;
    padding: 6px 8px;
    background: var(--surface-2);
    border-radius: var(--r);
    font-size: var(--fs-sm);
    overflow-x: auto;
    max-height: 240px;
  }
  @media (max-width: 760px) {
    .layers {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>

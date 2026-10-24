<script lang="ts" module>
  import type { JsonObject as Obj } from '$lib/settings';

  /** What a structured field's editor needs beyond its value, from the caller. */
  export type FieldExtras = {
    /** Values to suggest: entries of a list, names of a map. */
    suggestions?: readonly string[];
    /** The models to suggest for a role entry. */
    suggest?: (entry: Obj) => readonly string[];
    /** A map's inherited values, by member path. */
    inherited?: Record<string, unknown>;
    /** A map's names that the declared layer holds. */
    declared?: readonly string[];
  };
</script>

<script lang="ts">
  // One layered settings kind (C19 §10): a form for its fields, each with its source, lock
  // and reset, and an editor of the whole runtime layer for the members the form leaves
  // out. The form and the editor share one change: an edit of the form shows in the
  // editor's JSON, an edit of the JSON shows in the form, and a save sends the runtime
  // layer they agree on, so members the form does not know are kept. Structured fields
  // (lists, role entries, per-model options and headers) have editors of their own, and
  // every field is checked as it is edited. Writes send `If-Match`. A 412 reloads the
  // kind, a 409 highlights the locked fields, and a 400 shows the server's message, beside
  // the field it names when it names one. The kind is named by its URL, so a server-wide
  // kind uses the panel as a dataset's kind does.
  import { tick, type Snippet } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import {
    conflicts,
    dirtyFields,
    fieldAt,
    fieldInfo,
    formState,
    isStructured,
    layerText,
    overridesOf,
    overridesSummary,
    parseLayer,
    parsePath,
    patchKind,
    pathString,
    pendingLayer,
    previewEffective,
    readKind,
    rebase,
    resetFields,
    resetKind,
    runtimeText,
    startsWith,
    valueText,
    writeFailure,
    formOf,
    type FieldDef,
    type FormState,
    type FormValues,
    type JsonObject,
    type SettingsKind,
  } from '$lib/settings';
  import Icon from './Icon.svelte';
  import ListEditor from './ListEditor.svelte';
  import MapEditor from './MapEditor.svelte';
  import Modal from './Modal.svelte';
  import PairsEditor from './PairsEditor.svelte';
  import RecordsEditor from './RecordsEditor.svelte';
  import SecretField, { type SecretView } from './SecretField.svelte';
  import SettingRow from './SettingRow.svelte';

  let {
    title,
    text,
    url,
    fields,
    canEdit = false,
    forbiddenText = 'Changing these settings needs admin on the dataset.',
    declaredName = "the server's settings file",
    groupExtra,
    confirmWrite,
    onchange,
    check,
    extras,
    secrets,
  }: {
    /** Where the declared layer comes from, in words. */
    declaredName?: string;
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
    /**
     * Asked before a save sends a patch: false keeps the patch unsent, as for a change
     * that needs an acknowledgement first.
     */
    confirmWrite?: (patch: JsonObject, effective: JsonObject) => Promise<boolean>;
    /**
     * Checks that need several fields, by the dotted path of the member they concern,
     * given the form and the effective object the pending change gives.
     */
    check?: (form: FormValues, effective: JsonObject) => Record<string, string>;
    /** What a structured field's editor needs, by field. */
    extras?: (def: FieldDef) => FieldExtras;
    /** The secrets a `secret` field names, and how to store a key under one. */
    secrets?: {
      names: readonly string[];
      state: (name: string) => SecretView | null;
      store: (name: string, key: string) => Promise<boolean>;
    };
  } = $props();

  const slug = $derived(title.toLowerCase().replace(/[^a-z0-9]+/g, '-'));
  const lower = $derived(title.toLowerCase());

  let k = $state<SettingsKind | null>(null);
  let loadError = $state<string | null>(null);
  let unsupported = $state(false);
  /** The form of the loaded kind, which marks the fields with unsaved changes. */
  let initial = $state<FormValues>({});
  let st = $state<FormState>({ base: {}, baseForm: {}, form: {} });
  let busy = $state(false);
  /** A write was refused with 403: the tab stays read-only. */
  let forbidden = $state(false);
  let writeError = $state<string | null>(null);
  let conflictFields = $state<string[]>([]);
  /** The messages of a refused write, by the dotted path of the field they name. */
  let serverErrors = $state<Record<string, string>>({});
  let jsonText = $state('');
  let jsonError = $state<string | null>(null);
  let resetAllOpen = $state(false);
  let useConfigOpen = $state(false);
  let formEl = $state<HTMLFormElement | null>(null);

  const editable = $derived(canEdit && !forbidden);
  /** The runtime values that replace a value of the server's configuration. */
  const overrides = $derived(overridesOf(k));
  const dirty = $derived(dirtyFields(fields, initial, st.form));
  /** The runtime layer a save would leave, and what does not read. */
  const pending = $derived(k ? pendingLayer(fields, k, st) : null);
  const crossErrors = $derived(
    check && k && pending ? check(st.form, previewEffective(k, pending.layer)) : {},
  );
  /** Every member error, by its whole dotted path. */
  const memberErrors = $derived({
    ...(pending?.entryErrors ?? {}),
    ...crossErrors,
    ...serverErrors,
  });
  const errorCount = $derived(
    Object.keys(pending?.errors ?? {}).length +
      Object.keys(crossErrors).length +
      (jsonError ? 1 : 0),
  );
  const changed = $derived(pending?.patch != null || dirty.length > 0);
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
    st = formState(fields, next);
    jsonText = runtimeText(next);
    jsonError = null;
    serverErrors = {};
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

  /** Bring the first control with an error into view and focus it. */
  async function focusError() {
    await tick();
    const el = formEl?.querySelector<HTMLElement>(
      '[aria-invalid="true"], .setting .note.bad, .entry-field.bad',
    );
    const target =
      el?.matches('input, select, textarea, button') === true
        ? el
        : el?.closest('.setting')?.querySelector<HTMLElement>('input, select, textarea, button');
    el?.scrollIntoView({ block: 'center', behavior: 'smooth' });
    target?.focus({ preventScroll: true });
  }

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
          if (f.field && fieldAt(fields, f.field)) {
            serverErrors = { [f.field]: f.message };
            void focusError();
          }
          break;
        default:
          toasts.error(`The ${lower} settings were not saved`, e);
      }
      return false;
    } finally {
      busy = false;
    }
  }

  async function save(e?: SubmitEvent) {
    e?.preventDefault();
    if (!k || !pending) return;
    if (errorCount) {
      void focusError();
      return;
    }
    const patch = pending.patch;
    if (!patch) {
      discard();
      return;
    }
    if (confirmWrite && !(await confirmWrite(patch, k.effective))) return;
    await write(`Saved the ${lower} settings`, (etag) => patchKind(url, patch, etag));
  }

  function discard() {
    if (!k) return;
    st = formState(fields, k);
    jsonText = runtimeText(k);
    jsonError = null;
    serverErrors = {};
    writeError = null;
  }

  function resetField(def: FieldDef) {
    void write(`Reset ${def.label}`, (etag) => resetKind(url, def.path, etag));
  }

  async function resetAll() {
    if (await write(`Reset the ${lower} settings`, (etag) => resetKind(url, null, etag)))
      resetAllOpen = false;
  }

  /** Clear only the runtime values that override the server's configuration. */
  async function useConfigForAll() {
    const fields = overrides.map((o) => o.path);
    if (
      await write(
        `The ${lower} settings use the server config for ${fields.length} field${fields.length === 1 ? '' : 's'}`,
        (etag) => resetFields(url, fields, etag),
      )
    )
      useConfigOpen = false;
    // the resets before a refused one were made, so the section shows what is left
    else await load();
  }

  /** An edit of a field: the JSON shows the layer the form now gives. */
  function setValue(path: string, v: unknown) {
    st = { ...st, form: { ...st.form, [path]: v } };
    const p = parsePath(path);
    const left = Object.fromEntries(
      Object.entries(serverErrors).filter(([key]) => !startsWith(parsePath(key), p)),
    );
    if (Object.keys(left).length !== Object.keys(serverErrors).length) {
      serverErrors = left;
      writeError = null;
    }
    if (k && !jsonError) jsonText = layerText(pendingLayer(fields, k, st).layer);
  }

  /** An edit of the JSON: the form shows the effective object it gives. */
  function setJson(text: string) {
    jsonText = text;
    if (!k) return;
    const r = parseLayer(text);
    if ('error' in r) {
      jsonError = r.error;
      return;
    }
    jsonError = null;
    serverErrors = {};
    st = rebase(fields, k, r.layer);
  }

  const inputId = (d: FieldDef) => `setting-${slug}-${d.path.replace(/[^A-Za-z0-9]+/g, '-')}`;
  const placeholderOf = (d: FieldDef) =>
    d.placeholder ?? (d.fallback !== undefined ? String(d.fallback) : '');
  /** The member errors of a structured field, by the path below it. */
  function errorsBelow(d: FieldDef): Record<string, string> {
    const p = parsePath(d.path);
    const out: Record<string, string> = {};
    for (const [key, e] of Object.entries(memberErrors)) {
      const kp = parsePath(key);
      if (startsWith(kp, p) && kp.length > p.length) out[pathString(kp.slice(p.length))] = e;
    }
    return out;
  }
  /** The error a field's row shows under its control. */
  const rowError = (d: FieldDef) =>
    serverErrors[d.path] ??
    crossErrors[d.path] ??
    (isStructured(d) ? pending?.entryErrors[d.path] : pending?.errors[d.path]);
  const sectionStart = (defs: FieldDef[], i: number) =>
    defs[i].section !== undefined && (i === 0 || defs[i - 1].section !== defs[i].section);
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
      {#if overrides.length}
        <div class="overrides-bar" data-testid="overrides-{slug}">
          <Icon name="layers" size={13} />
          <span
            title={overrides
              .map(
                (o) =>
                  `${o.path}: ${valueText(o.runtime, 40)} in place of ${valueText(o.declared, 40)}`,
              )
              .join('\n')}>{overridesSummary(overrides.length)}</span
          >
          <span class="spacer"></span>
          {#if editable}
            <button class="btn sm" disabled={busy} onclick={() => (useConfigOpen = true)}
              >Use server config for all</button
            >
          {/if}
        </div>
      {/if}
      <form onsubmit={save} aria-label="{title} settings form" bind:this={formEl} novalidate>
        {#each groups as g (g.name)}
          <fieldset class="group">
            {#if g.name}<legend>{g.name}</legend>{/if}
            {#each g.defs as d, i (d.path)}
              {@const info = fieldInfo(k, d.path)}
              {@const id = inputId(d)}
              {@const disabled = !editable || info.locked || busy}
              {@const value = st.form[d.path]}
              {@const err = rowError(d)}
              {#if sectionStart(g.defs, i)}<h3 class="section">{d.section}</h3>{/if}
              <SettingRow
                {id}
                label={d.label}
                help={d.help}
                {info}
                conflict={conflicts(d.path, conflictFields)}
                dirty={dirty.includes(d.path)}
                error={err}
                canEdit={editable}
                {busy}
                onreset={() => resetField(d)}
              >
                {#if d.type === 'bool'}
                  <label class="check">
                    <input
                      {id}
                      type="checkbox"
                      checked={value === true}
                      {disabled}
                      onchange={(e) => setValue(d.path, e.currentTarget.checked)}
                    />
                    <span class="faint">{value === true ? 'on' : 'off'}</span>
                  </label>
                {:else if d.type === 'enum'}
                  <select
                    {id}
                    class="select"
                    value={String(value ?? '')}
                    {disabled}
                    aria-invalid={err ? 'true' : undefined}
                    onchange={(e) => setValue(d.path, e.currentTarget.value)}
                  >
                    {#if d.optional}<option value="">not set</option>{/if}
                    {#if value && !(d.options ?? []).includes(String(value))}<option
                        value={String(value)}>{value}</option
                      >{/if}
                    {#each d.options ?? [] as o (o)}<option value={o}>{o}</option>{/each}
                  </select>
                {:else if d.type === 'list'}
                  <ListEditor
                    def={d}
                    {value}
                    {id}
                    {disabled}
                    errors={errorsBelow(d)}
                    suggestions={extras?.(d).suggestions}
                    onchange={(v) => setValue(d.path, v)}
                  />
                {:else if d.type === 'records'}
                  <RecordsEditor
                    def={d}
                    {value}
                    {id}
                    {disabled}
                    errors={errorsBelow(d)}
                    suggest={extras?.(d).suggest}
                    onchange={(v) => setValue(d.path, v)}
                  />
                {:else if d.type === 'map'}
                  {@const x = extras?.(d)}
                  <MapEditor
                    def={d}
                    {value}
                    {id}
                    {disabled}
                    errors={errorsBelow(d)}
                    suggestions={x?.suggestions}
                    inherited={x?.inherited}
                    declared={x?.declared}
                    onchange={(v) => setValue(d.path, v)}
                  />
                {:else if d.type === 'pairs'}
                  <PairsEditor
                    def={d}
                    {value}
                    {id}
                    {disabled}
                    errors={errorsBelow(d)}
                    onchange={(v) => setValue(d.path, v)}
                  />
                {:else if d.type === 'secret'}
                  <SecretField
                    {value}
                    {id}
                    {disabled}
                    placeholder={placeholderOf(d)}
                    names={secrets?.names}
                    keyState={secrets?.state}
                    onstore={secrets?.store}
                    onchange={(v) => setValue(d.path, v)}
                  />
                {:else if d.type === 'lines' || d.type === 'json'}
                  <textarea
                    {id}
                    class="textarea"
                    rows={d.type === 'json' ? 3 : 2}
                    spellcheck="false"
                    placeholder={placeholderOf(d)}
                    value={String(value ?? '')}
                    {disabled}
                    aria-invalid={err ? 'true' : undefined}
                    oninput={(e) => setValue(d.path, e.currentTarget.value)}></textarea>
                {:else}
                  <span class="with-unit">
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
                      value={String(value ?? '')}
                      {disabled}
                      aria-invalid={err ? 'true' : undefined}
                      oninput={(e) => setValue(d.path, e.currentTarget.value)}
                    />
                    {#if d.unit}<span class="unit">{d.unit}</span>{/if}
                  </span>
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
            {#if errorCount}<span class="bad small" role="status"
                >{errorCount} field{errorCount === 1 ? ' needs' : 's need'} a fix</span
              >{:else if dirty.length}<span class="faint small"
                >{dirty.length} unsaved change{dirty.length === 1 ? '' : 's'}</span
              >{/if}
            <span class="spacer"></span>
            <button type="button" class="btn sm" disabled={busy || !changed} onclick={discard}
              >Discard</button
            >
            <button class="btn primary sm" type="submit" disabled={busy || !changed}>
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
              >The fields changed at runtime, with the form's unsaved changes. An edit here shows in
              the form, and members the form does not show are kept. Saving makes the runtime layer
              equal to this object, so removing a member resets it.</span
            >
            <textarea
              class="textarea"
              rows="10"
              spellcheck="false"
              aria-label="{title} runtime layer"
              aria-invalid={jsonError ? 'true' : undefined}
              value={jsonText}
              oninput={(e) => setJson(e.currentTarget.value)}
              readonly={!editable}></textarea>
          </label>
          <div class="field">
            <span>From {declaredName}</span>
            <span class="faint small">
              {k.locked.length
                ? `Locked: ${k.locked.join(', ')}.`
                : 'The settings file locks no field of this kind.'}
            </span>
            <pre class="declared">{JSON.stringify(k.declared ?? {}, null, 2)}</pre>
          </div>
        </div>
        {#if jsonError}<p class="bad small" role="alert">{jsonError}</p>{/if}
        {#if editable}
          <div class="row actions">
            <span class="spacer"></span>
            <button class="btn sm" disabled={busy || (!changed && !jsonError)} onclick={discard}
              >Discard</button
            >
            <button class="btn primary sm" disabled={busy || !changed} onclick={() => save()}
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
    {declaredName} or the default.
  </p>
  {#if k}<pre class="declared">{runtimeText(k)}</pre>{/if}
  {#snippet actions()}
    <button class="btn" onclick={() => (resetAllOpen = false)}>Cancel</button>
    <button class="btn danger solid" disabled={busy} onclick={resetAll}>
      {#if busy}<span class="spinner"></span>{/if} Reset all
    </button>
  {/snippet}
</Modal>

<Modal bind:open={useConfigOpen} title="Use the server config for these fields?" width={520}>
  <p>
    These {lower} fields were changed at runtime and override the server's configuration. Their runtime
    values are removed, so each field takes the server config's value again. Other changes stay.
  </p>
  <table class="data overrides-list">
    <thead><tr><th>Field</th><th>Now</th><th>Server config</th></tr></thead>
    <tbody>
      {#each overrides as o (o.path)}
        <tr>
          <td class="mono">{o.path}</td>
          <td class="mono" title={JSON.stringify(o.runtime)}
            >{o.runtime === null ? 'removed' : valueText(o.runtime, 40)}</td
          >
          <td class="mono" title={JSON.stringify(o.declared)}>{valueText(o.declared, 40)}</td>
        </tr>
      {/each}
    </tbody>
  </table>
  {#snippet actions()}
    <button class="btn" onclick={() => (useConfigOpen = false)}>Cancel</button>
    <button class="btn primary" disabled={busy} onclick={useConfigForAll}>
      {#if busy}<span class="spinner"></span>{/if} Use server config
    </button>
  {/snippet}
</Modal>

<style>
  .kind {
    min-width: 0;
  }
  .overrides-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 8px;
    margin: 0 0 10px;
    padding: 6px 10px;
    border-radius: var(--r);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent);
    background: color-mix(in srgb, var(--warn) 8%, transparent);
    font-size: var(--fs-sm);
    color: var(--text);
  }
  .overrides-bar :global(svg) {
    color: var(--warn);
  }
  .overrides-list {
    margin: 8px 0 0;
    font-size: var(--fs-sm);
  }
  .overrides-list td.mono {
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    overflow-wrap: anywhere;
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
  .section {
    margin: 6px 10px 0;
    font-size: var(--fs-xs);
    font-weight: 600;
    color: var(--text-2);
  }
  .with-unit {
    display: flex;
    align-items: center;
    gap: 6px;
    max-width: 420px;
  }
  .with-unit .input {
    flex: 1 1 auto;
  }
  .unit {
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
  .input[aria-invalid='true'],
  .select[aria-invalid='true'],
  .textarea[aria-invalid='true'] {
    border-color: var(--danger);
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

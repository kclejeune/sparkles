<script lang="ts">
  // The Settings tab's Prefixes section (spec C20 §7): the dataset's effective prefixes
  // with their source, from the well-known ones, the server's settings file and the
  // changes made here. Admins of the dataset add, edit and delete prefixes. A change over
  // a declared prefix, and a removed declared or well-known prefix, offer Use server
  // config. Writes send `If-Match`, and a 412 reloads the section.
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import {
    bindPatch,
    deletePatch,
    prefixField,
    prefixIriError,
    prefixNameError,
    prefixRows,
    prefixSourceTitle,
    prefixesUrl,
    type PrefixRow,
    type PrefixesKind,
  } from '$lib/prefixes-settings';
  import {
    overridesOf,
    overridesSummary,
    patchKind,
    readKind,
    resetFields,
    resetKind,
    writeFailure,
    type SettingsKind,
  } from '$lib/settings';
  import Icon from './Icon.svelte';

  let { name, canEdit = false }: { name: string; canEdit?: boolean } = $props();

  const url = $derived(prefixesUrl(name));
  let k = $state<PrefixesKind | null>(null);
  let loadError = $state<string | null>(null);
  let unsupported = $state(false);
  let busy = $state(false);
  let forbidden = $state(false);
  let writeError = $state<string | null>(null);
  let conflict = $state<string[]>([]);

  let addName = $state('');
  let addIri = $state('');
  let addError = $state<string | null>(null);
  /** The prefix whose IRI is being edited, and the text of the edit. */
  let editing = $state<string | null>(null);
  let editIri = $state('');
  let editError = $state<string | null>(null);

  const editable = $derived(canEdit && !forbidden);
  const rows = $derived(k ? prefixRows(k) : []);
  const overrides = $derived(overridesOf(k));
  const warnings = $derived(k?.warnings ?? []);

  async function load() {
    try {
      k = (await readKind(url)) as PrefixesKind;
      loadError = null;
    } catch (e) {
      // a server without the prefixes kind answers 404 with unknown-kind
      if (e instanceof api.ApiError && e.status === 404) unsupported = true;
      else loadError = api.errorMessage(e);
    }
  }

  $effect(() => {
    void url;
    k = null;
    unsupported = false;
    void load();
  });

  /** Run a write with the ETag last read: a 412 reloads, a 409 names the locked prefixes. */
  async function write(
    done: string,
    failed: string,
    fn: (etag: string | undefined) => Promise<SettingsKind>,
  ): Promise<boolean> {
    busy = true;
    writeError = null;
    conflict = [];
    try {
      k = (await fn(k?.etag)) as PrefixesKind;
      toasts.push('success', done);
      return true;
    } catch (e) {
      const f = writeFailure(e);
      if (f.kind === 'stale') {
        await load();
        toasts.push(
          'info',
          'Someone else changed the prefixes',
          'The section shows their version now. Make your change again.',
        );
      } else if (f.kind === 'locked') {
        conflict = f.fields;
        writeError = f.message;
      } else if (f.kind === 'forbidden') {
        forbidden = true;
        writeError = f.message;
      } else {
        writeError = f.message;
        toasts.error(failed, e);
      }
      return false;
    } finally {
      busy = false;
    }
  }

  async function add(e: SubmitEvent) {
    e.preventDefault();
    const n = addName.trim();
    const iri = addIri.trim();
    addError = prefixNameError(n) ?? prefixIriError(iri);
    if (addError) return;
    if (rows.some((r) => r.name === n && r.iri != null)) {
      addError = `${n}: is bound already. Edit it in the table.`;
      return;
    }
    if (
      await write(`Added the prefix ${n}:`, `The prefix ${n}: was not added`, (etag) =>
        patchKind(url, bindPatch(n, iri), etag),
      )
    ) {
      addName = addIri = '';
    }
  }

  function startEdit(r: PrefixRow) {
    editing = r.name;
    editIri = r.iri ?? r.declared ?? '';
    editError = null;
  }

  async function saveEdit(e: SubmitEvent, r: PrefixRow) {
    e.preventDefault();
    const iri = editIri.trim();
    editError = prefixIriError(iri);
    if (editError) return;
    if (iri === r.iri) {
      editing = null;
      return;
    }
    if (
      await write(
        `Changed the prefix ${r.name}:`,
        `The prefix ${r.name}: was not changed`,
        (etag) => patchKind(url, bindPatch(r.name, iri), etag),
      )
    )
      editing = null;
  }

  const remove = (r: PrefixRow) =>
    write(`Deleted the prefix ${r.name}:`, `The prefix ${r.name}: was not deleted`, (etag) =>
      patchKind(url, deletePatch(r.name), etag),
    );

  const reset = (r: PrefixRow) =>
    write(
      r.declared || r.source === 'removed'
        ? `The prefix ${r.name}: uses the server config again`
        : `Reset the prefix ${r.name}:`,
      `The prefix ${r.name}: was not reset`,
      (etag) => resetKind(url, prefixField(r.name), etag),
    );

  const useServerConfigForAll = () =>
    write('The prefixes use the server config again', 'The prefixes were not reset', (etag) =>
      resetFields(
        url,
        overrides.map((o) => o.path),
        etag,
      ),
    );

  /** The words of a row's reset: what the prefix goes back to. */
  function resetLabel(r: PrefixRow): string {
    if (r.declared !== undefined || r.source === 'removed') return 'Use server config';
    return r.info.overrides !== undefined ? 'Use server config' : 'Reset to default';
  }

  const label = (r: PrefixRow) => (r.name === '' ? '(empty)' : r.name) + ':';
</script>

<section class="panel kind" aria-label="Prefixes settings" data-testid="settings-prefixes">
  <div class="panel-head">
    <h2>Prefixes</h2>
    {#if k && !k.status.valid}
      <span class="badge danger" title={k.status.error}
        ><Icon name="alert" size={11} /> not valid</span
      >
    {/if}
    <span class="spacer"></span>
    <button class="btn ghost icon sm" aria-label="Reload Prefixes" onclick={load} disabled={busy}
      ><Icon name="refresh" size={13} /></button
    >
  </div>
  <div class="panel-body">
    <p class="faint intro">
      The namespace prefixes of the dataset. The query editor completes them and adds their PREFIX
      lines, and results are written with the ones from the server config and those added here.
    </p>
    {#if unsupported}
      <p class="faint">This server keeps no declared prefixes.</p>
    {:else if loadError}
      <div class="error-box"><strong>Could not load the prefixes.</strong> {loadError}</div>
    {:else if !k}
      <p class="faint"><span class="spinner"></span> Loading…</p>
    {:else}
      {#if !k.status.valid}
        <div class="error-box" role="alert">
          <strong>The effective prefixes are not valid.</strong>
          {k.status.error ?? ''}
        </div>
      {/if}
      {#if warnings.length}
        <div class="warn-box" data-testid="prefix-warnings">
          {#each warnings as w (w.prefix)}
            <p>
              <Icon name="alert" size={12} />
              <span class="mono">{w.prefix}:</span> is bound to
              <span class="mono">{w.iri}</span>, which shadows the well-known
              <span class="mono">{w.wellKnown}</span>.
            </p>
          {/each}
        </div>
      {/if}
      {#if overrides.length}
        <div class="overrides-bar" data-testid="overrides-prefixes">
          <Icon name="layers" size={13} />
          <span>{overridesSummary(overrides.length).replace('field', 'prefix')}</span>
          <span class="spacer"></span>
          {#if editable}
            <button class="btn sm" disabled={busy} onclick={useServerConfigForAll}
              >Use server config for all</button
            >
          {/if}
        </div>
      {/if}
      {#if writeError}<p class="bad small" role="alert">{writeError}</p>{/if}
      {#if editable}
        <form class="add" onsubmit={add} aria-label="Add a prefix">
          <label class="field"
            >Prefix <input
              class="input mono"
              bind:value={addName}
              placeholder="ex"
              aria-label="New prefix name"
            /></label
          >
          <label class="field grow"
            >IRI <input
              class="input mono"
              bind:value={addIri}
              placeholder="https://example.org/ns#"
              aria-label="New prefix IRI"
            /></label
          >
          <button class="btn primary sm" type="submit" disabled={busy}
            ><Icon name="plus" size={13} /> Add</button
          >
          {#if addError}<p class="bad small add-error">{addError}</p>{/if}
        </form>
      {/if}
      <div class="scroll-x">
        <table class="data" aria-label="Prefixes">
          <thead>
            <tr><th>Prefix</th><th>IRI</th><th>Source</th><th></th></tr>
          </thead>
          <tbody>
            {#each rows as r (r.name)}
              <tr
                data-prefix={r.name}
                data-source={r.source}
                class:conflict={conflict.includes(prefixField(r.name))}
              >
                <td class="mono">{label(r)}</td>
                <td class="mono small iri">
                  {#if editing === r.name}
                    <form class="edit" onsubmit={(e) => saveEdit(e, r)}>
                      <input
                        class="input mono"
                        bind:value={editIri}
                        aria-label="IRI of {label(r)}"
                      />
                      <button class="btn primary sm" type="submit" disabled={busy}>Save</button>
                      <button type="button" class="btn sm" onclick={() => (editing = null)}
                        >Cancel</button
                      >
                      {#if editError}<span class="bad small">{editError}</span>{/if}
                    </form>
                  {:else if r.iri != null}
                    {r.iri}
                    {#if r.info.overrides !== undefined}
                      <div class="faint small">server config: {String(r.info.overrides)}</div>
                    {/if}
                  {:else}
                    <span class="faint"
                      >removed{#if r.declared}, server config: {r.declared}{/if}</span
                    >
                  {/if}
                  {#if r.warning}
                    <div class="warn small" title={r.warning.message}>
                      <Icon name="alert" size={11} /> shadows the well-known {r.warning.wellKnown}
                    </div>
                  {/if}
                </td>
                <td>
                  <span
                    class="src {r.source.replace(' ', '-')}"
                    class:overrides={r.info.overrides !== undefined}
                    title={prefixSourceTitle(r)}
                    >{#if r.source === 'locked'}<Icon name="lock" size={11} />{/if}
                    {r.info.overrides !== undefined ? 'overrides server config' : r.source}</span
                  >
                </td>
                <td>
                  {#if editable && r.source !== 'locked'}
                    <div class="cell-actions">
                      {#if r.resettable}
                        <button
                          class="btn ghost sm"
                          disabled={busy}
                          onclick={() => reset(r)}
                          aria-label="{resetLabel(r)} for {label(r)}">{resetLabel(r)}</button
                        >
                      {/if}
                      {#if r.iri != null && editing !== r.name}
                        <button
                          class="btn ghost sm"
                          disabled={busy}
                          onclick={() => startEdit(r)}
                          aria-label="Edit {label(r)}">Edit</button
                        >
                        <button
                          class="btn ghost icon sm"
                          disabled={busy}
                          onclick={() => remove(r)}
                          title="Delete the prefix"
                          aria-label="Delete {label(r)}"><Icon name="trash" size={12} /></button
                        >
                      {/if}
                    </div>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
</section>

<style>
  .intro {
    margin: 0 0 10px;
    font-size: var(--fs-sm);
    max-width: 90ch;
  }
  .add {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: 8px;
    margin: 0 0 12px;
  }
  .add .field {
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 500;
  }
  .add .field.grow {
    flex: 1 1 260px;
    min-width: 0;
  }
  .add-error {
    flex-basis: 100%;
    margin: 0;
  }
  .edit {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .edit .input {
    flex: 1 1 220px;
    min-width: 0;
  }
  .iri {
    overflow-wrap: anywhere;
  }
  .src {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--fs-xs);
    font-weight: 500;
    color: var(--text-3);
    white-space: nowrap;
    cursor: help;
  }
  .src.changed {
    color: var(--spark-ink);
  }
  .src.overrides,
  .src.removed {
    color: var(--warn);
  }
  .src.locked {
    color: var(--text-2);
  }
  .warn,
  .warn-box {
    color: var(--warn);
  }
  .warn-box {
    margin: 0 0 10px;
    padding: 6px 10px;
    border-radius: var(--r);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent);
    background: color-mix(in srgb, var(--warn) 8%, transparent);
    font-size: var(--fs-sm);
  }
  .warn-box p {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
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
  }
  tr.conflict td {
    background: color-mix(in srgb, var(--danger) 10%, transparent);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .bad {
    color: var(--danger);
  }
</style>

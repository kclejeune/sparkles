<script lang="ts">
  // The server page's Models section (C19 §11.4), for server administrators: the
  // providers with their status, a form to add one, the model configuration as a layered
  // settings kind with the same sources, locks and resets as a dataset's settings, and the
  // API keys. Keys are write-only. A key typed in Replace is read from the form when it is
  // sent and never kept in the component's state. The Add form starts from a preset or from
  // nothing. Turning a provider's certificate checks off needs an acknowledgement before
  // the change is sent (C19 §11.6).
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { fmtTime } from '$lib/format';
  import {
    MODELS_URL,
    PROVIDER_KINDS,
    PROVIDER_PRESETS,
    deleteSecret,
    enablesInsecure,
    httpsProviders,
    groupProvider,
    listSecrets,
    modelFields,
    modelsStatus,
    newProviderPatch,
    presetForm,
    providerKinds,
    providerNames,
    putSecret,
    removedProviders,
    secretState,
    type ModelsStatus,
    type SecretInfo,
  } from '$lib/models-settings';
  import {
    fieldInfo,
    pathString,
    patchKind,
    readKind,
    resetKind,
    writeFailure,
    type JsonObject,
    type SettingsKind,
  } from '$lib/settings';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';
  import SettingsKindPanel from './SettingsKindPanel.svelte';

  let status = $state<ModelsStatus | null>(null);
  let statusError = $state<string | null>(null);
  let secrets = $state<SecretInfo[] | null>(null);
  let secretsError = $state<string | null>(null);
  /** The `models` kind as the panel last showed it. */
  let kind = $state<SettingsKind | null>(null);
  let kindError = $state<string | null>(null);
  let busy = $state(false);

  let addOpen = $state(false);
  /** The preset the Add form started from, or '' for Custom. */
  let addPreset = $state('');
  let addName = $state('');
  let addKind = $state<string>('openai');
  let addEndpoint = $state('');
  let addSecret = $state('');
  let addModel = $state('');
  let addError = $state<string | null>(null);

  /** The providers a pending write turns certificate checks off for, and its answer. */
  let insecureAsk = $state<{ names: string[]; resolve: (ok: boolean) => void } | null>(null);
  let insecureAck = $state(false);

  let removeProvider = $state<string | null>(null);
  /** The secret whose Replace form is open. */
  let replacing = $state<string | null>(null);
  let removeSecret = $state<SecretInfo | null>(null);

  const fields = $derived(modelFields(kind));
  // the panel starts again when the providers or their protocols change, since its
  // form has a group of fields for each
  const fieldsKey = $derived(JSON.stringify([providerKinds(kind), httpsProviders(kind)]));
  const removed = $derived(removedProviders(kind));

  async function loadStatus() {
    const [m, s] = await Promise.allSettled([modelsStatus(), listSecrets()]);
    if (m.status === 'fulfilled') {
      status = m.value;
      statusError = null;
    } else statusError = api.errorMessage(m.reason);
    if (s.status === 'fulfilled') {
      secrets = s.value.secrets;
      secretsError = null;
    } else secretsError = api.errorMessage(s.reason);
  }

  async function loadKind() {
    try {
      kind = await readKind(MODELS_URL);
      kindError = null;
    } catch (e) {
      kindError = api.errorMessage(e);
    }
  }

  onMount(() => {
    void loadKind();
    void loadStatus();
  });

  /** After a change of the kind: the providers' status and the keys follow it. */
  function onKind(k: SettingsKind) {
    const changed = kind?.etag !== k.etag;
    kind = k;
    if (changed) void loadStatus();
  }

  /**
   * A write outside the panel, with the ETag the panel last read. A 412 reloads the
   * kind, and any other refusal is shown with the server's message.
   */
  async function write(what: string, failed: string, fn: (etag?: string) => Promise<SettingsKind>) {
    busy = true;
    try {
      kind = await fn(kind?.etag);
      toasts.push('success', what);
      void loadStatus();
      return true;
    } catch (e) {
      if (writeFailure(e).kind === 'stale') {
        await loadKind();
        toasts.push(
          'info',
          'Someone else changed the model configuration',
          'The section shows their version now. Make your change again.',
        );
      } else toasts.error(failed, e);
      return false;
    } finally {
      busy = false;
    }
  }

  /** Fill the Add form from a preset, or empty it for Custom. */
  function pickPreset(name: string) {
    addPreset = name;
    const f = presetForm(name);
    // a name typed already stays, unless it is another preset's
    if (!addName.trim() || PROVIDER_PRESETS.some((p) => p.name === addName.trim()))
      addName = f.name;
    addKind = f.kind;
    addEndpoint = f.endpoint;
    addSecret = f.secret;
    addModel = f.model;
    addError = null;
  }

  /**
   * Before a write of the panel: a patch that turns certificate checks off for a provider
   * is sent only after the acknowledgement in the dialog.
   */
  function confirmWrite(patch: JsonObject, effective: JsonObject): Promise<boolean> {
    const names = enablesInsecure(patch, effective);
    if (!names.length) return Promise.resolve(true);
    insecureAck = false;
    return new Promise((resolve) => (insecureAsk = { names, resolve }));
  }

  function answerInsecure(ok: boolean) {
    insecureAsk?.resolve(ok && insecureAck);
    insecureAsk = null;
    insecureAck = false;
  }

  async function addProvider(e: SubmitEvent) {
    e.preventDefault();
    const r = newProviderPatch(providerNames(kind), {
      name: addName,
      kind: addKind,
      endpoint: addEndpoint,
      secret: addSecret,
      model: addModel,
    });
    if ('error' in r) {
      addError = r.error;
      return;
    }
    addError = null;
    const name = addName.trim();
    if (
      await write(`Added provider ${name}`, `Provider ${name} was not added`, (etag) =>
        patchKind(MODELS_URL, r.patch, etag),
      )
    ) {
      addOpen = false;
      addName = addEndpoint = addSecret = addModel = addPreset = '';
    }
  }

  async function confirmRemoveProvider() {
    const name = removeProvider;
    if (!name) return;
    const ok = await write(`Removed provider ${name}`, `Provider ${name} was not removed`, (etag) =>
      patchKind(MODELS_URL, { providers: { [name]: null } }, etag),
    );
    if (ok) removeProvider = null;
  }

  function restoreProvider(path: string, name: string) {
    void write(
      `Provider ${name} uses the server config again`,
      `Provider ${name} was not restored`,
      (etag) => resetKind(MODELS_URL, path, etag),
    );
  }

  /** Whether the operator locked the provider or a field of it, which keeps it. */
  const providerLocked = (name: string) => {
    if (!kind) return false;
    const f = fieldInfo(kind, pathString(['providers', name]));
    return f.locked || f.partlyLocked;
  };

  /** Send the key typed in a Replace form, then clear the form. */
  async function replaceKey(e: SubmitEvent, name: string) {
    e.preventDefault();
    const form = e.currentTarget as HTMLFormElement;
    const input = form.elements.namedItem('value') as HTMLInputElement | null;
    if (!input?.value.trim()) return;
    busy = true;
    try {
      await putSecret(name, input.value);
      toasts.push('success', `Stored a new key for ${name}`);
      replacing = null;
      await loadStatus();
    } catch (err) {
      toasts.error(`The key for ${name} was not stored`, err);
    } finally {
      input.value = '';
      form.reset();
      busy = false;
    }
  }

  async function confirmRemoveSecret() {
    const s = removeSecret;
    if (!s) return;
    busy = true;
    try {
      await deleteSecret(s.name);
      toasts.push(
        'success',
        s.declared
          ? `${s.name} uses the server config's key again`
          : `Removed the stored key of ${s.name}`,
      );
      removeSecret = null;
      await loadStatus();
    } catch (e) {
      toasts.error(`The key of ${s.name} was not removed`, e);
    } finally {
      busy = false;
    }
  }

  const keyOf = (p: { apiKey?: { secret: string; source: string } }) =>
    p.apiKey ? `${p.apiKey.secret} (${p.apiKey.source})` : 'none';
</script>

<section class="models" aria-label="Models" data-testid="models-section">
  <div class="panel">
    <div class="panel-head">
      <h2>Models</h2>
      {#if status}
        <span class="badge {status.configured ? 'ok' : ''}"
          >{status.configured
            ? `${status.providers.length} provider${status.providers.length === 1 ? '' : 's'}`
            : 'not configured'}</span
        >
      {/if}
      <span class="spacer"></span>
      <button class="btn sm" disabled={busy || !kind} onclick={() => (addOpen = !addOpen)}
        ><Icon name="plus" size={13} /> Add provider</button
      >
      <button
        class="btn ghost icon sm"
        aria-label="Reload the models"
        onclick={() => {
          void loadKind();
          void loadStatus();
        }}><Icon name="refresh" size={13} /></button
      >
    </div>
    <div class="panel-body">
      <p class="faint intro">
        The model providers, role lists and keys of the server. A change here applies to the next
        request without a restart and overrides the server's model configuration until it is reset.
        Fields the operator locked cannot be changed.
      </p>
      {#if addOpen}
        <form class="add" onsubmit={addProvider} aria-label="Add a provider">
          <label class="field"
            >Preset
            <select
              class="select"
              value={addPreset}
              onchange={(e) => pickPreset(e.currentTarget.value)}
            >
              <option value="">Custom</option>
              {#each PROVIDER_PRESETS as p (p.name)}<option value={p.name}>{p.label}</option>{/each}
            </select>
          </label>
          <label class="field"
            >Name <input class="input mono" bind:value={addName} placeholder="gateway" /></label
          >
          <label class="field"
            >Protocol
            <select class="select" bind:value={addKind}>
              {#each PROVIDER_KINDS as k (k)}<option value={k}>{k}</option>{/each}
            </select>
          </label>
          <label class="field grow"
            >Endpoint <input
              class="input mono"
              bind:value={addEndpoint}
              placeholder="https://llm.example/v1"
            /></label
          >
          <label class="field"
            >Key (secret name) <input
              class="input mono"
              bind:value={addSecret}
              placeholder="optional"
            /></label
          >
          <label class="field"
            >Model <input class="input mono" bind:value={addModel} placeholder="optional" /></label
          >
          <div class="add-actions">
            <button type="button" class="btn sm" onclick={() => (addOpen = false)}>Cancel</button>
            <button class="btn primary sm" type="submit" disabled={busy}>Add</button>
          </div>
          {#if addError}<p class="bad small">{addError}</p>{/if}
          <p class="faint small">
            A preset fills the fields for a provider with a well-known endpoint, and each stays
            editable. An OpenAI-compatible gateway uses Custom with the openai protocol. The model
            is added to the provider's models. Its other fields, TLS options and the role lists that
            use it can be set below once it is added, or in the runtime layer's JSON under Advanced.
          </p>
        </form>
      {/if}
      {#if statusError}
        <div class="error-box">
          <strong>The providers' status is not available.</strong>
          {statusError}
        </div>
      {:else if !status}
        <p class="faint"><span class="spinner"></span> Loading…</p>
      {:else if status.providers.length === 0}
        <p class="faint">
          No provider is configured. Add one, or give the server a model configuration with
          --model-config.
        </p>
      {:else}
        <div class="scroll-x">
          <table class="data" aria-label="Model providers">
            <thead>
              <tr><th>Provider</th><th>Protocol</th><th>Endpoint</th><th>Key</th><th>Models</th></tr
              >
            </thead>
            <tbody>
              {#each status.providers as p (p.name)}
                <tr data-provider={p.name}>
                  <td class="mono">{p.name}</td>
                  <td class="muted">{p.kind}</td>
                  <td class="mono small">
                    {p.endpoint}
                    {#if p.unverified}
                      <span
                        class="badge danger"
                        title="Certificate verification is off for this provider (tls.insecureSkipVerify). Anyone on the network path can read its API key and prompts."
                        ><Icon name="alert" size={11} /> unverified</span
                      >
                    {:else if p.tls?.caCert?.status === 'unreadable'}
                      <span class="badge danger" title={p.tls.caCert.message}>CA unreadable</span>
                    {:else if p.tls?.verification === 'custom-ca'}
                      <span class="badge" title="Its certificate is checked against tls.caCert too"
                        >custom CA</span
                      >
                    {/if}
                  </td>
                  <td>
                    <span
                      class="badge {p.status === 'ok' ? 'ok' : 'danger'}"
                      title={p.status === 'ok' ? 'The key can be read' : 'The key cannot be read'}
                      >{p.status === 'ok' ? 'ok' : 'key missing'}</span
                    >
                    <span class="faint small">{keyOf(p)}</span>
                  </td>
                  <td class="small">
                    {#each p.models as m, i (m.name)}{i ? ', ' : ''}<span
                        class="mono"
                        class:bad={m.status?.state === 'failing'}
                        title={m.status?.message ?? m.status?.state}>{m.name}</span
                      >{:else}<span class="faint">—</span>{/each}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
      {#if removed.length}
        <div class="removed">
          {#each removed as r (r.path)}
            <div class="removed-row" data-removed={r.name}>
              <Icon name="alert" size={13} />
              <span
                >The provider <span class="mono">{r.name}</span> of the server config is removed here.</span
              >
              <span class="spacer"></span>
              <button
                class="btn sm"
                disabled={busy}
                onclick={() => restoreProvider(r.path, r.name)}
                aria-label="Use server config for provider {r.name}">Use server config</button
              >
            </div>
          {/each}
        </div>
      {/if}
    </div>
  </div>

  {#if kindError}
    <div class="error-box">
      <strong>Could not load the model configuration.</strong>
      {kindError}
    </div>
  {:else if kind}
    {#key fieldsKey}
      <SettingsKindPanel
        title="Model configuration"
        text="Providers, their per-model options, the role lists and routing. Each field shows whether it comes from the server's model configuration or was changed here."
        url={MODELS_URL}
        {fields}
        canEdit
        declaredName="the server's model configuration"
        forbiddenText="Changing the model configuration needs the server-admin permission, and the server must not be read-only."
        onchange={onKind}
        {confirmWrite}
      >
        {#snippet groupExtra(group: string)}
          {@const name = groupProvider(group)}
          {#if name}
            {@const lock = providerLocked(name)}
            <div class="provider-actions">
              <span class="spacer"></span>
              <button
                type="button"
                class="btn danger sm"
                disabled={busy || lock}
                title={lock
                  ? 'The operator locked this provider, or some of its fields, in the settings file'
                  : 'Remove the provider at runtime'}
                onclick={() => (removeProvider = name)}
                aria-label="Remove provider {name}"><Icon name="trash" size={12} /> Remove</button
              >
            </div>
          {/if}
        {/snippet}
      </SettingsKindPanel>
    {/key}
  {/if}

  <div class="panel" data-testid="model-keys">
    <div class="panel-head"><h2>API keys</h2></div>
    <div class="panel-body">
      <p class="faint intro">
        Keys are write-only: the server never sends one back. A key stored here is kept in the data
        directory without encryption and overrides the server config's key until it is removed.
      </p>
      {#if secretsError}
        <div class="error-box">
          <strong>The keys are not available.</strong>
          {secretsError}
        </div>
      {:else if !secrets}
        <p class="faint"><span class="spinner"></span> Loading…</p>
      {:else if secrets.length === 0}
        <p class="faint">No provider names a key.</p>
      {:else}
        <ul class="keys">
          {#each secrets as s (s.name)}
            {@const st = secretState(s)}
            <li class="key" data-secret={s.name}>
              <div class="key-head">
                <span class="mono name">{s.name}</span>
                <span class="badge {st.set ? 'ok' : 'danger'}">{st.set ? 'set' : 'not set'}</span>
                <span class="src" class:overrides={st.overrides} title={st.title}>{st.label}</span>
                {#if s.locked}
                  <span
                    class="src"
                    title="The operator locked this secret in the settings file, so only the server config's key is used"
                    ><Icon name="lock" size={11} /> locked</span
                  >
                {/if}
                {#if s.overridden}
                  <span
                    class="src warn"
                    title="A stored key is ignored because the secret is locked"
                    ><Icon name="alert" size={11} /> stored key ignored</span
                  >
                {/if}
                <span class="spacer"></span>
                {#if !s.locked}
                  <button
                    class="btn sm"
                    disabled={busy}
                    onclick={() => (replacing = replacing === s.name ? null : s.name)}
                    aria-label="Replace the key of {s.name}">Replace</button
                  >
                {/if}
                {#if s.setAt}
                  <button
                    class="btn sm"
                    disabled={busy}
                    onclick={() => (removeSecret = s)}
                    aria-label="{s.declared
                      ? 'Use server config for'
                      : 'Remove the runtime value of'} {s.name}"
                    >{s.declared ? 'Use server config' : 'Remove runtime value'}</button
                  >
                {/if}
              </div>
              <div class="faint small">
                {s.setAt ? `Set ${fmtTime(s.setAt)}. ` : ''}{s.providers.length
                  ? `Used by ${s.providers.join(', ')}.`
                  : 'No provider uses it.'}
              </div>
              {#if replacing === s.name}
                <form class="replace" onsubmit={(e) => replaceKey(e, s.name)}>
                  <input
                    class="input mono"
                    type="password"
                    name="value"
                    autocomplete="off"
                    spellcheck="false"
                    aria-label="New key for {s.name}"
                    placeholder="Paste the key"
                  />
                  <button
                    type="button"
                    class="btn sm"
                    onclick={() => (replacing = null)}
                    disabled={busy}>Cancel</button
                  >
                  <button class="btn primary sm" type="submit" disabled={busy}>Save key</button>
                </form>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </div>
</section>

<Modal
  open={removeProvider != null}
  title="Remove provider {removeProvider ?? ''}?"
  onclose={() => (removeProvider = null)}
>
  <p>
    The provider is removed for the requests that start after this. A provider of the server config
    stays removed until it is restored with Use server config. Role lists that name it must be
    changed first.
  </p>
  {#snippet actions()}
    <button class="btn" onclick={() => (removeProvider = null)}>Cancel</button>
    <button class="btn danger solid" disabled={busy} onclick={confirmRemoveProvider}>
      {#if busy}<span class="spinner"></span>{/if} Remove
    </button>
  {/snippet}
</Modal>

<Modal
  open={insecureAsk != null}
  title="Turn off certificate verification?"
  onclose={() => answerInsecure(false)}
>
  <p>
    Without certificate verification for <span class="mono"
      >{insecureAsk?.names.join(', ') ?? ''}</span
    >, the server cannot tell the provider from anyone who intercepts the connection. The API key
    and every prompt sent to the provider, which can hold data from your datasets, can then be read
    and changed by anyone on the network path.
  </p>
  <p>Prefer a CA certificate (tls.caCert) for an internal CA or a self-signed certificate.</p>
  <label class="ack">
    <input type="checkbox" bind:checked={insecureAck} />
    I understand that the API key and the prompts sent to this provider can be read by anyone on the network
    path.
  </label>
  {#snippet actions()}
    <button class="btn" onclick={() => answerInsecure(false)}>Cancel</button>
    <button class="btn danger solid" disabled={!insecureAck} onclick={() => answerInsecure(true)}
      >Turn off verification</button
    >
  {/snippet}
</Modal>

<Modal
  open={removeSecret != null}
  title={removeSecret?.declared
    ? `Use the server config's key for ${removeSecret?.name}?`
    : `Remove the key of ${removeSecret?.name ?? ''}?`}
  onclose={() => (removeSecret = null)}
>
  <p>
    {#if removeSecret?.declared}
      The key stored here is removed, and the key of the server's configuration is used again.
    {:else}
      The key stored here is removed. The providers that use it cannot be called until a key is set
      again.
    {/if}
  </p>
  {#snippet actions()}
    <button class="btn" onclick={() => (removeSecret = null)}>Cancel</button>
    <button class="btn danger solid" disabled={busy} onclick={confirmRemoveSecret}>
      {#if busy}<span class="spinner"></span>{/if}
      {removeSecret?.declared ? 'Use server config' : 'Remove'}
    </button>
  {/snippet}
</Modal>

<style>
  .models {
    display: grid;
    gap: 16px;
    min-width: 0;
  }
  .intro {
    max-width: 80ch;
    margin: 0 0 10px;
    font-size: var(--fs-sm);
  }
  .add {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 8px 12px;
    margin: 0 0 12px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .add .field {
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
    color: var(--text-2);
    min-width: 0;
  }
  .add .grow {
    flex: 1 1 240px;
  }
  .add-actions {
    display: flex;
    gap: 6px;
  }
  .add p {
    flex-basis: 100%;
    margin: 0;
  }
  .scroll-x {
    overflow-x: auto;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .bad {
    color: var(--danger);
  }
  .removed {
    display: grid;
    gap: 6px;
    margin-top: 10px;
  }
  .removed-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 8px;
    padding: 6px 10px;
    border-radius: var(--r);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent);
    font-size: var(--fs-sm);
  }
  .removed-row :global(svg) {
    color: var(--warn);
  }
  .provider-actions {
    display: flex;
    padding: 2px 10px 6px;
  }
  .keys {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }
  .key {
    display: grid;
    gap: 4px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    min-width: 0;
  }
  .key-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 8px;
  }
  .key .name {
    font-weight: 600;
  }
  .src {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--fs-xs);
    font-weight: 500;
    color: var(--text-3);
    cursor: help;
  }
  .src.overrides,
  .src.warn {
    color: var(--warn);
  }
  .replace {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .ack {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    font-weight: 500;
  }
  .ack input {
    margin-top: 3px;
  }
  .replace .input {
    flex: 1 1 260px;
    max-width: 480px;
  }
</style>

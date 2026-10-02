<script lang="ts">
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import * as b from '$lib/backups';
  import { REPO_NAME } from '$lib/backups-format';
  import Modal from './Modal.svelte';
  import TestReportView from './TestReportView.svelte';

  let {
    open = $bindable(false),
    editing = null,
    taken = [],
    onsaved,
  }: {
    open?: boolean;
    /** The repository to edit (its location is fixed); null adds one. */
    editing?: b.Repository | null;
    /** Names already in use. */
    taken?: string[];
    onsaved?: (r: b.Repository) => void;
  } = $props();

  let name = $state('');
  let type = $state<b.RepositoryType>('fs');
  let path = $state('');
  let bucket = $state('');
  let prefix = $state('');
  let region = $state('');
  let endpoint = $state('');
  let pathStyle = $state(false);
  let allowHttp = $state(false);
  let sse = $state<'' | 'AES256' | 'aws:kms'>('');
  let kmsKeyId = $state('');
  /** The operator-defined credential source (`[credentials.<name>]` of the backup config). */
  let credName = $state('');
  let readonly = $state(false);
  let conditionalWrites = $state(true);
  let maxConcurrency = $state<number | null>(null);
  let upMbps = $state<number | null>(null);
  let downMbps = $state<number | null>(null);
  let verify = $state(true);

  let busy = $state(false);
  let error = $state<string | null>(null);
  /** After adding: the repository and its connection test. */
  let added = $state<b.Repository | null>(null);

  const MB = 1024 * 1024;

  $effect(() => {
    if (!open) return;
    const r = editing;
    added = null;
    error = null;
    name = r?.name ?? '';
    type = r?.type ?? 'fs';
    path = r?.path ?? '';
    bucket = r?.bucket ?? '';
    prefix = r?.prefix ?? '';
    region = r?.region ?? '';
    endpoint = r?.endpoint ?? '';
    pathStyle = r?.pathStyle ?? false;
    allowHttp = r?.allowHttp ?? false;
    sse = r?.sse ?? '';
    kmsKeyId = r?.kmsKeyId ?? '';
    credName = r?.credentials?.source === 'named' ? r.credentials.name : '';
    readonly = r?.readonly ?? false;
    conditionalWrites = r?.conditionalWrites ?? true;
    maxConcurrency = r?.maxConcurrency ?? null;
    upMbps = r?.maxUploadBytesPerSec ? r.maxUploadBytesPerSec / MB : null;
    downMbps = r?.maxDownloadBytesPerSec ? r.maxDownloadBytesPerSec / MB : null;
    verify = true;
  });

  const isEdit = $derived(editing != null);
  const nameOk = $derived(isEdit || (REPO_NAME.test(name) && !taken.includes(name)));
  const httpEndpoint = $derived(/^http:\/\//i.test(endpoint.trim()));
  const locationOk = $derived(
    type === 'fs' ? path.trim().startsWith('/') : bucket.trim().length > 0,
  );
  const credsOk = $derived(type === 'fs' || REPO_NAME.test(credName.trim()));
  const valid = $derived(nameOk && locationOk && credsOk && (!httpEndpoint || allowHttp));

  function config(): b.RepositoryConfig {
    const opt = (s: string) => s.trim() || undefined;
    const c: b.RepositoryConfig = {
      name,
      type,
      readonly,
      conditionalWrites,
      maxConcurrency: maxConcurrency || undefined,
      maxUploadBytesPerSec: upMbps ? Math.round(upMbps * MB) : null,
      maxDownloadBytesPerSec: downMbps ? Math.round(downMbps * MB) : null,
    };
    if (type === 'fs') c.path = path.trim();
    else {
      c.bucket = bucket.trim();
      c.prefix = opt(prefix);
      c.credentials = { source: 'named', name: credName.trim() };
    }
    if (type === 's3') {
      c.region = opt(region);
      c.endpoint = opt(endpoint);
      c.pathStyle = pathStyle || undefined;
      c.allowHttp = allowHttp || undefined;
      c.sse = sse || null;
      if (sse === 'aws:kms') c.kmsKeyId = opt(kmsKeyId);
    }
    return c;
  }

  async function submit(e: Event) {
    e.preventDefault();
    if (!valid || busy) return;
    busy = true;
    error = null;
    try {
      if (editing) {
        const r = await b.updateRepository(editing.name, config());
        toasts.push('success', `Saved repository ${editing.name}`);
        onsaved?.(r);
        open = false;
      } else {
        const r = await b.addRepository(config(), verify);
        onsaved?.(r);
        if (r.test) added = r;
        else {
          toasts.push('success', `Added repository ${r.name}`);
          open = false;
        }
      }
    } catch (err) {
      error = api.errorMessage(err);
    } finally {
      busy = false;
    }
  }

  async function removeAgain() {
    if (!added) return;
    busy = true;
    try {
      await b.removeRepository(added.name);
      toasts.push('info', `Removed ${added.name}`, 'Its location was not changed.');
      onsaved?.(added);
      added = null;
    } catch (err) {
      error = api.errorMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<Modal
  bind:open
  title={isEdit
    ? `Edit repository ${editing?.name}`
    : added
      ? `Added ${added.name}`
      : 'Add repository'}
  width={560}
>
  {#if added?.test}
    <p class="intro">
      {added.test.ok
        ? 'The repository is registered and reachable.'
        : 'The repository is registered, but the connection test failed: it is shown as unreachable until a test passes. Fix the location or credentials outside Sparkles and test again, or remove it and add it with other settings.'}
    </p>
    <TestReportView report={added.test} />
  {:else}
    <form id="repo-form" class="form" onsubmit={submit}>
      <fieldset class="types" disabled={isEdit}>
        <legend>Type</legend>
        <div class="seg" role="radiogroup" aria-label="Repository type">
          {#each [['fs', 'Filesystem'], ['s3', 'S3'], ['gcs', 'GCS'], ['azure', 'Azure']].filter(([v]) => v === 'fs' || v === 's3' || v === type) as [v, label] (v)}
            <label class:sel={type === v}>
              <input type="radio" bind:group={type} value={v} />{label}
            </label>
          {/each}
        </div>
      </fieldset>
      <label class="field">
        Name
        <input
          class="input mono"
          bind:value={name}
          disabled={isEdit}
          placeholder="s3-main"
          autocomplete="off"
          spellcheck="false"
          aria-invalid={!!name && !nameOk}
        />
        {#if name && !nameOk}
          <span class="hint bad"
            >{taken.includes(name)
              ? 'This name is taken.'
              : 'Lowercase letters, digits, “_” and “-”, starting with a letter or digit.'}</span
          >
        {/if}
      </label>

      {#if type === 'gcs' || type === 'azure'}
        <p class="note warn">Experimental: not covered by the test suite yet.</p>
      {/if}

      {#if type === 'fs'}
        <label class="field">
          Path
          <input
            class="input mono"
            bind:value={path}
            disabled={isEdit}
            placeholder="/srv/backups/sparkles"
            aria-invalid={!!path && !locationOk}
          />
          <span class="hint"
            >An absolute local or network path (NFS, SMB), outside the server's data directory.</span
          >
        </label>
      {:else}
        <div class="grid2">
          <label class="field">
            {type === 'azure' ? 'Container' : 'Bucket'}
            <input
              class="input mono"
              bind:value={bucket}
              disabled={isEdit}
              placeholder="kg-backups"
            />
          </label>
          <label class="field">
            <span>Prefix <span class="faint">(optional)</span></span>
            <input
              class="input mono"
              bind:value={prefix}
              disabled={isEdit}
              placeholder="prod/sparkles"
            />
          </label>
        </div>
        {#if type === 's3'}
          <div class="grid2">
            <label class="field">
              <span>Region <span class="faint">(optional)</span></span>
              <input class="input mono" bind:value={region} placeholder="eu-central-1" />
            </label>
            <label class="field">
              <span>Endpoint <span class="faint">(MinIO, R2, Ceph …)</span></span>
              <input
                class="input mono"
                bind:value={endpoint}
                disabled={isEdit}
                placeholder="https://s3.example.org"
              />
            </label>
          </div>
          <div class="checks">
            <label><input type="checkbox" bind:checked={pathStyle} /> Path-style addressing</label>
            <label><input type="checkbox" bind:checked={allowHttp} /> Allow plain http://</label>
          </div>
          {#if httpEndpoint}
            <p class="note warn">
              {allowHttp
                ? 'Data and credentials go over the network unencrypted. Use this for a local MinIO only.'
                : 'An http:// endpoint needs “Allow plain http://”.'}
            </p>
          {/if}
          <div class="grid2">
            <label class="field">
              Server-side encryption
              <select class="select" bind:value={sse}>
                <option value="">None (the bucket's default)</option>
                <option value="AES256">SSE-S3 (AES256)</option>
                <option value="aws:kms">SSE-KMS</option>
              </select>
            </label>
            {#if sse === 'aws:kms'}
              <label class="field">
                KMS key id
                <input class="input mono" bind:value={kmsKeyId} placeholder="arn:aws:kms:…" />
              </label>
            {/if}
          </div>
        {/if}

        <fieldset class="creds">
          <legend>Credentials</legend>
          <label class="field">
            Credential source
            <input
              class="input mono"
              bind:value={credName}
              placeholder="s3-main"
              autocomplete="off"
            />
          </label>
          <p class="hint">
            The name of a source the server's operator defined in its backup config file (<span
              class="mono">[credentials.&lt;name&gt;]</span
            >: environment variables, a file or the default chain). Sparkles never stores secrets,
            and repositories added here cannot pick the server's variables or files themselves.
          </p>
        </fieldset>
      {/if}

      <details>
        <summary>Advanced</summary>
        <div class="adv">
          <label class="check">
            <input type="checkbox" bind:checked={readonly} /> Read-only
            <span class="faint"
              >list, restore and verify only; never writes, not even locks (for restoring what
              another server writes)</span
            >
          </label>
          <label class="check">
            <input type="checkbox" bind:checked={conditionalWrites} /> Conditional writes
            <span class="faint">off for services without If-None-Match: then one writer only</span>
          </label>
          <div class="grid3">
            <label class="field">
              Parallel requests
              <input
                class="input"
                type="number"
                min="1"
                max="64"
                bind:value={maxConcurrency}
                placeholder={type === 'fs' ? '4' : '8'}
              />
            </label>
            <label class="field">
              Upload limit (MiB/s)
              <input
                class="input"
                type="number"
                min="0"
                step="any"
                bind:value={upMbps}
                placeholder="unlimited"
              />
            </label>
            <label class="field">
              Download limit (MiB/s)
              <input
                class="input"
                type="number"
                min="0"
                step="any"
                bind:value={downMbps}
                placeholder="unlimited"
              />
            </label>
          </div>
        </div>
      </details>

      {#if !isEdit}
        <label class="check">
          <input type="checkbox" bind:checked={verify} /> Test the connection
          <span class="faint">(the repository is added even when the test fails)</span>
        </label>
      {/if}
      {#if error}<div class="error-box">{error}</div>{/if}
    </form>
  {/if}
  {#snippet actions()}
    {#if added}
      {#if !added.test?.ok}
        <button class="btn danger" onclick={removeAgain} disabled={busy}>Remove it</button>
      {/if}
      <button class="btn primary" onclick={() => (open = false)}>
        {added.test?.ok ? 'Done' : 'Keep it'}
      </button>
    {:else}
      <button class="btn" type="button" onclick={() => (open = false)}>Cancel</button>
      <button class="btn primary" type="submit" form="repo-form" disabled={busy || !valid}>
        {#if busy}<span class="spinner"></span>{/if}
        {isEdit ? 'Save' : verify ? 'Add and test' : 'Add'}
      </button>
    {/if}
  {/snippet}
</Modal>

<style>
  .form {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
  }
  .intro {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .grid2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .grid3 {
    display: grid;
    grid-template-columns: 1fr 1fr 1fr;
    gap: 10px;
  }
  .hint {
    font-weight: 400;
    color: var(--text-3);
    font-size: var(--fs-sm);
    margin: 0;
  }
  .hint.bad {
    color: var(--danger);
  }
  fieldset {
    border: 0;
    padding: 0;
    margin: 0;
    min-width: 0;
  }
  legend {
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 500;
    margin-bottom: 4px;
    padding: 0;
  }
  .creds {
    display: grid;
    gap: 8px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
  }
  .seg {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .seg label {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 28px;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    font-size: var(--fs-sm);
    cursor: pointer;
  }
  .seg label.sel {
    border-color: var(--iri);
    background: color-mix(in srgb, var(--iri) 8%, transparent);
  }
  .seg input,
  .check input,
  .checks input {
    margin: 0;
    accent-color: var(--iri);
  }
  .checks {
    display: flex;
    gap: 16px;
    font-size: var(--fs-sm);
  }
  .checks label,
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
    flex-wrap: wrap;
  }
  .note {
    margin: 0;
    font-size: var(--fs-sm);
    padding: 6px 10px;
    border-radius: var(--r);
  }
  .note.warn {
    background: color-mix(in srgb, var(--warn) 12%, transparent);
    color: var(--warn);
  }
  details summary {
    cursor: pointer;
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 500;
  }
  .adv {
    display: grid;
    gap: 10px;
    padding-top: 10px;
  }
  @media (max-width: 560px) {
    .grid2,
    .grid3 {
      grid-template-columns: 1fr;
    }
  }
</style>
